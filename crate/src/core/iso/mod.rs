pub mod directory_table;
pub mod iso_type;
pub mod volume_descriptor;

use anyhow::Error;
pub use directory_table::DirectoryEntry;
pub use directory_table::DirectoryTable;
use std::io::{Read, Seek, SeekFrom};

pub use volume_descriptor::VolumeDescriptor;
pub const SECTOR_SIZE: u64 = 0x800;

pub struct IsoReader<R> {
    pub volume_descriptor: VolumeDescriptor,
    pub directory_table: DirectoryTable,
    /// `false` only when this reader came from `probe_staged` and the
    /// directory-tree walk didn't finish, so `directory_table.entries`
    /// is a definite undercount - `max_used_prefix_size()`'s result
    /// must not be trusted as a completeness bound when this is `false`.
    /// Always `true` for a reader built via `read()`.
    pub tree_fully_walked: bool,
    reader: R,
}

/// Cheap counterpart to `probe_source_over` for callers (`XisoSource::open`)
/// that only need the XDVDFS root offset, not a parsed `DirectoryTable`.
/// Stops after `VolumeDescriptor::read` - never calls
/// `DirectoryTable::read_root`, so it avoids the full directory-tree walk
/// that `inspect_source` performs anyway once the source is open.
pub(crate) fn probe_root_offset_over<R: Read + Seek + Send + Sync>(
    mut reader: R,
) -> Result<u64, Error> {
    let volume_descriptor = VolumeDescriptor::read(&mut reader)
        .map_err(|e| anyhow::anyhow!("failed to detect XDVDFS root offset: {e:?}"))?;
    Ok(volume_descriptor.root_offset)
}

/// Detects the XDVDFS root offset over any `Read + Seek`. Lets
/// `XisoSource::open_multi_part` probe a `MultiPartReader` (split xiso)
/// the same way a single-file source is probed. For real read/convert
/// access, unlike `probe_staged` below - a truncated file still fails.
pub(crate) fn probe_source_over<R: Read + Seek + Send + Sync>(
    reader: R,
) -> Result<IsoReader<R>, Error> {
    IsoReader::read(reader)
        .map_err(|e| anyhow::anyhow!("failed to detect XDVDFS root offset: {e:?}"))
}

impl<R> IsoReader<R> {
    pub fn max_used_prefix_size(&self) -> u64 {
        let root_end = u64::from(self.directory_table.root_sector) * SECTOR_SIZE
            + u64::from(self.directory_table.root_size);
        self.directory_table
            .entries
            .iter()
            .map(|entry| u64::from(entry.sector) * SECTOR_SIZE + u64::from(entry.size))
            .chain(std::iter::once(root_end))
            .max()
            .unwrap_or(root_end)
    }
}

impl<R: Read + Seek> IsoReader<R> {
    pub fn root(&mut self) -> Result<&mut R, Error> {
        self.reader
            .seek(SeekFrom::Start(self.volume_descriptor.root_offset))?;
        Ok(&mut self.reader)
    }

    pub fn entry(&mut self, path: &WindowsPath) -> Result<Option<&mut R>, Error> {
        let target = path.components.join("/");
        let entry = self.directory_table.find(&target);

        if let Some(entry) = entry {
            let position = self.volume_descriptor.root_offset
                + u64::from(entry.sector) * self.volume_descriptor.sector_size;

            self.reader.seek(SeekFrom::Start(position))?;

            Ok(Some(&mut self.reader))
        } else {
            Ok(None)
        }
    }
}

impl<R: Read + Seek + Send + Sync> IsoReader<R> {
    pub fn read(mut reader: R) -> Result<IsoReader<R>, Error> {
        let volume_descriptor = VolumeDescriptor::read(&mut reader)?;
        let directory_table = DirectoryTable::read_root(&mut reader, &volume_descriptor)?;

        Ok(IsoReader {
            volume_descriptor,
            directory_table,
            tree_fully_walked: true,
            reader,
        })
    }

