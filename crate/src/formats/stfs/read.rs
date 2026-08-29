//! Read-side parser for Xbox 360 STFS (LIVE/PIRS/CON) packages.
//! `<https://free60.org/System-Software/Formats/STFS/>`
//!
//! Parses header + hash-tree + file listing at `open()`; file bytes are
//! read on demand by `read_file_range`. Used as a `Backing` variant by
//! [`crate::core::extracted_fs::ExtractedFilesystem`].
//!
//! No signature/hash verification, no PEC support.

use super::format::{
    AvatarItemMetadata, BLOCK_SIZE, FILE_ENTRIES_PER_BLOCK, FILE_ENTRY_SIZE, HeaderPrefix,
    HeaderThumbnails, InstallerMetadata, Level, NAME_LEN_OFFSET, PATH_INDICATOR_OFFSET,
    ROOT_ENTRY_INDEX, StfsMetadata, StfsVolumeDescriptor, TOP_RECORD_SIZE, TOP_RECORD_SIZE_USIZE,
    VideoMetadata, header_offset, read_avatar_item_metadata, read_display_name, read_header_prefix,
    read_header_thumbnails, read_installer_metadata, read_video_metadata,
};
use crate::core::reader::JsReader;
use crate::core::title::ContentType;
use crate::utils::is_safe_path_component;
use anyhow::Context;
use js_sys::Function;
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};

/// Level-N hash-table block spacing, in data blocks: level-0 every 0xAA
/// (170) blocks, level-1 every 0xAA*0xAA (0x70E4) blocks, level-2 every
/// 0xAA^3 blocks.
const DATA_BLOCKS_PER_LEVEL: [u64; 3] = [1, 0xAA, 0x70E4];

fn read_int24_be(bytes: &[u8]) -> u32 {
    (u32::from(bytes[0]) << 16) | (u32::from(bytes[1]) << 8) | u32::from(bytes[2])
}

/// Used for file-listing entries' `starting_block_num`. Distinct from
/// the volume descriptor's `file_table_block_num`, which goes through
/// binrw's `#[br(map = ...)]` instead.
fn read_int24_le(bytes: &[u8]) -> u32 {
    u32::from(bytes[0]) | (u32::from(bytes[1]) << 8) | (u32::from(bytes[2]) << 16)
}

/// One entry from the top-level hash table, cached at `open()`.
struct TopTableEntry {
    status: u8,
}

/// One raw file-listing record, before path reconstruction.
struct RawEntry {
    name: String,
    entry_index: u16,
    path_indicator: u16,
    is_directory: bool,
    is_contiguous: bool,
    starting_block_num: u32,
    file_size: u32,
}

/// Reads one 0x40-byte file-listing entry at the reader's current
/// position. `Empty` = zeroed slot, keep scanning. `EndOfListing` =
/// defensive fallback, shouldn't trigger in practice.
enum EntrySlot {
    Empty,
    EndOfListing,
    Entry(RawEntry),
}

/// Pure decode of one already-read `FILE_ENTRY_SIZE` buffer. Split out
/// so a fuzz harness can drive it directly from arbitrary bytes.
fn decode_file_entry_bytes(buf: &[u8; FILE_ENTRY_SIZE], entry_index: u16) -> EntrySlot {
    let name_len_raw = buf[NAME_LEN_OFFSET];
    if name_len_raw.trailing_zeros() >= 6 {
        return EntrySlot::Empty;
    }
    let name_len = usize::from(name_len_raw & 0x3F);
    let flags = name_len_raw >> 6;
    let raw_name = &buf[0..NAME_LEN_OFFSET.min(name_len)];
    if raw_name.is_empty() {
        return EntrySlot::EndOfListing;
    }
    let name = String::from_utf8_lossy(raw_name).into_owned();
    let starting_block_num = read_int24_le(&buf[0x2F..PATH_INDICATOR_OFFSET]);
    let path_indicator =
        u16::from_be_bytes([buf[PATH_INDICATOR_OFFSET], buf[PATH_INDICATOR_OFFSET + 1]]);
    let file_size = u32::from_be_bytes(
        buf[PATH_INDICATOR_OFFSET + 2..PATH_INDICATOR_OFFSET + 6]
            .try_into()
            .expect("fixed 4-byte slice always converts to [u8; 4]"),
    );
    EntrySlot::Entry(RawEntry {
        name,
        entry_index,
        path_indicator,
        is_directory: flags & 2 != 0,
        is_contiguous: flags & 1 != 0,
        starting_block_num,
        file_size,
    })
}

