use super::format::{SPLIT_MARGIN, SPLIT_MARGIN_SECTORS};
use crate::core::extracted_fs::ExtractedFilesystem;
use crate::core::fs::SortedFsForSlbd;
use crate::core::iso::probe_source_over;
use crate::core::scrub::{self, SECTOR_SIZE};
use crate::core::source::{ImageSource, OwnedSourceReader, ProbedDirectoryTable, SourceReader};
use crate::core::writers::SliceWriter;
use crate::session::ChunkSource;
use serde::Deserialize;
use std::collections::HashSet;
use std::io::{Read, Seek, SeekFrom};
use tsify::Tsify;
use wasm_bindgen::prelude::*;
use xdvdfs::write::fs::{
    SectorLinearBlockDevice, SectorLinearBlockFilesystem, SectorLinearImage, XDVDFSFilesystem,
};
use xdvdfs::write::img::{NoOpProgressVisitor, create_xdvdfs_image};

/// Write mode for xiso output. `Trim` and `Zero` are independent,
/// single-axis, and never combined; `Full` is a complete XDVDFS reauthor.
/// Wire values are lowercase strings via
/// `#[serde(rename_all = "camelCase")]`.
#[derive(Deserialize, Default, Clone, Copy, Tsify)]
#[serde(rename_all = "camelCase")]
pub enum XisoMode {
    /// Full reauthor via a fresh XDVDFS rebuild. Slowest, but independent
    /// of source layout.
    #[default]
    Full,
    /// Cuts trailing padding after the last used byte; nothing is
    /// zeroed, so interior gaps pass through untouched.
    Trim,
    /// Zeroes unused sectors in place without trimming; total size
    /// matches the source (Xbox 360 images are an exception: still
    /// trimmed but not zeroed).
    Zero,
}

enum XisoBackend {
    /// Full reauthor - new XDVDFS image built from `SortedFsForSlbd`,
    /// which wraps an `OwnedSourceReader` rather than a raw `JsReader` so
    /// it can read through *any* opened `ImageSource`, not just a raw
    /// XISO.
    ///
    /// `slbfs` is boxed since `SectorLinearBlockFilesystem<SortedFsForSlbd<..>>`
    /// is large enough to otherwise blow up the size of every
    /// `XisoBackend` value, including the much smaller `Direct` variant.
    Rebuild {
        slbfs: Box<SectorLinearBlockFilesystem<SortedFsForSlbd<OwnedSourceReader>>>,
        slbd: SectorLinearBlockDevice,
    },
    /// Same reauthor path as `Rebuild`, sourced from an extracted-files
    /// directory instead of an open `ImageSource` - the only way to
    /// produce an xiso from an extracted source, since there's no raw
    /// XDVDFS byte stream to `Trim`/`Zero` from in that case.
    RebuildFromExtracted {
        slbfs: Box<SectorLinearBlockFilesystem<ExtractedFilesystem>>,
        slbd: SectorLinearBlockDevice,
    },
    /// Trim and Zero stream straight from the source at the detected
    /// XDVDFS root offset - no repack, no resort. They differ only in
    /// `total_sectors` and whether non-file sectors get zeroed. `reader`
    /// is already root-relative by construction.
    Direct {
        reader: OwnedSourceReader,
        /// Sectors (relative to `root_offset`) to zero instead of
        /// passing through untouched. `None` for Trim, which only cuts
        /// trailing padding after the last used byte - sectors within
        /// that range (e.g. alignment gaps between files) still pass
        /// through unmodified.
        zero_sectors: Option<HashSet<u64>>,
    },
}

pub(crate) struct XisoSession {
    backend: XisoBackend,
    total_sectors: u64,
    current_sector: u64,
    sectors_per_chunk: u32,
    /// Filename stem split parts are named after, e.g. "game" produces
    /// "game.1.xiso.iso", "game.2.xiso.iso", ... `None` when the `split`
    /// option is off: the session then behaves as one anonymous stream,
    /// `current_entry_name()` returns `None`, and `output_manifest()` is
    /// empty.
    base_name: Option<String>,
    /// Name of the split file the chunk most recently returned by
    /// `next_chunk` belongs to. Only ever `Some` when `base_name.is_some()`.
    current_entry_name: Option<String>,
    /// (name, size) per split part, populated up front in `finish()` -
    /// total output size is already known at `open()`, so no sizing pass
    /// is needed. Empty when splitting is off.
    output_manifest: Vec<(String, u64)>,
}

/// "game.{index+1}.xiso.iso" - one-indexed, with the `.xiso.iso`
/// extension (rather than a bare `.iso`) so split parts are recognizable
/// as xiso files rather than a raw, unprocessed ISO dump.
fn split_name_for(base_name: &str, index: u64) -> String {
    format!("{base_name}.{}.xiso.iso", index + 1)
}

