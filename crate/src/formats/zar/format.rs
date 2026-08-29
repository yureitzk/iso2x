use binrw::{BinRead, BinWrite};
use encoding_rs::{EncoderResult, WINDOWS_1252};
use wasm_bindgen::prelude::*;

pub(super) const BLOCK_SIZE: usize = 64 * 1024; // ZAR data block size, fixed by the format
#[wasm_bindgen(js_name = zarBlockSize)]
pub fn zar_block_size() -> u32 {
    u32::try_from(BLOCK_SIZE).expect("BLOCK_SIZE is a small compile-time constant")
}

/// Blocks per offset record: one 64-bit base offset + this many `u16` per-block sizes-minus-one.
pub(super) const ENTRIES_PER_OFFSET_RECORD: usize = 16;

/// Magic identifying a `.zar` footer.
pub(crate) const FOOTER_MAGIC: u32 = 0x169f_52d6;
/// Footer format version.
pub(super) const FOOTER_VERSION: u32 = 0x61bf_3a01;
/// 6 `SectionInfo` (16 bytes each) + 32-byte hash + 8-byte total size + two 4-byte fields.
pub(crate) const FOOTER_SIZE: usize = 6 * 16 + 32 + 8 + 4 + 4;
/// Byte offset of `ZarFooter::hash` in the serialized footer - lets `write.rs`'s
/// `Phase::Footer` patch it in place after writing once with it zeroed.
pub(crate) const FOOTER_HASH_OFFSET: usize = 6 * 16;

pub(super) fn output_file_name(base_name: &str) -> String {
    format!("{base_name}.zar")
}

/// Windows-1252 <-> `String` codec for `.zar` name-table entries, shared by `read.rs`/`write.rs`.
/// See `<https://github.com/Exzap/ZArchive#features--specifications>`. Implements the WHATWG
/// windows-1252 index (`<https://encoding.spec.whatwg.org/index-windows-1252.txt>`): pointers
/// 1/13/15/16/29 (bytes 0x81/0x8D/0x8F/0x90/0x9D, unassigned on real Windows) decode to their
/// own C1 control codepoint (U+0081 etc.) rather than U+FFFD - the WHATWG table is a total
/// bijection over 0x00-0xFF, so every byte round-trips.
pub(super) fn decode_windows_1252(bytes: &[u8]) -> String {
    // No BOM handling: these are archive name bytes, not a text stream - BOM sniffing
    // could silently decode them as a different encoding entirely.
    WINDOWS_1252
        .decode_without_bom_handling(bytes)
        .0
        .into_owned()
}

/// Inverse of `decode_windows_1252`; unmappable chars fall back to `?` (0x3F). Uses the
/// `_without_replacement` encoder, not `encoding_rs`'s `.encode()`, which emits multi-byte
/// HTML numeric references for unmappable chars instead of a single `?` byte.
fn encode_windows_1252_char(c: char) -> u8 {
    let mut src = [0u8; 4];
    let s = c.encode_utf8(&mut src);
    let mut dst = [0u8; 4];
    let (result, _read, written) = WINDOWS_1252
        .new_encoder()
        .encode_from_utf8_without_replacement(s, &mut dst, true);
    match result {
        EncoderResult::InputEmpty if written == 1 => dst[0],
        _ => b'?',
    }
}

pub(super) fn encode_windows_1252(name: &str) -> Vec<u8> {
    name.chars().map(encode_windows_1252_char).collect()
}

/// (offset, size) pair describing one footer section.
///
/// `pub(crate)` and the `arbitrary` derive exist solely for
/// `#[cfg(fuzzing)]` round-trip fuzzing of `ZarFooter` (which embeds
/// this type) - see `fuzz/fuzz_targets/zar_footer_roundtrip.rs`.
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
#[derive(BinRead, BinWrite, Default, Debug, Clone, Copy, PartialEq, Eq)]
#[brw(big)]
pub(crate) struct SectionInfo {
    pub(super) offset: u64,
    pub(super) size: u64,
}

impl SectionInfo {
    pub(super) fn in_range(&self, file_size: u64) -> bool {
        self.offset
            .checked_add(self.size)
            .is_some_and(|end| end <= file_size)
    }
}

/// The whole 144-byte `.zar` footer (`FOOTER_SIZE`).
///
/// `pub` and the `arbitrary` derive exist solely so `#[cfg(fuzzing)]`
/// code in the separate `crate/fuzz` crate can reach this type for
/// round-trip fuzzing - see `fuzz/fuzz_targets/zar_footer_roundtrip.rs`.
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
#[derive(BinRead, BinWrite, Debug, Clone, PartialEq, Eq)]
#[brw(big)]
pub struct ZarFooter {
    pub(super) compressed_data: SectionInfo,
    pub(super) offset_records: SectionInfo,
    pub(super) names: SectionInfo,
    pub(super) file_tree: SectionInfo,
    pub(super) meta_directory: SectionInfo,
    pub(super) meta_data: SectionInfo,

