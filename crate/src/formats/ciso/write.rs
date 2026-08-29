use super::format::{
    FILE_SPLIT_POINT, SECTOR_SIZE, SIZING_BATCH_SECTORS, compress_sector, output_name_for,
    padded_part_size, serialize_index_table,
};
use crate::core::extracted_fs::ExtractedFilesystem;
use crate::core::fs::SortedFsForSlbd;
use crate::core::iso::probe_source_over;
use crate::core::scrub::{self, ScrubMode};
use crate::core::source::{ImageSource, OwnedSourceReader, ProbedDirectoryTable, SourceReader};
use crate::core::writers::SliceWriter;
use crate::session::ChunkSource;
use anyhow::Context;
use arbitrary_int::u31;
use ciso::layout::{CSOHeader, IndexTableEntry};
use std::collections::HashSet;
use std::io::{Read, Seek, SeekFrom};
use xdvdfs::write::fs::{
    SectorLinearBlockDevice, SectorLinearBlockFilesystem, SectorLinearImage, XDVDFSFilesystem,
};
use xdvdfs::write::img::{NoOpProgressVisitor, create_xdvdfs_image};

/// Where a `CisoSession` reads its pre-compression sector bytes from:
/// `Rebuild` reauthors the source into a fresh XDVDFS image; `Direct`
/// streams straight from the source at the detected XDVDFS root offset
/// with no repack/resort. Compression happens identically for both
/// afterwards; only where the raw sector bytes come from differs.
///
/// `slbfs` is boxed to avoid making every `CisoBackend` value (including
/// small `Direct` ones) as large as the biggest variant.
enum CisoBackend {
    Rebuild {
        slbfs: Box<SectorLinearBlockFilesystem<SortedFsForSlbd<OwnedSourceReader>>>,
        slbd: SectorLinearBlockDevice,
    },
    RebuildFromExtracted {
        slbfs: Box<SectorLinearBlockFilesystem<ExtractedFilesystem>>,
        slbd: SectorLinearBlockDevice,
    },
    Direct {
        reader: OwnedSourceReader,
        zero_sectors: Option<HashSet<u64>>,
    },
}

pub(crate) struct CisoSession {
    backend: CisoBackend,
    header: CSOHeader,
    index_table: Vec<IndexTableEntry>,
    total_data_sectors: u64,
    sizing_next_sector: u64,
    sizing_position: u64,
    /// Logical position (same units as `sizing_position`) at which the
    /// *current* split part began. `sizing_position - sizing_part_start` is
    /// this part's length so far, and is the value actually recorded into
    /// each index-table entry, since positions must be relative to
    /// whichever physical file a sector lands in.
    sizing_part_start: u64,
    /// Padded on-disk size of each split part finished so far during
    /// sizing, in completion order. Names aren't assigned until sizing is
    /// fully done (see `output_name_for`) - the true part *count* isn't
    /// known until then, so this only tracks sizes; `output_manifest` is
    /// built from it in one pass once `sizing_done` is set.
    sizing_part_sizes: Vec<u64>,
    sizing_done: bool,
    /// Total number of split parts, set once sizing completes. `0` before
    /// then; `split_name`/placeholders must not be called with it unset.
    total_parts: usize,
    header_and_index: Vec<u8>,
    header_index_pos: usize,
    stream_next_sector: u64,
    stream_position: u64,
    /// Streaming-phase counterpart to `sizing_part_start`. Must reproduce
    /// the exact same sequence of part boundaries the sizing phase already
    /// computed (both are deterministic functions of the same sector byte
    /// stream, so they agree as long as the two loops stay in lockstep).
    stream_part_start: u64,
    /// 0-based index of the split part currently being streamed.
    stream_part_index: u64,
    /// Set once the sector just written completed the current part (either
    /// by crossing `FILE_SPLIT_POINT`, or by being the very last sector).
    /// The next call to `next_chunk` must emit that part's trailing pad
    /// (still under the outgoing name) before anything else goes out.
    stream_part_needs_padding: bool,
    base_name: String,
    current_entry_name: String,
    output_manifest: Vec<(String, u64)>,
    /// Reused across both `hash_next_part` and `next_chunk` so `read_sector`
    /// never allocates a fresh `Vec` per sector - mirrors
    /// `PartState::read_scratch` in `formats::god::write`.
    sector_scratch: Vec<u8>,
}

