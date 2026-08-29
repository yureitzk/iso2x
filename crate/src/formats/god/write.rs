use super::format::{
    self, BLOCK_SIZE, BLOCKS_PER_PART, HashList, MHT_SIZE, SUBPART_SIZE, SUBPARTS_PER_PART,
};
use crate::core::executable::TitleExecutionInfo;
use crate::core::extracted_fs::ExtractedFilesystem;
use crate::core::fs::SortedFsForSlbd;
use crate::core::iso;
use crate::core::scrub::{self, ScrubMode};
use crate::core::signing::ConsoleSigningKey;
use crate::core::source::{self, ImageSource, OwnedSourceReader, title_info_from_exe_bytes};
use crate::core::title::{ContentType, TitleInfo};
use crate::core::writers::SliceWriter;
use crate::game_list;
use crate::session::ChunkSource;
use crate::utils::JsErrExt;
use std::cmp;
use std::collections::HashSet;
use std::io::{Cursor, Read, Seek, SeekFrom};
use std::mem;
use wasm_bindgen::prelude::*;
use xdvdfs::write::fs::{
    SectorLinearBlockDevice, SectorLinearBlockFilesystem, SectorLinearImage, XDVDFSFilesystem,
};
use xdvdfs::write::img::{NoOpProgressVisitor, create_xdvdfs_image};

fn chain_mht_digest_inner(mht_bytes: &[u8], next_digest: &[u8]) -> Result<Vec<u8>, anyhow::Error> {
    let mut mht = HashList::read(Cursor::new(mht_bytes))?;
    let hash: [u8; 20] = next_digest
        .try_into()
        .map_err(|_| anyhow::anyhow!("digest must be 20 bytes"))?;
    mht.add_hash(&hash);
    Ok(mht.digest().to_vec())
}

/// Chains `next_digest` onto the end of the hash list encoded in `mht_bytes`
/// and returns the recomputed digest bytes.
///
/// # Errors
///
/// Returns an error if `mht_bytes` is not a valid hash list, or if
/// `next_digest` is not exactly 20 bytes long.
#[wasm_bindgen(js_name = chainMhtDigest)]
pub fn chain_mht_digest(mht_bytes: &[u8], next_digest: &[u8]) -> Result<Vec<u8>, JsError> {
    chain_mht_digest_inner(mht_bytes, next_digest).js_err()
}

struct PrecomputedPart {
    sub_hash_lists: Vec<HashList>,
    subpart_sizes: Vec<u64>,
}

/// A console-signed ('CON ') package always writes `InstalledGame`
/// (`GamesOnDemand` is exclusively the LIVE-signed Marketplace shape), so
/// a signing key forces `InstalledGame` regardless of the source's own
/// content type. Restricted to `GamesOnDemand` (XEX) sources - OGX discs
/// were never installable to an Xbox 360 hard drive.
fn resolve_god_content_type(
    signing_key: Option<&ConsoleSigningKey>,
    content_type: ContentType,
) -> Result<ContentType, anyhow::Error> {
    match (signing_key.is_some(), content_type) {
        (false, ct) => Ok(ct),
        (true, ContentType::GamesOnDemand) => Ok(ContentType::InstalledGame),
        (true, other) => anyhow::bail!(
            "god: console-signing is only supported for GamesOnDemand (XEX) sources \
             right now, not content type {:#06x}",
            other as u32
        ),
    }
}

/// Where `GodSession` reads its pre-hash source bytes from. `Rebuild`
/// reauthors a fresh XDVDFS image; `Direct` streams from the source's
/// XDVDFS root offset, optionally zero-masking `ScrubMode::Partial` gaps.
/// `slbfs` is boxed since `Rebuild` is much larger than `Direct`.
enum GodBackend {
    Rebuild {
        slbfs: Box<SectorLinearBlockFilesystem<SortedFsForSlbd<OwnedSourceReader>>>,
        slbd: SectorLinearBlockDevice,
    },
    /// Same reauthor path as `Rebuild`, sourced from an extracted-files
    /// directory instead of an `ImageSource` (always `Full` mode).
    RebuildFromExtracted {
        slbfs: Box<SectorLinearBlockFilesystem<ExtractedFilesystem>>,
        slbd: SectorLinearBlockDevice,
    },
    Direct {
        reader: OwnedSourceReader,
        /// Sectors to zero instead of passing through untouched: `None`
        /// for `ScrubMode::None`, and for `ScrubMode::Partial` on Xbox
        /// 360 images.
        zero_sectors: Option<HashSet<u64>>,
    },
}