fn read_file_entry(reader: &mut JsReader, entry_index: u16) -> Result<EntrySlot, anyhow::Error> {
    let mut buf = [0u8; FILE_ENTRY_SIZE];
    reader.read_exact(&mut buf)?;
    Ok(decode_file_entry_bytes(&buf, entry_index))
}

/// Bound check shared with `hash_address_of_block`, pulled into a free
/// function (plain scalars, not `&StfsReader`) so it's testable with a
/// plain `#[test]` - `StfsReader` needs a live JS `Function` to construct.
fn check_block_in_range(block_num: u64, allocated_block_count: u32) -> Result<(), anyhow::Error> {
    anyhow::ensure!(
        block_num < u64::from(allocated_block_count),
        "stfs: reference to illegal block number"
    );
    Ok(())
}

pub(crate) struct StfsReader {
    /// Console ID (0x36C). Display-only, kept for stfs->stfs round-trip.
    console_id: [u8; 5],
    /// Profile ID / XUID (0x371). Display-only.
    profile_id: [u8; 8],
    /// Online Creator XUID (0x3AD), distinct from `profile_id`.
    online_creator: [u8; 8],
    /// Device ID (0x3FD).
    device_id: [u8; 20],
    /// License table entries 1..16 (0x23C), opaque, entry 0 excluded
    /// since it's always re-bound on write.
    license_entries: [u8; header_offset::LICENSE_ENTRIES_1_15_LEN],
    /// Thumbnail Image (0x171A), PNG bytes, `None` if absent/invalid.
    thumbnail: Option<Vec<u8>>,
    /// Title Thumbnail Image (0x571A), same conditions as `thumbnail`.
    title_thumbnail: Option<Vec<u8>>,
    /// File-data reads: block payloads and file-listing blocks.
    reader: JsReader,
    /// Separate `JsReader` for hash-tree reads, to avoid thrashing a
    /// shared cache with the interleaved file-data access pattern.
    hash_reader: JsReader,
    first_hash_table_address: u64,
    sex_shift: u32,
    block_step: [u64; 2],
    top_level: Level,
    top_table: Vec<TopTableEntry>,
    /// Level-1 status bytes for `Level::Two`, `[top_idx][block_num % 0xAA]`.
    /// Empty for `Level::Zero`/`Level::One`.
    level1_status: Vec<Vec<u8>>,
    block_separation: u8,
    allocated_block_count: u32,
    files: Vec<(String, u32, u32, bool)>,
    /// `(file idx, block, byte-offset of block start)` from the last
    /// `read_file_range` call, so sequential reads skip re-walking the chain.
    chain_cursor: Option<(usize, u64, u64)>,
    /// Content type from the header (0x344), for stfs->stfs round-trip.
    content_type: Option<ContentType>,
    /// Same field as `content_type`, unfiltered, for unknown types.
    raw_content_type: u32,
    /// Display Name (0x411), for stfs->stfs round-trip - see
    /// `format::read_display_name`. `None` if absent/empty.
    display_name: Option<String>,
    /// `AvatarItem`-only metadata (0x3D9), for stfs->stfs round-trip.
    /// Only ever `Some` when `content_type == Some(AvatarItem)`.
    avatar_item_metadata: Option<AvatarItemMetadata>,
    /// `Video`-only metadata (same offset as `avatar_item_metadata`),
    /// for stfs->stfs round-trip. Only ever `Some` when
    /// `content_type == Some(Video)`.
    video_metadata: Option<VideoMetadata>,
    /// Installer trailer (0x971A), for stfs->stfs round-trip. Gated
    /// only by header size (matching Velocity), not by `content_type`.
    installer_metadata: Option<InstallerMetadata>,
}

