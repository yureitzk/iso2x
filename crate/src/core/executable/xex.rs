use super::TitleExecutionInfo;
use anyhow::Error;
use binrw::BinRead;
use byteorder::{BE, ReadBytesExt};
use num_enum::TryFromPrimitive;
use std::io::{Read, Seek, SeekFrom};

bitflags::bitflags! {
    // https://free60.org/System-Software/Formats/XEX/#xex-header
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub struct XexModuleFlags: u32 {
        const TITLE_MODULE = 0x01;
        const EXPORTS_TO_TITLE = 0x02;
        const SYSTEM_DEBUGGER = 0x04;
        const DLL_MODULE = 0x08;
        const MODULE_PATCH = 0x10;
        const FULL_PATCH = 0x20;
        const DELTA_PATCH = 0x40;
        const USER_MODE = 0x80;
    }
}

#[derive(Clone, Default, Debug)]
pub struct XexHeaderFields {
    pub execution_info: Option<TitleExecutionInfo>,
    // other fields will be added if and when necessary
}

#[derive(Clone, Debug)]
pub struct XexHeader {
    pub module_flags: XexModuleFlags,
    /// File offset the (possibly compressed/encrypted) image body starts
    /// at - `header_size` in Xenia's `xex2_header` struct
    /// (`xex2_info.h`, offset 0x08), which doubles as the body's
    /// starting offset.
    pub header_size: u32,
    /// Header-relative offset to the `SecurityInfo` block -
    /// `security_offset` in Xenia's `xex2_header`.
    pub security_offset: u32,
    pub fields: XexHeaderFields,
}

// https://free60.org/System-Software/Formats/XEX/#header-ids
#[repr(u32)]
#[derive(Clone, Debug, PartialEq, Eq, TryFromPrimitive)]
#[allow(dead_code)]
enum XexHeaderFieldId {
    ResourceInfo = 0x_00_00_02_ff,
    BaseFileFormat = 0x_00_00_03_ff,
    BaseReference = 0x_00_00_04_05,
    DeltaPatchDescriptor = 0x_00_00_05_ff,
    BoundingPath = 0x_00_00_80_ff,
    DeviceId = 0x_00_00_81_05,
    OriginalBaseAddress = 0x_00_01_00_01,
    EntryPoint = 0x_00_01_01_00,
    ImageBaseAddress = 0x_00_01_02_01,
    ImportLibraries = 0x_00_01_03_ff,
    ChecksumTimestamp = 0x_00_01_80_02,
    EnabledForCallcap = 0x_00_01_81_02,
    EnabledForFastcap = 0x_00_01_82_00,
    OriginalPeName = 0x_00_01_83_ff,
    StaticLibraries = 0x_00_02_00_ff,
    TlsInfo = 0x_00_02_01_04,
    DefaultStackSize = 0x_00_02_02_00,
    DefaultFilesystemCacheSize = 0x_00_02_03_01,
    DefaultHeapSize = 0x_00_02_04_01,
    PageHeapSizeAndFlags = 0x_00_02_80_02,
    SystemFlags = 0x_00_03_00_00,
    ExecutionId = 0x_00_04_00_06,
    ServiceIdList = 0x_00_04_01_ff,
    TitleWorkspaceSize = 0x_00_04_02_01,
    GameRatings = 0x_00_04_03_10,
    LanKey = 0x_00_04_04_04,
    Xbox360Logo = 0x_00_04_05_ff,
    MultidiscMediaIds = 0x_00_04_06_ff,
    AlternateTitleIds = 0x_00_04_07_ff,
    AdditionalTitleMemory = 0x_00_04_08_01,
    ExportsByName = 0x_00_e1_04_02,
}

/// The fixed 24-byte prefix of a XEX header: magic, `module_flags`,
/// `header_size`, 4 reserved bytes, `security_offset`, `field_count`.
#[derive(BinRead, Debug, Clone, Copy)]
#[br(big, magic = b"XEX2")]
struct XexHeaderPrefixWire {
    module_flags: u32,
    /// File offset the (possibly compressed/encrypted) image body
    /// starts at.
    header_size: u32,
    /// 4 reserved bytes (Xenia's `xex2_header` doesn't name this
    /// field), unmodeled.
    #[br(pad_before = 4)]
    security_offset: u32,
    field_count: u32,
}