impl GodBackend {
    /// Reads `len` root-relative bytes from `start` into `out`, replacing
    /// its previous contents. May leave `out` shorter than `len` only for
    /// `Direct` at true end-of-image.
    ///
    /// Takes a caller-owned `out` instead of returning a fresh `Vec` so
    /// `PartState::read_scratch` can reuse one buffer across every
    /// subpart, in both the hash and stream phases.
    fn read_range(
        &mut self,
        start: u64,
        len: usize,
        out: &mut Vec<u8>,
    ) -> Result<(), anyhow::Error> {
        match self {
            GodBackend::Rebuild { slbfs, slbd } => {
                // &mut Box<T> derefs to Box<T>, not T - .as_mut() unwraps
                // that layer for the DerefMut<Target = ...> SectorLinearImage needs.
                let mut img = SectorLinearImage::new(slbd, slbfs.as_mut());
                out.clear();
                let mut offset = start;
                let mut remaining = len as u64;
                while remaining > 0 {
                    let chunk_len = remaining.min(scrub::SECTOR_SIZE);
                    let data = img
                        .read_linear(offset, chunk_len)
                        .map_err(|e| anyhow::anyhow!("{e:?}"))?;
                    out.extend_from_slice(data.as_slice());
                    offset += chunk_len;
                    remaining -= chunk_len;
                }
                Ok(())
            }
            GodBackend::RebuildFromExtracted { slbfs, slbd } => {
                let mut img = SectorLinearImage::new(slbd, slbfs.as_mut());
                out.clear();
                let mut offset = start;
                let mut remaining = len as u64;
                while remaining > 0 {
                    let chunk_len = remaining.min(scrub::SECTOR_SIZE);
                    let data = img
                        .read_linear(offset, chunk_len)
                        .map_err(|e| anyhow::anyhow!("{e:?}"))?;
                    out.extend_from_slice(data.as_slice());
                    offset += chunk_len;
                    remaining -= chunk_len;
                }
                Ok(())
            }
            GodBackend::Direct {
                reader,
                zero_sectors,
            } => {
                reader.seek(SeekFrom::Start(start))?;
                // Grow-only: past this session's high-water mark, this is
                // just a cheap truncate - no alloc, no zero-fill.
                if out.len() < len {
                    out.resize(len, 0);
                } else {
                    out.truncate(len);
                }
                let mut total_read = 0usize;
                while total_read < len {
                    let n = match reader.read(&mut out[total_read..len]) {
                        Ok(n) => n,
                        Err(e) => {
                            // Don't leave `out` holding a mix of real
                            // bytes [0, total_read) and stale leftovers
                            // from whatever this buffer held before this
                            // call [total_read, len) - truncate to only
                            // what's actually valid before propagating.
                            out.truncate(total_read);
                            return Err(e.into());
                        }
                    };
                    if n == 0 {
                        break;
                    }
                    total_read += n;
                }
                out.truncate(total_read);
                if let Some(zero) = zero_sectors {
                    let mut pos = start;
                    let sector_size = usize::try_from(scrub::SECTOR_SIZE)
                        .expect("SECTOR_SIZE is a small compile-time constant");
                    for chunk in out.chunks_mut(sector_size) {
                        if zero.contains(&(pos / scrub::SECTOR_SIZE)) {
                            chunk.fill(0);
                        }
                        pos += chunk.len() as u64;
                    }
                }
                Ok(())
            }
        }
    }
}

/// Per-part hashing scratchpad: owns no reader, just bookkeeping, and
/// takes `&mut GodBackend` explicitly per read so the Rebuild backend is
/// built once per session, not once per part.
struct PartState {
    part_start: u64,
    master_hash_list: HashList,
    sub_hash_lists: Vec<HashList>,
    subpart_sizes: Vec<u64>,
    /// Reused across every subpart in both `compute_hashes` (hash phase)
    /// and `read_subpart_chunk` (stream phase) instead of allocating
    /// fresh per subpart per phase. `PartState` is rebuilt fresh per part
    /// (`GodSession::open_part`), so this never carries bytes across the
    /// hash/stream phase boundary or across parts - the format's
    /// backward MHT chaining requires every part to be hashed before any
    /// part can stream, so that second pass can't be cached away here.
    read_scratch: Vec<u8>,
}

