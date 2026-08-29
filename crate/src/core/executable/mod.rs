use anyhow::Error;
use binrw::BinRead;
use std::io::{Read, Seek};

pub mod xbe;
pub(crate) mod xbe_sections;
pub mod xex;
pub(crate) mod xex_crypto;
pub(crate) mod xex_image;

/// Read back by `formats::god::format::ConHeaderBuilder::with_execution_info`
/// when building the CON header - every field here is load-bearing.
#[derive(Clone, Debug)]
pub struct TitleExecutionInfo {
    pub media_id: u32,
    pub version: u32,
    pub base_version: u32,
    pub title_id: u32,
    pub platform: u8,
    pub executable_type: u8,
    pub disc_number: u8,
    pub disc_count: u8,
    pub save_game_id: u32,
}

/// Wire layout of the 24-byte execution-info record `from_xex` reads,
/// pointed to by the XEX field table's `ExecutionId` entry (see
/// `xex::XexHeader::read`). Every field is contiguous with the next, so
/// declaration order alone reproduces the layout.
#[derive(BinRead, Debug, Clone, Copy)]
#[br(big)]
struct XexExecutionInfoWire {
    media_id: u32,
    version: u32,
    base_version: u32,
    title_id: u32,
    platform: u8,
    executable_type: u8,
    disc_number: u8,
    disc_count: u8,
    save_game_id: u32,
}

/// Wire layout of the execution-info fields `from_xbe` reads out of the
/// XBE certificate (reader already positioned at the cert's start by
/// `XbeHeader::read`). Not contiguous like the XEX version: `title_id`
/// at cert+0x08, then a 160-byte gap (title_name/alternate_title_ids/
/// allowed_media/game_region/game_ratings/disk_number) before `version`
/// at cert+0xAC. See OpenXDK/Cxbx-Reloaded's Xbe.h or
/// <https://xboxdevwiki.net/Xbe#Certificate>.
#[derive(BinRead, Debug, Clone, Copy)]
#[br(little)]
struct XbeExecutionInfoWire {
    #[br(pad_before = 8)]
    title_id: u32,
    #[br(pad_before = 160)]
    version: u32,
}

impl TitleExecutionInfo {
    pub fn from_xex<R: Read + Seek>(mut reader: R) -> Result<TitleExecutionInfo, Error> {
        let wire = XexExecutionInfoWire::read(&mut reader)?;
        Ok(TitleExecutionInfo {
            media_id: wire.media_id,
            version: wire.version,
            base_version: wire.base_version,
            title_id: wire.title_id,
            platform: wire.platform,
            executable_type: wire.executable_type,
            disc_number: wire.disc_number,
            disc_count: wire.disc_count,
            save_game_id: wire.save_game_id,
        })
    }

    pub fn from_xbe<R: Read + Seek>(mut reader: R) -> Result<TitleExecutionInfo, Error> {
        let wire = XbeExecutionInfoWire::read(&mut reader)?;

        Ok(TitleExecutionInfo {
            media_id: 0,
            version: wire.version,
            base_version: 0,
            title_id: wire.title_id,
            platform: 0,
            executable_type: 0,
            disc_number: 1,
            disc_count: 1,
            save_game_id: 0,
        })
    }

    /// XEX-only: `version` here is a packed Xbox 360 version. For XBE,
    /// `version` is a flat build counter with no major/minor/build/qfe
    /// structure, so use the raw field directly instead.
    pub fn xex_version(&self) -> Xex360Version {
        Xex360Version::from_packed(self.version)
    }

    /// Same decoding, for `base_version` (the version a patch title
    /// patches against). Also XEX-only.
    pub fn xex_base_version(&self) -> Xex360Version {
        Xex360Version::from_packed(self.base_version)
    }
}

/// Parsed Xbox 360 title version, as packed into the XEX execution-info
/// `version`/`base_version` fields. Layout (MSB to LSB): major:4,
/// minor:4, build:16, qfe:8.
#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize, tsify::Tsify)]
#[serde(rename_all = "camelCase")]
pub struct Xex360Version {
    pub major: u8,
    pub minor: u8,
    pub build: u16,
    pub qfe: u8,
}

impl Xex360Version {
    pub fn from_packed(value: u32) -> Xex360Version {
        Xex360Version {
            major: ((value >> 28) & 0xF) as u8,
            minor: ((value >> 24) & 0xF) as u8,
            build: ((value >> 8) & 0xFFFF) as u16,
            qfe: (value & 0xFF) as u8,
        }
    }

    /// True for an all-zero version, i.e. `base_version` on a title that
    /// isn't a patch.
    pub(crate) fn is_zero(self) -> bool {
        self.major == 0 && self.minor == 0 && self.build == 0 && self.qfe == 0
    }
}

impl std::fmt::Display for Xex360Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}.{}.{}.{}",
            self.major, self.minor, self.build, self.qfe
        )
    }
}
