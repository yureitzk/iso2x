use super::format::{
    FILE_SPLIT_POINT, HEADER_SIZE, INDEX_ALIGNMENT, SECTOR_SIZE, SIZING_BATCH_SECTORS,
    compress_sector_cci, compressed_written_len, output_name_for, serialize_cci_header,
};
use crate::core::extracted_fs::ExtractedFilesystem;
use crate::core::fs::SortedFsForSlbd;
use crate::core::iso::probe_source_over;
use crate::core::scrub::{self, ScrubMode};
use crate::core::source::{ImageSource, OwnedSourceReader, ProbedDirectoryTable, SourceReader};
use crate::core::writers::SliceWriter;
use crate::session::ChunkSource;
use std::collections::HashSet;
use std::io::{Read, Seek, SeekFrom};
use std::mem;
use xdvdfs::write::fs::{
    SectorLinearBlockDevice, SectorLinearBlockFilesystem, SectorLinearImage, XDVDFSFilesystem,
};
use xdvdfs::write::img::{NoOpProgressVisitor, create_xdvdfs_image};

/// Metadata for one output `.N.cci` file, computed once sizing finishes.
/// Each part is self-contained: header, then data, then its own index.
/// The header's `index_offset`/`uncompressed_size` aren't known until
/// every sector in the part has been sized.
struct CciPart {
    first_sector: u64,
    sector_count: u64,
    header_bytes: Vec<u8>,
    index_bytes: Vec<u8>,
    total_size: u64,
}

enum StreamStage {
    Header,
    Sectors,
    Index,
}

/// Where a `CciSession` reads its pre-compression sector bytes from.
/// `Rebuild` reauthors the source into a fresh XDVDFS image; `Direct`
/// streams straight from the source with no repack.
///
/// `slbfs` is boxed so the larger `Rebuild*` variants don't force every
/// `CciBackend` (including the small `Direct` one) to be that big.
enum CciBackend {
    Rebuild {
        slbfs: Box<SectorLinearBlockFilesystem<SortedFsForSlbd<OwnedSourceReader>>>,
        slbd: SectorLinearBlockDevice,
    },
    /// Same reauthor path as `Rebuild`, but sourced from an extracted-files
    /// directory instead of an open `ImageSource` - there's no raw XDVDFS
    /// stream to run `Direct` against in that case.
    RebuildFromExtracted {
        slbfs: Box<SectorLinearBlockFilesystem<ExtractedFilesystem>>,
        slbd: SectorLinearBlockDevice,
    },
    Direct {
        reader: OwnedSourceReader,
        /// Sectors (relative to `root_offset`) to zero instead of copying
        /// as-is. `None` for `ScrubMode::None`, and for `ScrubMode::Partial`
        /// on X360 images.
        zero_sectors: Option<HashSet<u64>>,
    },
}

pub(crate) struct CciSession {
    backend: CciBackend,
    total_data_sectors: u64,
    sizing_next_sector: u64,
    sizing_part_first_sector: u64,
    sizing_part_position: u64,
    sizing_part_index_infos: Vec<(u32, bool)>,
    sizing_done: bool,
    parts: Vec<CciPart>,
    stream_part: usize,
    stream_stage: StreamStage,
    stream_blob_pos: usize,
    stream_sector_cursor: u64,
    base_name: String,
    current_entry_name: String,
    output_manifest: Vec<(String, u64)>,
    /// Reused across both `hash_next_part` and `next_chunk` so `read_sector`
    /// never allocates a fresh `Vec` per sector - mirrors
    /// `PartState::read_scratch` in `formats::god::write`.
    sector_scratch: Vec<u8>,
}

