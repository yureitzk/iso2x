use binrw::{BinRead, BinWrite};
use std::io::Cursor;
use wasm_bindgen::prelude::*;

pub const MAGIC: [u8; 4] = *b"CCIM";
pub(crate) const SECTOR_SIZE: u64 = 2048;

#[wasm_bindgen(js_name = cciSectorSize)]
pub fn cci_sector_size() -> u32 {
    u32::try_from(SECTOR_SIZE).expect("SECTOR_SIZE fits in u32")
}

pub(super) const HEADER_SIZE: u64 = 32;
pub(super) const BLOCK_SIZE: u32 = 2048;
pub(super) const VERSION: u8 = 1;
pub(super) const INDEX_ALIGNMENT: u8 = 2;

const ALIGN_MULT: u64 = 1 << INDEX_ALIGNMENT;

pub(super) const FILE_SPLIT_POINT: u64 = 0xFF00_0000;
/// Threshold at which cci output splits across "name.1.cci",
/// "name.2.cci", etc., once a part's output crosses it. Unlike ciso, each
/// cci part is fully self-contained - its own header and index - rather
/// than sharing one index in part 1.
#[wasm_bindgen(js_name = cciFileSplitPoint)]
pub fn cci_file_split_point() -> f64 {
    f64::from(u32::try_from(FILE_SPLIT_POINT).expect("FILE_SPLIT_POINT fits in u32"))
}

/// Sectors processed per `hash_next_part()` call - bounded so the sizing
/// pass stays cancellable/yieldable.
pub(super) const SIZING_BATCH_SECTORS: u64 = 256;
#[wasm_bindgen(js_name = cciSizingBatchSectors)]
pub fn cci_sizing_batch_sectors() -> u32 {
    u32::try_from(SIZING_BATCH_SECTORS).expect("SIZING_BATCH_SECTORS fits in u32")
}

/// "game.{index+1}.cci" - one-indexed. This is the always-numbered
/// building block; it is only used directly for genuinely multi-part
/// output. See `output_name_for` for the name actually assigned to each
/// part, which collapses to a bare "game.cci" in the single-part case.
fn split_name_for(base_name: &str, index: u64) -> String {
    format!("{base_name}.{}.cci", index + 1)
}

/// Output filename for part `index` (0-based) out of `total_parts`.
/// Matches xgdtool's `out_paths()`: a single-part rip gets a bare
/// "game.cci", never "game.1.cci". Only once there's a real second part
/// do names switch to the numbered "game.1.cci", "game.2.cci", ... form.
pub(super) fn output_name_for(base_name: &str, index: u64, total_parts: usize) -> String {
    if total_parts <= 1 {
        format!("{base_name}.cci")
    } else {
        split_name_for(base_name, index)
    }
}

/// The fixed 32-byte `.cci` header: `MAGIC` (consumed by binrw as a
/// literal match, not a stored field) followed by six little-endian
/// fields and 2 reserved/padding bytes. One struct shared by both the
/// reader (`CciHeader::read`) and the writer (`CciHeader::write`) so the
/// layout can't drift between the two directions.
/// <https://github.com/Team-Resurgent/Repackinator> (origin of the CCI
/// format, developed by Team Resurgent in collaboration with Team Cerbios)
///
/// `pub` and the `arbitrary` derive exist solely for `#[cfg(fuzzing)]`
/// round-trip fuzzing from the separate `crate/fuzz` crate - see
/// `fuzz/fuzz_targets/cci_header_roundtrip.rs`.
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
#[derive(BinRead, BinWrite, Debug, Clone, Copy, PartialEq, Eq)]
#[brw(little, magic = b"CCIM")]
pub struct CciHeader {
    pub(super) header_size: u32,
    pub(super) uncompressed_size: u64,
    pub(super) index_offset: u64,
    pub(super) block_size: u32,
    pub(super) version: u8,
    /// 2 reserved bytes follow this field - always zero on write, never
    /// inspected on read.
    #[brw(pad_after = 2)]
    pub(super) index_alignment: u8,
}