impl XexHeader {
    pub fn read<R: Read + Seek>(mut reader: R) -> Result<XexHeader, Error> {
        let header_offset = reader.stream_position()?;
        let prefix = XexHeaderPrefixWire::read(&mut reader)
            .map_err(|e| anyhow::anyhow!("missing 'XEX2' magic bytes in XEX header: {e}"))?;
        let module_flags = XexModuleFlags::from_bits_truncate(prefix.module_flags);

        let mut fields = XexHeaderFields::default();

        for _ in 0..prefix.field_count {
            let key = reader.read_u32::<BE>()?;
            let value = reader.read_u32::<BE>()?;

            let key = XexHeaderFieldId::try_from(key).ok();

            if let Some(XexHeaderFieldId::ExecutionId) = key {
                let offset = reader.stream_position()?;
                reader.seek(SeekFrom::Start(header_offset + u64::from(value)))?;
                fields.execution_info = Some(TitleExecutionInfo::from_xex(&mut reader)?);
                reader.seek(SeekFrom::Start(offset))?;
            }
        }

        Ok(XexHeader {
            module_flags,
            header_size: prefix.header_size,
            security_offset: prefix.security_offset,
            fields,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn valid_xex_bytes(title_id: u32) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(b"XEX2");
        buf.extend_from_slice(&0u32.to_be_bytes()); // module_flags
        buf.extend_from_slice(&0u32.to_be_bytes()); // header_size
        buf.extend(std::iter::repeat_n(0u8, 4)); // reserved
        buf.extend_from_slice(&0u32.to_be_bytes()); // security_offset
        buf.extend_from_slice(&1u32.to_be_bytes()); // field_count = 1
        assert_eq!(
            buf.len(),
            24,
            "magic(4)+flags(4)+header_size(4)+reserved(4)+security_offset(4)+field_count(4)"
        );

        let exec_info_offset: u32 = 32; // 24 (prefix) + 8 (one field entry)
        buf.extend_from_slice(&0x0004_0006u32.to_be_bytes());
        buf.extend_from_slice(&exec_info_offset.to_be_bytes());
        assert_eq!(buf.len(), exec_info_offset as usize);

        buf.extend_from_slice(&title_id.to_be_bytes()); // media_id (unused by test)
        buf.extend_from_slice(&0u32.to_be_bytes()); // version
        buf.extend_from_slice(&0u32.to_be_bytes()); // base_version
        buf.extend_from_slice(&title_id.to_be_bytes()); // title_id
        buf.push(0); // platform
        buf.push(0); // executable_type
        buf.push(1); // disc_number
        buf.push(1); // disc_count
        buf.extend_from_slice(&0u32.to_be_bytes()); // save_game_id
        buf
    }

    #[test]
    fn read_parses_a_valid_minimal_header() {
        let buf = valid_xex_bytes(0x4744_0134);
        let header = XexHeader::read(Cursor::new(buf)).expect("should parse");
        let info = header
            .fields
            .execution_info
            .expect("execution info should be present");
        assert_eq!(info.title_id, 0x4744_0134);
    }

    #[test]
    fn read_rejects_missing_magic() {
        let mut buf = valid_xex_bytes(1);
        buf[3] = b'1'; // "XEX1" instead of "XEX2"
        assert!(XexHeader::read(Cursor::new(buf)).is_err());
    }

    #[test]
    fn read_never_panics_on_any_truncation_of_a_valid_header() {
        let base = valid_xex_bytes(1);
        for len in 0..base.len() {
            let truncated = base[..len].to_vec();
            let result = std::panic::catch_unwind(|| XexHeader::read(Cursor::new(truncated)));
            assert!(
                result.is_ok(),
                "read() must not panic on a header truncated to {len} bytes"
            );
        }
    }

    #[test]
    fn read_does_not_hang_or_panic_on_a_huge_field_count_with_a_short_buffer() {
        let mut buf = valid_xex_bytes(1);
        buf[20..24].copy_from_slice(&u32::MAX.to_be_bytes()); // field_count
        buf.truncate(28);
        let result = std::panic::catch_unwind(|| XexHeader::read(Cursor::new(buf)));
        assert!(result.is_ok());
    }

    #[test]
    fn read_does_not_panic_when_execution_id_points_far_past_the_buffer_end() {
        let mut buf = valid_xex_bytes(1);
        buf[28..32].copy_from_slice(&u32::MAX.to_be_bytes()); // field[0].value
        let result = std::panic::catch_unwind(|| XexHeader::read(Cursor::new(buf)));
        assert!(result.is_ok());
    }

    #[test]
    fn read_skips_an_unrecognized_field_key_without_panicking() {
        let mut buf = valid_xex_bytes(1);
        buf[24..28].copy_from_slice(&0xDEAD_BEEFu32.to_be_bytes()); // unknown key
        let header = XexHeader::read(Cursor::new(buf)).expect("unknown key should not error");
        assert!(header.fields.execution_info.is_none());
    }

    #[test]
    #[ignore = "writes fuzz corpus seed files; run explicitly with --ignored"]
    fn write_fuzz_corpus_seed_for_xex_header() {
        let bytes = valid_xex_bytes(0x4744_0134);
        let dir = "fuzz/corpus/xex_header";
        std::fs::create_dir_all(dir).expect("corpus directory should be creatable");
        std::fs::write(format!("{dir}/seed-minimal-xex"), &bytes)
            .expect("seed file should be writable");
    }
}
