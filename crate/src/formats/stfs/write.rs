//! Write-side (STFS/CON as a target format).
//! `<https://free60.org/System-Software/Formats/STFS/>`
//!
//! STFS interleaves hash-table blocks with data blocks, so this writer
//! walks physical blocks in address order and decides block-by-block
//! whether to emit a hash-table block or the next data block. See
//! [`StfsLayout::step_physical_block`].

use super::format::{
    AvatarItemMetadata, BLOCK_SIZE, FILE_ENTRIES_PER_BLOCK, FILE_ENTRY_SIZE, InstallerMetadata,
    Level, MAGIC_CON, NAME_LEN_OFFSET, PATH_INDICATOR_OFFSET, ROOT_ENTRY_INDEX, StfsMetadata,
    StfsVolumeDescriptor, TOP_RECORD_SIZE, VideoMetadata, header_offset,
};
use super::hash_tree::{BlockLink, HashTree, HashTreeBuilder};
use crate::core::executable::TitleExecutionInfo;
use crate::core::extracted_fs::ExtractedFilesystem;
use crate::core::iso::{IsoReader, SECTOR_SIZE};
use crate::core::signing::{ConHeaderBuilder, ConsoleSigningKey};
use crate::core::source::{
    ImageSource, ProbedDirectoryTable, SourceReader, title_info_from_exe_bytes,
};
use crate::core::title::{ContentType, TitleInfo};
use crate::game_list;
use crate::session::ChunkSource;
use crate::utils::mstime::ms_timestamp_now;
use std::collections::HashMap;
use std::io::Cursor;

const DEFAULT_BLOCK_SEPARATION: u8 = 0x00;

/// Header size for a non-PEC package with `DEFAULT_BLOCK_SEPARATION`.
const DEFAULT_HEADER_SIZE: u32 = 0x971A;

/// Header size used when an Installer trailer is being written, sized
/// for the largest trailer variant so any variant fits.
fn installer_trailer_header_size() -> u32 {
    u32::try_from(header_offset::INSTALLER_METADATA + header_offset::INSTALLER_TRAILER_GATE_LEN)
        .expect("INSTALLER_METADATA + INSTALLER_TRAILER_GATE_LEN is a small fixed header offset")
}

const END_OF_CHAIN: u32 = 0x00FF_FFFF;

/// Metadata field offsets within the header, relative to its start.
/// All multi-byte fields are big-endian.
mod write_meta_offset {
    #[cfg(test)]
    pub(crate) const CONTENT_TYPE: usize = 0x344;
    #[cfg(test)]
    pub(crate) const TITLE_ID: usize = 0x360;
}

/// The four identity fields written into the STFS header as one group.
#[derive(Clone, Copy)]
struct HeaderIdentity<'a> {
    console_id: [u8; 5],
    profile_id: [u8; 8],
    device_id: &'a [u8; 20],
    online_creator: [u8; 8],
}

#[derive(Clone, Copy, Default)]
pub(crate) struct IdentityOverrides {
    pub(crate) console_id: Option<[u8; 5]>,
    pub(crate) profile_id: Option<[u8; 8]>,
    pub(crate) device_id: Option<[u8; 20]>,
    pub(crate) online_creator: Option<[u8; 8]>,
}

struct PlannedEntry {
    name: String,
    is_directory: bool,
    /// `ROOT_ENTRY_INDEX` for root-level entries.
    parent_index: u16,
    is_contiguous: bool,
    /// 0 for directories.
    starting_block_num: u32,
    /// 0 for directories.
    file_size: u32,
    source_file_index: Option<usize>,
}

pub(crate) struct StfsLayout {
    entries: Vec<PlannedEntry>,
    total_blocks: u32,
    top_level: Level,
    sex_shift: u32,
    block_step: [u64; 2],
    first_hash_table_address: u64,
    header_size: u32,
    file_table_block_count: u16,
    file_table_block_num: u32,
}

fn entry_block_count(file_size: u32) -> u32 {
    if file_size == 0 {
        1
    } else {
        u32::try_from(u64::from(file_size).div_ceil(BLOCK_SIZE)).expect("block count fits in u32")
    }
}

struct PlanNode {
    name: String,
    is_directory: bool,
    parent: Option<usize>,
    entry_index: Option<u16>,
    source_file_index: Option<usize>,
    file_size: u32,
}

fn build_tree(files: &[(String, u64)]) -> Result<Vec<PlanNode>, anyhow::Error> {
    let mut nodes: Vec<PlanNode> = Vec::new();
    let mut path_to_node: HashMap<String, usize> = HashMap::new();

    for (file_idx, (path, size)) in files.iter().enumerate() {
        let parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
        anyhow::ensure!(!parts.is_empty(), "stfs: empty path in file list");
        let mut current_path = String::new();
        let mut parent_idx: Option<usize> = None;

        for (i, part) in parts.iter().enumerate() {
            let is_last = i == parts.len() - 1;
            if !current_path.is_empty() {
                current_path.push('/');
            }
            current_path.push_str(part);

            let node_idx = if let Some(&idx) = path_to_node.get(&current_path) {
                idx
            } else {
                let idx = nodes.len();
                nodes.push(PlanNode {
                    name: (*part).to_string(),
                    is_directory: !is_last,
                    parent: parent_idx,
                    entry_index: None,
                    source_file_index: if is_last { Some(file_idx) } else { None },
                    file_size: if is_last {
                        u32::try_from(*size)
                            .map_err(|_| anyhow::anyhow!("stfs: file size exceeds u32::MAX"))?
                    } else {
                        0
                    },
                });
                path_to_node.insert(current_path.clone(), idx);
                idx
            };
            parent_idx = Some(node_idx);
        }
    }

    anyhow::ensure!(!nodes.is_empty(), "stfs: at least one file is required");
    anyhow::ensure!(
        nodes.len() < usize::from(u16::MAX),
        "stfs: too many entries for a 16-bit entry index"
    );

    for (idx, node) in nodes.iter_mut().enumerate() {
        node.entry_index = Some(u16::try_from(idx).expect("checked above"));
    }

    Ok(nodes)
}

fn assign_layout(nodes: &[PlanNode], file_table_block_count: u16) -> (Vec<PlannedEntry>, u32) {
    let parent_entry_index_of = |node: &PlanNode| -> u16 {
        match node.parent {
            Some(p) => nodes[p].entry_index.expect("parents assigned above"),
            None => ROOT_ENTRY_INDEX,
        }
    };

    let mut next_data_block = u32::from(file_table_block_count);
    let mut planned_entries = Vec::with_capacity(nodes.len());
    for node in nodes {
        let parent_index = parent_entry_index_of(node);
        let (starting_block_num, is_contiguous) = if node.is_directory {
            (0, false)
        } else {
            let start = next_data_block;
            next_data_block += entry_block_count(node.file_size);
            (start, true)
        };

        planned_entries.push(PlannedEntry {
            name: node.name.clone(),
            is_directory: node.is_directory,
            parent_index,
            is_contiguous,
            starting_block_num,
            file_size: node.file_size,
            source_file_index: node.source_file_index,
        });
    }

    (planned_entries, next_data_block)
}

