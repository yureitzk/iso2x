use crate::core::source::SECTOR_SIZE as SOURCE_SECTOR_SIZE;
use ciso::layout::IndexTableEntry;
use std::io::{Read, Write};
use wasm_bindgen::prelude::*;

pub const MAGIC: [u8; 4] = *b"CISO";

pub(super) const SIZING_BATCH_SECTORS: u64 = 256;

#[wasm_bindgen(js_name = cisoSizingBatchSectors)]
pub fn ciso_sizing_batch_sectors() -> u32 {
    u32::try_from(SIZING_BATCH_SECTORS)
        .expect("SIZING_BATCH_SECTORS is a small compile-time constant")
}

pub(crate) const SECTOR_SIZE: u64 = 2048;

#[wasm_bindgen(js_name = cisoSectorSize)]
pub fn ciso_sector_size() -> u32 {
    u32::try_from(SECTOR_SIZE).expect("SECTOR_SIZE is a small compile-time constant")
}

pub(super) const FILE_SPLIT_POINT: u64 = 0xffbf_6000;
/// Byte threshold at which CISO output splits into a new `.N.cso` file.
/// Matches the ~4 GiB split point used by the reference `ciso` crate/tool:
/// `<https://github.com/antangelo/ciso>`
#[wasm_bindgen(js_name = cisoFileSplitPoint)]
pub fn ciso_file_split_point() -> f64 {
    f64::from(
        u32::try_from(FILE_SPLIT_POINT)
            .expect("FILE_SPLIT_POINT is a small compile-time constant that fits in u32"),
    )
}

const FILE_PADDING_MODULUS: u64 = 0x400;
/// Every split part's on-disk size is rounded up to this, so its stored
/// byte length always lines up with `CSOHeader.alignment`.
#[wasm_bindgen(js_name = cisoFilePaddingModulus)]
pub fn ciso_file_padding_modulus() -> u32 {
    u32::try_from(FILE_PADDING_MODULUS)
        .expect("FILE_PADDING_MODULUS is a small compile-time constant")
}

pub(super) fn serialize_index_table(table: &[IndexTableEntry]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 * table.len());
    for entry in table {
        out.extend_from_slice(&entry.raw_value().to_le_bytes());
    }
    out
}

pub(super) fn compress_sector(data: &[u8]) -> (Vec<u8>, bool) {
    let cfg = lz4_flex::frame::FrameInfo::new()
        .block_mode(lz4_flex::frame::BlockMode::Independent)
        .block_size(lz4_flex::frame::BlockSize::Max64KB)
        .content_checksum(false)
        .block_checksums(false)
        .legacy_frame(true)
        .content_size(None);
    let mut encoder = lz4_flex::frame::FrameEncoder::with_frame_info(cfg, Vec::new());
    encoder
        .write_all(data)
        .expect("writing to an in-memory buffer cannot fail");
    let compressed = encoder.finish().expect("lz4 frame finish");
    // Strip the frame header (7 bytes) and EndMark (4 bytes): CISO stores
    // bare LZ4 blocks and reconstructs the frame around them on read, see
    // `decode_lz4_sector` below.
    let compressed = compressed[7..compressed.len() - 4].to_vec();
    if compressed.len() + 12 < data.len() {
        (compressed, true)
    } else {
        (data.to_vec(), false)
    }
}

/// "game.{index+1}.cso" - one-indexed. This is the always-numbered
/// building block; it is only used directly for genuinely multi-part
/// output. See `output_name_for` for the name actually assigned to each
/// part, which collapses to a bare "game.cso" in the single-part case.
fn split_name_for(base_name: &str, index: u64) -> String {
    format!("{base_name}.{}.cso", index + 1)
}

/// Output filename for split part `index` (0-based) out of `total_parts`.
/// Only genuinely multi-part output uses the numbered "game.1.cso",
/// "game.2.cso", ... form via `split_name_for`.
pub(super) fn output_name_for(base_name: &str, index: u64, total_parts: usize) -> String {
    if total_parts <= 1 {
        format!("{base_name}.cso")
    } else {
        split_name_for(base_name, index)
    }
}

/// Rounds `size` up to the next multiple of `FILE_PADDING_MODULUS`.
pub(super) fn padded_part_size(size: u64) -> u64 {
    let rem = size % FILE_PADDING_MODULUS;
    if rem == 0 {
        size
    } else {
        size + (FILE_PADDING_MODULUS - rem)
    }
}

/// LZ4 frame magic number (4 bytes, `<https://github.com/lz4/lz4/blob/dev/doc/lz4_Frame_format.md>`)
/// plus a minimal FLG/BD/HC frame descriptor matching `compress_sector`'s
/// config: independent blocks, 64KB max block size, no checksums, no
/// content size.
const LZ4_HEADER: [u8; 7] = [0x4, 0x22, 0x4d, 0x18, 0x60, 0x40, 0x82];

pub(super) fn decode_lz4_sector(compressed: &[u8]) -> Result<Vec<u8>, anyhow::Error> {
    // Reassemble a full LZ4 frame around the bare block `compress_sector`
    // stored, so the standard frame decoder can be used unmodified.
    let mut frame = vec![0u8; 7 + compressed.len() + 4];
    frame[0..7].copy_from_slice(&LZ4_HEADER);
    frame[7..7 + compressed.len()].copy_from_slice(compressed);
    let mut decoder = lz4_flex::frame::FrameDecoder::new(frame.as_slice());
    let mut decompressed = Vec::with_capacity(
        usize::try_from(SOURCE_SECTOR_SIZE)
            .expect("SOURCE_SECTOR_SIZE is a small compile-time constant"),
    );
    decoder
        .read_to_end(&mut decompressed)
        .map_err(|e| anyhow::anyhow!("ciso: lz4 frame decode failed: {e}"))?;
    Ok(decompressed)
}