impl PartState {
    fn new(part_start: u64) -> Self {
        Self {
            part_start,
            master_hash_list: HashList::new(),
            sub_hash_lists: Vec::with_capacity(SUBPARTS_PER_PART as usize),
            subpart_sizes: Vec::with_capacity(SUBPARTS_PER_PART as usize),
            read_scratch: Vec::new(),
        }
    }

    /// Hashes each subpart's blocks into the part's master hash list,
    /// bounded by `data_size` since only `Direct` can legitimately come
    /// up short on its final subpart.
    fn compute_hashes(
        &mut self,
        backend: &mut GodBackend,
        data_size: u64,
    ) -> Result<(), anyhow::Error> {
        for _ in 0..SUBPARTS_PER_PART {
            let subpart_index = self.sub_hash_lists.len() as u64;
            let subpart_start = self.part_start + subpart_index * SUBPART_SIZE;
            if subpart_start >= data_size {
                break;
            }
            let want = usize::try_from(SUBPART_SIZE.min(data_size - subpart_start))
                .expect("bounded by SUBPART_SIZE, which fits in usize");
            backend.read_range(subpart_start, want, &mut self.read_scratch)?;
            if self.read_scratch.is_empty() {
                break;
            }
            let mut sub_hash_list = HashList::new();
            let block_size =
                usize::try_from(BLOCK_SIZE).expect("BLOCK_SIZE is a small compile-time constant");
            for block in self.read_scratch.chunks(block_size) {
                sub_hash_list.add_block_hash(block);
            }
            self.master_hash_list.add_block_hash(sub_hash_list.bytes());
            self.subpart_sizes.push(self.read_scratch.len() as u64);
            self.sub_hash_lists.push(sub_hash_list);
            if self.read_scratch.len() < want {
                break;
            }
        }
        Ok(())
    }

    /// Reads one subpart's raw bytes plus its precomputed hash - the
    /// payload shape a streamed subpart is handed off in.
    fn read_subpart_chunk(
        &mut self,
        backend: &mut GodBackend,
        subpart_index: u32,
    ) -> Result<Vec<u8>, anyhow::Error> {
        let sub_hash_list = self
            .sub_hash_lists
            .get(subpart_index as usize)
            .ok_or_else(|| anyhow::anyhow!("subpart index out of range"))?;
        let size = usize::try_from(self.subpart_sizes[subpart_index as usize])
            .expect("subpart size is bounded by SUBPART_SIZE, which fits in usize");
        let subpart_start = self.part_start + u64::from(subpart_index) * SUBPART_SIZE;
        backend.read_range(subpart_start, size, &mut self.read_scratch)?;
        let mut out = Vec::with_capacity(sub_hash_list.bytes().len() + self.read_scratch.len());
        out.extend_from_slice(sub_hash_list.bytes());
        out.extend_from_slice(&self.read_scratch);
        Ok(out)
    }

    fn mht_bytes(&self) -> Vec<u8> {
        self.master_hash_list.bytes().to_vec()
    }

    fn actual_size(&self) -> u64 {
        MHT_SIZE as u64
            + self.sub_hash_lists.len() as u64 * BLOCK_SIZE
            + self.subpart_sizes.iter().sum::<u64>()
    }

    fn subpart_count(&self) -> u32 {
        u32::try_from(self.sub_hash_lists.len()).expect("fewer than u32::MAX subparts per part")
    }
}

pub(crate) struct GodSession {
    backend: GodBackend,
    /// Logical byte length of the (possibly rebuilt/trimmed) source.
    data_size: u64,
    part_count: u32,
    block_count: u32,
    exe_info: TitleExecutionInfo,
    content_type: ContentType,
    game_title: Option<String>,
    /// `Some` console-signs the package (`'CON '`) via `finalize_signed`
    /// instead of the default unsigned `'LIVE'`.
    signing_key: Option<ConsoleSigningKey>,
    /// `Some` overwrites the header's Device ID field verbatim - see
    /// `ConHeaderBuilder::with_device_id`.
    device_id: Option<[u8; 20]>,
    chained_mhts: Vec<[u8; MHT_SIZE]>,
    precomputed_parts: Vec<Option<PrecomputedPart>>,
    master_digest: [u8; 20],
    last_part_size: u64,
    raw_mhts: Vec<Vec<u8>>,
    next_hash_part: u32,
    hashing_done: bool,
    current_part_index: u32,
    current_subpart_index: u32,
    current_part: Option<PartState>,
    part_header_emitted: bool,
    header_emitted: bool,
    last_entry_name: Option<String>,
}