impl StfsLayout {
    fn plan(
        files: &[(String, u64)],
        block_separation: u8,
        has_installer_trailer: bool,
    ) -> Result<Self, anyhow::Error> {
        let nodes = build_tree(files)?;

        let total_entries = nodes.len();
        let file_table_block_count =
            u16::try_from(total_entries.div_ceil(FILE_ENTRIES_PER_BLOCK).max(1))
                .map_err(|_| anyhow::anyhow!("stfs: file table too large"))?;
        let file_table_block_num = 0u32;

        let (planned_entries, total_blocks) = assign_layout(&nodes, file_table_block_count);

        let sex_shift = u32::from((!block_separation) & 1);
        let block_step = if sex_shift == 0 {
            [0xAB, 0x718F]
        } else {
            [0xAC, 0x723A]
        };
        let header_size = if has_installer_trailer {
            installer_trailer_header_size()
        } else {
            DEFAULT_HEADER_SIZE
        };
        let first_hash_table_address = u64::from((header_size + 0xFFF) & 0xFFFF_F000);

        let top_level = match total_blocks {
            n if n <= 0xAA => Level::Zero,
            n if n <= 0x70E4 => Level::One,
            n if n <= 0x4A_F768 => Level::Two,
            _ => anyhow::bail!("stfs: total blocks exceed Level Two capacity"),
        };

        Ok(Self {
            entries: planned_entries,
            total_blocks,
            top_level,
            sex_shift,
            block_step,
            first_hash_table_address,
            header_size,
            file_table_block_count,
            file_table_block_num,
        })
    }

    // Address math, identical to StfsReader's - see read.rs.
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

    fn compute_level1_backing_hash_block_number(&self, block_num: u64) -> u64 {
        let shift = self.sex_shift;
        if block_num < 0x70E4 {
            self.block_step[0]
        } else {
            (1 << shift) + (block_num / 0x70E4) * self.block_step[1]
        }
    }

    /// Physical block housing the level-2 (top) table - always one.
    fn compute_level2_backing_hash_block_number(&self, _block_num: u64) -> u64 {
        self.block_step[1]
    }

    fn total_physical_blocks(&self) -> u64 {
        if self.total_blocks == 0 {
            return 0;
        }
        self.compute_backing_data_block_number(u64::from(self.total_blocks - 1)) + 1
    }

    fn total_output_size(&self) -> u64 {
        self.first_hash_table_address + self.total_physical_blocks() * BLOCK_SIZE
    }

    fn entry_for_data_block(&self, block_num: u32) -> Option<usize> {
        self.entries.iter().position(|e| {
            !e.is_directory
                && block_num >= e.starting_block_num
                && block_num < e.starting_block_num + entry_block_count(e.file_size)
        })
    }

    fn is_last_block_of_chain(&self, block_num: u32) -> bool {
        if block_num + 1 == u32::from(self.file_table_block_count) {
            return true;
        }
        if block_num < u32::from(self.file_table_block_count) {
            return false;
        }
        match self.entry_for_data_block(block_num) {
            Some(idx) => {
                let e = &self.entries[idx];
                let last = e.starting_block_num + entry_block_count(e.file_size) - 1;
                block_num == last
            }
            None => true, // shouldn't happen for a well-formed layout
        }
    }
}

enum PhysicalStep {
    HashLevel0 { group_start: u32, group_end: u32 },
    HashUpperLevel { at_top: bool, group_index: u32 },
    Data { block_num: u32 },
}

impl StfsLayout {
    fn step_physical_block(
        &self,
        physical_cursor: &mut u64,
        block_num: &mut u32,
    ) -> Option<PhysicalStep> {
        if *block_num >= self.total_blocks {
            return None;
        }
        let target = self.compute_backing_data_block_number(u64::from(*block_num));
        if *physical_cursor < target {
            let at_level1_table = self.top_level != Level::Zero
                && *physical_cursor
                    == self.compute_level1_backing_hash_block_number(u64::from(*block_num));
            let at_level2_table = self.top_level == Level::Two
                && *physical_cursor
                    == self.compute_level2_backing_hash_block_number(u64::from(*block_num));
            let step = if at_level1_table || at_level2_table {
                PhysicalStep::HashUpperLevel {
                    at_top: at_level2_table || self.top_level == Level::One,
                    group_index: *block_num / 0x70E4,
                }
            } else {
                let group_start = *block_num - (*block_num % 0xAA);
                let group_end = (group_start + 0xAA).min(self.total_blocks);
                PhysicalStep::HashLevel0 {
                    group_start,
                    group_end,
                }
            };
            *physical_cursor += 1;
            return Some(step);
        }
        let step = PhysicalStep::Data {
            block_num: *block_num,
        };
        *physical_cursor += 1;
        *block_num += 1;
        Some(step)
    }
}

#[cfg(test)]
mod physical_block_placement_tests {
    use super::*;

    fn level_two_layout() -> StfsLayout {
        const TARGET_DATA_BLOCKS: u64 = 0x70E4 + 1; // total_blocks = 0x70E4 + 2
        let files = vec![(
            "big.bin".to_string(),
            TARGET_DATA_BLOCKS * BLOCK_SIZE, // exact multiple, no ceil-rounding surprises
        )];
        let layout = StfsLayout::plan(&files, DEFAULT_BLOCK_SEPARATION, false)
            .expect("level-two layout should plan fine");
        assert_eq!(
            layout.top_level,
            Level::Two,
            "test setup should force Level::Two"
        );
        layout
    }

    #[test]
    fn second_top_groups_level1_table_is_not_at_the_first_groups_position() {
        let layout = level_two_layout();
        let first_group_pos = layout.compute_level1_backing_hash_block_number(0);
        let second_group_pos = layout.compute_level1_backing_hash_block_number(0x70E4);
        assert_eq!(first_group_pos, layout.block_step[0]);
        assert_ne!(second_group_pos, first_group_pos);
    }

    #[test]
    fn step_physical_block_places_a_hash_table_at_the_second_groups_level1_position() {
        let layout = level_two_layout();
        let expected_pos = layout.compute_level1_backing_hash_block_number(0x70E4);

        let mut physical_cursor = 0u64;
        let mut block_num = 0u32;
        let mut step_at_expected_pos = None;
        while step_at_expected_pos.is_none() {
            let cursor_before = physical_cursor;
            match layout.step_physical_block(&mut physical_cursor, &mut block_num) {
                None => break,
                Some(step) if cursor_before == expected_pos => {
                    step_at_expected_pos = Some(step);
                }
                Some(_) => {}
            }
        }
        assert!(
            matches!(
                step_at_expected_pos,
                Some(PhysicalStep::HashUpperLevel { .. })
            ),
            "expected a HashUpperLevel step at the second top-group's \
             level-1 table position ({expected_pos})"
        );
    }
}

// Backing dispatch: an open image source, or an already-extracted filesystem.

enum WriteBacking {
    Image {
        reader: Box<dyn ImageSource>,
        file_offsets: Vec<u64>,
        probed: Option<ProbedDirectoryTable>,
    },
    Fs(Box<ExtractedFilesystem>),
}

impl WriteBacking {
    fn read_exact_in_file(
        &mut self,
        file_idx: usize,
        offset: u64,
        buf: &mut [u8],
    ) -> Result<(), anyhow::Error> {
        match self {
            Self::Image {
                reader,
                file_offsets,
                ..
            } => {
                let base = *file_offsets
                    .get(file_idx)
                    .ok_or_else(|| anyhow::anyhow!("stfs: file index {file_idx} out of range"))?;
                reader.read_bytes(base + offset, buf)
            }
            Self::Fs(fs) => fs.read_file_range(file_idx, offset, buf),
        }
    }

    fn probed(&mut self) -> Result<&ProbedDirectoryTable, anyhow::Error> {
        match self {
            Self::Image { reader, probed, .. } => {
                if probed.is_none() {
                    let source_reader = SourceReader::new(reader.as_mut());
                    let mut iso_reader = IsoReader::read(source_reader).map_err(|e| {
                        anyhow::anyhow!("failed to detect XDVDFS root offset: {e:?}")
                    })?;
                    let title_info = TitleInfo::from_image(&mut iso_reader)?;
                    *probed = Some(ProbedDirectoryTable {
                        directory_table: iso_reader.directory_table,
                        title_info,
                    });
                }
                Ok(probed.as_ref().expect("just set above"))
            }
            Self::Fs(_) => {
                anyhow::bail!("stfs: extracted sources have no XDVDFS directory table to probe")
            }
        }
    }