    pub(super) hash: [u8; 32],
    pub(super) total_size: u64,
    pub(super) version: u32,
    pub(super) magic: u32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn sample_footer() -> ZarFooter {
        ZarFooter {
            compressed_data: SectionInfo {
                offset: 0,
                size: 100,
            },
            offset_records: SectionInfo {
                offset: 100,
                size: 40,
            },
            names: SectionInfo {
                offset: 140,
                size: 20,
            },
            file_tree: SectionInfo {
                offset: 160,
                size: 16,
            },
            meta_directory: SectionInfo {
                offset: 176,
                size: 0,
            },
            meta_data: SectionInfo {
                offset: 176,
                size: 0,
            },
            hash: [0xAB; 32],
            total_size: 176 + FOOTER_SIZE as u64,
            version: FOOTER_VERSION,
            magic: FOOTER_MAGIC,
        }
    }

    /// binrw round trip, and exactly `FOOTER_SIZE` bytes - proves no gap/overlap.
    #[test]
    fn binrw_round_trip_preserves_all_fields() {
        let footer = sample_footer();
        let mut buf = Vec::new();
        footer
            .write(&mut Cursor::new(&mut buf))
            .expect("write should succeed against a Vec<u8>");
        assert_eq!(buf.len(), FOOTER_SIZE);

        let parsed = ZarFooter::read(&mut Cursor::new(&buf)).expect("read should parse it back");
        assert_eq!(parsed, footer);
    }

    #[test]
    fn fields_land_at_documented_byte_offsets() {
        let footer = sample_footer();
        let mut buf = Vec::new();
        footer
            .write(&mut Cursor::new(&mut buf))
            .expect("write should succeed against a Vec<u8>");

        assert_eq!(read_be_section(&buf[0..16]), footer.compressed_data);
        assert_eq!(read_be_section(&buf[16..32]), footer.offset_records);
        assert_eq!(read_be_section(&buf[32..48]), footer.names);
        assert_eq!(read_be_section(&buf[48..64]), footer.file_tree);
        assert_eq!(read_be_section(&buf[64..80]), footer.meta_directory);
        assert_eq!(read_be_section(&buf[80..96]), footer.meta_data);
        assert_eq!(
            &buf[FOOTER_HASH_OFFSET..FOOTER_HASH_OFFSET + 32],
            &footer.hash
        );
        assert_eq!(
            u64::from_be_bytes(buf[128..136].try_into().unwrap()),
            footer.total_size
        );
        assert_eq!(
            u32::from_be_bytes(buf[136..140].try_into().unwrap()),
            footer.version
        );
        assert_eq!(
            u32::from_be_bytes(buf[140..144].try_into().unwrap()),
            footer.magic
        );
    }

    fn read_be_section(bytes: &[u8]) -> SectionInfo {
        SectionInfo {
            offset: u64::from_be_bytes(bytes[0..8].try_into().unwrap()),
            size: u64::from_be_bytes(bytes[8..16].try_into().unwrap()),
        }
    }

    #[test]
    fn ascii_round_trips() {
        assert_eq!(encode_windows_1252("default.xbe"), b"default.xbe");
        assert_eq!(decode_windows_1252(b"default.xbe"), "default.xbe");
    }

    /// 'é' is Windows-1252's Latin-1-identical range - one byte (0xE9), not UTF-8's two.
    #[test]
    fn latin1_range_char_round_trips() {
        assert_eq!(encode_windows_1252("café"), b"caf\xE9");
        assert_eq!(decode_windows_1252(b"caf\xE9"), "café");
    }

    /// 0x80 = EURO SIGN in Windows-1252, unlike Latin-1 where it's an unassigned control code.
    #[test]
    fn high_table_char_round_trips() {
        assert_eq!(encode_windows_1252("\u{20AC}"), vec![0x80]);
        assert_eq!(decode_windows_1252(&[0x80]), "\u{20AC}");
    }

    #[test]
    fn unmappable_char_falls_back_to_question_mark() {
        assert_eq!(encode_windows_1252("\u{4E2D}"), b"?");
    }

    #[test]
    fn decode_never_panics_on_any_byte() {
        let all_bytes: Vec<u8> = (0..=255).collect();
        let _ = decode_windows_1252(&all_bytes);
    }

    #[test]
    fn whatwg_unassigned_slots_decode_to_their_own_c1_control_codepoint() {
        assert_eq!(decode_windows_1252(&[0x81]), "\u{0081}");
        assert_eq!(decode_windows_1252(&[0x8D]), "\u{008D}");
        assert_eq!(decode_windows_1252(&[0x8F]), "\u{008F}");
        assert_eq!(decode_windows_1252(&[0x90]), "\u{0090}");
        assert_eq!(decode_windows_1252(&[0x9D]), "\u{009D}");
    }

    #[test]
    fn every_byte_round_trips() {
        for b in 0u8..=255 {
            let decoded = decode_windows_1252(&[b]);
            assert_eq!(
                decoded.chars().count(),
                1,
                "byte {b:#04x} decoded to != 1 char"
            );
            let reencoded = encode_windows_1252(&decoded);
            assert_eq!(reencoded, vec![b], "byte {b:#04x} didn't round-trip");
        }
    }
}