impl StfsReader {
    /// No `sequential_window` param: every access here is a scattered
    /// Cached-mode read against `JsReader`'s fixed cache block size -
    /// `set_sequential_mode` is never called.
    pub(crate) fn open(read_fn: Function, file_size: u64) -> Result<Self, anyhow::Error> {
        anyhow::ensure!(
            file_size > header_offset::DEVICE_ID + 0x14,
            "stfs: file too small to contain a header"
        );
        let hash_reader = JsReader::new(read_fn.clone(), file_size);
        let mut reader = JsReader::new(read_fn, file_size);

        // Magic + header_size + content_type (0x344), shared prefix with
        // GoD headers.
        let HeaderPrefix {
            header_size,
            raw_content_type,
            content_type,
        } = read_header_prefix(&mut reader).context("stfs: not a LIVE/PIRS/CON package")?;

        let metadata = Self::read_metadata(&mut reader)?;
        // Copied out so `metadata` can still be destructured below.
        let descriptor = metadata.volume_descriptor;
        anyhow::ensure!(
            descriptor.allocated_block_count > 0,
            "stfs: package has no allocated blocks"
        );

        // Defensive check: a genuine STFS package should always be
        // descriptor type 0. SVOD (multi-file) packages have a
        // different volume-descriptor shape and are handled by
        // `formats::god` instead, so this rejects a mismatch rather
        // than misinterpreting SVOD-shaped bytes as STFS.
        //
        // Read as a big-endian u32, not a single byte: the top byte is
        // 0x00 for both valid values (STFS=0, SVOD=1), so a 1-byte
        // check couldn't distinguish them.
        reader.seek(SeekFrom::Start(header_offset::DESCRIPTOR_TYPE))?;
        let mut descriptor_type = [0u8; 4];
        reader.read_exact(&mut descriptor_type)?;
        let descriptor_type = u32::from_be_bytes(descriptor_type);
        anyhow::ensure!(
            descriptor_type == 0,
            "stfs: volume descriptor is not STFS-shaped (descriptor type {descriptor_type}, expected 0)"
        );

        let sex_shift = u32::from((!descriptor.block_separation) & 1);
        let block_step = if sex_shift == 0 {
            [0xAB, 0x718F]
        } else {
            [0xAC, 0x723A]
        };
        let first_hash_table_address = u64::from((header_size + 0xFFF) & 0xFFFF_F000);

        let top_level = match descriptor.allocated_block_count {
            n if n <= 0xAA => Level::Zero,
            n if n <= 0x70E4 => Level::One,
            n if n <= 0x4A_F768 => Level::Two,
            _ => anyhow::bail!("stfs: invalid allocated block count"),
        };
        let block_separation = descriptor.block_separation;
        let allocated_block_count = descriptor.allocated_block_count;

        let StfsMetadata {
            license_entries_1_15: license_entries,
            console_id,
            profile_id,
            online_creator,
            device_id,
            ..
        } = metadata;
        let HeaderThumbnails {
            thumbnail,
            title_thumbnail,
        } = read_header_thumbnails(&mut reader);
        let display_name = read_display_name(&mut reader);
        // Only meaningful for AvatarItem - see the field's doc comment.
        let avatar_item_metadata = if content_type == Some(ContentType::AvatarItem) {
            read_avatar_item_metadata(&mut reader)
        } else {
            None
        };
        // Only meaningful for Video - see the field's doc comment.
        let video_metadata = if content_type == Some(ContentType::Video) {
            read_video_metadata(&mut reader)
        } else {
            None
        };
        // Gated purely by header size, not content_type - see the
        // field's doc comment.
        let installer_metadata = read_installer_metadata(&mut reader, first_hash_table_address);

        let mut this = Self {
            console_id,
            profile_id,
            online_creator,
            device_id,
            license_entries,
            thumbnail,
            title_thumbnail,
            reader,
            hash_reader,
            first_hash_table_address,
            sex_shift,
            block_step,
            top_level,
            top_table: Vec::new(),
            level1_status: Vec::new(),
            block_separation,
            allocated_block_count,
            files: Vec::new(),
            chain_cursor: None,
            content_type,
            raw_content_type,
            display_name,
            avatar_item_metadata,
            video_metadata,
            installer_metadata,
        };

        this.load_top_table(&descriptor)?;
        this.load_level1_status()?;
        let raw_entries = this.read_file_listing(&descriptor)?;
        this.files = Self::build_paths(&raw_entries)?;

        Ok(this)
    }