    fn resolve_title_info(&mut self) -> Result<TitleInfo, anyhow::Error> {
        match self {
            Self::Image { .. } => Ok(self.probed()?.title_info.clone()),
            Self::Fs(fs) => {
                let (exe_bytes, is_xex) = fs.read_launch_executable()?;
                title_info_from_exe_bytes(&exe_bytes, is_xex)
            }
        }
    }

    fn original_content_type(&self) -> Option<ContentType> {
        match self {
            Self::Image { .. } => None,
            Self::Fs(fs) => fs.stfs_content_type(),
        }
    }

    fn original_raw_content_type(&self) -> Option<u32> {
        match self {
            Self::Image { .. } => None,
            Self::Fs(fs) => fs.stfs_raw_content_type(),
        }
    }

    fn original_console_id(&self) -> Option<[u8; 5]> {
        match self {
            Self::Image { .. } => None,
            Self::Fs(fs) => fs.stfs_console_id(),
        }
    }

    fn original_profile_id(&self) -> Option<[u8; 8]> {
        match self {
            Self::Image { .. } => None,
            Self::Fs(fs) => fs.stfs_profile_id(),
        }
    }

    fn original_device_id(&self) -> Option<[u8; 20]> {
        match self {
            Self::Image { .. } => None,
            Self::Fs(fs) => fs.stfs_device_id(),
        }
    }

    fn original_online_creator(&self) -> Option<[u8; 8]> {
        match self {
            Self::Image { .. } => None,
            Self::Fs(fs) => fs.stfs_online_creator(),
        }
    }

    fn original_display_name(&self) -> Option<String> {
        match self {
            Self::Image { .. } => None,
            Self::Fs(fs) => fs.stfs_display_name(),
        }
    }

    fn original_avatar_item_metadata(&self) -> Option<AvatarItemMetadata> {
        match self {
            Self::Image { .. } => None,
            Self::Fs(fs) => fs.stfs_avatar_item_metadata(),
        }
    }

    fn original_video_metadata(&self) -> Option<VideoMetadata> {
        match self {
            Self::Image { .. } => None,
            Self::Fs(fs) => fs.stfs_video_metadata(),
        }
    }

    fn original_installer_metadata(&self) -> Option<InstallerMetadata> {
        match self {
            Self::Image { .. } => None,
            Self::Fs(fs) => fs.stfs_installer_metadata(),
        }
    }

    fn original_license_entries(&self) -> Option<[u8; header_offset::LICENSE_ENTRIES_1_15_LEN]> {
        match self {
            Self::Image { .. } => None,
            Self::Fs(fs) => fs.stfs_license_entries(),
        }
    }

    fn file_entries(&mut self) -> Result<Vec<(String, u64)>, anyhow::Error> {
        match self {
            Self::Image { .. } => {
                let directory_table = self.probed()?.directory_table.clone();
                let entries: Vec<_> = directory_table
                    .entries
                    .into_iter()
                    .filter(|e| !e.is_directory())
                    .collect();
                let Self::Image { file_offsets, .. } = self else {
                    unreachable!("checked above")
                };
                *file_offsets = entries
                    .iter()
                    .map(|e| u64::from(e.sector) * SECTOR_SIZE)
                    .collect();
                Ok(entries
                    .into_iter()
                    .map(|e| (e.path, u64::from(e.size)))
                    .collect())
            }
            Self::Fs(fs) => Ok(fs.file_entries()),
        }
    }
}

// Phase B: streaming writer

#[derive(Clone, Copy, PartialEq, Eq)]
enum WritePhase {
    Header,
    Body,
    Done,
}

pub(crate) struct StfsWriteSession {
    layout: StfsLayout,
    backing: WriteBacking,
    display_name: Option<String>,
    title_id: u32,
    execution_info: Option<TitleExecutionInfo>,
    content_type: u32,
    /// Console ID (0x36C). Zeroed unless overridden or preserved.
    console_id: [u8; 5],
    profile_id: [u8; 8],
    device_id: [u8; 20],
    /// Online Creator XUID (0x3AD), distinct from `profile_id`.
    online_creator: [u8; 8],
    license_entries: [u8; header_offset::LICENSE_ENTRIES_1_15_LEN],
    avatar_item_metadata: Option<AvatarItemMetadata>,
    video_metadata: Option<VideoMetadata>,
    installer_metadata: Option<InstallerMetadata>,
    created_timestamp: u32,
    output_name: String,
    phase: WritePhase,
    physical_cursor: u64,
    block_num: u32,

    signing_key: Option<ConsoleSigningKey>,
    /// Only allocated while a signed session's pre-hash pass is running.
    hasher: Option<HashTreeBuilder>,
    hash_tree: Option<HashTree>,
    next_hash_block: u32,
    hashing_done: bool,
    /// One block's worth of scratch space, reused by `fill_data_block_scratch`/
    /// `fill_file_table_block_scratch` instead of allocating a fresh `Vec` per
    /// call. For a signed session these are called twice per logical block -
    /// once from `hash_next_block`'s pre-pass, once from `next_chunk`'s
    /// streaming pass - and the buffer is reused across both.
    block_scratch: Vec<u8>,
}

impl StfsWriteSession {
    pub(crate) fn open(
        image_source: Box<dyn ImageSource>,
        content_type_override: Option<ContentType>,
        display_name_override: Option<String>,
        title_id_override: Option<u32>,
        identity_overrides: IdentityOverrides,
        signing_key: Option<ConsoleSigningKey>,
        probed: Option<ProbedDirectoryTable>,
    ) -> Result<Self, anyhow::Error> {
        let backing = WriteBacking::Image {
            reader: image_source,
            file_offsets: Vec::new(),
            probed,
        };
        Self::open_inner(
            backing,
            content_type_override,
            display_name_override,
            title_id_override,
            identity_overrides,
            signing_key,
        )
    }

    pub(crate) fn open_from_extracted(
        fs: ExtractedFilesystem,
        content_type_override: Option<ContentType>,
        display_name_override: Option<String>,
        title_id_override: Option<u32>,
        identity_overrides: IdentityOverrides,
        signing_key: Option<ConsoleSigningKey>,
    ) -> Result<Self, anyhow::Error> {
        Self::open_inner(
            WriteBacking::Fs(Box::new(fs)),
            content_type_override,
            display_name_override,
            title_id_override,
            identity_overrides,
            signing_key,
        )
    }