impl CciSession {
    /// `probed`, when present, is a directory-tree walk a caller already
    /// did on this exact `source` - reused by `None`/`Partial` mode
    /// instead of walking again. `Full` mode never needs one.
    pub(crate) fn open(
        source: Box<dyn ImageSource>,
        base_name: String,
        mode: ScrubMode,
        probed: Option<ProbedDirectoryTable>,
    ) -> Result<Self, anyhow::Error> {
        let root_offset = source.image_offset();
        let (backend, total_data_sectors) = match mode {
            ScrubMode::Full => {
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
                let total_data_sectors = slbd.num_sectors();
                (
                    CciBackend::Rebuild {
                        slbfs: Box::new(slbfs),
                        slbd,
                    },
                    total_data_sectors,
                )
            }
            ScrubMode::None | ScrubMode::Partial => {
                let mut source = source;
                // Scoped so this borrow ends before `source` moves into
                // OwnedSourceReader below.
                let directory_table = if let Some(p) = probed {
                    p.directory_table
                } else {
                    let probe_reader = SourceReader::new(source.as_mut());
                    let detected = probe_source_over(probe_reader)
                        .map_err(|e| anyhow::anyhow!("cci: {e:#}"))?;
                    detected.directory_table
                };
                let total_size = root_offset + source.total_sectors() * SECTOR_SIZE;
                let mut reader = OwnedSourceReader::new(source);
                let (total_data_sectors, zero_sectors) = scrub::plan_direct(
                    mode,
                    &directory_table,
                    root_offset,
                    total_size,
                    &mut reader,
                )
                .map_err(|e| anyhow::anyhow!("cci: {e:#}"))?;
                // plan_direct's probe needs Cached mode; the bulk read
                // that follows is a single forward linear pass. Same
                // reasoning as in god::write::GodSession::open.
                reader.set_sequential_mode(true);
                (
                    CciBackend::Direct {
                        reader,
                        zero_sectors,
                    },
                    total_data_sectors,
                )
            }
        };
        // Placeholder name assuming a single output part. Overwritten
        // with the real part count once sizing finishes - see
        // `finish_sizing` / `output_name_for`.
        let current_entry_name = output_name_for(&base_name, 0, 1);
        Ok(Self {
            backend,
            total_data_sectors,
            sizing_next_sector: 0,
            sizing_part_first_sector: 0,
            sizing_part_position: 0,
            sizing_part_index_infos: Vec::new(),
            sizing_done: false,
            parts: Vec::new(),
            stream_part: 0,
            stream_stage: StreamStage::Header,
            stream_blob_pos: 0,
            stream_sector_cursor: 0,
            base_name,
            current_entry_name,
            output_manifest: Vec::new(),
            sector_scratch: Vec::new(),
        })
    }

    /// Extracted-source counterpart to `open()`. There's no raw XDVDFS
    /// byte stream to run `Direct` against here, but that's fine: a
    /// from-scratch rebuild has no leftover padding to trim or zero, so
    /// every scrub mode behaves like `Full`. `mode` is accepted but
    /// ignored.
    pub(crate) fn open_from_extracted(
        fs: ExtractedFilesystem,
        base_name: String,
        _mode: ScrubMode,
    ) -> Result<Self, anyhow::Error> {
        let mut slbfs = SectorLinearBlockFilesystem::new(fs);
        let mut slbd = SectorLinearBlockDevice::default();
        create_xdvdfs_image(&mut slbfs, &mut slbd, NoOpProgressVisitor)
            .map_err(|e| anyhow::anyhow!("create_xdvdfs_image: {e:?}"))?;
        let total_data_sectors = slbd.num_sectors();
        let backend = CciBackend::RebuildFromExtracted {
            slbfs: Box::new(slbfs),
            slbd,
        };
        // See the comment in `open()` - placeholder until sizing finishes.
        let current_entry_name = output_name_for(&base_name, 0, 1);
        Ok(Self {
            backend,
            total_data_sectors,
            sizing_next_sector: 0,
            sizing_part_first_sector: 0,
            sizing_part_position: 0,
            sizing_part_index_infos: Vec::new(),
            sizing_done: false,
            parts: Vec::new(),
            stream_part: 0,
            stream_stage: StreamStage::Header,
            stream_blob_pos: 0,
            stream_sector_cursor: 0,
            base_name,
            current_entry_name,
            output_manifest: Vec::new(),
            sector_scratch: Vec::new(),
        })
    }