pub(super) fn serialize_cci_header(uncompressed_size: u64, index_offset: u64) -> Vec<u8> {
    let header = CciHeader {
        header_size: u32::try_from(HEADER_SIZE).expect("HEADER_SIZE fits in u32"),
        uncompressed_size,
        index_offset,
        block_size: BLOCK_SIZE,
        version: VERSION,
        index_alignment: INDEX_ALIGNMENT,
    };
    let mut buf = Vec::new();
    header
        .write(&mut Cursor::new(&mut buf))
        .expect("writing a fixed-size header into an in-memory Vec<u8> cannot fail");
    buf
}

/// Raw LZ4 block compression (not framed).
/// A sector is only kept compressed if the result is non-empty and
/// fits under `SECTOR_SIZE - (4 + ALIGN_MULT)`.
pub(super) fn compress_sector_cci(data: &[u8]) -> (Vec<u8>, bool) {
    let compressed = lz4_flex::block::compress(data);
    let threshold = usize::try_from(SECTOR_SIZE).expect("SECTOR_SIZE fits in usize")
        - usize::try_from(4 + ALIGN_MULT).expect("4 + ALIGN_MULT fits in usize");
    if !compressed.is_empty() && compressed.len() < threshold {
        (compressed, true)
    } else {
        (data.to_vec(), false)
    }
}

/// Layout for a compressed sector: 1 padding-length byte, the compressed
/// bytes, then zero padding so the whole write is a multiple of
/// `ALIGN_MULT`. Returns `(total_written_len, padding_byte_count)`.
/// Sectors sit back-to-back with no inter-sector gap - padding is added
/// after each sector, not before.
pub(super) fn compressed_written_len(compressed_size: usize) -> (u64, u8) {
    let raw = compressed_size as u64 + 1;
    let padded = raw.div_ceil(ALIGN_MULT) * ALIGN_MULT;
    (
        padded,
        u8::try_from(padded - raw).expect("padding is always < ALIGN_MULT"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn split_name_is_one_indexed() {
        assert_eq!(split_name_for("game", 0), "game.1.cci");
        assert_eq!(split_name_for("game", 1), "game.2.cci");
        assert_eq!(split_name_for("game", 41), "game.42.cci");
    }

    #[test]
    fn output_name_is_bare_for_single_part_but_numbered_for_multi() {
        assert_eq!(output_name_for("game", 0, 1), "game.cci");
        assert_eq!(output_name_for("game", 0, 2), "game.1.cci");
        assert_eq!(output_name_for("game", 1, 2), "game.2.cci");
        assert_eq!(output_name_for("game", 3, 4), "game.4.cci");
    }

    #[test]
    fn header_round_trips_expected_byte_layout() {
        let header = serialize_cci_header(0x1234_5678_9abc, 0xdead_beef);
        assert_eq!(&header[0..4], b"CCIM");
        assert_eq!(
            u32::from_le_bytes(header[4..8].try_into().unwrap()),
            u32::try_from(HEADER_SIZE).unwrap()
        );
        assert_eq!(
            u64::from_le_bytes(header[8..16].try_into().unwrap()),
            0x1234_5678_9abc
        );
        assert_eq!(
            u64::from_le_bytes(header[16..24].try_into().unwrap()),
            0xdead_beef
        );
        assert_eq!(
            u32::from_le_bytes(header[24..28].try_into().unwrap()),
            BLOCK_SIZE
        );
        assert_eq!(header[28], VERSION);
        assert_eq!(header[29], INDEX_ALIGNMENT);
        assert_eq!(header.len(), usize::try_from(HEADER_SIZE).unwrap());
    }

    #[test]
    fn compressed_written_len_rounds_up_to_align_mult() {
        assert_eq!(compressed_written_len(0), (4, 3));
        assert_eq!(compressed_written_len(3), (4, 0));
        assert_eq!(compressed_written_len(4), (8, 3));
        assert_eq!(compressed_written_len(7), (8, 0));
    }

    #[test]
    fn index_entries_stay_back_to_back_with_no_gaps() {
        let index_infos: Vec<(u32, bool)> = vec![(4, false), (8, true), (2048, false)];
        let mut position: u64 = HEADER_SIZE;
        let mut positions = Vec::new();
        for (value, _compressed) in &index_infos {
            positions.push(position);
            position += u64::from(*value);
        }
        assert_eq!(positions, vec![32, 36, 44]);
        assert_eq!(position, 32 + 4 + 8 + 2048);
    }
}