impl GodSession {
    /// `probed`, when present, is a directory-tree walk a caller already
    /// did on this exact `source` - reused here instead of walking again.
    pub(crate) fn open(
        source: Box<dyn ImageSource>,
        mode: ScrubMode,
        game_title: Option<String>,
        signing_key: Option<ConsoleSigningKey>,
        device_id: Option<[u8; 20]>,
        probed: Option<source::ProbedDirectoryTable>,
    ) -> Result<Self, anyhow::Error> {
        let mut source = source;
        let root_offset = source.image_offset();
        // Scoped (in the `None` branch) so the borrow ends before
        // `source` moves into OwnedSourceReader below.
        let (directory_table, exe_info, content_type) = if let Some(p) = probed {
            (
                p.directory_table,
                p.title_info.execution_info,
                p.title_info.content_type,
            )
        } else {
            let probe_reader = source::SourceReader::new(source.as_mut());
            let mut iso = iso::IsoReader::read(probe_reader)
                .map_err(|e| anyhow::anyhow!("god: failed to read XDVDFS root: {e:?}"))?;
            let title_info = TitleInfo::from_image(&mut iso)?;
            (
                iso.directory_table,
                title_info.execution_info,
                title_info.content_type,
            )
        };
        let content_type = resolve_god_content_type(signing_key.as_ref(), content_type)?;
        let (backend, data_size) = match mode {
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
                let data_size = slbd.num_sectors() * scrub::SECTOR_SIZE;
                (
                    GodBackend::Rebuild {
                        slbfs: Box::new(slbfs),
                        slbd,
                    },
                    data_size,
                )
            }
            ScrubMode::None | ScrubMode::Partial => {
                // The size scrub::plan_direct needs, derived from the
                // source rather than taken as a parameter.
                let total_size = root_offset + source.total_sectors() * scrub::SECTOR_SIZE;
                let mut reader = OwnedSourceReader::new(source);
                let (total_sectors, zero_sectors) = scrub::plan_direct(
                    mode,
                    &directory_table,
                    root_offset,
                    total_size,
                    &mut reader,
                )
                .map_err(|e| anyhow::anyhow!("god: {e:#}"))?;
                // plan_direct's directory-table probe needed Cached mode's
                // scattered reads; everything from here on (compute_hashes
                // and, later, read_subpart_chunk, both via
                // GodBackend::read_range) is a single forward linear pass
                // over the source, so switch to Sequential for it.
                reader.set_sequential_mode(true);
                let data_size = total_sectors * scrub::SECTOR_SIZE;
                (
                    GodBackend::Direct {
                        reader,
                        zero_sectors,
                    },
                    data_size,
                )
            }
        };
        Self::finish(
            backend,
            data_size,
            exe_info,
            content_type,
            game_title,
            signing_key,
            device_id,
        )
    }

    /// Shared tail of `open()`/`open_from_extracted()`: turns a built
    /// `backend` plus the source's resolved title/content-type metadata
    /// into a `GodSession`, deriving `part_count`/`block_count` from
    /// `data_size` and zero-initializing per-part hashing state.
    fn finish(
        backend: GodBackend,
        data_size: u64,
        exe_info: TitleExecutionInfo,
        content_type: ContentType,
        game_title: Option<String>,
        signing_key: Option<ConsoleSigningKey>,
        device_id: Option<[u8; 20]>,
    ) -> Result<Self, anyhow::Error> {
        let block_count = data_size.div_ceil(BLOCK_SIZE);
        let part_count = u32::try_from(block_count.div_ceil(BLOCKS_PER_PART))
            .map_err(|_| anyhow::anyhow!("image too large: part count overflows u32"))?;
        Ok(Self {
            backend,
            data_size,
            part_count,
            block_count: u32::try_from(block_count)
                .map_err(|_| anyhow::anyhow!("image too large: block count overflows u32"))?,
            exe_info,
            content_type,
            game_title,
            signing_key,
            device_id,
            chained_mhts: Vec::new(),
            precomputed_parts: (0..part_count).map(|_| None).collect(),
            master_digest: [0; 20],
            last_part_size: 0,
            raw_mhts: Vec::with_capacity(part_count as usize),
            next_hash_part: 0,
            hashing_done: part_count == 0,
            current_part_index: 0,
            current_subpart_index: 0,
            current_part: None,
            part_header_emitted: false,
            header_emitted: false,
            last_entry_name: None,
        })
    }

    /// Extracted-source counterpart to `open()`: no XDVDFS root to read
    /// `exe_info`/`content_type` from, so this parses
    /// `default.xbe`/`default.xex` directly and always reauthors from
    /// scratch. `mode` is accepted but ignored - a from-scratch rebuild
    /// has no leftover padding to trim or zero in the first place.
    pub(crate) fn open_from_extracted(
        mut fs: ExtractedFilesystem,
        _mode: ScrubMode,
        game_title: Option<String>,
        signing_key: Option<ConsoleSigningKey>,
        device_id: Option<[u8; 20]>,
    ) -> Result<Self, anyhow::Error> {
        let (exe_bytes, is_xex) = fs.read_launch_executable()?;
        let title_info = title_info_from_exe_bytes(&exe_bytes, is_xex)?;
        let (exe_info, content_type) = (title_info.execution_info, title_info.content_type);
        let content_type = resolve_god_content_type(signing_key.as_ref(), content_type)?;
        let mut slbfs = SectorLinearBlockFilesystem::new(fs);
        let mut slbd = SectorLinearBlockDevice::default();
        create_xdvdfs_image(&mut slbfs, &mut slbd, NoOpProgressVisitor)
            .map_err(|e| anyhow::anyhow!("create_xdvdfs_image: {e:?}"))?;
        let data_size = slbd.num_sectors() * scrub::SECTOR_SIZE;
        let backend = GodBackend::RebuildFromExtracted {
            slbfs: Box::new(slbfs),
            slbd,
        };
        Self::finish(
            backend,
            data_size,
            exe_info,
            content_type,
            game_title,
            signing_key,
            device_id,
        )
    }

    /// Hashes one more part. Returns `true` once every part is hashed
    /// and the MHT chain is built (i.e. once `next_chunk` is safe to
    /// call). Bounded per call so the caller can check a cancellation
    /// flag and yield between parts.
    pub(crate) fn hash_next_part(&mut self) -> Result<bool, anyhow::Error> {
        if self.hashing_done {
            return Ok(true);
        }
        let i = self.next_hash_part;
        let mut part = Self::open_part(i);
        part.compute_hashes(&mut self.backend, self.data_size)?;
        self.raw_mhts.push(part.mht_bytes());
        if i == self.part_count - 1 {
            self.last_part_size = part.actual_size();
        }
        self.precomputed_parts[i as usize] = Some(PrecomputedPart {
            sub_hash_lists: mem::take(&mut part.sub_hash_lists),
            subpart_sizes: mem::take(&mut part.subpart_sizes),
        });
        self.next_hash_part += 1;
        if self.next_hash_part == self.part_count {
            self.finish_hashing()?;
            self.hashing_done = true;
        }
        Ok(self.hashing_done)
    }

    /// Reverse-order MHT chaining tail, run once after every part's hash
    /// has been computed: each part's MHT embeds the digest of the part
    /// after it, so the chain has to be built back-to-front.
    fn finish_hashing(&mut self) -> Result<(), anyhow::Error> {
        let part_count = self.part_count as usize;
        let mut chained = vec![[0u8; MHT_SIZE]; part_count];
        chained[part_count - 1].copy_from_slice(&self.raw_mhts[part_count - 1][..MHT_SIZE]);
        let mut current_digest = HashList::read(Cursor::new(&chained[part_count - 1]))?.digest();
        for i in (0..part_count - 1).rev() {
            let mut prev_mht = HashList::read(Cursor::new(&self.raw_mhts[i]))?;
            prev_mht.add_hash(&current_digest);
            let mut c = Cursor::new(&mut chained[i][..]);
            prev_mht.write(&mut c)?;
            current_digest = prev_mht.digest();
        }
        self.chained_mhts = chained;
        self.master_digest = current_digest;
        self.raw_mhts = Vec::new();
        Ok(())
    }

    fn open_part(part_index: u32) -> PartState {
        let part_start = u64::from(part_index) * BLOCKS_PER_PART * BLOCK_SIZE;
        PartState::new(part_start)
    }

    fn output_path_prefix(&self) -> String {
        let title_id = format!("{:08X}", self.exe_info.title_id);
        let content_type = format!("{:08X}", self.content_type as u32);
        // `self.content_type` is always `GamesOnDemand`/`XboxOriginal`
        // (or `InstalledGame` when signed) - never one of the
        // non-bootable content types, since a GoD target always requires
        // a resolvable launch executable. So this is a two-way split.
        let media_id = match self.content_type {
            ContentType::XboxOriginal => format!("{:08X}", self.exe_info.title_id),
            _ => format!("{:08X}", self.exe_info.media_id),
        };
        format!("{title_id}/{content_type}/{media_id}")
    }

    fn header_size() -> u64 {
        format::ConHeaderBuilder::new().finalize().len() as u64
    }

    fn current_part_entry_name(&self) -> String {
        format!(
            "{}.data/Data{:04}",
            self.output_path_prefix(),
            self.current_part_index
        )
    }

    /// Builds the CON header. Unsigned (`'LIVE'`, the default) unless
    /// `signing_key` was supplied at `open`/`open_from_extracted` time, in
    /// which case it's console-signed (`'CON '`) with that key instead -
    /// see `ConHeaderBuilder::{finalize, finalize_signed}`. The Device ID
    /// field is left zeroed unless `device_id` was also supplied, in
    /// which case it overwrites that field verbatim regardless of
    /// whether the package ends up signed.
    fn finalize_inner(&mut self) -> Result<Vec<u8>, anyhow::Error> {
        let part_count_u64 = u64::from(self.part_count);
        let mut builder = format::ConHeaderBuilder::new()
            .with_execution_info(&self.exe_info)
            .with_block_counts(self.block_count, 0)
            .with_data_parts_info(
                self.part_count,
                // 0xa290 = one full part's on-disk block count: 1 master
                // hash block + SUBPARTS_PER_PART (0xcb) subhash blocks +
                // BLOCKS_PER_PART (0xa1c4) data blocks.
                self.last_part_size + part_count_u64.saturating_sub(1) * BLOCK_SIZE * 0xa290,
            )
            .with_content_type(self.content_type)
            .with_mht_hash(&self.master_digest);
        if let Some(device_id) = &self.device_id {
            builder = builder.with_device_id(device_id);
        }
        // Only applies to the fallback lookup - an explicit `game_title`
        // is used verbatim.
        let resolved_title = self.game_title.clone().or_else(|| {
            game_list::find_title_by_id(self.exe_info.title_id).map(|title| {
                source::disc_suffixed_title(
                    &title,
                    self.exe_info.disc_number,
                    self.exe_info.disc_count,
                )
            })
        });
        if let Some(title) = resolved_title {
            builder = builder.with_game_title(&title);
        }
        match &self.signing_key {
            Some(key) => builder.finalize_signed(key),
            None => Ok(builder.finalize()),
        }
    }
}