    /// Reads one sector into `self.sector_scratch`, reused across calls -
    /// called once per sector from both `hash_next_part` and `next_chunk`.
    fn read_sector(&mut self, sector: u64) -> Result<(), anyhow::Error> {
        let sector_len = usize::try_from(SECTOR_SIZE).expect("SECTOR_SIZE fits in usize");
        match &mut self.backend {
            CciBackend::Rebuild { slbfs, slbd } => {
                let mut img = SectorLinearImage::new(slbd, slbfs.as_mut());
                let data = img
                    .read_linear(sector * SECTOR_SIZE, SECTOR_SIZE)
                    .map_err(|e| anyhow::anyhow!("{e:?}"))?;
                self.sector_scratch.clear();
                self.sector_scratch.extend_from_slice(data.as_slice());
            }
            CciBackend::RebuildFromExtracted { slbfs, slbd } => {
                let mut img = SectorLinearImage::new(slbd, slbfs.as_mut());
                let data = img
                    .read_linear(sector * SECTOR_SIZE, SECTOR_SIZE)
                    .map_err(|e| anyhow::anyhow!("{e:?}"))?;
                self.sector_scratch.clear();
                self.sector_scratch.extend_from_slice(data.as_slice());
            }
            CciBackend::Direct {
                reader,
                zero_sectors,
            } => {
                reader.seek(SeekFrom::Start(sector * SECTOR_SIZE))?;
                if self.sector_scratch.len() < sector_len {
                    self.sector_scratch.resize(sector_len, 0);
                }
                reader.read_exact(&mut self.sector_scratch[..sector_len])?;
                if zero_sectors
                    .as_ref()
                    .is_some_and(|zero| zero.contains(&sector))
                {
                    self.sector_scratch[..sector_len].fill(0);
                }
            }
        }
        Ok(())
    }

    fn check_and_manage_sizing_write(&mut self) -> Result<(), anyhow::Error> {
        // Split point matches Repackinator's 4,290,735,312-byte cap per
        // part, keeping files under the FATX limit with headroom for FTP
        // clients that round up: https://consolemods.org/wiki/Xbox:Repackinator
        //
        // This also keeps `finalize_sizing_part`'s `position >>
        // INDEX_ALIGNMENT` safely inside `u32` - see its doc comment.
        if self.sizing_part_position > FILE_SPLIT_POINT {
            self.finalize_sizing_part()?;
        }
        if self.sizing_part_index_infos.is_empty() && self.sizing_part_position == 0 {
            self.sizing_part_position = HEADER_SIZE;
        }
        Ok(())
    }

    /// # Errors
    ///
    /// Fails if `position >> INDEX_ALIGNMENT` overflows `u32` for any
    /// index entry. In practice this shouldn't happen: `FILE_SPLIT_POINT`
    /// keeps `position` well under `u32::MAX << INDEX_ALIGNMENT`, but
    /// that's enforced by control flow elsewhere rather than by the type
    /// system, so it's a real (if unreachable in normal use) error path.
    fn finalize_sizing_part(&mut self) -> Result<(), anyhow::Error> {
        let index_infos = mem::take(&mut self.sizing_part_index_infos);
        let sector_count = index_infos.len() as u64;
        let first_sector = self.sizing_part_first_sector;
        let mut position: u64 = HEADER_SIZE;
        let mut index_bytes = Vec::with_capacity(4 * (index_infos.len() + 1));
        for (value, compressed) in &index_infos {
            let idx = u32::try_from(position >> INDEX_ALIGNMENT).map_err(|_| {
                anyhow::anyhow!(
                    "cci: index position {position} overflows u32 after >> {INDEX_ALIGNMENT} \
                         - part exceeded the expected FILE_SPLIT_POINT-bounded size"
                )
            })? | if *compressed { 0x8000_0000 } else { 0 };
            index_bytes.extend_from_slice(&idx.to_le_bytes());
            position += u64::from(*value);
        }
        let index_offset = position;
        let index_end = u32::try_from(position >> INDEX_ALIGNMENT).map_err(|_| {
            anyhow::anyhow!(
                "cci: final index position {position} overflows u32 after >> {INDEX_ALIGNMENT}"
            )
        })?;
        index_bytes.extend_from_slice(&index_end.to_le_bytes());
        let uncompressed_size = sector_count * SECTOR_SIZE;
        let header_bytes = serialize_cci_header(uncompressed_size, index_offset);
        let total_size = index_offset + index_bytes.len() as u64;
        self.parts.push(CciPart {
            first_sector,
            sector_count,
            header_bytes,
            index_bytes,
            total_size,
        });
        self.sizing_part_first_sector = first_sector + sector_count;
        self.sizing_part_position = 0;
        Ok(())
    }