/// Builds the full (name, size) manifest from the total xiso output size.
/// Callable immediately at `open()` since `total_sectors` is already known.
fn build_output_manifest_for(base_name: &str, total_size: u64) -> Vec<(String, u64)> {
    if total_size == 0 {
        return vec![(split_name_for(base_name, 0), 0)];
    }
    let mut manifest = Vec::new();
    let mut remaining = total_size;
    let mut index = 0u64;
    while remaining > 0 {
        let part_size = remaining.min(SPLIT_MARGIN);
        manifest.push((split_name_for(base_name, index), part_size));
        remaining -= part_size;
        index += 1;
    }
    manifest
}

impl XisoSession {
    /// `source` arrives already opened - probing happens once, upstream,
    /// regardless of which target format is being built. `probed`, when
    /// present, is a directory-tree walk a caller already did on this
    /// exact source (inspection, or `OpenedSource`) - `Trim`/`Zero` mode
    /// reuse it instead of walking again; `Full` mode never needs one.
    pub(crate) fn open(
        source: Box<dyn ImageSource>,
        mode: XisoMode,
        sectors_per_chunk: u32,
        split: bool,
        output_name: Option<String>,
        probed: Option<ProbedDirectoryTable>,
    ) -> Result<Self, anyhow::Error> {
        if split && output_name.is_none() {
            anyhow::bail!("xiso: split requires an outputName");
        }
        let sectors_per_chunk = sectors_per_chunk.max(1);
        let root_offset = source.image_offset();
        let source_total_size = root_offset + source.total_sectors() * SECTOR_SIZE;
        match mode {
            XisoMode::Full => {
                let reader = OwnedSourceReader::new(source);
                let inner_fs = XDVDFSFilesystem::<
                    OwnedSourceReader,
                    SliceWriter,
                    xdvdfs::write::fs::DefaultCopier<OwnedSourceReader, SliceWriter>,
                >::new(reader)
                .ok_or_else(|| anyhow::anyhow!("failed to open XDVDFS filesystem"))?;
                let sorted = SortedFsForSlbd(inner_fs);
                let mut slbfs = SectorLinearBlockFilesystem::new(sorted);
                let mut slbd = SectorLinearBlockDevice::default();
                create_xdvdfs_image(&mut slbfs, &mut slbd, NoOpProgressVisitor)
                    .map_err(|e| anyhow::anyhow!("create_xdvdfs_image: {e:?}"))?;
                let total_sectors = slbd.num_sectors();
                Ok(Self::finish(
                    XisoBackend::Rebuild {
                        slbfs: Box::new(slbfs),
                        slbd,
                    },
                    total_sectors,
                    sectors_per_chunk,
                    split,
                    output_name,
                ))
            }
            XisoMode::Trim | XisoMode::Zero => {
                let mut source = source;
                // `probed`, when present, saves the walk this block
                // would otherwise do itself. Scoped so the borrow (in
                // the `else` branch) ends before `source` moves into
                // `OwnedSourceReader` below.
                let directory_table = if let Some(p) = probed {
                    p.directory_table
                } else {
                    let probe_reader = SourceReader::new(source.as_mut());
                    let detected = probe_source_over(probe_reader)
                        .map_err(|e| anyhow::anyhow!("xiso: {e:#}"))?;
                    detected.directory_table
                };
                let scrub_info = scrub::scan(&directory_table, root_offset, source_total_size)
                    .map_err(|e| anyhow::anyhow!("xiso: {e:#}"))?;
                let (total_sectors, zero_sectors) = match mode {
                    XisoMode::Trim => {
                        let trimmed_len = scrub_info.max_end - root_offset;
                        (trimmed_len.div_ceil(SECTOR_SIZE), None)
                    }
                    XisoMode::Zero => {
                        // div_ceil, not floor: Zero must preserve a
                        // trailing partial sector rather than drop it,
                        // since it's meant to keep the original size
                        // intact.
                        let full_sectors = (source_total_size - root_offset).div_ceil(SECTOR_SIZE);
                        // Xbox 360 images are only ever trimmed, not
                        // zeroed interior-to-interior, so on X360 this
                        // degenerates to a same-size passthrough.
                        let zero_sectors = if scrub_info.platform == scrub::Platform::X360 {
                            None
                        } else {
                            Some(
                                (0..full_sectors)
                                    .filter(|s| !scrub_info.used_sectors.contains(s))
                                    .collect(),
                            )
                        };
                        (full_sectors, zero_sectors)
                    }
                    XisoMode::Full => unreachable!(),
                };
                let mut reader = OwnedSourceReader::new(source);
                // The directory-table probe above ran over `probe_reader`
                // (a separate borrow), so `reader` here has done no reads
                // yet - safe to mark it Sequential before the single
                // forward linear pass that follows (XisoBackend::read_range).
                reader.set_sequential_mode(true);
                Ok(Self::finish(
                    XisoBackend::Direct {
                        reader,
                        zero_sectors,
                    },
                    total_sectors,
                    sectors_per_chunk,
                    split,
                    output_name,
                ))
            }
        }
    }