    fn open_inner(
        mut backing: WriteBacking,
        content_type_override: Option<ContentType>,
        display_name_override: Option<String>,
        title_id_override: Option<u32>,
        identity_overrides: IdentityOverrides,
        signing_key: Option<ConsoleSigningKey>,
    ) -> Result<Self, anyhow::Error> {
        let IdentityOverrides {
            console_id: console_id_override,
            profile_id: profile_id_override,
            device_id: device_id_override,
            online_creator: online_creator_override,
        } = identity_overrides;
        let exe_probe = if title_id_override.is_none() {
            Some(backing.resolve_title_info())
        } else {
            None
        };
        let detected_content_type = exe_probe
            .as_ref()
            .and_then(|r| r.as_ref().ok())
            .map(|info| info.content_type);

        let content_type = content_type_override
            .or_else(|| backing.original_content_type())
            .or(detected_content_type);
        let content_type_u32 = content_type
            .map(|ct| ct as u32)
            .or_else(|| backing.original_raw_content_type())
            .unwrap_or(ContentType::GamesOnDemand as u32);

        let console_id = console_id_override
            .or_else(|| backing.original_console_id())
            .unwrap_or([0u8; 5]);
        let profile_id = profile_id_override
            .or_else(|| backing.original_profile_id())
            .unwrap_or([0u8; 8]);
        let device_id = device_id_override
            .or_else(|| backing.original_device_id())
            .unwrap_or([0u8; 20]);
        let online_creator = online_creator_override
            .or_else(|| backing.original_online_creator())
            .unwrap_or([0u8; 8]);
        let license_entries = backing
            .original_license_entries()
            .unwrap_or([0u8; header_offset::LICENSE_ENTRIES_1_15_LEN]);

        let (title_id, execution_info) = match (title_id_override, exe_probe) {
            (Some(id), _) => (id, None),
            (None, Some(Ok(info))) => (info.execution_info.title_id, Some(info.execution_info)),
            (None, Some(Err(e))) => {
                if content_type.is_some_and(ContentType::requires_launch_executable) {
                    return Err(anyhow::anyhow!(
                        "stfs: failed to resolve launch executable (pass an explicit \
                         title_id to skip this for non-bootable content): {e:#}"
                    ));
                }
                (0, None)
            }
            (None, None) => (0, None), // unreachable: exe_probe is Some whenever title_id_override is None
        };

        let display_name = display_name_override
            .or_else(|| backing.original_display_name())
            .or_else(|| game_list::find_title_by_id(title_id));

        let avatar_item_metadata = if content_type == Some(ContentType::AvatarItem) {
            backing.original_avatar_item_metadata()
        } else {
            None
        };
        let video_metadata = if content_type == Some(ContentType::Video) {
            backing.original_video_metadata()
        } else {
            None
        };

        let installer_metadata = backing.original_installer_metadata();

        let files = backing.file_entries()?;
        let layout = StfsLayout::plan(
            &files,
            DEFAULT_BLOCK_SEPARATION,
            installer_metadata.is_some(),
        )?;

        let output_name = format!("{title_id:08X}");

        let hasher = signing_key
            .as_ref()
            .map(|_| HashTreeBuilder::new(layout.top_level, layout.total_blocks));

        Ok(Self {
            layout,
            backing,
            display_name,
            title_id,
            execution_info,
            content_type: content_type_u32,
            console_id,
            profile_id,
            device_id,
            online_creator,
            license_entries,
            avatar_item_metadata,
            video_metadata,
            installer_metadata,
            created_timestamp: ms_timestamp_now(),
            output_name,
            phase: WritePhase::Header,
            physical_cursor: 0,
            block_num: 0,
            hashing_done: signing_key.is_none(),
            signing_key,
            hasher,
            hash_tree: None,
            next_hash_block: 0,
            block_scratch: Vec::new(),
        })
    }

    pub(crate) fn hash_next_block(&mut self) -> Result<bool, anyhow::Error> {
        if self.hashing_done {
            return Ok(true);
        }
        let block_num = self.next_hash_block;
        if block_num < u32::from(self.layout.file_table_block_count) {
            self.fill_file_table_block_scratch(block_num);
        } else {
            self.fill_data_block_scratch(block_num)?;
        }
        let link = BlockLink {
            status: 0x80,
            next_block: if self.layout.is_last_block_of_chain(block_num) {
                END_OF_CHAIN
            } else {
                block_num + 1
            },
        };
        self.hasher
            .as_mut()
            .expect("hasher is Some whenever hashing_done is false - set together in open_inner")
            .hash_block(block_num, &self.block_scratch, link);
        self.next_hash_block += 1;
        if self.next_hash_block == self.layout.total_blocks {
            let finished = self
                .hasher
                .take()
                .expect("checked non-None just above")
                .finish();
            self.hash_tree = Some(finished);
            self.hashing_done = true;
        }
        Ok(self.hashing_done)
    }

    fn emit_header(&self) -> Result<Vec<u8>, anyhow::Error> {
        match &self.signing_key {
            None => Ok(Self::build_header(
                &self.layout,
                self.content_type,
                self.title_id,
                self.execution_info.as_ref(),
                self.display_name.as_deref(),
                self.avatar_item_metadata,
                self.video_metadata,
                self.installer_metadata.as_ref(),
                HeaderIdentity {
                    console_id: self.console_id,
                    profile_id: self.profile_id,
                    device_id: &self.device_id,
                    online_creator: self.online_creator,
                },
                &[0u8; 20],
            )),
            Some(key) => self.build_signed_header(key),
        }
    }

    fn build_signed_header(&self, key: &ConsoleSigningKey) -> Result<Vec<u8>, anyhow::Error> {
        let hash_tree = self
            .hash_tree
            .as_ref()
            .expect("hashing_done implies hash_tree is Some for a signed session");
        let buf = Self::build_header(
            &self.layout,
            self.content_type,
            self.title_id,
            self.execution_info.as_ref(),
            self.display_name.as_deref(),
            self.avatar_item_metadata,
            self.video_metadata,
            self.installer_metadata.as_ref(),
            HeaderIdentity {
                console_id: self.console_id,
                profile_id: self.profile_id,
                device_id: &self.device_id,
                online_creator: self.online_creator,
            },
            &hash_tree.top_hash,
        );
        let license_off = usize::try_from(header_offset::LICENSE_ENTRIES_1_15)
            .expect("LICENSE_ENTRIES_1_15 offset fits in usize");
        ConHeaderBuilder::from_buffer(buf)
            .with_raw_bytes(license_off, &self.license_entries)
            .finalize_signed(key)
    }