    fn finish_sizing(&mut self) {
        let total_parts = self.parts.len();
        self.output_manifest = self
            .parts
            .iter()
            .enumerate()
            .map(|(i, p)| {
                (
                    output_name_for(&self.base_name, i as u64, total_parts),
                    p.total_size,
                )
            })
            .collect();
        self.current_entry_name = output_name_for(&self.base_name, 0, total_parts);
        self.stream_part = 0;
        self.stream_stage = StreamStage::Header;
        self.stream_blob_pos = 0;
        self.sizing_done = true;
    }

    pub(crate) fn hash_next_part(&mut self) -> Result<bool, anyhow::Error> {
        if self.sizing_done {
            return Ok(true);
        }
        if self.total_data_sectors == 0 {
            self.sizing_part_position = HEADER_SIZE;
            self.finalize_sizing_part()?;
            self.finish_sizing();
            return Ok(true);
        }
        let batch_end =
            (self.sizing_next_sector + SIZING_BATCH_SECTORS).min(self.total_data_sectors);
        for sector in self.sizing_next_sector..batch_end {
            self.check_and_manage_sizing_write()?;
            self.read_sector(sector)?;
            let (compressed_data, is_compressed) = compress_sector_cci(&self.sector_scratch);
            let written_len = if is_compressed {
                compressed_written_len(compressed_data.len()).0
            } else {
                SECTOR_SIZE
            };
            self.sizing_part_index_infos.push((
                u32::try_from(written_len).expect("written_len fits in u32"),
                is_compressed,
            ));
            self.sizing_part_position += written_len;
        }
        self.sizing_next_sector = batch_end;
        if self.sizing_next_sector < self.total_data_sectors {
            return Ok(false);
        }
        self.finalize_sizing_part()?;
        self.finish_sizing();
        Ok(true)
    }
}

impl ChunkSource for CciSession {
    fn next_chunk(&mut self, max_bytes: usize) -> Result<Option<Vec<u8>>, anyhow::Error> {
        if !self.sizing_done {
            anyhow::bail!("cci: next_chunk() called before hash_next_part() finished sizing");
        }
        if self.stream_part >= self.parts.len() {
            return Ok(None);
        }
        self.current_entry_name =
            output_name_for(&self.base_name, self.stream_part as u64, self.parts.len());
        match self.stream_stage {
            StreamStage::Header => {
                let end = (self.stream_blob_pos + max_bytes)
                    .min(self.parts[self.stream_part].header_bytes.len());
                let chunk =
                    self.parts[self.stream_part].header_bytes[self.stream_blob_pos..end].to_vec();
                self.stream_blob_pos = end;
                if self.stream_blob_pos >= self.parts[self.stream_part].header_bytes.len() {
                    self.stream_stage = StreamStage::Sectors;
                    self.stream_sector_cursor = self.parts[self.stream_part].first_sector;
                    self.stream_blob_pos = 0;
                }
                Ok(Some(chunk))
            }
            StreamStage::Sectors => {
                let part = &self.parts[self.stream_part];
                let end_sector = part.first_sector + part.sector_count;
                if self.stream_sector_cursor >= end_sector {
                    self.stream_stage = StreamStage::Index;
                    self.stream_blob_pos = 0;
                    return self.next_chunk(max_bytes);
                }
                // max_bytes is just an upper-bound hint, not a guarantee
                // that much data exists - reserving it verbatim risks a
                // multi-GB allocation that OOMs in WASM's limited linear
                // memory. Reserve only for what this call could actually
                // still emit: each remaining sector is at most one
                // SECTOR_SIZE plus the 1-byte padding-length prefix a
                // compressed slot adds.
                let remaining_sectors = end_sector - self.stream_sector_cursor;
                let per_sector_ceiling =
                    usize::try_from(SECTOR_SIZE).expect("SECTOR_SIZE fits in usize") + 1;
                let remaining_bytes_ceiling = usize::try_from(remaining_sectors)
                    .unwrap_or(usize::MAX)
                    .saturating_mul(per_sector_ceiling);
                let reserve = max_bytes.min(remaining_bytes_ceiling);
                let mut out = Vec::with_capacity(reserve);
                while self.stream_sector_cursor < end_sector && out.len() < max_bytes {
                    self.read_sector(self.stream_sector_cursor)?;
                    let (compressed_data, is_compressed) =
                        compress_sector_cci(&self.sector_scratch);
                    if is_compressed {
                        let (_written_len, padding) = compressed_written_len(compressed_data.len());
                        out.push(padding);
                        out.extend_from_slice(&compressed_data);
                        out.resize(out.len() + padding as usize, 0);
                    } else {
                        out.extend_from_slice(&self.sector_scratch);
                    }
                    self.stream_sector_cursor += 1;
                }
                Ok(Some(out))
            }
            StreamStage::Index => {
                let end = (self.stream_blob_pos + max_bytes)
                    .min(self.parts[self.stream_part].index_bytes.len());
                let chunk =
                    self.parts[self.stream_part].index_bytes[self.stream_blob_pos..end].to_vec();
                self.stream_blob_pos = end;
                if self.stream_blob_pos >= self.parts[self.stream_part].index_bytes.len() {
                    self.stream_part += 1;
                    self.stream_stage = StreamStage::Header;
                    self.stream_blob_pos = 0;
                }
                Ok(Some(chunk))
            }
        }
    }