    /// Like `read`, but distinguishes "no valid volume descriptor"
    /// (returns `None`) from "volume descriptor parsed, but the
    /// directory-tree walk ran out of bytes partway through" (still
    /// strong evidence of a truncated XDVDFS image - a split's part 1,
    /// or a corrupt dump). In the latter case `directory_table` holds
    /// whatever the walk collected and `tree_fully_walked` is `false`.
    ///
    /// Detection-only: callers that need to read sectors back out must
    /// go through `probe_source_over`/`read` instead.
    pub(crate) fn probe_staged(mut reader: R) -> Option<Self> {
        let Ok(volume_descriptor) = VolumeDescriptor::read(&mut reader) else {
            return None;
        };

        match DirectoryTable::read_root(&mut reader, &volume_descriptor) {
            Ok(directory_table) => Some(IsoReader {
                volume_descriptor,
                directory_table,
                tree_fully_walked: true,
                reader,
            }),
            Err(_) => Some(IsoReader {
                directory_table: DirectoryTable {
                    root_sector: volume_descriptor.root_directory_sector,
                    root_size: volume_descriptor.root_directory_size,
                    entries: Vec::new(),
                },
                volume_descriptor,
                tree_fully_walked: false,
                reader,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal, spec-correct XDVDFS image via `xdvdfs`'s own writer.
    fn valid_xiso_bytes() -> Vec<u8> {
        let mut memfs = xdvdfs::write::fs::MemoryFilesystem::default();
        memfs.create("/default.xbe", b"XBEH");

        let mut slbd = xdvdfs::write::fs::SectorLinearBlockDevice::default();
        let mut slbfs = xdvdfs::write::fs::SectorLinearBlockFilesystem::new(memfs);
        xdvdfs::write::img::create_xdvdfs_image(
            &mut slbfs,
            &mut slbd,
            xdvdfs::write::img::NoOpProgressVisitor,
        )
        .expect("building a minimal in-memory XDVDFS image cannot fail");

        let total_size = slbd.num_sectors() * SECTOR_SIZE;
        let mut img = xdvdfs::write::fs::SectorLinearImage::new(&slbd, &mut slbfs);
        img.read_linear(0, total_size)
            .expect("reading the freshly-built image back out cannot fail")
    }

    #[test]
    fn valid_xiso_bytes_probes_to_offset_zero() {
        let bytes = valid_xiso_bytes();
        let offset = probe_root_offset_over(std::io::Cursor::new(bytes.as_slice()))
            .expect("a freshly-built image should have a detectable root offset");
        assert_eq!(offset, iso_type::IsoType::Xsf.root_offset());
    }

    /// Same call `xiso_tree`'s fuzz target makes.
    #[test]
    fn valid_xiso_bytes_walks_via_probe_source_over() {
        let bytes = valid_xiso_bytes();
        let reader = probe_source_over(std::io::Cursor::new(bytes.as_slice()))
            .expect("a freshly-built image should fully parse");
        assert!(
            reader.directory_table.find("default.xbe").is_some(),
            "root directory table should contain the file written into the fixture"
        );
    }

    #[test]
    #[ignore = "writes fuzz corpus seed files; run explicitly with --ignored"]
    fn write_fuzz_corpus_seeds() {
        let bytes = valid_xiso_bytes();
        for target in ["xiso_probe_root_offset", "xiso_tree"] {
            let dir = format!("fuzz/corpus/{target}");
            std::fs::create_dir_all(&dir).expect("corpus directory should be creatable");
            std::fs::write(format!("{dir}/seed-minimal-xdvdfs"), &bytes)
                .expect("seed file should be writable");
        }
    }
}

#[derive(Clone, Debug)]
pub struct WindowsPath {
    pub components: Vec<String>,
}

/// Case-insensitive (ascii case, for simplicity). Uses `\` as separator.
impl<'a, S: Into<&'a str>> From<S> for WindowsPath {
    fn from(path: S) -> WindowsPath {
        let path: &'a str = path.into();

        WindowsPath {
            components: path
                .split('\\')
                .filter(|s| !s.is_empty())
                .map(String::from)
                .collect(),
        }
    }
}