    pub(crate) fn content_type(&self) -> Option<ContentType> {
        self.content_type
    }

    pub(crate) fn raw_content_type(&self) -> u32 {
        self.raw_content_type
    }

    pub(crate) fn display_name(&self) -> Option<&str> {
        self.display_name.as_deref()
    }

    pub(crate) fn avatar_item_metadata(&self) -> Option<AvatarItemMetadata> {
        self.avatar_item_metadata
    }

    pub(crate) fn video_metadata(&self) -> Option<VideoMetadata> {
        self.video_metadata
    }

    pub(crate) fn installer_metadata(&self) -> Option<&InstallerMetadata> {
        self.installer_metadata.as_ref()
    }

    /// Single seek + sequential binrw read of the whole fixed-offset
    /// header region (license table through device ID). See
    /// `format::StfsMetadata`.
    fn read_metadata(reader: &mut JsReader) -> Result<StfsMetadata, anyhow::Error> {
        StfsMetadata::read_at(reader)
    }

    pub(crate) fn console_id(&self) -> &[u8; 5] {
        &self.console_id
    }

    pub(crate) fn profile_id(&self) -> &[u8; 8] {
        &self.profile_id
    }

    pub(crate) fn device_id(&self) -> &[u8; 20] {
        &self.device_id
    }

    pub(crate) fn online_creator(&self) -> &[u8; 8] {
        &self.online_creator
    }

    pub(crate) fn license_entries(&self) -> &[u8; header_offset::LICENSE_ENTRIES_1_15_LEN] {
        &self.license_entries
    }

    pub(crate) fn thumbnail(&self) -> Option<&[u8]> {
        self.thumbnail.as_deref()
    }

    pub(crate) fn title_thumbnail(&self) -> Option<&[u8]> {
        self.title_thumbnail.as_deref()
    }

    // Block-hash-tree addressing. Matches Velocity/XboxInternals's
    // `StfsPackage` methods term for term, not free60.org's C# sample
    // (which gates on `Magic == CON` and has a known Level::Two type
    // error).
    fn compute_backing_data_block_number(&self, block_num: u64) -> u64 {
        let shift = self.sex_shift;
        let base = (((block_num + 0xAA) / 0xAA) << shift) + block_num;
        if block_num < 0xAA {
            base
        } else if block_num < 0x70E4 {
            base + (((block_num + 0x70E4) / 0x70E4) << shift)
        } else {
            (1 << shift) + base + (((block_num + 0x70E4) / 0x70E4) << shift)
        }
    }

    /// Callers pass either an already-checked `next_block` chain pointer,
    /// or an unchecked attacker-controlled value straight from the header/
    /// file listing (`file_table_block_num`, `starting_block_num`).
    /// Bounding against `allocated_block_count` here - the same bound
    /// `hash_address_of_block` enforces - closes that gap at the one
    /// choke point both callers share.
    fn block_to_address(&self, block_num: u64) -> Result<u64, anyhow::Error> {
        check_block_in_range(block_num, self.allocated_block_count)?;
        Ok((self.compute_backing_data_block_number(block_num) << 0xC)
            + self.first_hash_table_address)
    }

    fn compute_level0_backing_hash_block_number(&self, block_num: u64) -> u64 {
        if block_num < 0xAA {
            return 0;
        }
        let shift = self.sex_shift;
        let mut num = (block_num / 0xAA) * self.block_step[0];
        num += ((block_num / 0x70E4) + 1) << shift;
        if block_num / 0x70E4 == 0 {
            num
        } else {
            num + (1 << shift)
        }
    }

    fn compute_level1_backing_hash_block_number(&self, block_num: u64) -> u64 {
        let shift = self.sex_shift;
        if block_num < 0x70E4 {
            self.block_step[0]
        } else {
            (1 << shift) + (block_num / 0x70E4) * self.block_step[1]
        }
    }

    /// Physical block housing the level-2 (top) hash table - always one,
    /// so this is just `block_step[1]`.
    fn compute_level2_backing_hash_block_number(&self, _block_num: u64) -> u64 {
        self.block_step[1]
    }

