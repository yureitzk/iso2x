use super::format::{BLOCK_SIZE, BLOCKS_PER_PART, BLOCKS_PER_SUBPART};
use crate::core::reader::JsReader;
use crate::core::source::{self, ImageSource, SourcePart};
use crate::core::title::ContentType;
use crate::formats::stfs::{HeaderThumbnails, read_header_prefix, read_header_thumbnails};
use std::io::{Read, Seek, SeekFrom};

/// The read-side counterpart to `GodSession`: GOD's sector layout is a
/// pure function of its constants, so no header/index needs parsing.
/// `remap_sector` inverts the write-side layout (one master hash block,
/// then per subpart a subhash block plus up to `BLOCKS_PER_SUBPART` data
/// blocks, repeated `SUBPARTS_PER_PART` times per `Data%04d` file).
pub(crate) struct GodSource {
    /// One `Data%04d` file per GOD part, already sorted by name.
    parts: Vec<SourcePart>,
    sequential_window: usize,
    /// Only one part's `JsReader` is open at a time: no `read_sector`
    /// call ever straddles a part boundary, so one reader is enough.
    active: Option<(usize, JsReader)>,
    /// Remembered so a `JsReader` rebuilt in `reader_for` starts in the
    /// right mode instead of reverting to `Cached`. GOD's sector layout
    /// is read in strictly ascending order during a bulk conversion
    /// pass, so reading a GOD source is safe to mark sequential.
    sequential_mode: bool,
    total_sectors: u64,
    /// Content type declared in the source's own `CON`/`LIVE`/`PIRS`
    /// header, when one was supplied - `None` for a header-less GOD
    /// source or an unrecognized header value. Lets `inspect_source`
    /// report a re-signed package's real content type instead of always
    /// inferring it from the launch executable alone.
    header_content_type: Option<ContentType>,
    /// Thumbnail Image (0x171A) from the same optional header file,
    /// when present and valid PNG. See `ImageSource::header_thumbnail`.
    header_thumbnail: Option<Vec<u8>>,
    /// Title Thumbnail Image (0x571A), same conditions as
    /// `header_thumbnail`.
    header_title_thumbnail: Option<Vec<u8>>,
}

impl GodSource {
    /// `header` is the package's `CON`/`LIVE`/`PIRS` header file, if the
    /// caller has one available (a re-signed or already-installed GOD
    /// package). It's entirely optional and doesn't participate in sector
    /// mapping - `remap_sector` only ever addresses into `parts`.
    pub(crate) fn open(
        parts: Vec<SourcePart>,
        sequential_window: usize,
        header: Option<SourcePart>,
    ) -> Result<Self, anyhow::Error> {
        anyhow::ensure!(
            !parts.is_empty(),
            "god: at least one Data%04d part is required"
        );
        let num_parts = parts.len() as u64;
        let last_size = parts.last().unwrap().size;
        anyhow::ensure!(
            last_size >= BLOCK_SIZE,
            "god: last part ({last_size} bytes) is smaller than one block"
        );
        // Last part's data-block count = its block count minus its own
        // hash-table blocks; every earlier part is necessarily full
        // (BLOCKS_PER_PART data blocks each).
        let total_blocks_last = last_size / BLOCK_SIZE;
        let hash_table_blocks = (total_blocks_last - 1) / (BLOCKS_PER_SUBPART + 1);
        let data_blocks_last = total_blocks_last - 1 - hash_table_blocks;
        let total_data_blocks = data_blocks_last + BLOCKS_PER_PART * (num_parts - 1);
        let total_sectors = total_data_blocks * BLOCK_SIZE / source::SECTOR_SIZE;

        let (header_content_type, header_thumbnail, header_title_thumbnail) = match header {
            Some(part) => {
                // One-off local reader, parsed once for the header
                // prefix/thumbnails and then dropped - it never gets
                // handed to anything that calls set_sequential_mode, so
                // there's no Sequential-mode window to size here. Just
                // the fixed internal cache block size.
                let mut reader = JsReader::new(part.read_fn, part.size);
                let content_type = read_header_prefix(&mut reader)?.content_type;
                let HeaderThumbnails {
                    thumbnail,
                    title_thumbnail,
                } = read_header_thumbnails(&mut reader);
                (content_type, thumbnail, title_thumbnail)
            }
            None => (None, None, None),
        };

        Ok(Self {
            parts,
            sequential_window,
            active: None,
            sequential_mode: false,
            total_sectors,
            header_content_type,
            header_thumbnail,
            header_title_thumbnail,
        })
    }

