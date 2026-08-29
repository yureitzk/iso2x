//! Shared on-disk layout constants for Xbox 360 STFS (LIVE/PIRS/CON)
//! packages, used by both `read.rs` and `write.rs`.
//! `<https://free60.org/System-Software/Formats/STFS/>`

use crate::core::title::ContentType;
use binrw::{BinRead, BinWrite};
use std::io::{Read, Seek, SeekFrom, Write};
use wasm_bindgen::prelude::*;

pub(crate) const MAGIC_CON: [u8; 4] = *b"CON ";
pub(crate) const MAGIC_LIVE: [u8; 4] = *b"LIVE";
pub(crate) const MAGIC_PIRS: [u8; 4] = *b"PIRS";

/// Fixed absolute header offsets - identical for CON/LIVE/PIRS.
pub(super) mod header_offset {
    pub(crate) const HEADER_SIZE: u64 = 0x340;
    /// Test-only cross-check against `StfsMetadata`'s binrw layout - see
    /// `stfs_metadata_fields_land_at_documented_header_offsets`.
    #[cfg(test)]
    pub(crate) const VOLUME_DESCRIPTOR: u64 = 0x379;
    #[cfg(test)]
    pub(crate) const CONSOLE_ID: u64 = 0x36C;
    #[cfg(test)]
    pub(crate) const PROFILE_ID: u64 = 0x371;
    /// Unconfirmed meaning.
    #[cfg(test)]
    pub(crate) const ONLINE_CREATOR: u64 = 0x3AD;

    pub(crate) const DEVICE_ID: u64 = 0x3FD;
    pub(crate) const DESCRIPTOR_TYPE: u64 = 0x3A9;

    pub(crate) const LICENSE_TABLE: u64 = 0x22C;
    pub(crate) const LICENSE_ENTRY_SIZE: u64 = 0x10;
    pub(crate) const LICENSE_ENTRIES_1_15: u64 = LICENSE_TABLE + LICENSE_ENTRY_SIZE;
    pub(crate) const LICENSE_ENTRIES_1_15_LEN: usize = 0xF0;

    pub(crate) const METADATA_VERSION: u64 = 0x348;
    pub(crate) const THUMBNAIL_SIZE: u64 = 0x1712;
    pub(crate) const TITLE_THUMBNAIL_SIZE: u64 = 0x1716;
    pub(crate) const THUMBNAIL_IMAGE: u64 = 0x171A;

    pub(crate) const TITLE_THUMBNAIL_IMAGE: u64 = 0x571A;

    pub(crate) const INSTALLER_METADATA: u64 = 0x971A;
    pub(crate) const INSTALLER_TRAILER_GATE_LEN: u64 = 0x15F4;
    pub(crate) const INSTALLER_CAB_RESUME_DATA_LEN: usize = 0x15D0;

    pub(crate) const DISPLAY_NAME: u64 = 0x411;
    pub(crate) const DISPLAY_NAME_MAX_UNITS: usize = 0x80;

    pub(crate) const AVATAR_ITEM_METADATA: u64 = 0x3D9;
    pub(crate) const AVATAR_ITEM_METADATA_LEN: usize = 25;

    pub(crate) const VIDEO_METADATA: u64 = 0x3D9;
    pub(crate) const VIDEO_METADATA_LEN: usize = 0x24;
}

#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
#[derive(BinRead, BinWrite, Debug, Clone, Copy, PartialEq, Eq)]
#[brw(big)]
pub(crate) struct StfsVolumeDescriptor {
    pub(crate) size: u8,
    #[brw(pad_before = 1)]
    pub(crate) block_separation: u8,
    #[brw(little)]
    pub(crate) file_table_block_count: u16,
    #[br(map = |b: [u8; 3]| u32::from(b[0]) | (u32::from(b[1]) << 8) | (u32::from(b[2]) << 16))]
    #[bw(map = |v: &u32| [(*v & 0xFF) as u8, ((*v >> 8) & 0xFF) as u8, ((*v >> 16) & 0xFF) as u8])]
    pub(crate) file_table_block_num: u32,
    pub(crate) top_hash_table_hash: [u8; 20],
    #[brw(pad_after = 4)]
    pub(crate) allocated_block_count: u32,
}

#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
#[derive(BinRead, BinWrite, Debug, Clone, PartialEq, Eq)]
#[brw(big)]
pub struct StfsMetadata {
    #[brw(pad_before = 0x10)]
    pub(crate) license_entries_1_15: [u8; header_offset::LICENSE_ENTRIES_1_15_LEN],
    pub(crate) header_hash: [u8; 20],
    pub(crate) header_size: u32,
    pub(crate) content_type: u32,
    pub(crate) metadata_version: u32,
    #[brw(pad_before = 8)]
    pub(crate) media_id: u32,
    #[brw(pad_before = 8)]
    pub(crate) title_id: u32,
    pub(crate) platform: u8,
    pub(crate) executable_type: u8,
    pub(crate) disc_number: u8,
    pub(crate) disc_count: u8,
    pub(crate) save_game_id: u32,
    pub(crate) console_id: [u8; 5],
    pub(crate) profile_id: [u8; 8],
    pub(crate) volume_descriptor: StfsVolumeDescriptor,
    #[brw(pad_before = 0x10)]
    pub(crate) online_creator: [u8; 8],
    #[brw(pad_before = 0x48)]
    pub(crate) device_id: [u8; 20],
}