    /// File offset of the hash-tree entry describing `block_num`'s
    /// status/next-block.
    fn hash_address_of_block(&mut self, block_num: u64) -> Result<u64, anyhow::Error> {
        anyhow::ensure!(
            block_num < u64::from(self.allocated_block_count),
            "stfs: reference to illegal block number"
        );
        let mut hash_addr = (self.compute_level0_backing_hash_block_number(block_num) << 0xC)
            + self.first_hash_table_address;
        hash_addr += (block_num % 0xAA) * TOP_RECORD_SIZE;

        match self.top_level {
            Level::Zero => {
                hash_addr += u64::from(self.block_separation & 2) << 0xB;
            }
            Level::One => {
                let idx = usize::try_from(block_num / 0xAA)?;
                let status = self
                    .top_table
                    .get(idx)
                    .ok_or_else(|| anyhow::anyhow!("stfs: top table index out of range"))?
                    .status;
                hash_addr += u64::from(u32::from(status & 0x40)) << 6;
            }
            Level::Two => {
                let top_idx = usize::try_from(block_num / 0x70E4)?;
                let entry_idx = usize::try_from(block_num % 0xAA)?;
                let status_byte = *self
                    .level1_status
                    .get(top_idx)
                    .and_then(|group| group.get(entry_idx))
                    .ok_or_else(|| anyhow::anyhow!("stfs: level1 status index out of range"))?;
                hash_addr += u64::from(u32::from(status_byte & 0x40)) << 6;
            }
        }
        Ok(hash_addr)
    }

    /// Reads the status byte + next-block pointer for `block_num`'s hash
    /// entry (the 0x14-byte hash itself is skipped - never verified).
    fn block_hash_entry(&mut self, block_num: u64) -> Result<(u8, u32), anyhow::Error> {
        let addr = self.hash_address_of_block(block_num)?;
        self.hash_reader.seek(SeekFrom::Start(addr + 0x14))?;
        let mut buf = [0u8; 4];
        self.hash_reader.read_exact(&mut buf)?;
        Ok((buf[0], read_int24_be(&buf[1..4])))
    }

    /// Locates and reads the top hash table into `self.top_table`, once
    /// at open time.
    fn load_top_table(&mut self, descriptor: &StfsVolumeDescriptor) -> Result<(), anyhow::Error> {
        let true_block_number = match self.top_level {
            Level::Zero => 0,
            Level::One => self.compute_level1_backing_hash_block_number(0),
            Level::Two => self.compute_level2_backing_hash_block_number(0),
        };
        let base_address = (true_block_number << 0xC) + self.first_hash_table_address;
        let address_in_file = base_address + (u64::from(descriptor.block_separation & 2) << 0xB);

        let divisor = DATA_BLOCKS_PER_LEVEL[self.top_level as usize];
        // Ceiling division: one extra entry for a partially-filled group.
        let mut entry_count = u64::from(descriptor.allocated_block_count) / divisor;
        if u64::from(descriptor.allocated_block_count) % divisor != 0 {
            entry_count += 1;
        }
        self.hash_reader.seek(SeekFrom::Start(address_in_file))?;
        let mut entries = Vec::with_capacity(usize::try_from(entry_count)?);
        for _ in 0..entry_count {
            let mut buf = [0u8; TOP_RECORD_SIZE_USIZE];
            self.hash_reader.read_exact(&mut buf)?;
            entries.push(TopTableEntry { status: buf[0x14] });
        }
        self.top_table = entries;
        Ok(())
    }