/// Sector index at which each split part begins - index 0 is always `0`.
/// Derived purely from the index table's stored positions: a part
/// boundary is wherever the *next* sector's recorded position is lower
/// than the current one. Positions are relative to whichever physical
/// file a sector landed in (see the writer's `sizing_part_start`), so a
/// reset back down is the only signal a reader has that a new part
/// started - there's no explicit "part count" or "part N starts here"
/// field anywhere in the format.
pub(super) fn part_boundaries(index_table: &[IndexTableEntry]) -> Vec<u64> {
    let mut starts = vec![0u64];
    // The trailing entry (index_table[sector_count]) is a boundary marker,
    // not a real sector - excluded from the scan.
    let sector_count = index_table.len().saturating_sub(1);
    for i in 1..sector_count {
        let prev: u32 = index_table[i - 1].position().into();
        let cur: u32 = index_table[i].position().into();
        if cur < prev {
            starts.push(i as u64);
        }
    }
    starts
}

/// Which part `sector` lives in, given `starts` from `part_boundaries`.
/// Same shape as `core::source::locate_in`, just over sector-index
/// boundaries instead of byte-size cumulative ends.
pub(super) fn locate_part(starts: &[u64], sector: u64) -> usize {
    match starts.binary_search(&sector) {
        Ok(i) => i,
        Err(i) => i - 1, // i is always >= 1 here since starts[0] == 0
    }
}

#[cfg(test)]
mod split_tests {
    use super::*;

    #[test]
    fn split_name_is_one_indexed() {
        assert_eq!(split_name_for("game", 0), "game.1.cso");
        assert_eq!(split_name_for("game", 1), "game.2.cso");
        assert_eq!(split_name_for("game", 41), "game.42.cso");
    }

    #[test]
    fn output_name_is_bare_for_single_part_but_numbered_for_multi() {
        assert_eq!(output_name_for("game", 0, 1), "game.cso");
        assert_eq!(output_name_for("game", 0, 2), "game.1.cso");
        assert_eq!(output_name_for("game", 1, 2), "game.2.cso");
        assert_eq!(output_name_for("game", 3, 4), "game.4.cso");
    }

    #[test]
    fn padded_part_size_rounds_up_to_modulus() {
        assert_eq!(padded_part_size(0), 0);
        assert_eq!(padded_part_size(1), FILE_PADDING_MODULUS);
        assert_eq!(padded_part_size(FILE_PADDING_MODULUS), FILE_PADDING_MODULUS);
        assert_eq!(
            padded_part_size(FILE_PADDING_MODULUS + 1),
            FILE_PADDING_MODULUS * 2
        );
        assert_eq!(
            padded_part_size(FILE_PADDING_MODULUS * 3),
            FILE_PADDING_MODULUS * 3
        );
    }

    #[test]
    fn padded_part_size_never_shrinks_and_stays_within_one_modulus() {
        for size in [0u64, 1, 1023, 1024, 1025, 500_000, FILE_SPLIT_POINT] {
            let padded = padded_part_size(size);
            assert!(padded >= size);
            assert!(padded - size < FILE_PADDING_MODULUS);
            assert_eq!(padded % FILE_PADDING_MODULUS, 0);
        }
    }
}

#[cfg(test)]
mod part_boundary_tests {
    use super::*;
    use arbitrary_int::u31;

    fn entry(pos: u32) -> IndexTableEntry {
        IndexTableEntry::default().with_position(u31::new(pos))
    }

    #[test]
    fn single_part_has_only_the_zero_boundary() {
        let table = vec![entry(0), entry(10), entry(25), entry(40), entry(60)];
        assert_eq!(part_boundaries(&table), vec![0]);
    }

    #[test]
    fn detects_a_single_reset_as_the_second_part_boundary() {
        let table = vec![entry(0), entry(50), entry(90), entry(5), entry(30)];
        assert_eq!(part_boundaries(&table), vec![0, 3]);
    }

    #[test]
    fn detects_multiple_resets_for_a_three_part_split() {
        let table = vec![
            entry(0),
            entry(80), // part 1: sectors 0,1
            entry(2),
            entry(60), // part 2: sectors 2,3 (reset at index 2)
            entry(4),
            entry(45),  // part 3: sectors 4,5 (reset at index 4)
            entry(100), // trailing marker
        ];
        assert_eq!(part_boundaries(&table), vec![0, 2, 4]);
    }

    #[test]
    fn equal_consecutive_positions_are_not_a_boundary() {
        let table = vec![entry(0), entry(10), entry(10), entry(25)];
        assert_eq!(part_boundaries(&table), vec![0]);
    }
}

#[cfg(test)]
mod locate_part_tests {
    use super::*;

    #[test]
    fn single_part_always_resolves_to_zero() {
        let starts = vec![0u64];
        assert_eq!(locate_part(&starts, 0), 0);
        assert_eq!(locate_part(&starts, 500), 0);
    }

    #[test]
    fn resolves_each_side_of_a_boundary() {
        let starts = vec![0u64, 100, 300];
        assert_eq!(locate_part(&starts, 0), 0);
        assert_eq!(locate_part(&starts, 99), 0);
        assert_eq!(locate_part(&starts, 100), 1); // exactly on the boundary
        assert_eq!(locate_part(&starts, 250), 1);
        assert_eq!(locate_part(&starts, 300), 2);
        assert_eq!(locate_part(&starts, 9999), 2);
    }
}