    fn is_done(&self) -> bool {
        self.sizing_done && self.stream_part >= self.parts.len()
    }

    fn total_units(&self) -> u64 {
        self.total_data_sectors
    }

    fn current_entry_name(&self) -> Option<&str> {
        Some(&self.current_entry_name)
    }

    fn output_manifest(&self) -> Vec<(String, u64)> {
        self.output_manifest.clone()
    }
}

#[cfg(test)]
mod max_bytes_reservation_tests {
    use super::*;

    struct ZeroSource {
        sectors: u64,
    }
    impl ImageSource for ZeroSource {
        fn read_sector(&mut self, sector: u64, out: &mut [u8]) -> Result<(), anyhow::Error> {
            anyhow::ensure!(sector < self.sectors, "sector out of range");
            out.fill(0);
            Ok(())
        }
        fn read_bytes(&mut self, _offset: u64, out: &mut [u8]) -> Result<(), anyhow::Error> {
            out.fill(0);
            Ok(())
        }
        fn total_sectors(&self) -> u64 {
            self.sectors
        }
        fn image_offset(&self) -> u64 {
            0
        }
    }

    #[test]
    fn next_chunk_does_not_balloon_to_the_callers_raw_max_bytes_hint() {
        let sectors = 4u64;
        let source: Box<dyn ImageSource> = Box::new(ZeroSource { sectors });
        let reader = OwnedSourceReader::new(source);
        let mut session = CciSession {
            backend: CciBackend::Direct {
                reader,
                zero_sectors: None,
            },
            total_data_sectors: sectors,
            sizing_next_sector: sectors,
            sizing_part_first_sector: 0,
            sizing_part_position: 0,
            sizing_part_index_infos: Vec::new(),
            sizing_done: true,
            parts: vec![CciPart {
                first_sector: 0,
                sector_count: sectors,
                header_bytes: serialize_cci_header(
                    sectors * SECTOR_SIZE,
                    HEADER_SIZE + sectors * SECTOR_SIZE,
                ),
                index_bytes: Vec::new(),
                total_size: HEADER_SIZE + sectors * SECTOR_SIZE,
            }],
            stream_part: 0,
            stream_stage: StreamStage::Sectors,
            stream_blob_pos: 0,
            stream_sector_cursor: 0,
            base_name: "game".to_string(),
            current_entry_name: "game.cci".to_string(),
            output_manifest: Vec::new(),
            sector_scratch: Vec::new(),
        };
        let chunk = session
            .next_chunk(0x7fff_ffff)
            .expect("next_chunk should succeed")
            .expect("sectors remain, so a chunk should be returned");
        assert!(
            chunk.len() < 1_000_000,
            "chunk unexpectedly large ({} bytes) for a {sectors}-sector fixture - \
             looks like max_bytes leaked back into the reservation/return size",
            chunk.len()
        );
    }
}