    /// Precomputes level-1 status bytes for `hash_address_of_block`'s
    /// `Level::Two` arm. No-op for `Level::Zero`/`Level::One`.
    fn load_level1_status(&mut self) -> Result<(), anyhow::Error> {
        if self.top_level != Level::Two {
            return Ok(());
        }
        let mut groups = Vec::with_capacity(self.top_table.len());
        for top_idx in 0..self.top_table.len() {
            let block_num = (top_idx as u64) * 0x70E4;
            let status = self.top_table[top_idx].status;
            let level1_off = u64::from(u32::from(status & 0x40)) << 6;
            let base = (self.compute_level1_backing_hash_block_number(block_num) << 0xC)
                + self.first_hash_table_address
                + level1_off;
            self.hash_reader.seek(SeekFrom::Start(base))?;
            let mut buf = vec![0u8; 0xAA * TOP_RECORD_SIZE_USIZE];
            self.hash_reader.read_exact(&mut buf)?;
            let mut entries = Vec::with_capacity(0xAA);
            for i in 0..0xAA_usize {
                entries.push(buf[i * TOP_RECORD_SIZE_USIZE + 0x14]);
            }
            groups.push(entries);
        }
        self.level1_status = groups;
        Ok(())
    }

    // File listing.
    fn read_file_listing(
        &mut self,
        descriptor: &StfsVolumeDescriptor,
    ) -> Result<Vec<RawEntry>, anyhow::Error> {
        let mut entries = Vec::new();
        let mut block = descriptor.file_table_block_num;
        let mut entry_index: u16 = 0;

        'blocks: for _ in 0..descriptor.file_table_block_count {
            let addr = self.block_to_address(u64::from(block))?;
            // File-table blocks are plain data blocks - via `reader`.
            self.reader.seek(SeekFrom::Start(addr))?;

            for _ in 0..FILE_ENTRIES_PER_BLOCK {
                match read_file_entry(&mut self.reader, entry_index)? {
                    EntrySlot::Empty => {
                        entry_index = entry_index.wrapping_add(1);
                    }
                    EntrySlot::EndOfListing => break,
                    EntrySlot::Entry(e) => {
                        entries.push(e);
                        entry_index = entry_index.wrapping_add(1);
                    }
                }
            }

            let (_, next_block) = self.block_hash_entry(u64::from(block))?;
            if next_block == 0xFF_FFFF {
                break 'blocks;
            }
            block = next_block;
        }