    /// Extracted-source counterpart to `open()`. `mode` is accepted but
    /// ignored: an extracted-files directory has no raw XDVDFS byte
    /// stream to `Trim`/`Zero` from, but also has no leftover padding to
    /// trim or zero in the first place, so every mode is equivalent to
    /// `Full` here.
    pub(crate) fn open_from_extracted(
        fs: ExtractedFilesystem,
        _mode: XisoMode,
        sectors_per_chunk: u32,
        split: bool,
        output_name: Option<String>,
    ) -> Result<Self, anyhow::Error> {
        if split && output_name.is_none() {
            anyhow::bail!("xiso: split requires an outputName");
        }
        let sectors_per_chunk = sectors_per_chunk.max(1);
        let mut slbfs = SectorLinearBlockFilesystem::new(fs);
        let mut slbd = SectorLinearBlockDevice::default();
        create_xdvdfs_image(&mut slbfs, &mut slbd, NoOpProgressVisitor)
            .map_err(|e| anyhow::anyhow!("create_xdvdfs_image: {e:?}"))?;
        let total_sectors = slbd.num_sectors();
        Ok(Self::finish(
            XisoBackend::RebuildFromExtracted {
                slbfs: Box::new(slbfs),
                slbd,
            },
            total_sectors,
            sectors_per_chunk,
            split,
            output_name,
        ))
    }

    /// Shared tail end of `open()` for every mode - computes the
    /// split-related fields once `total_sectors` is known.
    fn finish(
        backend: XisoBackend,
        total_sectors: u64,
        sectors_per_chunk: u32,
        split: bool,
        output_name: Option<String>,
    ) -> Self {
        let base_name = if split { output_name } else { None };
        let output_manifest = match &base_name {
            Some(name) => build_output_manifest_for(name, total_sectors * SECTOR_SIZE),
            None => Vec::new(),
        };
        let current_entry_name = base_name.as_ref().map(|name| split_name_for(name, 0));
        Self {
            backend,
            total_sectors,
            current_sector: 0,
            sectors_per_chunk,
            base_name,
            current_entry_name,
            output_manifest,
        }
    }
}

impl ChunkSource for XisoSession {
    fn next_chunk(&mut self, max_bytes: usize) -> Result<Option<Vec<u8>>, anyhow::Error> {
        if self.current_sector >= self.total_sectors {
            return Ok(None);
        }
        let sector_size_usize = usize::try_from(SECTOR_SIZE)
            .map_err(|e| anyhow::anyhow!("SECTOR_SIZE does not fit in usize: {e}"))?;
        let max_sectors_for_bytes = (max_bytes / sector_size_usize).max(1) as u64;
        let mut sectors_per_chunk = u64::from(self.sectors_per_chunk).min(max_sectors_for_bytes);
        let start = self.current_sector;
        if let Some(base_name) = &self.base_name {
            // Clamp so this chunk never crosses a split boundary - exact,
            // since SPLIT_MARGIN is a whole number of sectors.
            let index = start / SPLIT_MARGIN_SECTORS;
            let boundary_sector = (index + 1) * SPLIT_MARGIN_SECTORS;
            sectors_per_chunk = sectors_per_chunk.min(boundary_sector - start);
            self.current_entry_name = Some(split_name_for(base_name, index));
        }
        let end = (start + sectors_per_chunk).min(self.total_sectors);
        let byte_len = usize::try_from((end - start) * SECTOR_SIZE)
            .map_err(|e| anyhow::anyhow!("chunk byte length does not fit in usize: {e}"))?;
        let data = match &mut self.backend {
            XisoBackend::Rebuild { slbfs, slbd } => {
                let mut img = SectorLinearImage::new(slbd, slbfs.as_mut());
                img.read_linear(start * SECTOR_SIZE, byte_len as u64)
                    .map_err(|e| anyhow::anyhow!("{e:?}"))?
                    .as_slice()
                    .to_vec()
            }
            XisoBackend::RebuildFromExtracted { slbfs, slbd } => {
                let mut img = SectorLinearImage::new(slbd, slbfs.as_mut());
                img.read_linear(start * SECTOR_SIZE, byte_len as u64)
                    .map_err(|e| anyhow::anyhow!("{e:?}"))?
                    .as_slice()
                    .to_vec()
            }
            XisoBackend::Direct {
                reader,
                zero_sectors,
            } => {
                reader.seek(SeekFrom::Start(start * SECTOR_SIZE))?;
                let mut buf = vec![0u8; byte_len];
                reader.read_exact(&mut buf)?;
                if let Some(zero) = zero_sectors {
                    for sector in start..end {
                        if zero.contains(&sector) {
                            let rel =
                                usize::try_from((sector - start) * SECTOR_SIZE).map_err(|e| {
                                    anyhow::anyhow!("sector offset does not fit in usize: {e}")
                                })?;
                            buf[rel..rel + sector_size_usize].fill(0);
                        }
                    }
                }
                buf
            }
        };
        self.current_sector = end;
        Ok(Some(data))
    }

    fn is_done(&self) -> bool {
        self.current_sector >= self.total_sectors
    }

    fn total_units(&self) -> u64 {
        self.total_sectors
    }

    fn current_entry_name(&self) -> Option<&str> {
        self.current_entry_name.as_deref()
    }

    fn output_manifest(&self) -> Vec<(String, u64)> {
        self.output_manifest.clone()
    }
}