impl ChunkSource for GodSession {
    fn next_chunk(&mut self, max_bytes: usize) -> Result<Option<Vec<u8>>, anyhow::Error> {
        let _ = max_bytes;
        loop {
            if !self.hashing_done {
                return Err(anyhow::anyhow!(
                    "GodSession::next_chunk called before hashing finished - \
                 call hash_next_part() until it returns true first"
                ));
            }
            if self.header_emitted {
                return Ok(None);
            }
            if self.current_part_index >= self.part_count {
                let header = self.finalize_inner()?;
                self.header_emitted = true;
                self.last_entry_name = Some(self.output_path_prefix());
                return Ok(Some(header));
            }
            if self.current_part.is_none() {
                let mut part = Self::open_part(self.current_part_index);
                if let Some(precomputed) =
                    self.precomputed_parts[self.current_part_index as usize].take()
                {
                    part.sub_hash_lists = precomputed.sub_hash_lists;
                    part.subpart_sizes = precomputed.subpart_sizes;
                }
                self.current_part = Some(part);
                self.current_subpart_index = 0;
                self.part_header_emitted = false;
            }
            if !self.part_header_emitted {
                self.part_header_emitted = true;
                self.last_entry_name = Some(self.current_part_entry_name());
                return Ok(Some(
                    self.chained_mhts[self.current_part_index as usize].to_vec(),
                ));
            }
            let part = self.current_part.as_mut().expect("just initialized");
            if self.current_subpart_index < part.subpart_count() {
                let chunk =
                    part.read_subpart_chunk(&mut self.backend, self.current_subpart_index)?;
                self.current_subpart_index += 1;
                self.last_entry_name = Some(self.current_part_entry_name());
                return Ok(Some(chunk));
            }
            self.current_part = None;
            self.current_part_index += 1;
        }
    }