    fn build_header(
        layout: &StfsLayout,
        content_type: u32,
        title_id: u32,
        execution_info: Option<&TitleExecutionInfo>,
        display_name: Option<&str>,
        avatar_item_metadata: Option<AvatarItemMetadata>,
        video_metadata: Option<VideoMetadata>,
        installer_metadata: Option<&InstallerMetadata>,
        identity: HeaderIdentity<'_>,
        top_hash: &[u8; 20],
    ) -> Vec<u8> {
        let HeaderIdentity {
            console_id,
            profile_id,
            device_id,
            online_creator,
        } = identity;
        let mut buf = vec![
            0u8;
            usize::try_from(layout.first_hash_table_address)
                .expect("first_hash_table_address fits in usize")
        ];
        buf[0..4].copy_from_slice(&MAGIC_CON);

        let metadata = StfsMetadata {
            license_entries_1_15: [0u8; header_offset::LICENSE_ENTRIES_1_15_LEN],
            header_hash: [0u8; 20],
            header_size: layout.header_size,
            content_type,
            metadata_version: 0,
            media_id: execution_info.map_or(0, |info| info.media_id),
            title_id,
            platform: execution_info.map_or(0, |info| info.platform),
            executable_type: execution_info.map_or(0, |info| info.executable_type),
            disc_number: execution_info.map_or(0, |info| info.disc_number),
            disc_count: execution_info.map_or(0, |info| info.disc_count),
            save_game_id: execution_info.map_or(0, |info| info.save_game_id),
            console_id,
            profile_id,
            volume_descriptor: StfsVolumeDescriptor {
                size: 0x24,
                block_separation: DEFAULT_BLOCK_SEPARATION,
                file_table_block_count: layout.file_table_block_count,
                file_table_block_num: layout.file_table_block_num,
                top_hash_table_hash: *top_hash,
                allocated_block_count: layout.total_blocks,
            },
            online_creator,
            device_id: *device_id,
        };
        metadata
            .write_at(&mut Cursor::new(&mut buf))
            .expect("writing a fixed-size header region into an already-sized buffer cannot fail");

        if let Some(name) = display_name {
            let display_name_off = usize::try_from(header_offset::DISPLAY_NAME)
                .expect("DISPLAY_NAME offset fits in usize");
            for (i, unit) in name
                .encode_utf16()
                .take(header_offset::DISPLAY_NAME_MAX_UNITS)
                .enumerate()
            {
                let off = display_name_off + i * 2;
                buf[off..off + 2].copy_from_slice(&unit.to_be_bytes());
            }
        }

        if let Some(AvatarItemMetadata {
            sub_category,
            colorizable,
            guid,
            skeleton_version,
        }) = avatar_item_metadata
        {
            let off = usize::try_from(header_offset::AVATAR_ITEM_METADATA)
                .expect("AVATAR_ITEM_METADATA offset fits in usize");
            // Little-endian, matching Velocity's SwapEndian() here.
            buf[off..off + 4].copy_from_slice(&sub_category.to_le_bytes());
            buf[off + 4..off + 8].copy_from_slice(&colorizable.to_le_bytes());
            buf[off + 8..off + 24].copy_from_slice(&guid);
            buf[off + 24] = skeleton_version;
        }

        if let Some(VideoMetadata {
            series_id,
            season_id,
            season_number,
            episode_number,
        }) = video_metadata
        {
            let off = usize::try_from(header_offset::VIDEO_METADATA)
                .expect("VIDEO_METADATA offset fits in usize");
            // Big-endian, unlike avatar_item_metadata's SwapEndian()'d region.
            buf[off..off + 16].copy_from_slice(&series_id);
            buf[off + 16..off + 32].copy_from_slice(&season_id);
            buf[off + 32..off + 34].copy_from_slice(&season_number.to_be_bytes());
            buf[off + 34..off + 36].copy_from_slice(&episode_number.to_be_bytes());
        }

        if let Some(metadata) = installer_metadata {
            let off = usize::try_from(header_offset::INSTALLER_METADATA)
                .expect("INSTALLER_METADATA offset fits in usize");
            buf[off..off + 4].copy_from_slice(&metadata.raw_installer_type().to_be_bytes());
            match metadata {
                InstallerMetadata::None => {}
                InstallerMetadata::Version {
                    base_version,
                    version,
                    ..
                } => {
                    buf[off + 4..off + 8].copy_from_slice(&base_version.to_packed().to_be_bytes());
                    buf[off + 8..off + 12].copy_from_slice(&version.to_packed().to_be_bytes());
                }
                InstallerMetadata::ProgressCache {
                    resume_state,
                    current_file_index,
                    current_file_offset,
                    bytes_processed,
                    last_modified_high,
                    last_modified_low,
                    cab_resume_data,
                    ..
                } => {
                    buf[off + 4..off + 8].copy_from_slice(&resume_state.to_be_bytes());
                    buf[off + 8..off + 12].copy_from_slice(&current_file_index.to_be_bytes());
                    buf[off + 12..off + 20].copy_from_slice(&current_file_offset.to_be_bytes());
                    buf[off + 20..off + 28].copy_from_slice(&bytes_processed.to_be_bytes());
                    buf[off + 28..off + 32].copy_from_slice(&last_modified_high.to_be_bytes());
                    buf[off + 32..off + 36].copy_from_slice(&last_modified_low.to_be_bytes());
                    buf[off + 36..off + 36 + header_offset::INSTALLER_CAB_RESUME_DATA_LEN]
                        .copy_from_slice(cab_resume_data.as_slice());
                }
            }
        }

        buf
    }

    fn emit_hash_level0(&self, group_start: u32, group_end: u32) -> Vec<u8> {
        if let Some(tree) = &self.hash_tree {
            let table = if self.layout.top_level == Level::Zero {
                &tree.top
            } else {
                let idx = usize::try_from(group_start / 0xAA).expect("group index fits in usize");
                &tree.level0[idx]
            };
            return table.to_vec();
        }

        let mut buf = vec![0u8; usize::try_from(BLOCK_SIZE).expect("BLOCK_SIZE fits in usize")];
        for block_num in group_start..group_end {
            let local = usize::try_from(block_num % 0xAA).expect("small");
            let off =
                local * usize::try_from(TOP_RECORD_SIZE).expect("TOP_RECORD_SIZE fits in usize");
            buf[off + 0x14] = 0x80;
            let next = if self.layout.is_last_block_of_chain(block_num) {
                END_OF_CHAIN
            } else {
                block_num + 1
            };
            buf[off + 0x15] = ((next >> 16) & 0xFF) as u8;
            buf[off + 0x16] = ((next >> 8) & 0xFF) as u8;
            buf[off + 0x17] = (next & 0xFF) as u8;
        }
        buf
    }

    fn emit_hash_upper_level(&self, at_top: bool, group_index: u32) -> Vec<u8> {
        if let Some(tree) = &self.hash_tree {
            let table = if at_top {
                &tree.top
            } else {
                &tree.level1[usize::try_from(group_index).expect("group index fits in usize")]
            };
            return table.to_vec();
        }
        vec![0u8; usize::try_from(BLOCK_SIZE).expect("BLOCK_SIZE fits in usize")]
    }

    /// Writes file-table block `table_block` into `self.block_scratch`
    /// (growing it once to `BLOCK_SIZE` on first use, zeroed fresh on every
    /// call so a shorter final table block doesn't leak a previous call's
    /// tail bytes past its used entries).
    fn fill_file_table_block_scratch(&mut self, table_block: u32) {
        let block_len = usize::try_from(BLOCK_SIZE).expect("BLOCK_SIZE fits in usize");
        if self.block_scratch.len() < block_len {
            self.block_scratch.resize(block_len, 0);
        }
        self.block_scratch[..block_len].fill(0);
        let start = table_block as usize * FILE_ENTRIES_PER_BLOCK;
        let end = (start + FILE_ENTRIES_PER_BLOCK).min(self.layout.entries.len());
        let entries = &self.layout.entries[start..end];
        let created_timestamp = self.created_timestamp;
        for (i, entry) in entries.iter().enumerate() {
            let off = i * FILE_ENTRY_SIZE;
            let slice = &mut self.block_scratch[off..off + FILE_ENTRY_SIZE];

            let name_bytes = entry.name.as_bytes();
            let name_len = name_bytes.len().min(NAME_LEN_OFFSET);
            slice[..name_len].copy_from_slice(&name_bytes[..name_len]);

            let mut name_len_byte = u8::try_from(name_len).unwrap_or(0x3F) & 0x3F;
            if name_len_byte == 0 {
                name_len_byte = 1;
            }
            let flags = (u8::from(entry.is_contiguous)) | (u8::from(entry.is_directory) << 1);
            slice[NAME_LEN_OFFSET] = name_len_byte | (flags << 6);

            let blocks_for_file = if entry.is_directory {
                0u32
            } else {
                entry_block_count(entry.file_size)
            };
            let bff = blocks_for_file.to_le_bytes();
            slice[0x29] = bff[0];
            slice[0x2A] = bff[1];
            slice[0x2B] = bff[2];
            slice[0x2C] = bff[0];
            slice[0x2D] = bff[1];
            slice[0x2E] = bff[2];

            let sb = entry.starting_block_num;
            slice[0x2F] = (sb & 0xFF) as u8;
            slice[0x30] = ((sb >> 8) & 0xFF) as u8;
            slice[0x31] = ((sb >> 16) & 0xFF) as u8;

            slice[PATH_INDICATOR_OFFSET..PATH_INDICATOR_OFFSET + 2]
                .copy_from_slice(&entry.parent_index.to_be_bytes());
            slice[PATH_INDICATOR_OFFSET + 2..PATH_INDICATOR_OFFSET + 6]
                .copy_from_slice(&entry.file_size.to_be_bytes());

            slice[0x38..0x3C].copy_from_slice(&created_timestamp.to_be_bytes());
            slice[0x3C..0x40].copy_from_slice(&created_timestamp.to_be_bytes());
        }
    }

