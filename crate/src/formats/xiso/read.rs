use crate::core::iso::probe_root_offset_over;
use crate::core::scrub::SECTOR_SIZE;
use crate::core::source::{ImageSource, MultiPartReader, SourcePart};
use js_sys::Function;
use std::io::{Read, Seek, SeekFrom};

/// `ImageSource` backed directly by a raw XISO/ISO file, possibly split
/// across multiple parts (`open_multi_part`). `probe_source_over` scans
/// for the XDVDFS volume descriptor (magic `MICROSOFT*XBOX*MEDIA` at
/// sector 32 of the game partition - see
/// `<https://xboxdevwiki.net/XDVDFS#Volume_descriptor>`) against whichever
/// `Read + Seek` it's handed, split or not.
///
/// `root_offset` isn't baked into `reader` as a base offset, since
/// `MultiPartReader` doesn't have one and part boundaries don't shift to
/// accommodate it - every read here adds `root_offset` in explicitly.
pub(crate) struct XisoSource {
    reader: MultiPartReader,
    root_offset: u64,
    total_sectors: u64,
}

impl XisoSource {
    pub(crate) fn open(
        read_fn: Function,
        file_size: u64,
        sequential_window: usize,
    ) -> Result<Self, anyhow::Error> {
        Self::open_multi_part(
            vec![SourcePart {
                name: String::new(),
                read_fn,
                size: file_size,
            }],
            sequential_window,
        )
    }

    pub(crate) fn open_multi_part(
        parts: Vec<SourcePart>,
        sequential_window: usize,
    ) -> Result<Self, anyhow::Error> {
        // `probe_root_offset_over` takes the reader by value, and a fresh one
        // is wanted afterward regardless of what state parsing left this
        // one in.
        let probe_reader = MultiPartReader::new(parts.clone(), sequential_window)?;
        // Offset-only probe: this never needs a DirectoryTable - only
        // root_offset, to compute total_sectors/image_offset() below.
        // inspect_source() walks the full tree separately, over this
        // source once it's open; doing that walk here too would be pure
        // duplicate work (see probe_root_offset_over's doc comment).
        let root_offset = probe_root_offset_over(probe_reader)?;
        let total_size: u64 = parts.iter().map(|p| p.size).sum();
        anyhow::ensure!(
            total_size >= root_offset,
            "xiso: total size ({total_size}) is smaller than the detected root offset ({root_offset})"
        );
        let reader = MultiPartReader::new(parts, sequential_window)?;
        let total_sectors = (total_size - root_offset) / SECTOR_SIZE;
        Ok(Self {
            reader,
            root_offset,
            total_sectors,
        })
    }
}

impl ImageSource for XisoSource {
    fn set_sequential_mode(&mut self, enabled: bool) {
        self.reader.set_sequential_mode(enabled);
    }

    fn read_sector(&mut self, sector: u64, out: &mut [u8]) -> Result<(), anyhow::Error> {
        self.read_bytes(sector * SECTOR_SIZE, out)
    }

    fn read_bytes(&mut self, offset: u64, out: &mut [u8]) -> Result<(), anyhow::Error> {
        self.reader
            .seek(SeekFrom::Start(self.root_offset + offset))?;
        self.reader.read_exact(out)?;
        Ok(())
    }

    fn total_sectors(&self) -> u64 {
        self.total_sectors
    }

    fn image_offset(&self) -> u64 {
        self.root_offset
    }
}