    /// Inverts the GOD write layout: given an XDVDFS sector, returns the
    /// `Data%04d` file index and byte offset within it.
    fn remap_sector(&self, xiso_sector: u64) -> Result<(usize, u64), anyhow::Error> {
        let block_num = (xiso_sector * source::SECTOR_SIZE) / BLOCK_SIZE;
        let file_index = (block_num / BLOCKS_PER_PART) as usize;
        let block_in_file = block_num % BLOCKS_PER_PART;
        let subpart_index = block_in_file / BLOCKS_PER_SUBPART;
        anyhow::ensure!(
            file_index < self.parts.len(),
            "god: sector {xiso_sector} maps to part {file_index}, but only {} part(s) were supplied",
            self.parts.len()
        );
        let offset = BLOCK_SIZE                                 // master hash table
            + (subpart_index + 1) * BLOCK_SIZE                  // subhash table blocks
            + block_in_file * BLOCK_SIZE                        // preceding data blocks
            + (xiso_sector * source::SECTOR_SIZE) % BLOCK_SIZE; // offset within block
        Ok((file_index, offset))
    }

    fn reader_for(&mut self, idx: usize) -> &mut JsReader {
        if !matches!(&self.active, Some((active_idx, _)) if *active_idx == idx) {
            let part = &self.parts[idx];
            let mut reader = JsReader::new(part.read_fn.clone(), part.size);
            // Sizes the Sequential-mode readahead window - a GOD source
            // is read in strictly ascending sector order during a bulk
            // conversion pass (see the `sequential_mode` field doc
            // comment above), so that window is what actually matters
            // here; the scattered-read cache uses a fixed internal
            // block size regardless.
            reader.set_sequential_window(self.sequential_window);
            reader.set_sequential_mode(self.sequential_mode);
            self.active = Some((idx, reader));
        }
        &mut self
            .active
            .as_mut()
            .expect("just set to Some(...) above if it wasn't already the active entry")
            .1
    }
}

impl ImageSource for GodSource {
    fn set_sequential_mode(&mut self, enabled: bool) {
        self.sequential_mode = enabled;
        if let Some((_, reader)) = &mut self.active {
            reader.set_sequential_mode(enabled);
        }
    }

    fn read_sector(&mut self, sector: u64, out: &mut [u8]) -> Result<(), anyhow::Error> {
        let (file_index, offset) = self.remap_sector(sector)?;
        let reader = self.reader_for(file_index);
        reader.seek(SeekFrom::Start(offset))?;
        reader.read_exact(out)?;
        Ok(())
    }

    /// Arbitrary, not necessarily sector-aligned, byte range (needed for
    /// directory-tree walking). Walks sector-by-sector, copying each
    /// sector's relevant slice into `out`.
    fn read_bytes(&mut self, offset: u64, out: &mut [u8]) -> Result<(), anyhow::Error> {
        if out.is_empty() {
            return Ok(());
        }
        let start_sector = offset / source::SECTOR_SIZE;
        let end_sector = (offset + out.len() as u64 - 1) / source::SECTOR_SIZE;
        let mut sector_buf = vec![
            0u8;
            usize::try_from(source::SECTOR_SIZE)
                .expect("SECTOR_SIZE is a small compile-time constant")
        ];
        let mut written = 0usize;
        for sector in start_sector..=end_sector {
            self.read_sector(sector, &mut sector_buf)?;
            let sector_start = sector * source::SECTOR_SIZE;
            let copy_start = offset.max(sector_start) - sector_start;
            let copy_end =
                (offset + out.len() as u64).min(sector_start + source::SECTOR_SIZE) - sector_start;
            let (copy_start, copy_end) = (
                usize::try_from(copy_start).expect("bounded by SECTOR_SIZE, which fits in usize"),
                usize::try_from(copy_end).expect("bounded by SECTOR_SIZE, which fits in usize"),
            );
            let n = copy_end - copy_start;
            out[written..written + n].copy_from_slice(&sector_buf[copy_start..copy_end]);
            written += n;
        }
        Ok(())
    }

    fn total_sectors(&self) -> u64 {
        self.total_sectors
    }

    /// GOD stores only the XDVDFS payload - no magic-offset scan needed.
    fn image_offset(&self) -> u64 {
        0
    }

    fn content_type_override(&self) -> Option<ContentType> {
        self.header_content_type
    }

    fn header_thumbnail(&self) -> Option<&[u8]> {
        self.header_thumbnail.as_deref()
    }

    fn header_title_thumbnail(&self) -> Option<&[u8]> {
        self.header_title_thumbnail.as_deref()
    }
}