    /// Fills `self.block_scratch` with data block `block_num`'s content
    /// (growing it once to `BLOCK_SIZE` on first use, zeroed fresh on every
    /// call so a shorter final block of a file doesn't leak a previous
    /// call's tail bytes past `to_read` into this block's zero-padding).
    ///
    /// Called from both `hash_next_block`'s pre-pass and `next_chunk`'s
    /// streaming pass for a signed session, so the underlying source bytes
    /// genuinely get read twice - that's inherent to hashing before
    /// streaming without holding the whole image in memory in between -
    /// but the buffer itself is reused rather than freshly allocated
    /// each time.
    fn fill_data_block_scratch(&mut self, block_num: u32) -> Result<(), anyhow::Error> {
        let idx = self.layout.entry_for_data_block(block_num).ok_or_else(|| {
            anyhow::anyhow!("stfs: internal error - no entry owns block {block_num}")
        })?;
        let entry = &self.layout.entries[idx];
        let file_idx = entry.source_file_index.ok_or_else(|| {
            anyhow::anyhow!("stfs: internal error - data block on a directory entry")
        })?;
        let offset_in_file = u64::from(block_num - entry.starting_block_num) * BLOCK_SIZE;
        let remaining = u64::from(entry.file_size) - offset_in_file;
        let to_read = remaining.min(BLOCK_SIZE) as usize;

        let block_len = usize::try_from(BLOCK_SIZE).expect("BLOCK_SIZE fits in usize");
        if self.block_scratch.len() < block_len {
            self.block_scratch.resize(block_len, 0);
        }
        self.block_scratch[..block_len].fill(0);
        self.backing.read_exact_in_file(
            file_idx,
            offset_in_file,
            &mut self.block_scratch[..to_read],
        )?;
        Ok(())
    }
}

impl ChunkSource for StfsWriteSession {
    fn next_chunk(&mut self, _max_bytes: usize) -> Result<Option<Vec<u8>>, anyhow::Error> {
        if !self.hashing_done {
            return Err(anyhow::anyhow!(
                "stfs: next_chunk called before hashing finished - call \
                 hash_next_block() until it returns true first"
            ));
        }
        match self.phase {
            WritePhase::Header => {
                self.phase = WritePhase::Body;
                Ok(Some(self.emit_header()?))
            }
            WritePhase::Body => {
                match self
                    .layout
                    .step_physical_block(&mut self.physical_cursor, &mut self.block_num)
                {
                    None => {
                        self.phase = WritePhase::Done;
                        Ok(None)
                    }
                    Some(PhysicalStep::HashUpperLevel {
                        at_top,
                        group_index,
                    }) => Ok(Some(self.emit_hash_upper_level(at_top, group_index))),
                    Some(PhysicalStep::HashLevel0 {
                        group_start,
                        group_end,
                    }) => Ok(Some(self.emit_hash_level0(group_start, group_end))),
                    Some(PhysicalStep::Data { block_num }) => {
                        if block_num < u32::from(self.layout.file_table_block_count) {
                            self.fill_file_table_block_scratch(block_num);
                        } else {
                            self.fill_data_block_scratch(block_num)?;
                        }
                        Ok(Some(self.block_scratch.clone()))
                    }
                }
            }
            WritePhase::Done => Ok(None),
        }
    }

    fn is_done(&self) -> bool {
        self.phase == WritePhase::Done
    }

    fn total_units(&self) -> u64 {
        u64::from(self.layout.total_blocks)
    }

    fn units_done(&self) -> Option<u64> {
        None
    }

    fn current_entry_name(&self) -> Option<&str> {
        if self.phase != WritePhase::Body || self.block_num == 0 {
            return None;
        }
        let just_emitted = self.block_num - 1;
        if just_emitted < u32::from(self.layout.file_table_block_count) {
            return None;
        }
        self.layout
            .entry_for_data_block(just_emitted)
            .map(|idx| self.layout.entries[idx].name.as_str())
    }

    fn output_manifest(&self) -> Vec<(String, u64)> {
        vec![(self.output_name.clone(), self.layout.total_output_size())]
    }
}

#[cfg(test)]
mod header_padding_tests {
    use super::super::format::{InstallerVersion, ProgressCacheKind};
    use super::*;

    pub(super) fn small_layout() -> StfsLayout {
        let files = vec![("default.xex".to_string(), 100u64)];
        StfsLayout::plan(&files, DEFAULT_BLOCK_SEPARATION, false)
            .expect("tiny layout should plan fine")
    }

    fn small_layout_with_installer_trailer() -> StfsLayout {
        let files = vec![("default.xex".to_string(), 100u64)];
        StfsLayout::plan(&files, DEFAULT_BLOCK_SEPARATION, true)
            .expect("tiny layout with installer trailer should plan fine")
    }