    fn is_done(&self) -> bool {
        self.header_emitted
    }

    fn total_units(&self) -> u64 {
        u64::from(self.part_count)
    }

    fn current_entry_name(&self) -> Option<&str> {
        self.last_entry_name.as_deref()
    }

    fn output_manifest(&self) -> Vec<(String, u64)> {
        let prefix = self.output_path_prefix();
        let mut entries: Vec<(String, u64)> = Vec::with_capacity(self.part_count as usize + 1);
        for i in 0..self.part_count {
            let part_start = u64::from(i) * BLOCKS_PER_PART * BLOCK_SIZE;
            let part_data_bytes = cmp::min(
                u64::from(SUBPARTS_PER_PART) * SUBPART_SIZE,
                self.data_size.saturating_sub(part_start),
            );
            let mut remaining = part_data_bytes;
            let mut subparts_count = 0u64;
            let mut total_subpart_sizes = 0u64;
            while remaining > 0 && subparts_count < u64::from(SUBPARTS_PER_PART) {
                let subpart_size = cmp::min(remaining, SUBPART_SIZE);
                remaining -= subpart_size;
                subparts_count += 1;
                total_subpart_sizes += subpart_size;
            }
            let part_size = MHT_SIZE as u64 + subparts_count * BLOCK_SIZE + total_subpart_sizes;
            entries.push((format!("{prefix}.data/Data{i:04}"), part_size));
        }
        entries.push((prefix, Self::header_size()));
        entries
    }
}