impl CisoSession {
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
                    CisoBackend::Rebuild {
                        slbfs: Box::new(slbfs),
                        slbd,
                    },
                    total_data_sectors,
                )
            }
            ScrubMode::None | ScrubMode::Partial => {
                let mut source = source;
                // Scoped (in the `else` branch) so this borrow ends
                // before `source` moves into `OwnedSourceReader` below.
                let directory_table = if let Some(p) = probed {
                    p.directory_table
                } else {
                    let probe_reader = SourceReader::new(source.as_mut());
                    let detected = probe_source_over(probe_reader)
                        .map_err(|e| anyhow::anyhow!("ciso: {e:#}"))?;
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
                .map_err(|e| anyhow::anyhow!("ciso: {e:#}"))?;
                // See the equivalent comment in god::write::GodSession::open -
                // plan_direct's probe needs Cached mode; the bulk read that
                // follows (via CisoBackend::read_range) is a single forward
                // linear pass.
                reader.set_sequential_mode(true);
                (
                    CisoBackend::Direct {
                        reader,
                        zero_sectors,
                    },
                    total_data_sectors,
                )
            }
        };

        let mut header = CSOHeader::new();
        header.uncompressed_size = total_data_sectors * SECTOR_SIZE;
        let index_table = vec![IndexTableEntry::default(); header.index_table_len()];
        let start_position = 24 + 4 * index_table.len() as u64;
        // Placeholder until sizing completes: assumes the common
        // single-part case. hash_next_part()'s finalization step
        // overwrites this once the real part count is known - see
        // `output_name_for`.
        let current_entry_name = output_name_for(&base_name, 0, 1);
        Ok(Self {
            backend,
            header,
            index_table,
            total_data_sectors,
            sizing_next_sector: 0,
            sizing_position: start_position,
            sizing_part_start: 0,
            sizing_part_sizes: Vec::new(),
            sizing_done: false,
            total_parts: 0,
            header_and_index: Vec::new(),
            header_index_pos: 0,
            stream_next_sector: 0,
            stream_position: start_position,
            stream_part_start: 0,
            stream_part_index: 0,
            stream_part_needs_padding: false,
            base_name,
            current_entry_name,
            output_manifest: Vec::new(),
            sector_scratch: Vec::new(),
        })
    }

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
        let backend = CisoBackend::RebuildFromExtracted {
            slbfs: Box::new(slbfs),
            slbd,
        };

        let mut header = CSOHeader::new();
        header.uncompressed_size = total_data_sectors * SECTOR_SIZE;
        let index_table = vec![IndexTableEntry::default(); header.index_table_len()];
        let start_position = 24 + 4 * index_table.len() as u64;
        // Placeholder until sizing completes - see the comment in `open()`.
        let current_entry_name = output_name_for(&base_name, 0, 1);
        Ok(Self {
            backend,
            header,
            index_table,
            total_data_sectors,
            sizing_next_sector: 0,
            sizing_position: start_position,
            sizing_part_start: 0,
            sizing_part_sizes: Vec::new(),
            sizing_done: false,
            total_parts: 0,
            header_and_index: Vec::new(),
            header_index_pos: 0,
            stream_next_sector: 0,
            stream_position: start_position,
            stream_part_start: 0,
            stream_part_index: 0,
            stream_part_needs_padding: false,
            base_name,
            current_entry_name,
            output_manifest: Vec::new(),
            sector_scratch: Vec::new(),
        })
    }

    fn align_padding(&self, position: u64) -> u64 {
        let align_b = 1u64 << self.header.alignment;
        let align_m = align_b - 1;
        let align = position & align_m;
        if align == 0 { 0 } else { align_b - align }
    }

    /// Reads one sector into `self.sector_scratch`, reused across calls -
    /// called once per sector from both `hash_next_part` and `next_chunk`.
    fn read_sector(&mut self, sector: u64) -> Result<(), anyhow::Error> {
        let sector_len =
            usize::try_from(SECTOR_SIZE).expect("SECTOR_SIZE is a small compile-time constant");
        match &mut self.backend {
            CisoBackend::Rebuild { slbfs, slbd } => {
                let mut img = SectorLinearImage::new(slbd, slbfs.as_mut());
                let data = img
                    .read_linear(sector * SECTOR_SIZE, SECTOR_SIZE)
                    .map_err(|e| anyhow::anyhow!("{e:?}"))?;
                self.sector_scratch.clear();
                self.sector_scratch.extend_from_slice(data.as_slice());
            }
            CisoBackend::RebuildFromExtracted { slbfs, slbd } => {
                let mut img = SectorLinearImage::new(slbd, slbfs.as_mut());
                let data = img
                    .read_linear(sector * SECTOR_SIZE, SECTOR_SIZE)
                    .map_err(|e| anyhow::anyhow!("{e:?}"))?;
                self.sector_scratch.clear();
                self.sector_scratch.extend_from_slice(data.as_slice());
            }
            CisoBackend::Direct {
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

    /// Name for split part `index`, using the final `total_parts` computed
    /// once sizing completes. Must only be called once `sizing_done` is
    /// true (i.e. `total_parts` has actually been set) - every call site in
    /// `next_chunk` is already gated behind that check.
    fn split_name(&self, index: u64) -> String {
        output_name_for(&self.base_name, index, self.total_parts)
    }

    /// Records the just-completed part's on-disk size (rounded up to
    /// `FILE_PADDING_MODULUS`) in `sizing_part_sizes`, then advances
    /// `sizing_part_start` so the next sector's position is relative to the
    /// new part.
    fn finish_sizing_part(&mut self) {
        let raw_len = self.sizing_position - self.sizing_part_start;
        self.sizing_part_sizes.push(padded_part_size(raw_len));
        self.sizing_part_start = self.sizing_position;
    }

    pub(crate) fn hash_next_part(&mut self) -> Result<bool, anyhow::Error> {
        if self.sizing_done {
            return Ok(true);
        }

        let batch_end =
            (self.sizing_next_sector + SIZING_BATCH_SECTORS).min(self.total_data_sectors);
        for sector in self.sizing_next_sector..batch_end {
            self.sizing_position += self.align_padding(self.sizing_position);

            if self.sizing_position - self.sizing_part_start >= FILE_SPLIT_POINT {
                self.finish_sizing_part();
            }

            let local_position = self.sizing_position - self.sizing_part_start;
            self.read_sector(sector)?;
            let (compressed, is_compressed) = compress_sector(&self.sector_scratch);
            let position = u32::try_from(local_position >> self.header.alignment).context(
                "ciso: image too large to represent in a CSO index table \
                 (aligned position overflowed u32)",
            )?;
            let sector_idx =
                usize::try_from(sector).context("ciso: sector index does not fit in usize")?;
            self.index_table[sector_idx] = IndexTableEntry::default()
                .with_position(u31::new(position))
                .with_compression_type(is_compressed);
            self.sizing_position += compressed.len() as u64;
        }
        self.sizing_next_sector = batch_end;

        if self.sizing_next_sector < self.total_data_sectors {
            return Ok(false);
        }

        let last = usize::try_from(self.total_data_sectors)
            .context("ciso: total sector count does not fit in usize")?;
        let last_local_position = self.sizing_position - self.sizing_part_start;
        let last_position = u32::try_from(last_local_position >> self.header.alignment).context(
            "ciso: image too large to represent in a CSO index table \
             (aligned position overflowed u32)",
        )?;
        self.index_table[last] = IndexTableEntry::default().with_position(u31::new(last_position));
        self.finish_sizing_part();

        // The true part count is only known now that every sector has been
        // sized - assign every part's final name in one pass (see
        // `output_name_for` and `sizing_part_sizes`'s doc comment).
        let total_parts = self.sizing_part_sizes.len();
        self.total_parts = total_parts;
        self.output_manifest = self
            .sizing_part_sizes
            .iter()
            .enumerate()
            .map(|(i, &size)| {
                (
                    output_name_for(&self.base_name, i as u64, total_parts),
                    size,
                )
            })
            .collect();

        let mut header_and_index = Vec::with_capacity(24 + 4 * self.index_table.len());
        header_and_index.extend_from_slice(&self.header.serialize());
        header_and_index.extend_from_slice(&serialize_index_table(&self.index_table));
        self.header_and_index = header_and_index;
        self.current_entry_name = self.split_name(0);
        self.sizing_done = true;
        Ok(true)
    }
}

impl ChunkSource for CisoSession {
    fn next_chunk(&mut self, max_bytes: usize) -> Result<Option<Vec<u8>>, anyhow::Error> {
        if !self.sizing_done {
            anyhow::bail!("ciso: next_chunk() called before hash_next_part() finished sizing");
        }

        if self.header_index_pos < self.header_and_index.len() {
            // Header + index table always live at the very start of part 0
            // and are well under FILE_SPLIT_POINT for any real image, so no
            // split handling is needed here.
            self.current_entry_name = self.split_name(0);
            let end = (self.header_index_pos + max_bytes).min(self.header_and_index.len());
            let chunk = self.header_and_index[self.header_index_pos..end].to_vec();
            self.header_index_pos = end;
            return Ok(Some(chunk));
        }

        // A previous call's sector finished the current part. Emit that
        // part's trailing pad - still under the outgoing name - before any
        // byte of the next part goes out.
        if self.stream_part_needs_padding {
            let part_len = self.stream_position - self.stream_part_start;
            let padded_len = padded_part_size(part_len);
            let pad_len = usize::try_from(padded_len - part_len)
                .context("ciso: trailing part padding does not fit in usize")?;
            self.stream_part_start = self.stream_position;
            self.stream_part_needs_padding = false;
            if self.stream_next_sector < self.total_data_sectors {
                self.stream_part_index += 1;
            }
            return Ok(Some(vec![0u8; pad_len]));
        }

        if self.stream_next_sector >= self.total_data_sectors {
            return Ok(None);
        }

        self.current_entry_name = self.split_name(self.stream_part_index);
        // max_bytes is an upper bound/hint, not a promise that much data
        // exists - reserving it verbatim risks a multi-GB OOM in WASM's
        // limited linear memory. Reserve only for the sectors this call
        // could actually still emit: each is at most one SECTOR_SIZE
        // plus 64 bytes of alignment slack.
        let remaining_sectors = self.total_data_sectors - self.stream_next_sector;
        let per_sector_ceiling =
            usize::try_from(SECTOR_SIZE).expect("SECTOR_SIZE fits in usize") + 64;
        let remaining_bytes_ceiling = usize::try_from(remaining_sectors)
            .unwrap_or(usize::MAX)
            .saturating_mul(per_sector_ceiling);
        let reserve = max_bytes.min(remaining_bytes_ceiling);
        let mut out = Vec::with_capacity(reserve);
        while self.stream_next_sector < self.total_data_sectors
            && out.len() < max_bytes
            && !self.stream_part_needs_padding
        {
            let pad = self.align_padding(self.stream_position);
            let pad_usize =
                usize::try_from(pad).context("ciso: alignment padding does not fit in usize")?;
            out.resize(out.len() + pad_usize, 0);
            self.stream_position += pad;

            self.read_sector(self.stream_next_sector)?;
            let (compressed, _is_compressed) = compress_sector(&self.sector_scratch);
            out.extend_from_slice(&compressed);
            self.stream_position += compressed.len() as u64;
            self.stream_next_sector += 1;

            let part_len = self.stream_position - self.stream_part_start;
            if part_len >= FILE_SPLIT_POINT || self.stream_next_sector >= self.total_data_sectors {
                // Mirrors hash_next_part's boundary check exactly, so the
                // two passes agree on where every part ends.
                self.stream_part_needs_padding = true;
            }
        }
        Ok(Some(out))
    }

    fn is_done(&self) -> bool {
        self.sizing_done
            && self.header_index_pos >= self.header_and_index.len()
            && self.stream_next_sector >= self.total_data_sectors
            && !self.stream_part_needs_padding
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

        let mut session = CisoSession {
            backend: CisoBackend::Direct {
                reader,
                zero_sectors: None,
            },
            header: CSOHeader::new(),
            index_table: Vec::new(),
            total_data_sectors: sectors,
            sizing_next_sector: sectors,
            sizing_position: 0,
            sizing_part_start: 0,
            sizing_part_sizes: Vec::new(),
            sizing_done: true,
            total_parts: 1,
            header_and_index: Vec::new(),
            header_index_pos: 0,
            stream_next_sector: 0,
            stream_position: 0,
            stream_part_start: 0,
            stream_part_index: 0,
            stream_part_needs_padding: false,
            base_name: "game".to_string(),
            current_entry_name: "game.cso".to_string(),
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
mod corpus_seed_tests {
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

    fn valid_ciso_bytes() -> Vec<u8> {
        let sectors = 4u64;
        let source: Box<dyn ImageSource> = Box::new(ZeroSource { sectors });
        let reader = OwnedSourceReader::new(source);

        let mut header = CSOHeader::new();
        header.uncompressed_size = sectors * SECTOR_SIZE;
        let index_table = vec![IndexTableEntry::default(); header.index_table_len()];
        let start_position = 24 + 4 * index_table.len() as u64;

        let mut session = CisoSession {
            backend: CisoBackend::Direct {
                reader,
                zero_sectors: None,
            },
            header,
            index_table,
            total_data_sectors: sectors,
            sizing_next_sector: 0,
            sizing_position: start_position,
            sizing_part_start: 0,
            sizing_part_sizes: Vec::new(),
            sizing_done: false,
            total_parts: 0,
            header_and_index: Vec::new(),
            header_index_pos: 0,
            stream_next_sector: 0,
            stream_position: start_position,
            stream_part_start: 0,
            stream_part_index: 0,
            stream_part_needs_padding: false,
            base_name: "game".to_string(),
            current_entry_name: "game.cso".to_string(),
            output_manifest: Vec::new(),
            sector_scratch: Vec::new(),
        };

        while !session
            .hash_next_part()
            .expect("sizing a 4-sector fixture cannot fail")
        {}

        let mut out = Vec::new();
        while let Some(chunk) = session
            .next_chunk(1 << 20)
            .expect("streaming a 4-sector fixture cannot fail")
        {
            out.extend_from_slice(&chunk);
        }
        assert!(session.is_done());
        out
    }

    #[test]
    fn valid_ciso_bytes_header_parses_with_expected_block_size() {
        let bytes = valid_ciso_bytes();
        let header_buf: [u8; 24] = bytes[0..24].try_into().expect("slice is exactly 24 bytes");
        let header = CSOHeader::deserialize::<anyhow::Error>(&header_buf)
            .expect("a freshly-built CISO header should parse");
        assert_eq!(u64::from(header.block_size), SECTOR_SIZE);
    }

    #[test]
    #[ignore = "writes fuzz corpus seed files; run explicitly with --ignored"]
    fn write_fuzz_corpus_seed_for_ciso_header() {
        let bytes = valid_ciso_bytes();
        let dir = "fuzz/corpus/ciso_header";
        std::fs::create_dir_all(dir).expect("corpus directory should be creatable");
        std::fs::write(format!("{dir}/seed-minimal-cso"), &bytes)
            .expect("seed file should be writable");
    }
}