        Ok(entries)
    }

    /// Reassembles `/`-joined paths from the flat, parent-indexed
    /// listing. `ROOT_ENTRY_INDEX` stands in for the synthetic root.
    fn build_paths(entries: &[RawEntry]) -> Result<Vec<(String, u32, u32, bool)>, anyhow::Error> {
        let mut by_index: HashMap<u16, &RawEntry> = HashMap::new();
        for e in entries {
            by_index.insert(e.entry_index, e);
        }

        let resolve = |parent: u16, own_name: &str| -> Result<String, anyhow::Error> {
            anyhow::ensure!(
                is_safe_path_component(own_name),
                "stfs: unsafe path component in file listing: {own_name:?}"
            );
            let mut parent = parent;
            let mut parts = vec![own_name.to_owned()];
            let mut guard = 0;
            while parent != ROOT_ENTRY_INDEX {
                guard += 1;
                anyhow::ensure!(guard < 4096, "stfs: file-listing parent chain too deep");
                let dir = by_index
                    .get(&parent)
                    .ok_or_else(|| anyhow::anyhow!("stfs: dangling parent index {parent}"))?;
                anyhow::ensure!(dir.is_directory, "stfs: parent entry isn't a directory");
                anyhow::ensure!(
                    is_safe_path_component(&dir.name),
                    "stfs: unsafe path component in file listing: {:?}",
                    dir.name
                );
                parts.push(dir.name.clone());
                parent = dir.path_indicator;
            }
            parts.reverse();
            Ok(parts.join("/"))
        };

        let mut files = Vec::new();
        for e in entries {
            if e.is_directory {
                continue;
            }
            let path = resolve(e.path_indicator, &e.name)?;
            files.push((path, e.starting_block_num, e.file_size, e.is_contiguous));
        }
        Ok(files)
    }

    /// Fuzz-only entry point: chunks bytes into `FILE_ENTRY_SIZE` slots
    /// and runs them through `decode_file_entry_bytes` + `build_paths`.
    #[cfg(fuzzing)]
    pub(crate) fn fuzz_build_paths(data: &[u8]) {
        let entries: Vec<RawEntry> = data
            .chunks_exact(FILE_ENTRY_SIZE)
            .enumerate()
            .filter_map(|(i, chunk)| {
                let buf: &[u8; FILE_ENTRY_SIZE] = chunk.try_into().expect("chunks_exact");
                match decode_file_entry_bytes(buf, i as u16) {
                    EntrySlot::Entry(e) => Some(e),
                    _ => None,
                }
            })
            .collect();
        let _ = Self::build_paths(&entries);
    }

    // Public, file-content-facing API.
    /// (path, size) per file, in file-listing traversal order.
    pub(crate) fn file_entries(&self) -> Vec<(String, u64)> {
        self.files
            .iter()
            .map(|(path, _, size, _)| (path.clone(), u64::from(*size)))
            .collect()
    }

    /// Reads `buf.len()` bytes at `offset_in_file` in file `idx`,
    /// following its block chain. Resumes from `self.chain_cursor` for
    /// sequential reads; otherwise walks cold from `starting_block`.
    pub(crate) fn read_file_range(
        &mut self,
        idx: usize,
        offset_in_file: u64,
        buf: &mut [u8],
    ) -> Result<(), anyhow::Error> {
        let &(_, starting_block, file_size, _) = self
            .files
            .get(idx)
            .ok_or_else(|| anyhow::anyhow!("stfs: file index {idx} out of range"))?;
        anyhow::ensure!(
            offset_in_file + buf.len() as u64 <= u64::from(file_size),
            "stfs: read past end of file"
        );

        let (mut block, mut block_pos) = match self.chain_cursor {
            Some((cached_idx, cached_block, cached_pos))
                if cached_idx == idx && cached_pos <= offset_in_file =>
            {
                (cached_block, cached_pos)
            }
            _ => (u64::from(starting_block), 0u64),
        };
        let mut written = 0usize;
        while block_pos + BLOCK_SIZE <= offset_in_file {
            let (_, next_block) = self.block_hash_entry(block)?;
            anyhow::ensure!(
                next_block != 0xFF_FFFF,
                "stfs: block chain ended before reaching requested offset"
            );
            block = u64::from(next_block);
            block_pos += BLOCK_SIZE;
        }
        while written < buf.len() {
            let pos_in_block = offset_in_file + written as u64 - block_pos;
            let addr = self.block_to_address(block)?;
            self.reader.seek(SeekFrom::Start(addr + pos_in_block))?;
            let n = usize::try_from((BLOCK_SIZE - pos_in_block).min((buf.len() - written) as u64))
                .map_err(|e| anyhow::anyhow!("read length does not fit in usize: {e}"))?;
            self.reader.read_exact(&mut buf[written..written + n])?;
            written += n;
            if written < buf.len() {
                let (_, next_block) = self.block_hash_entry(block)?;
                anyhow::ensure!(
                    next_block != 0xFF_FFFF,
                    "stfs: block chain ended before end of requested range"
                );
                block = u64::from(next_block);
                block_pos += BLOCK_SIZE;
            }
        }
        self.chain_cursor = Some((idx, block, block_pos));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, entry_index: u16, path_indicator: u16, is_directory: bool) -> RawEntry {
        RawEntry {
            name: name.to_owned(),
            entry_index,
            path_indicator,
            is_directory,
            is_contiguous: true,
            starting_block_num: 0,
            file_size: 0,
        }
    }

    #[test]
    fn check_block_in_range_rejects_block_beyond_allocated_count() {
        let err = check_block_in_range(1000, 5).unwrap_err();
        assert!(err.to_string().contains("illegal block number"));
    }

    #[test]
    fn check_block_in_range_accepts_every_in_range_block() {
        for block in 0..5u64 {
            assert!(
                check_block_in_range(block, 5).is_ok(),
                "block {block} should be accepted for a 5-block package"
            );
        }
    }

    #[test]
    fn check_block_in_range_rejects_block_equal_to_allocated_count() {
        // allocated_block_count itself is one past the last valid index.
        let err = check_block_in_range(5, 5).unwrap_err();
        assert!(err.to_string().contains("illegal block number"));
    }

    #[test]
    fn build_paths_resolves_nested_paths() {
        let entries = [
            entry("sub", 0, ROOT_ENTRY_INDEX, true),
            entry("a.bin", 1, 0, false),
        ];
        let files = StfsReader::build_paths(&entries).unwrap();
        assert_eq!(files, [("sub/a.bin".to_owned(), 0, 0, true)]);
    }

    #[test]
    fn build_paths_rejects_traversal_in_own_name() {
        let entries = [entry("..", 0, ROOT_ENTRY_INDEX, false)];
        let err = StfsReader::build_paths(&entries).unwrap_err();
        assert!(err.to_string().contains("unsafe path component"));
    }

    #[test]
    fn build_paths_rejects_traversal_in_ancestor_directory_name() {
        let entries = [
            entry("..", 0, ROOT_ENTRY_INDEX, true),
            entry("evil.bin", 1, 0, false),
        ];
        let err = StfsReader::build_paths(&entries).unwrap_err();
        assert!(err.to_string().contains("unsafe path component"));
    }

    #[test]
    fn build_paths_rejects_embedded_separator() {
        let entries = [entry("a/b", 0, ROOT_ENTRY_INDEX, false)];
        assert!(StfsReader::build_paths(&entries).is_err());
    }

    fn raw_file_entry_bytes(
        name: &str,
        is_directory: bool,
        is_contiguous: bool,
        path_indicator: u16,
        starting_block_num: u32,
        file_size: u32,
    ) -> [u8; FILE_ENTRY_SIZE] {
        let mut buf = [0u8; FILE_ENTRY_SIZE];
        buf[0..name.len()].copy_from_slice(name.as_bytes());
        let flags = (u8::from(is_directory) << 1) | u8::from(is_contiguous);
        buf[NAME_LEN_OFFSET] = (flags << 6) | (name.len() as u8 & 0x3F);
        buf[0x2F..PATH_INDICATOR_OFFSET].copy_from_slice(&starting_block_num.to_le_bytes()[0..3]);
        buf[PATH_INDICATOR_OFFSET..PATH_INDICATOR_OFFSET + 2]
            .copy_from_slice(&path_indicator.to_be_bytes());
        buf[PATH_INDICATOR_OFFSET + 2..PATH_INDICATOR_OFFSET + 6]
            .copy_from_slice(&file_size.to_be_bytes());
        buf
    }

    #[test]
    fn raw_file_entry_bytes_round_trips_through_decode_file_entry_bytes() {
        let buf = raw_file_entry_bytes("dir", true, false, ROOT_ENTRY_INDEX, 0, 0);
        match decode_file_entry_bytes(&buf, 0) {
            EntrySlot::Entry(e) => {
                assert_eq!(e.name, "dir");
                assert!(e.is_directory);
                assert_eq!(e.path_indicator, ROOT_ENTRY_INDEX);
            }
            _ => panic!("expected a real entry"),
        }
    }

    fn valid_stfs_paths_bytes() -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(&raw_file_entry_bytes(
            "dir",
            true,
            false,
            ROOT_ENTRY_INDEX,
            0,
            0,
        ));
        buf.extend_from_slice(&raw_file_entry_bytes("file.txt", false, true, 0, 5, 1024));
        buf
    }

    #[test]
    fn valid_stfs_paths_bytes_resolves_via_fuzz_build_paths_decode() {
        let data = valid_stfs_paths_bytes();
        let entries: Vec<RawEntry> = data
            .chunks_exact(FILE_ENTRY_SIZE)
            .enumerate()
            .filter_map(|(i, chunk)| {
                let buf: &[u8; FILE_ENTRY_SIZE] = chunk.try_into().expect("chunks_exact");
                match decode_file_entry_bytes(buf, i as u16) {
                    EntrySlot::Entry(e) => Some(e),
                    _ => None,
                }
            })
            .collect();
        let files = StfsReader::build_paths(&entries).expect("listing should resolve");
        assert_eq!(files, [("dir/file.txt".to_owned(), 5, 1024, true)]);
    }

    #[test]
    #[ignore = "writes fuzz corpus seed files; run explicitly with --ignored"]
    fn write_fuzz_corpus_seed_for_stfs_paths() {
        let data = valid_stfs_paths_bytes();
        let dir = "fuzz/corpus/stfs_paths";
        std::fs::create_dir_all(dir).expect("corpus directory should be creatable");
        std::fs::write(format!("{dir}/seed-minimal-nested"), &data)
            .expect("seed file should be writable");
    }
}