#[cfg(test)]
mod read_range_scratch_tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use wasm_bindgen_test::*;

    /// Fills reads with an offset-dependent byte pattern so a bug that
    /// leaks a previous call's bytes into a later, shorter call is
    /// visible as wrong content, not just a wrong length.
    struct PatternThenFailSource {
        total_sectors: u64,
        /// Errors on every `read_bytes` call once this many successful
        /// calls have already happened - lets one test reuse a single
        /// backend for "first call succeeds, second call fails".
        fail_after_calls: u32,
        calls: AtomicU32,
    }

    impl ImageSource for PatternThenFailSource {
        fn read_sector(&mut self, _sector: u64, out: &mut [u8]) -> Result<(), anyhow::Error> {
            out.fill(0);
            Ok(())
        }
        fn read_bytes(&mut self, offset: u64, out: &mut [u8]) -> Result<(), anyhow::Error> {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            if call >= self.fail_after_calls {
                anyhow::bail!("simulated read failure on call {call}");
            }
            for (i, b) in out.iter_mut().enumerate() {
                *b = ((offset + i as u64) % 256) as u8;
            }
            Ok(())
        }
        fn total_sectors(&self) -> u64 {
            self.total_sectors
        }
        fn image_offset(&self) -> u64 {
            0
        }
    }

    fn direct_backend(source: PatternThenFailSource) -> GodBackend {
        GodBackend::Direct {
            reader: OwnedSourceReader::new(Box::new(source)),
            zero_sectors: None,
        }
    }

    /// The scratch buffer passed as `out` is reused across calls, so a
    /// later, shorter read must not leave any of an earlier, longer
    /// read's tail bytes visible - either as wrong content within the new
    /// length or as extra length beyond it.
    #[wasm_bindgen_test]
    fn shorter_read_after_longer_read_does_not_leak_previous_tail() {
        let mut backend = direct_backend(PatternThenFailSource {
            total_sectors: 1_000_000,
            fail_after_calls: u32::MAX,
            calls: AtomicU32::new(0),
        });
        let mut scratch = Vec::new();

        let long_len = 8192usize;
        backend.read_range(0, long_len, &mut scratch).unwrap();
        assert_eq!(scratch.len(), long_len);

        let short_start = 100_000u64;
        let short_len = 256usize;
        backend
            .read_range(short_start, short_len, &mut scratch)
            .unwrap();
        assert_eq!(
            scratch.len(),
            short_len,
            "shorter read must truncate away the longer read's leftover tail"
        );
        let expected: Vec<u8> = (0..short_len as u64)
            .map(|i| ((short_start + i) % 256) as u8)
            .collect();
        assert_eq!(
            scratch, expected,
            "shorter read's content must match its own offset, not bleed the previous \
             (longer, different-offset) read's bytes"
        );
    }

    /// If the underlying read errors partway, `out` must be truncated to
    /// only the bytes actually read before the error propagates - not
    /// left holding a mix of stale data from a previous call and
    /// zero-padding, mislabeled as this call's (failed) result.
    #[wasm_bindgen_test]
    fn failed_read_truncates_scratch_instead_of_leaking_stale_content() {
        let mut backend = direct_backend(PatternThenFailSource {
            total_sectors: 1_000_000,
            fail_after_calls: 1, // succeeds once, fails on every call after
            calls: AtomicU32::new(0),
        });
        let mut scratch = Vec::new();

        // First call succeeds and leaves real data in `scratch`.
        backend.read_range(0, 4096, &mut scratch).unwrap();
        assert_eq!(scratch.len(), 4096);

        // Second call fails immediately (fail_after_calls: 1 already used up).
        let err = backend.read_range(0, 4096, &mut scratch);
        assert!(err.is_err(), "test setup bug: second call should fail");
        assert_eq!(
            scratch.len(),
            0,
            "a failed read must truncate `out` rather than leave the previous \
             successful call's stale bytes (or zero-padding) sitting in it"
        );
    }
}