    fn zero_identity(device_id: &[u8; 20]) -> HeaderIdentity<'_> {
        HeaderIdentity {
            console_id: [0u8; 5],
            profile_id: [0u8; 8],
            device_id,
            online_creator: [0u8; 8],
        }
    }

    #[test]
    fn header_length_is_padded_to_first_hash_table_address() {
        let layout = small_layout();
        let header = StfsWriteSession::build_header(
            &layout,
            0,
            0,
            None, // execution_info
            None,
            None,
            None,
            None,
            zero_identity(&[0u8; 20]),
            &[0u8; 20],
        );
        assert_eq!(header.len() as u64, layout.first_hash_table_address);
    }

    #[test]
    fn execution_info_fields_are_written_at_their_header_offsets() {
        let layout = small_layout();
        let info = TitleExecutionInfo {
            media_id: 0x1122_3344,
            version: 0,
            base_version: 0,
            title_id: 0x5555_6666,
            platform: 2,
            executable_type: 1,
            disc_number: 1,
            disc_count: 1,
            save_game_id: 0x7788_99AA,
        };
        let header = StfsWriteSession::build_header(
            &layout,
            0,
            info.title_id,
            Some(&info),
            None,
            None,
            None,
            None,
            zero_identity(&[0u8; 20]),
            &[0u8; 20],
        );
        assert_eq!(&header[0x354..0x358], &info.media_id.to_be_bytes());
        assert_eq!(&header[0x360..0x364], &info.title_id.to_be_bytes());
        assert_eq!(header[0x364], info.platform);
        assert_eq!(header[0x365], info.executable_type);
        assert_eq!(header[0x366], info.disc_number);
        assert_eq!(header[0x367], info.disc_count);
        assert_eq!(&header[0x368..0x36C], &info.save_game_id.to_be_bytes());
    }

    #[test]
    fn header_length_is_not_the_raw_unpadded_header_size() {
        let layout = small_layout();
        let header = StfsWriteSession::build_header(
            &layout,
            0,
            0,
            None, // execution_info
            None,
            None,
            None,
            None,
            zero_identity(&[0u8; 20]),
            &[0u8; 20],
        );
        assert_ne!(header.len(), DEFAULT_HEADER_SIZE as usize);
        assert!(header.len() as u32 > DEFAULT_HEADER_SIZE);
    }

    #[test]
    fn padding_bytes_past_metadata_are_zeroed() {
        let layout = small_layout();
        let header = StfsWriteSession::build_header(
            &layout,
            0,
            0,
            None, // execution_info
            None,
            None,
            None,
            None,
            zero_identity(&[0u8; 20]),
            &[0u8; 20],
        );
        let padding = &header[DEFAULT_HEADER_SIZE as usize..];
        assert!(padding.iter().all(|&b| b == 0));
    }

    #[test]
    fn total_output_size_uses_padded_header_length() {
        let layout = small_layout();
        let expected =
            layout.first_hash_table_address + layout.total_physical_blocks() * BLOCK_SIZE;
        assert_eq!(layout.total_output_size(), expected);
        let unpadded_total =
            u64::from(DEFAULT_HEADER_SIZE) + layout.total_physical_blocks() * BLOCK_SIZE;
        assert_ne!(layout.total_output_size(), unpadded_total);
    }

    #[test]
    fn header_size_field_is_written_and_resolves_to_the_same_first_hash_table_address() {
        let layout = small_layout();
        let header = StfsWriteSession::build_header(
            &layout,
            0,
            0,
            None, // execution_info
            None,
            None,
            None,
            None,
            zero_identity(&[0u8; 20]),
            &[0u8; 20],
        );

        let header_size_off = usize::try_from(header_offset::HEADER_SIZE).unwrap();
        let written = u32::from_be_bytes(
            header[header_size_off..header_size_off + 4]
                .try_into()
                .unwrap(),
        );
        assert_eq!(
            written, DEFAULT_HEADER_SIZE,
            "header_size field must be written, not left zeroed"
        );

        let resolved_first_hash_table_address = u64::from((written + 0xFFF) & 0xFFFF_F000);
        assert_eq!(
            resolved_first_hash_table_address,
            layout.first_hash_table_address
        );
    }

    #[test]
    fn magic_and_metadata_survive_the_padded_buffer() {
        let layout = small_layout();
        let header = StfsWriteSession::build_header(
            &layout,
            0x7000,
            0x4141_4141,
            None, // execution_info
            Some("Test"),
            None,
            None,
            None,
            zero_identity(&[0u8; 20]),
            &[0u8; 20],
        );
        assert_eq!(&header[0..4], &MAGIC_CON);
        assert_eq!(
            &header[write_meta_offset::CONTENT_TYPE..write_meta_offset::CONTENT_TYPE + 4],
            &0x7000u32.to_be_bytes()
        );
        assert_eq!(
            &header[write_meta_offset::TITLE_ID..write_meta_offset::TITLE_ID + 4],
            &0x4141_4141u32.to_be_bytes()
        );
    }

    #[test]
    fn console_profile_device_id_are_written_at_their_header_offsets() {
        let layout = small_layout();
        let console_id = [1u8, 2, 3, 4, 5];
        let profile_id = [6u8, 7, 8, 9, 10, 11, 12, 13];
        let device_id: [u8; 20] = std::array::from_fn(|i| i as u8 + 20);
        let online_creator = [14u8, 15, 16, 17, 18, 19, 20, 21];
        let header = StfsWriteSession::build_header(
            &layout,
            0,
            0,
            None, // execution_info
            None,
            None,
            None,
            None,
            HeaderIdentity {
                console_id,
                profile_id,
                device_id: &device_id,
                online_creator,
            },
            &[0u8; 20],
        );

        let cid = usize::try_from(header_offset::CONSOLE_ID).unwrap();
        assert_eq!(&header[cid..cid + 5], &console_id);

        let pid = usize::try_from(header_offset::PROFILE_ID).unwrap();
        assert_eq!(&header[pid..pid + 8], &profile_id);

        let did = usize::try_from(header_offset::DEVICE_ID).unwrap();
        assert_eq!(&header[did..did + 20], &device_id);

        let ocid = usize::try_from(header_offset::ONLINE_CREATOR).unwrap();
        assert_eq!(&header[ocid..ocid + 8], &online_creator);

        assert_eq!(
            pid + 8,
            usize::try_from(header_offset::VOLUME_DESCRIPTOR).unwrap()
        );
        assert!(ocid >= usize::try_from(header_offset::VOLUME_DESCRIPTOR).unwrap() + 0x24);
    }

    #[test]
    fn avatar_item_metadata_is_written_at_its_header_offset() {
        let layout = small_layout();
        let metadata = AvatarItemMetadata {
            sub_category: 0x0102_0304,
            colorizable: 1,
            guid: std::array::from_fn(|i| i as u8),
            skeleton_version: 2,
        };
        let header = StfsWriteSession::build_header(
            &layout,
            0x9000,
            0,
            None, // execution_info
            None,
            Some(metadata),
            None,
            None,
            zero_identity(&[0u8; 20]),
            &[0u8; 20],
        );

        let off = usize::try_from(header_offset::AVATAR_ITEM_METADATA).unwrap();
        assert_eq!(&header[off..off + 4], &metadata.sub_category.to_le_bytes());
        assert_eq!(
            &header[off + 4..off + 8],
            &metadata.colorizable.to_le_bytes()
        );
        assert_eq!(&header[off + 8..off + 24], &metadata.guid);
        assert_eq!(header[off + 24], metadata.skeleton_version);
    }

    #[test]
    fn avatar_item_metadata_region_stays_zeroed_when_none() {
        let layout = small_layout();
        let header = StfsWriteSession::build_header(
            &layout,
            0,
            0,
            None, // execution_info
            None,
            None,
            None,
            None,
            zero_identity(&[0u8; 20]),
            &[0u8; 20],
        );
        let off = usize::try_from(header_offset::AVATAR_ITEM_METADATA).unwrap();
        assert!(
            header[off..off + header_offset::AVATAR_ITEM_METADATA_LEN]
                .iter()
                .all(|&b| b == 0)
        );
    }

    #[test]
    fn video_metadata_is_written_at_its_header_offset() {
        let layout = small_layout();
        let series_id: [u8; 16] = std::array::from_fn(|i| i as u8);
        let season_id: [u8; 16] = std::array::from_fn(|i| i as u8 + 0x40);
        let metadata = VideoMetadata {
            series_id,
            season_id,
            season_number: 7,
            episode_number: 13,
        };
        let header = StfsWriteSession::build_header(
            &layout,
            0x90000,
            0,
            None, // execution_info
            None,
            None,
            Some(metadata),
            None,
            zero_identity(&[0u8; 20]),
            &[0u8; 20],
        );

        let off = usize::try_from(header_offset::VIDEO_METADATA).unwrap();
        assert_eq!(&header[off..off + 16], &series_id);
        assert_eq!(&header[off + 16..off + 32], &season_id);
        assert_eq!(&header[off + 32..off + 34], &7u16.to_be_bytes());
        assert_eq!(&header[off + 34..off + 36], &13u16.to_be_bytes());
    }

    #[test]
    fn video_metadata_region_stays_zeroed_when_none() {
        let layout = small_layout();
        let header = StfsWriteSession::build_header(
            &layout,
            0,
            0,
            None, // execution_info
            None,
            None,
            None,
            None,
            zero_identity(&[0u8; 20]),
            &[0u8; 20],
        );
        let off = usize::try_from(header_offset::VIDEO_METADATA).unwrap();
        assert!(
            header[off..off + header_offset::VIDEO_METADATA_LEN]
                .iter()
                .all(|&b| b == 0)
        );
    }

    #[test]
    fn installer_trailer_grows_header_past_default_size() {
        let default_layout = small_layout();
        let trailer_layout = small_layout_with_installer_trailer();
        assert_ne!(
            trailer_layout.first_hash_table_address,
            default_layout.first_hash_table_address
        );
        assert_eq!(trailer_layout.first_hash_table_address, 0xB000);
        assert_eq!(default_layout.first_hash_table_address, 0xA000);

        let header = StfsWriteSession::build_header(
            &trailer_layout,
            0,
            0,
            None, // execution_info
            None,
            None,
            None,
            Some(&InstallerMetadata::None),
            zero_identity(&[0u8; 20]),
            &[0u8; 20],
        );
        assert_eq!(header.len() as u64, trailer_layout.first_hash_table_address);
        assert_eq!(header.len(), 0xB000);
    }

    #[test]
    fn installer_metadata_none_variant_is_written_at_its_header_offset() {
        let layout = small_layout_with_installer_trailer();
        let header = StfsWriteSession::build_header(
            &layout,
            0,
            0,
            None, // execution_info
            None,
            None,
            None,
            Some(&InstallerMetadata::None),
            zero_identity(&[0u8; 20]),
            &[0u8; 20],
        );
        let off = usize::try_from(header_offset::INSTALLER_METADATA).unwrap();
        assert_eq!(&header[off..off + 4], &0u32.to_be_bytes());
        let gate_len = usize::try_from(header_offset::INSTALLER_TRAILER_GATE_LEN).unwrap();
        assert!(header[off + 4..off + gate_len].iter().all(|&b| b == 0));
    }

    #[test]
    fn installer_metadata_version_variant_is_written_at_its_header_offset() {
        let layout = small_layout_with_installer_trailer();
        let base_version = InstallerVersion {
            major: 1,
            minor: 2,
            build: 0x1234,
            revision: 5,
        };
        let version = InstallerVersion {
            major: 3,
            minor: 4,
            build: 0x5678,
            revision: 9,
        };
        let metadata = InstallerMetadata::Version {
            is_title_update: true,
            base_version,
            version,
        };
        let header = StfsWriteSession::build_header(
            &layout,
            0,
            0,
            None, // execution_info
            None,
            None,
            None,
            Some(&metadata),
            zero_identity(&[0u8; 20]),
            &[0u8; 20],
        );
        let off = usize::try_from(header_offset::INSTALLER_METADATA).unwrap();
        assert_eq!(
            &header[off..off + 4],
            &metadata.raw_installer_type().to_be_bytes()
        );
        assert_eq!(
            &header[off + 4..off + 8],
            &base_version.to_packed().to_be_bytes()
        );
        assert_eq!(
            &header[off + 8..off + 12],
            &version.to_packed().to_be_bytes()
        );
    }

    #[test]
    fn installer_metadata_progress_cache_variant_is_written_at_its_header_offset() {
        let layout = small_layout_with_installer_trailer();
        let mut cab_resume_data = Box::new([0u8; header_offset::INSTALLER_CAB_RESUME_DATA_LEN]);
        for (i, b) in cab_resume_data.iter_mut().enumerate() {
            *b = (i % 251) as u8;
        }
        let cab_resume_data_expected = cab_resume_data.clone();
        let metadata = InstallerMetadata::ProgressCache {
            kind: ProgressCacheKind::TitleContent,
            resume_state: 0x0102_0304,
            current_file_index: 7,
            current_file_offset: 0x1122_3344_5566_7788,
            bytes_processed: 0x99AA_BBCC_DDEE_FF00,
            last_modified_high: 0x0A0B_0C0D,
            last_modified_low: 0x0E0F_1011,
            cab_resume_data,
        };
        let header = StfsWriteSession::build_header(
            &layout,
            0,
            0,
            None, // execution_info
            None,
            None,
            None,
            Some(&metadata),
            zero_identity(&[0u8; 20]),
            &[0u8; 20],
        );
        let off = usize::try_from(header_offset::INSTALLER_METADATA).unwrap();
        assert_eq!(
            &header[off..off + 4],
            &metadata.raw_installer_type().to_be_bytes()
        );
        assert_eq!(&header[off + 4..off + 8], &0x0102_0304u32.to_be_bytes());
        assert_eq!(&header[off + 8..off + 12], &7u32.to_be_bytes());
        assert_eq!(
            &header[off + 12..off + 20],
            &0x1122_3344_5566_7788u64.to_be_bytes()
        );
        assert_eq!(
            &header[off + 20..off + 28],
            &0x99AA_BBCC_DDEE_FF00u64.to_be_bytes()
        );
        assert_eq!(&header[off + 28..off + 32], &0x0A0B_0C0Du32.to_be_bytes());
        assert_eq!(&header[off + 32..off + 36], &0x0E0F_1011u32.to_be_bytes());
        assert_eq!(
            &header[off + 36..off + 36 + header_offset::INSTALLER_CAB_RESUME_DATA_LEN],
            cab_resume_data_expected.as_slice()
        );
    }

    #[test]
    fn installer_metadata_region_stays_zeroed_and_header_default_sized_when_none() {
        let layout = small_layout();
        let header = StfsWriteSession::build_header(
            &layout,
            0,
            0,
            None, // execution_info
            None,
            None,
            None,
            None,
            zero_identity(&[0u8; 20]),
            &[0u8; 20],
        );
        assert_eq!(header.len(), 0xA000);
        let off = usize::try_from(header_offset::INSTALLER_METADATA).unwrap();
        assert!(header[off..].iter().all(|&b| b == 0));
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
            0
        }
        fn image_offset(&self) -> u64 {
            0
        }
    }

    fn unsigned_session() -> StfsWriteSession {
        let layout = header_padding_tests::small_layout();
        StfsWriteSession {
            layout,
            backing: WriteBacking::Image {
                reader: Box::new(ZeroSource),
                file_offsets: vec![0],
                probed: None,
            },
            display_name: None,
            title_id: 0,
            execution_info: None,
            content_type: ContentType::GamesOnDemand as u32,
            console_id: [0u8; 5],
            profile_id: [0u8; 8],
            device_id: [0u8; 20],
            online_creator: [0u8; 8],
            license_entries: [0u8; header_offset::LICENSE_ENTRIES_1_15_LEN],
            avatar_item_metadata: None,
            video_metadata: None,
            installer_metadata: None,
            created_timestamp: 0,
            output_name: "00000000".to_string(),
            phase: WritePhase::Header,
            physical_cursor: 0,
            block_num: 0,
            signing_key: None,
            hasher: None,
            hash_tree: None,
            next_hash_block: 0,
            hashing_done: true,
            block_scratch: Vec::new(),
        }
    }

    #[wasm_bindgen_test]
    fn streams_full_output_through_the_real_wasm_bindgen_abi() {
        let session = unsigned_session();
        let expected_total = session.layout.total_output_size();
        let mut conversion = ConversionSession::new(SessionInner::Stfs(Box::new(session)));

        assert!(
            conversion
                .hash_next_part()
                .expect("hash_next_part must not error for an unsigned session")
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
                    // Round-trips through js_sys::Uint8Array - the actual ABI boundary under test.
                    collected.extend(bytes.to_vec());
                }
                None => break,
            }
        }
        assert!(conversion.is_done());
        assert_eq!(
            collected.len() as u64,
            expected_total,
            "uncompressed STFS output must match the layout's declared total size exactly"
        );
    }

    #[wasm_bindgen_test]
    fn next_chunk_before_hashing_done_is_a_catchable_error_not_a_panic() {
        let mut session = unsigned_session();
        // Force the "signed but not yet hashed" state without needing a real ConsoleSigningKey.
        session.hashing_done = false;
        let mut conversion = ConversionSession::new(SessionInner::Stfs(Box::new(session)));

        let result = conversion.next_chunk(4096);
        assert!(
            result.is_err(),
            "next_chunk before hashing_done must surface as Err through the real ABI"
        );
    }
}