impl StfsMetadata {
    pub(crate) fn read_at<R: Read + Seek>(reader: &mut R) -> Result<Self, anyhow::Error> {
        reader.seek(SeekFrom::Start(header_offset::LICENSE_TABLE))?;
        <Self as BinRead>::read(reader)
            .map_err(|e| anyhow::anyhow!("stfs: failed to parse fixed header region: {e}"))
    }

    pub(crate) fn write_at<W: Write + Seek>(&self, writer: &mut W) -> Result<(), anyhow::Error> {
        writer.seek(SeekFrom::Start(header_offset::LICENSE_TABLE))?;
        BinWrite::write(self, writer)
            .map_err(|e| anyhow::anyhow!("stfs: failed to write fixed header region: {e}"))
    }
}

pub(super) const TOP_RECORD_SIZE: u64 = 0x18; // 0x14-byte hash + 1-byte status + 3-byte nextBlock
pub(super) const TOP_RECORD_SIZE_USIZE: usize = 0x18;
pub(super) const FILE_ENTRY_SIZE: usize = 0x40;
pub(super) const FILE_ENTRIES_PER_BLOCK: usize = 0x1000 / FILE_ENTRY_SIZE;
pub(super) const ROOT_ENTRY_INDEX: u16 = 0xFFFF;

pub(super) const NAME_LEN_OFFSET: usize = 0x28;
pub(super) const PATH_INDICATOR_OFFSET: usize = 0x32;

/// Fixed STFS data-block size.
pub(super) const BLOCK_SIZE: u64 = 0x1000;

const THUMBNAIL_MAX_SIZE_V1: u32 = 0x4000;
const THUMBNAIL_MAX_SIZE_V2: u32 = 0x3D00;

const PNG_MAGIC: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

#[wasm_bindgen(js_name = stfsFileEntrySize)]
pub fn stfs_file_entry_size() -> u32 {
    u32::try_from(FILE_ENTRY_SIZE).expect("FILE_ENTRY_SIZE is a small compile-time constant")
}

#[wasm_bindgen(js_name = stfsFileEntryNameLenOffset)]
pub fn stfs_file_entry_name_len_offset() -> u32 {
    u32::try_from(NAME_LEN_OFFSET).expect("NAME_LEN_OFFSET is a small compile-time constant")
}

#[wasm_bindgen(js_name = stfsFileEntryPathIndicatorOffset)]
pub fn stfs_file_entry_path_indicator_offset() -> u32 {
    u32::try_from(PATH_INDICATOR_OFFSET)
        .expect("PATH_INDICATOR_OFFSET is a small compile-time constant")
}

/// Which hash-table level tops a package, determined by total block count.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Level {
    Zero = 0,
    One = 1,
    Two = 2,
}

#[derive(Debug)]
pub(crate) struct HeaderPrefix {
    pub(crate) header_size: u32,
    pub(crate) raw_content_type: u32,
    pub(crate) content_type: Option<ContentType>,
}

pub(crate) fn read_header_prefix<R: Read + Seek>(
    reader: &mut R,
) -> Result<HeaderPrefix, anyhow::Error> {
    let mut magic = [0u8; 4];
    reader.read_exact(&mut magic)?;
    anyhow::ensure!(
        magic == MAGIC_CON || magic == MAGIC_LIVE || magic == MAGIC_PIRS,
        "not a LIVE/PIRS/CON-style header"
    );

    reader.seek(SeekFrom::Start(header_offset::HEADER_SIZE))?;
    let mut header_size_bytes = [0u8; 4];
    reader.read_exact(&mut header_size_bytes)?;
    let header_size = u32::from_be_bytes(header_size_bytes);

    let mut content_type_bytes = [0u8; 4];
    reader.read_exact(&mut content_type_bytes)?;
    let raw_content_type = u32::from_be_bytes(content_type_bytes);

    Ok(HeaderPrefix {
        header_size,
        raw_content_type,
        content_type: ContentType::from_u32(raw_content_type),
    })
}

#[derive(Default)]
pub(crate) struct HeaderThumbnails {
    pub(crate) thumbnail: Option<Vec<u8>>,
    pub(crate) title_thumbnail: Option<Vec<u8>>,
}

pub(crate) fn read_header_thumbnails<R: Read + Seek>(reader: &mut R) -> HeaderThumbnails {
    try_read_header_thumbnails(reader).unwrap_or_default()
}

fn try_read_header_thumbnails<R: Read + Seek>(
    reader: &mut R,
) -> Result<HeaderThumbnails, anyhow::Error> {
    reader.seek(SeekFrom::Start(header_offset::METADATA_VERSION))?;
    let mut version_bytes = [0u8; 4];
    reader.read_exact(&mut version_bytes)?;
    let max_size = if u32::from_be_bytes(version_bytes) == 2 {
        THUMBNAIL_MAX_SIZE_V2
    } else {
        THUMBNAIL_MAX_SIZE_V1
    };

    reader.seek(SeekFrom::Start(header_offset::THUMBNAIL_SIZE))?;
    let mut thumb_size_bytes = [0u8; 4];
    reader.read_exact(&mut thumb_size_bytes)?;
    let thumb_size = u32::from_be_bytes(thumb_size_bytes);

    reader.seek(SeekFrom::Start(header_offset::TITLE_THUMBNAIL_SIZE))?;
    let mut title_thumb_size_bytes = [0u8; 4];
    reader.read_exact(&mut title_thumb_size_bytes)?;
    let title_thumb_size = u32::from_be_bytes(title_thumb_size_bytes);

    Ok(HeaderThumbnails {
        thumbnail: read_one_thumbnail(reader, header_offset::THUMBNAIL_IMAGE, thumb_size, max_size),
        title_thumbnail: read_one_thumbnail(
            reader,
            header_offset::TITLE_THUMBNAIL_IMAGE,
            title_thumb_size,
            max_size,
        ),
    })
}