#[cfg(test)]
mod abi_crossing_tests {
    use super::*;
    use crate::session::{ConversionSession, SessionInner};
    use wasm_bindgen_test::*;

    struct ZeroSource;

    impl ImageSource for ZeroSource {
        fn read_sector(&mut self, _sector: u64, out: &mut [u8]) -> Result<(), anyhow::Error> {
            out.fill(0);
            Ok(())
        }
        fn read_bytes(&mut self, _offset: u64, out: &mut [u8]) -> Result<(), anyhow::Error> {
            out.fill(0);
            Ok(())
        }
        fn total_sectors(&self) -> u64 {
            4
        }
        fn image_offset(&self) -> u64 {
            0
        }
    }

    fn unsigned_session() -> GodSession {
        let data_size = BLOCK_SIZE;
        GodSession {
            backend: GodBackend::Direct {
                reader: OwnedSourceReader::new(Box::new(ZeroSource)),
                zero_sectors: None,
            },
            data_size,
            part_count: 1,
            block_count: 1,
            exe_info: TitleExecutionInfo {
                media_id: 0,
                version: 0,
                base_version: 0,
                title_id: 0,
                platform: 0,
                executable_type: 0,
                disc_number: 0,
                disc_count: 0,
                save_game_id: 0,
            },
            content_type: ContentType::GamesOnDemand,
            game_title: None,
            signing_key: None,
            device_id: None,
            chained_mhts: Vec::new(),
            precomputed_parts: vec![None],
            master_digest: [0; 20],
            last_part_size: 0,
            raw_mhts: Vec::with_capacity(1),
            next_hash_part: 0,
            hashing_done: false,
            current_part_index: 0,
            current_subpart_index: 0,
            current_part: None,
            part_header_emitted: false,
            header_emitted: false,
            last_entry_name: None,
        }
    }

    #[wasm_bindgen_test]
    fn streams_full_output_through_the_real_wasm_bindgen_abi() {
        let session = unsigned_session();
        let expected_total: u64 = ChunkSource::output_manifest(&session)
            .into_iter()
            .map(|(_, size)| size)
            .sum();
        let mut conversion = ConversionSession::new(SessionInner::God(Box::new(session)));

        loop {
            let done = conversion
                .hash_next_part()
                .expect("hash_next_part must not error for a zero-filled fixture");
            if done {
                break;
            }
        }

        conversion
            .output_manifest()
            .expect("outputManifest must serialize through Ts<T> without error");

        let mut collected = Vec::new();
        loop {
            let chunk = conversion
                .next_chunk(4096)
                .expect("next_chunk must not error mid-stream");
            match chunk {
                // Round-trips through js_sys::Uint8Array - the actual ABI boundary under test.
                Some(bytes) => collected.extend(bytes.to_vec()),
                None => break,
            }
        }
        assert!(conversion.is_done());
        assert_eq!(
            collected.len() as u64,
            expected_total,
            "uncompressed GOD output must match outputManifest's declared total exactly"
        );
    }

    #[wasm_bindgen_test]
    fn next_chunk_before_hashing_done_is_a_catchable_error_not_a_panic() {
        let session = unsigned_session();
        let mut conversion = ConversionSession::new(SessionInner::God(Box::new(session)));

        let result = conversion.next_chunk(4096);
        assert!(
            result.is_err(),
            "next_chunk before hashing_done must surface as Err through the real ABI"
        );
    }
}