#[cfg(test)]
mod abi_crossing_tests {
    use super::*;
    use crate::session::{ConversionSession, SessionInner};
    use wasm_bindgen_test::*;

    struct ZeroSource {
        sectors: u64,
    }
    impl ImageSource for ZeroSource {
        fn read_sector(&mut self, sector: u64, out: &mut [u8]) -> Result<(), anyhow::Error> {
            anyhow::ensure!(sector < self.sectors, "sector out of range");
            out.fill(0);
            Ok(())
        }
        fn read_bytes(&mut self, _offset: u64, out: &mut [u8]) -> Result<(), anyhow::Error> {
            out.fill(0);
            Ok(())
        }
        fn total_sectors(&self) -> u64 {
            self.sectors
        }
        fn image_offset(&self) -> u64 {
            0
        }
    }

    fn sizing_done_session(sectors: u64) -> CciSession {
        let source: Box<dyn ImageSource> = Box::new(ZeroSource { sectors });
        let reader = OwnedSourceReader::new(source);
        let header_bytes =
            serialize_cci_header(sectors * SECTOR_SIZE, HEADER_SIZE + sectors * SECTOR_SIZE);
        CciSession {
            backend: CciBackend::Direct {
                reader,
                zero_sectors: None,
            },
            total_data_sectors: sectors,
            sizing_next_sector: sectors,
            sizing_part_first_sector: 0,
            sizing_part_position: 0,
            sizing_part_index_infos: Vec::new(),
            sizing_done: true,
            parts: vec![CciPart {
                first_sector: 0,
                sector_count: sectors,
                header_bytes,
                index_bytes: Vec::new(),
                total_size: HEADER_SIZE + sectors * SECTOR_SIZE,
            }],
            stream_part: 0,
            stream_stage: StreamStage::Header,
            stream_blob_pos: 0,
            stream_sector_cursor: 0,
            base_name: "game".to_string(),
            current_entry_name: "game.cci".to_string(),
            output_manifest: vec![("game.cci".to_string(), HEADER_SIZE + sectors * SECTOR_SIZE)],
            sector_scratch: Vec::new(),
        }
    }

    #[wasm_bindgen_test]
    fn streams_full_output_through_the_real_wasm_bindgen_abi() {
        let sectors = 8u64;
        let session = sizing_done_session(sectors);
        let mut conversion = ConversionSession::new(SessionInner::Cci(session));

        assert!(
            conversion
                .hash_next_part()
                .expect("hash_next_part must not error on an already-sized session")
        );

        conversion
            .output_manifest()
            .expect("outputManifest must serialize through Ts<T> without error");

        let mut collected = Vec::new();
        loop {
            let chunk = conversion
                .next_chunk(4096)
                .expect("next_chunk must not error mid-stream");
            match chunk {
                Some(bytes) => {
                    collected.extend(bytes.to_vec());
                }
                None => break,
            }
        }
        assert!(conversion.is_done());

        assert!(!collected.is_empty());
        assert!(collected.len() as u64 <= HEADER_SIZE + sectors * (SECTOR_SIZE + 1));
        assert!(
            collected.len() as u64 >= HEADER_SIZE,
            "output must contain at least the header bytes"
        );
    }

    #[wasm_bindgen_test]
    fn sizing_overflow_surfaces_as_catchable_js_error() {
        let sectors = 1u64;
        let mut session = sizing_done_session(sectors);

        session.sizing_done = false;
        session.sizing_next_sector = 0;

        session.sizing_part_index_infos = vec![(u32::MAX, false); 6];

        session.sizing_part_position = (u64::from(u32::MAX) + 1) << u32::from(INDEX_ALIGNMENT);

        let mut conversion = ConversionSession::new(SessionInner::Cci(session));
        let result = conversion.hash_next_part();
        assert!(
            result.is_err(),
            "an index position that overflows u32 must surface as Err, not panic"
        );
    }
}