pub(crate) fn read_display_name<R: Read + Seek>(reader: &mut R) -> Option<String> {
    try_read_display_name(reader).ok().flatten()
}

fn try_read_display_name<R: Read + Seek>(reader: &mut R) -> Result<Option<String>, anyhow::Error> {
    reader.seek(SeekFrom::Start(header_offset::DISPLAY_NAME))?;
    let mut buf = vec![0u8; header_offset::DISPLAY_NAME_MAX_UNITS * 2];
    reader.read_exact(&mut buf)?;
    let units: Vec<u16> = buf
        .chunks_exact(2)
        .map(|b| u16::from_be_bytes([b[0], b[1]]))
        .take_while(|&u| u != 0)
        .collect();
    if units.is_empty() {
        return Ok(None);
    }
    Ok(Some(String::from_utf16_lossy(&units)))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct AvatarItemMetadata {
    pub(crate) sub_category: u32,
    pub(crate) colorizable: u32,
    pub(crate) guid: [u8; 16],
    pub(crate) skeleton_version: u8,
}

pub(crate) fn read_avatar_item_metadata<R: Read + Seek>(
    reader: &mut R,
) -> Option<AvatarItemMetadata> {
    try_read_avatar_item_metadata(reader).ok().flatten()
}

fn try_read_avatar_item_metadata<R: Read + Seek>(
    reader: &mut R,
) -> Result<Option<AvatarItemMetadata>, anyhow::Error> {
    reader.seek(SeekFrom::Start(header_offset::AVATAR_ITEM_METADATA))?;
    let mut buf = [0u8; header_offset::AVATAR_ITEM_METADATA_LEN];
    reader.read_exact(&mut buf)?;
    let sub_category = u32::from_le_bytes(buf[0..4].try_into().expect("4-byte slice"));
    let colorizable = u32::from_le_bytes(buf[4..8].try_into().expect("4-byte slice"));
    let mut guid = [0u8; 16];
    guid.copy_from_slice(&buf[8..24]);
    let skeleton_version = buf[24];
    if !(1..=3).contains(&skeleton_version) {
        return Ok(None);
    }
    Ok(Some(AvatarItemMetadata {
        sub_category,
        colorizable,
        guid,
        skeleton_version,
    }))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct VideoMetadata {
    pub(crate) series_id: [u8; 16],
    pub(crate) season_id: [u8; 16],
    pub(crate) season_number: u16,
    pub(crate) episode_number: u16,
}

pub(crate) fn read_video_metadata<R: Read + Seek>(reader: &mut R) -> Option<VideoMetadata> {
    try_read_video_metadata(reader).ok()
}

fn try_read_video_metadata<R: Read + Seek>(reader: &mut R) -> Result<VideoMetadata, anyhow::Error> {
    reader.seek(SeekFrom::Start(header_offset::VIDEO_METADATA))?;
    let mut buf = [0u8; header_offset::VIDEO_METADATA_LEN];
    reader.read_exact(&mut buf)?;
    let mut series_id = [0u8; 16];
    series_id.copy_from_slice(&buf[0..16]);
    let mut season_id = [0u8; 16];
    season_id.copy_from_slice(&buf[16..32]);
    let season_number = u16::from_be_bytes(buf[32..34].try_into().expect("2-byte slice"));
    let episode_number = u16::from_be_bytes(buf[34..36].try_into().expect("2-byte slice"));
    Ok(VideoMetadata {
        series_id,
        season_id,
        season_number,
        episode_number,
    })
}

mod installer_type {
    pub(super) const NONE: u32 = 0;
    pub(super) const SYSTEM_UPDATE: u32 = 0x5355_5044;
    pub(super) const TITLE_UPDATE: u32 = 0x5455_5044;
    pub(super) const SYSTEM_UPDATE_PROGRESS_CACHE: u32 = 0x5024_5355;
    pub(super) const TITLE_UPDATE_PROGRESS_CACHE: u32 = 0x5024_5455;
    pub(super) const TITLE_CONTENT_PROGRESS_CACHE: u32 = 0x5024_5443;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct InstallerVersion {
    /// 4-bit field, stored widened to `u8`.
    pub(crate) major: u8,
    /// 4-bit field, stored widened to `u8`.
    pub(crate) minor: u8,
    pub(crate) build: u16,
    pub(crate) revision: u8,
}

impl InstallerVersion {
    fn from_packed(raw: u32) -> Self {
        Self {
            major: (raw >> 28) as u8,
            minor: ((raw >> 24) & 0xF) as u8,
            build: ((raw >> 8) & 0xFFFF) as u16,
            revision: (raw & 0xFF) as u8,
        }
    }

    pub(crate) fn to_packed(self) -> u32 {
        (u32::from(self.major & 0xF) << 28)
            | (u32::from(self.minor & 0xF) << 24)
            | (u32::from(self.build) << 8)
            | u32::from(self.revision)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProgressCacheKind {
    SystemUpdate,
    TitleUpdate,
    TitleContent,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum InstallerMetadata {
    None,
    /// `installerType` is `SystemUpdate` or `TitleUpdate`.
    Version {
        is_title_update: bool,
        base_version: InstallerVersion,
        version: InstallerVersion,
    },
    ProgressCache {
        kind: ProgressCacheKind,
        resume_state: u32,
        current_file_index: u32,
        current_file_offset: u64,
        bytes_processed: u64,
        last_modified_high: u32,
        last_modified_low: u32,
        cab_resume_data: Box<[u8; header_offset::INSTALLER_CAB_RESUME_DATA_LEN]>,
    },
}

impl InstallerMetadata {
    pub(crate) fn raw_installer_type(&self) -> u32 {
        match self {
            Self::None => installer_type::NONE,
            Self::Version {
                is_title_update, ..
            } => {
                if *is_title_update {
                    installer_type::TITLE_UPDATE
                } else {
                    installer_type::SYSTEM_UPDATE
                }
            }
            Self::ProgressCache { kind, .. } => match kind {
                ProgressCacheKind::SystemUpdate => installer_type::SYSTEM_UPDATE_PROGRESS_CACHE,
                ProgressCacheKind::TitleUpdate => installer_type::TITLE_UPDATE_PROGRESS_CACHE,
                ProgressCacheKind::TitleContent => installer_type::TITLE_CONTENT_PROGRESS_CACHE,
            },
        }
    }
}

pub(crate) fn read_installer_metadata<R: Read + Seek>(
    reader: &mut R,
    first_hash_table_address: u64,
) -> Option<InstallerMetadata> {
    if first_hash_table_address.saturating_sub(header_offset::INSTALLER_METADATA)
        < header_offset::INSTALLER_TRAILER_GATE_LEN
    {
        return None;
    }
    try_read_installer_metadata(reader).ok().flatten()
}

fn read_installer_version<R: Read + Seek>(
    reader: &mut R,
) -> Result<InstallerVersion, anyhow::Error> {
    let mut buf = [0u8; 4];
    reader.read_exact(&mut buf)?;
    Ok(InstallerVersion::from_packed(u32::from_be_bytes(buf)))
}

fn try_read_installer_metadata<R: Read + Seek>(
    reader: &mut R,
) -> Result<Option<InstallerMetadata>, anyhow::Error> {
    reader.seek(SeekFrom::Start(header_offset::INSTALLER_METADATA))?;
    let mut raw_type = [0u8; 4];
    reader.read_exact(&mut raw_type)?;
    let raw_type = u32::from_be_bytes(raw_type);

    match raw_type {
        installer_type::NONE => Ok(Some(InstallerMetadata::None)),
        installer_type::SYSTEM_UPDATE | installer_type::TITLE_UPDATE => {
            let base_version = read_installer_version(reader)?;
            let version = read_installer_version(reader)?;
            Ok(Some(InstallerMetadata::Version {
                is_title_update: raw_type == installer_type::TITLE_UPDATE,
                base_version,
                version,
            }))
        }
        installer_type::SYSTEM_UPDATE_PROGRESS_CACHE
        | installer_type::TITLE_UPDATE_PROGRESS_CACHE
        | installer_type::TITLE_CONTENT_PROGRESS_CACHE => {
            let kind = match raw_type {
                installer_type::SYSTEM_UPDATE_PROGRESS_CACHE => ProgressCacheKind::SystemUpdate,
                installer_type::TITLE_UPDATE_PROGRESS_CACHE => ProgressCacheKind::TitleUpdate,
                _ => ProgressCacheKind::TitleContent,
            };
            let mut buf4 = [0u8; 4];
            reader.read_exact(&mut buf4)?;
            let resume_state = u32::from_be_bytes(buf4);
            reader.read_exact(&mut buf4)?;
            let current_file_index = u32::from_be_bytes(buf4);
            let mut buf8 = [0u8; 8];
            reader.read_exact(&mut buf8)?;
            let current_file_offset = u64::from_be_bytes(buf8);
            reader.read_exact(&mut buf8)?;
            let bytes_processed = u64::from_be_bytes(buf8);
            // WINFILETIME is written high DWORD then low DWORD.
            reader.read_exact(&mut buf4)?;
            let last_modified_high = u32::from_be_bytes(buf4);
            reader.read_exact(&mut buf4)?;
            let last_modified_low = u32::from_be_bytes(buf4);
            let mut cab_resume_data = Box::new([0u8; header_offset::INSTALLER_CAB_RESUME_DATA_LEN]);
            reader.read_exact(cab_resume_data.as_mut_slice())?;
            Ok(Some(InstallerMetadata::ProgressCache {
                kind,
                resume_state,
                current_file_index,
                current_file_offset,
                bytes_processed,
                last_modified_high,
                last_modified_low,
                cab_resume_data,
            }))
        }
        _ => Ok(None),
    }
}

fn read_one_thumbnail<R: Read + Seek>(
    reader: &mut R,
    offset: u64,
    size: u32,
    max_size: u32,
) -> Option<Vec<u8>> {
    if size == 0 || size > max_size {
        return None;
    }
    reader.seek(SeekFrom::Start(offset)).ok()?;
    let mut buf = vec![0u8; size as usize];
    reader.read_exact(&mut buf).ok()?;
    buf.starts_with(&PNG_MAGIC).then_some(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn con_header_with_content_type(content_type: u32) -> Vec<u8> {
        let mut buf = vec![0u8; 0x348];
        buf[0..4].copy_from_slice(b"CON ");
        buf[0x344..0x348].copy_from_slice(&content_type.to_be_bytes());
        buf
    }

    #[test]
    fn reads_recognized_content_type_from_con_header() {
        let buf = con_header_with_content_type(ContentType::InstalledGame as u32);
        let prefix = read_header_prefix(&mut Cursor::new(buf)).unwrap();
        assert_eq!(prefix.content_type, Some(ContentType::InstalledGame));
        assert_eq!(prefix.raw_content_type, ContentType::InstalledGame as u32);
    }

    #[test]
    fn reads_recognized_content_type_from_live_header() {
        let mut buf = con_header_with_content_type(ContentType::GamesOnDemand as u32);
        buf[0..4].copy_from_slice(b"LIVE");
        let prefix = read_header_prefix(&mut Cursor::new(buf)).unwrap();
        assert_eq!(prefix.content_type, Some(ContentType::GamesOnDemand));
    }

    #[test]
    fn reads_recognized_content_type_from_pirs_header() {
        let mut buf = con_header_with_content_type(ContentType::GamesOnDemand as u32);
        buf[0..4].copy_from_slice(b"PIRS");
        let prefix = read_header_prefix(&mut Cursor::new(buf)).unwrap();
        assert_eq!(prefix.content_type, Some(ContentType::GamesOnDemand));
    }

    #[test]
    fn unrecognized_content_type_value_maps_to_none_not_an_error() {
        let buf = con_header_with_content_type(0x00FF_0000);
        let prefix = read_header_prefix(&mut Cursor::new(buf)).unwrap();
        assert_eq!(prefix.content_type, None);
        assert_eq!(prefix.raw_content_type, 0x00FF_0000);
    }

    #[test]
    fn installer_metadata_gate_passes_at_exact_boundary() {
        let mut buf = vec![
            0u8;
            (header_offset::INSTALLER_METADATA + header_offset::INSTALLER_TRAILER_GATE_LEN)
                as usize
        ];
        buf[header_offset::INSTALLER_METADATA as usize..][..4]
            .copy_from_slice(&installer_type::NONE.to_be_bytes());
        let first_hash_table_address =
            header_offset::INSTALLER_METADATA + header_offset::INSTALLER_TRAILER_GATE_LEN;
        let metadata = read_installer_metadata(&mut Cursor::new(buf), first_hash_table_address);
        assert_eq!(metadata, Some(InstallerMetadata::None));
    }

    #[test]
    fn installer_metadata_gate_rejects_one_byte_short() {
        let buf = vec![
            0u8;
            (header_offset::INSTALLER_METADATA + header_offset::INSTALLER_TRAILER_GATE_LEN)
                as usize
        ];
        let first_hash_table_address =
            header_offset::INSTALLER_METADATA + header_offset::INSTALLER_TRAILER_GATE_LEN - 1;
        let metadata = read_installer_metadata(&mut Cursor::new(buf), first_hash_table_address);
        assert_eq!(metadata, None);
    }

    #[test]
    fn installer_metadata_none_variant_reads_back() {
        let mut buf = vec![0u8; header_offset::INSTALLER_METADATA as usize + 4];
        buf[header_offset::INSTALLER_METADATA as usize..][..4]
            .copy_from_slice(&installer_type::NONE.to_be_bytes());
        let first_hash_table_address =
            header_offset::INSTALLER_METADATA + header_offset::INSTALLER_TRAILER_GATE_LEN;
        let metadata = read_installer_metadata(&mut Cursor::new(buf), first_hash_table_address);
        assert_eq!(metadata, Some(InstallerMetadata::None));
    }

    #[test]
    fn installer_metadata_version_variant_reads_back() {
        let mut buf = vec![0u8; header_offset::INSTALLER_METADATA as usize + 12];
        let off = header_offset::INSTALLER_METADATA as usize;
        buf[off..][..4].copy_from_slice(&installer_type::TITLE_UPDATE.to_be_bytes());
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
        buf[off + 4..][..4].copy_from_slice(&base_version.to_packed().to_be_bytes());
        buf[off + 8..][..4].copy_from_slice(&version.to_packed().to_be_bytes());
        let first_hash_table_address =
            header_offset::INSTALLER_METADATA + header_offset::INSTALLER_TRAILER_GATE_LEN;
        let metadata = read_installer_metadata(&mut Cursor::new(buf), first_hash_table_address);
        assert_eq!(
            metadata,
            Some(InstallerMetadata::Version {
                is_title_update: true,
                base_version,
                version,
            })
        );
    }

    #[test]
    fn installer_metadata_progress_cache_variant_reads_back() {
        let off = header_offset::INSTALLER_METADATA as usize;
        let mut buf =
            vec![0u8; off + 4 + 4 + 4 + 8 + 8 + 8 + header_offset::INSTALLER_CAB_RESUME_DATA_LEN];
        buf[off..][..4]
            .copy_from_slice(&installer_type::SYSTEM_UPDATE_PROGRESS_CACHE.to_be_bytes());
        buf[off + 4..][..4].copy_from_slice(&0x4649_4C48u32.to_be_bytes()); // FileHeadersNotReady
        buf[off + 8..][..4].copy_from_slice(&7u32.to_be_bytes());
        buf[off + 12..][..8].copy_from_slice(&0x1122_3344_5566_7788u64.to_be_bytes());
        buf[off + 20..][..8].copy_from_slice(&0x99AA_BBCC_DDEE_FF00u64.to_be_bytes());
        buf[off + 28..][..4].copy_from_slice(&0x1111_2222u32.to_be_bytes());
        buf[off + 32..][..4].copy_from_slice(&0x3333_4444u32.to_be_bytes());
        let cab_start = off + 36;
        for (i, b) in buf[cab_start..][..header_offset::INSTALLER_CAB_RESUME_DATA_LEN]
            .iter_mut()
            .enumerate()
        {
            *b = (i % 256) as u8;
        }
        let first_hash_table_address =
            header_offset::INSTALLER_METADATA + header_offset::INSTALLER_TRAILER_GATE_LEN;
        let metadata = read_installer_metadata(&mut Cursor::new(buf), first_hash_table_address)
            .expect("progress cache variant should parse");
        match metadata {
            InstallerMetadata::ProgressCache {
                kind,
                resume_state,
                current_file_index,
                current_file_offset,
                bytes_processed,
                last_modified_high,
                last_modified_low,
                cab_resume_data,
            } => {
                assert_eq!(kind, ProgressCacheKind::SystemUpdate);
                assert_eq!(resume_state, 0x4649_4C48);
                assert_eq!(current_file_index, 7);
                assert_eq!(current_file_offset, 0x1122_3344_5566_7788);
                assert_eq!(bytes_processed, 0x99AA_BBCC_DDEE_FF00);
                assert_eq!(last_modified_high, 0x1111_2222);
                assert_eq!(last_modified_low, 0x3333_4444);
                assert_eq!(cab_resume_data[1], 1);
            }
            other => panic!("expected ProgressCache, got {other:?}"),
        }
    }

    #[test]
    fn installer_metadata_unrecognized_type_maps_to_none_not_an_error() {
        let mut buf = vec![0u8; header_offset::INSTALLER_METADATA as usize + 4];
        buf[header_offset::INSTALLER_METADATA as usize..][..4]
            .copy_from_slice(&0xDEAD_BEEFu32.to_be_bytes());
        let first_hash_table_address =
            header_offset::INSTALLER_METADATA + header_offset::INSTALLER_TRAILER_GATE_LEN;
        let metadata = read_installer_metadata(&mut Cursor::new(buf), first_hash_table_address);
        assert_eq!(metadata, None);
    }

    #[test]
    fn video_metadata_reads_back() {
        let off = header_offset::VIDEO_METADATA as usize;
        let mut buf = vec![0u8; off + header_offset::VIDEO_METADATA_LEN];
        let series_id: [u8; 16] = std::array::from_fn(|i| i as u8);
        let season_id: [u8; 16] = std::array::from_fn(|i| i as u8 + 0x40);
        buf[off..off + 16].copy_from_slice(&series_id);
        buf[off + 16..off + 32].copy_from_slice(&season_id);
        buf[off + 32..off + 34].copy_from_slice(&7u16.to_be_bytes());
        buf[off + 34..off + 36].copy_from_slice(&13u16.to_be_bytes());
        let metadata = read_video_metadata(&mut Cursor::new(buf))
            .expect("video metadata should parse from a full-length buffer");
        assert_eq!(metadata.series_id, series_id);
        assert_eq!(metadata.season_id, season_id);
        assert_eq!(metadata.season_number, 7);
        assert_eq!(metadata.episode_number, 13);
    }

    #[test]
    fn video_metadata_short_buffer_degrades_to_none_instead_of_erroring() {
        let buf = vec![0u8; header_offset::VIDEO_METADATA as usize];
        let metadata = read_video_metadata(&mut Cursor::new(buf));
        assert_eq!(metadata, None);
    }

    #[test]
    fn rejects_bad_magic() {
        let mut buf = con_header_with_content_type(ContentType::InstalledGame as u32);
        buf[0..4].copy_from_slice(b"NOPE");
        let err = read_header_prefix(&mut Cursor::new(buf)).unwrap_err();
        assert!(err.to_string().contains("not a LIVE/PIRS/CON-style header"));
    }

    #[test]
    fn header_size_is_read_from_the_correct_offset() {
        let mut buf = con_header_with_content_type(ContentType::InstalledGame as u32);
        buf[header_offset::HEADER_SIZE as usize..][..4]
            .copy_from_slice(&0x0000_AD00u32.to_be_bytes());
        let prefix = read_header_prefix(&mut Cursor::new(buf)).unwrap();
        assert_eq!(prefix.header_size, 0x0000_AD00);
    }

    fn header_with_thumbnails(metadata_version: u32, thumb: &[u8], title_thumb: &[u8]) -> Vec<u8> {
        let mut buf = vec![0u8; 0x971A + 0x4000];
        buf[0..4].copy_from_slice(b"CON ");
        buf[header_offset::METADATA_VERSION as usize..][..4]
            .copy_from_slice(&metadata_version.to_be_bytes());
        buf[header_offset::THUMBNAIL_SIZE as usize..][..4]
            .copy_from_slice(&(thumb.len() as u32).to_be_bytes());
        buf[header_offset::TITLE_THUMBNAIL_SIZE as usize..][..4]
            .copy_from_slice(&(title_thumb.len() as u32).to_be_bytes());
        buf[header_offset::THUMBNAIL_IMAGE as usize..][..thumb.len()].copy_from_slice(thumb);
        buf[header_offset::TITLE_THUMBNAIL_IMAGE as usize..][..title_thumb.len()]
            .copy_from_slice(title_thumb);
        buf
    }

    fn png(payload: &[u8]) -> Vec<u8> {
        let mut v = PNG_MAGIC.to_vec();
        v.extend_from_slice(payload);
        v
    }

    #[test]
    fn reads_both_thumbnails_when_present() {
        let thumb = png(b"thumb");
        let title_thumb = png(b"title-thumb");
        let buf = header_with_thumbnails(1, &thumb, &title_thumb);
        let result = read_header_thumbnails(&mut Cursor::new(buf));
        assert_eq!(result.thumbnail, Some(thumb));
        assert_eq!(result.title_thumbnail, Some(title_thumb));
    }

    #[test]
    fn zero_size_field_means_no_thumbnail() {
        let buf = header_with_thumbnails(1, &[], &[]);
        let result = read_header_thumbnails(&mut Cursor::new(buf));
        assert!(result.thumbnail.is_none());
        assert!(result.title_thumbnail.is_none());
    }

    #[test]
    fn oversized_v1_size_field_is_rejected() {
        let mut buf = header_with_thumbnails(1, &png(b"x"), &[]);
        buf[header_offset::THUMBNAIL_SIZE as usize..][..4]
            .copy_from_slice(&(THUMBNAIL_MAX_SIZE_V1 + 1).to_be_bytes());
        let result = read_header_thumbnails(&mut Cursor::new(buf));
        assert!(result.thumbnail.is_none());
    }

    #[test]
    fn v2_max_size_boundary() {
        let payload_len = THUMBNAIL_MAX_SIZE_V2 as usize - PNG_MAGIC.len();
        let thumb = png(&vec![0u8; payload_len]);
        let buf = header_with_thumbnails(2, &thumb, &[]);
        let result = read_header_thumbnails(&mut Cursor::new(buf));
        assert!(result.thumbnail.is_some());

        let mut buf = header_with_thumbnails(2, &thumb, &[]);
        buf[header_offset::THUMBNAIL_SIZE as usize..][..4]
            .copy_from_slice(&(THUMBNAIL_MAX_SIZE_V2 + 1).to_be_bytes());
        let result = read_header_thumbnails(&mut Cursor::new(buf));
        assert!(result.thumbnail.is_none());
    }

    #[test]
    fn non_png_bytes_at_valid_size_are_rejected() {
        let mut buf = header_with_thumbnails(1, &[0u8; 8], &[]);
        buf[header_offset::THUMBNAIL_SIZE as usize..][..4].copy_from_slice(&8u32.to_be_bytes());
        buf[header_offset::THUMBNAIL_IMAGE as usize..][..8].copy_from_slice(&[0u8; 8]);
        let result = read_header_thumbnails(&mut Cursor::new(buf));
        assert!(result.thumbnail.is_none());
    }

    #[test]
    fn short_buffer_degrades_to_no_thumbnails_instead_of_erroring() {
        let buf = vec![0u8; 0x10];
        let result = read_header_thumbnails(&mut Cursor::new(buf));
        assert!(result.thumbnail.is_none());
        assert!(result.title_thumbnail.is_none());
    }

    fn sample_metadata() -> StfsMetadata {
        StfsMetadata {
            license_entries_1_15: [0xAB; header_offset::LICENSE_ENTRIES_1_15_LEN],
            header_hash: [0xCDu8; 20],
            header_size: 0x0000_AD00,
            content_type: 0x7000,
            metadata_version: 2,
            media_id: 0x1111_2222,
            title_id: 0x4141_4141,
            platform: 3,
            executable_type: 4,
            disc_number: 1,
            disc_count: 1,
            save_game_id: 0,
            console_id: [1, 2, 3, 4, 5],
            profile_id: [6, 7, 8, 9, 10, 11, 12, 13],
            volume_descriptor: StfsVolumeDescriptor {
                size: 0x24,
                block_separation: 0,
                file_table_block_count: 1,
                file_table_block_num: 0x02_03_04,
                top_hash_table_hash: [0xEFu8; 20],
                allocated_block_count: 7,
            },
            online_creator: [14, 15, 16, 17, 18, 19, 20, 21],
            device_id: std::array::from_fn(|i| i as u8 + 20),
        }
    }

    #[test]
    fn stfs_metadata_round_trips() {
        let metadata = sample_metadata();
        let mut buf = vec![0u8; 0x412];
        metadata
            .write_at(&mut Cursor::new(&mut buf))
            .expect("write_at should succeed against a large-enough buffer");
        let parsed =
            StfsMetadata::read_at(&mut Cursor::new(buf)).expect("read_at should parse it back");

        assert_eq!(parsed.license_entries_1_15, metadata.license_entries_1_15);
        assert_eq!(parsed.header_hash, metadata.header_hash);
        assert_eq!(parsed.header_size, metadata.header_size);
        assert_eq!(parsed.content_type, metadata.content_type);
        assert_eq!(parsed.metadata_version, metadata.metadata_version);
        assert_eq!(parsed.media_id, metadata.media_id);
        assert_eq!(parsed.title_id, metadata.title_id);
        assert_eq!(parsed.platform, metadata.platform);
        assert_eq!(parsed.executable_type, metadata.executable_type);
        assert_eq!(parsed.disc_number, metadata.disc_number);
        assert_eq!(parsed.disc_count, metadata.disc_count);
        assert_eq!(parsed.save_game_id, metadata.save_game_id);
        assert_eq!(parsed.console_id, metadata.console_id);
        assert_eq!(parsed.profile_id, metadata.profile_id);
        assert_eq!(parsed.volume_descriptor, metadata.volume_descriptor);
        assert_eq!(parsed.online_creator, metadata.online_creator);
        assert_eq!(parsed.device_id, metadata.device_id);
    }

    #[test]
    fn stfs_metadata_fields_land_at_documented_header_offsets() {
        let metadata = sample_metadata();
        let mut buf = vec![0u8; 0x412];
        metadata
            .write_at(&mut Cursor::new(&mut buf))
            .expect("write_at should succeed against a large-enough buffer");

        let off = |o: u64| usize::try_from(o).unwrap();

        assert_eq!(
            &buf[off(header_offset::LICENSE_ENTRIES_1_15)..]
                [..header_offset::LICENSE_ENTRIES_1_15_LEN],
            &metadata.license_entries_1_15
        );
        assert_eq!(
            &buf[off(header_offset::CONSOLE_ID)..][..5],
            &metadata.console_id
        );
        assert_eq!(
            &buf[off(header_offset::PROFILE_ID)..][..8],
            &metadata.profile_id
        );
        assert_eq!(
            &buf[off(header_offset::VOLUME_DESCRIPTOR)],
            &metadata.volume_descriptor.size
        );
        assert_eq!(
            &buf[off(header_offset::ONLINE_CREATOR)..][..8],
            &metadata.online_creator
        );
        assert_eq!(
            &buf[off(header_offset::DEVICE_ID)..][..20],
            &metadata.device_id
        );
        assert_eq!(
            u32::from_be_bytes(
                buf[off(header_offset::HEADER_SIZE)..][..4]
                    .try_into()
                    .unwrap()
            ),
            metadata.header_size
        );
        let vd = off(header_offset::VOLUME_DESCRIPTOR);
        assert_eq!(
            u16::from_le_bytes(buf[vd + 3..vd + 5].try_into().unwrap()),
            metadata.volume_descriptor.file_table_block_count
        );
        assert_eq!(
            u32::from(buf[vd + 5]) | (u32::from(buf[vd + 6]) << 8) | (u32::from(buf[vd + 7]) << 16),
            metadata.volume_descriptor.file_table_block_num
        );
    }

    fn valid_stfs_header_bytes() -> Vec<u8> {
        let metadata = sample_metadata();
        let mut buf = vec![0u8; 0x971A + 0x4000];
        buf[0..4].copy_from_slice(&MAGIC_CON);
        metadata
            .write_at(&mut Cursor::new(&mut buf))
            .expect("write_at should succeed against a large-enough buffer");

        let thumb = png(b"thumb");
        let title_thumb = png(b"title-thumb");
        buf[header_offset::THUMBNAIL_SIZE as usize..][..4]
            .copy_from_slice(&(thumb.len() as u32).to_be_bytes());
        buf[header_offset::TITLE_THUMBNAIL_SIZE as usize..][..4]
            .copy_from_slice(&(title_thumb.len() as u32).to_be_bytes());
        buf[header_offset::THUMBNAIL_IMAGE as usize..][..thumb.len()].copy_from_slice(&thumb);
        buf[header_offset::TITLE_THUMBNAIL_IMAGE as usize..][..title_thumb.len()]
            .copy_from_slice(&title_thumb);
        buf
    }

    /// Same two calls `stfs_header`'s fuzz target makes, in order.
    #[test]
    fn valid_stfs_header_bytes_parses_prefix_and_both_thumbnails() {
        let buf = valid_stfs_header_bytes();
        let prefix = read_header_prefix(&mut Cursor::new(&buf)).expect("prefix should parse");
        assert_eq!(prefix.header_size, sample_metadata().header_size);
        let thumbs = read_header_thumbnails(&mut Cursor::new(&buf));
        assert!(thumbs.thumbnail.is_some());
        assert!(thumbs.title_thumbnail.is_some());
    }

    #[test]
    #[ignore = "writes fuzz corpus seed files; run explicitly with --ignored"]
    fn write_fuzz_corpus_seed_for_stfs_header() {
        let buf = valid_stfs_header_bytes();
        let dir = "fuzz/corpus/stfs_header";
        std::fs::create_dir_all(dir).expect("corpus directory should be creatable");
        std::fs::write(format!("{dir}/seed-minimal-con"), &buf)
            .expect("seed file should be writable");
    }
}
