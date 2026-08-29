use super::volume_descriptor::VolumeDescriptor;
use super::volume_descriptor::xdvdfs_offset_of;
use anyhow::Error;
use bitflags::bitflags;
use std::io::{Read, Seek};
use xdvdfs::blockdev::OffsetWrapper;
use xdvdfs::layout::{DirectoryEntryTable, DiskRegion};

/// Every directory entry on the disc, flattened. `path` is the full
/// `/`-joined path from root, computed once here so consumers don't
/// each re-derive it from a nested tree walk.
#[derive(Clone)]
pub struct DirectoryTable {
    pub root_sector: u32,
    pub root_size: u32,
    pub entries: Vec<DirectoryEntry>,
}

#[derive(Clone)]
pub struct DirectoryEntry {
    pub attributes: DirectoryEntryAttributes,
    /// Full path from root, e.g. `"media/audio/track.wma"`.
    pub path: String,
    pub name: String,
    /// For a file: its data's sector. For a directory: its own
    /// directory-table's sector (xdvdfs reuses the same dirent field for
    /// both, so this doubles as "where is this subdirectory's table"
    /// with no separate lookup needed).
    pub sector: u32,
    pub size: u32,
}

bitflags! {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct DirectoryEntryAttributes: u8 {
        const ARCHIVE = 0x20;
        const DIRECTORY = 0x10;
        const HIDDEN = 0x02;
        const NORMAL = 0x80;
        const READ_ONLY = 0x01;
        const SYSTEM = 0x04;
    }
}

impl DirectoryTable {
    /// Walks the whole tree, recursing into every subdirectory, using
    /// xdvdfs's `scan_dirent_tree` (one sequential pass per directory)
    /// rather than a random-access walk - matters for chunked/remote
    /// readers.
    pub fn read_root<R: Read + Seek + Send + Sync>(
        mut reader: R,
        volume: &VolumeDescriptor,
    ) -> Result<DirectoryTable, Error> {
        let root_region = DirectoryEntryTable {
            region: DiskRegion {
                sector: volume.root_directory_sector,
                size: volume.root_directory_size,
            },
        };

        if root_region.is_empty() {
            return Ok(DirectoryTable {
                root_sector: volume.root_directory_sector,
                root_size: volume.root_directory_size,
                entries: Vec::new(),
            });
        }

        let mut dev =
            OffsetWrapper::new_with_provided_offset(&mut reader, xdvdfs_offset_of(volume.iso_type));

        let mut entries = Vec::new();
        // (parent_path, region) stack - iterative rather than recursive
        // so traversal depth is bounded only by available heap, not
        // native call-stack size.
        let mut stack = vec![(String::new(), root_region)];
        // Guards against a crafted image where directories share a
        // starting sector, which would otherwise let the walk revisit
        // the same region forever and OOM. Never fires on real images.
        let mut visited_sectors = std::collections::HashSet::new();
        visited_sectors.insert(volume.root_directory_sector);
        while let Some((parent_path, region)) = stack.pop() {
            if region.is_empty() {
                continue;
            }
            // The cheap, sequential primitive - one bounded pass per
            // directory.
            let flat = region
                .scan_dirent_tree(&mut dev)
                .map_err(|e| anyhow::anyhow!("failed to scan XDVDFS directory: {e:?}"))?;

            for node in flat {
                let name = node
                    .name_str()
                    .map_err(|_| {
                        anyhow::anyhow!("directory entry name is not valid Windows-1252/UTF-8")
                    })?
                    .into_owned();
                let data = node.node.dirent;
                let path = if parent_path.is_empty() {
                    name.clone()
                } else {
                    format!("{parent_path}/{name}")
                };
                let is_dir = DirectoryEntryAttributes::from_bits_truncate(data.attributes.attrs())
                    .contains(DirectoryEntryAttributes::DIRECTORY);
                // A directory's sector/size double as its subdirectory
                // table's region - descend into it, unless already visited.
                if is_dir && data.data.size != 0 && visited_sectors.insert(data.data.sector) {
                    stack.push((
                        path.clone(),
                        DirectoryEntryTable {
                            region: DiskRegion {
                                sector: data.data.sector,
                                size: data.data.size,
                            },
                        },
                    ));
                }
                entries.push(DirectoryEntry {
                    // Bit-for-bit identical layout to xdvdfs's DirentAttributes
                    attributes: DirectoryEntryAttributes::from_bits_truncate(
                        data.attributes.attrs(),
                    ),
                    path,
                    name,
                    sector: data.data.sector,
                    size: data.data.size,
                });
            }
        }

        Ok(DirectoryTable {
            root_sector: volume.root_directory_sector,
            root_size: volume.root_directory_size,
            entries,
        })
    }

    /// Case-insensitive lookup by full `/`-joined path.
    pub fn find(&self, path: &str) -> Option<&DirectoryEntry> {
        self.entries
            .iter()
            .find(|e| e.path.eq_ignore_ascii_case(path))
    }
}

impl DirectoryEntry {
    pub fn is_directory(&self) -> bool {
        self.attributes
            .contains(DirectoryEntryAttributes::DIRECTORY)
    }
}
