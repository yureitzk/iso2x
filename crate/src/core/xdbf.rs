//! Reads a resource out of an XDBF blob by `(id, section)`. Generic over
//! which resource is wanted - [`crate::core::thumbnail`] uses this to
//! find a title's `Thumb` resource today, but the same lookup works for
//! anything else stored in an XDBF blob (e.g. a title's display name,
//! under [`XdbfSection::StringTable`]).
//!
//! See [`XdbfHeader`] for the header/entry-table layout.

use anyhow::{Context, Result};
use binrw::{BinRead, binread};
use std::io::Cursor;

const XDBF_HEADER_SIZE: u64 = 24;
const XDBF_ENTRY_SIZE: u64 = 18;
const XDBF_FREE_LOC_SIZE: u64 = 8;
/// Sanity cap on the entry count read from an (untrusted) blob.
const MAX_XDBF_ENTRIES: u32 = 65536;

/// XDBF section IDs. `Image` is what holds the title thumbnail (id
/// `0x8000`); `StringTable` holds localized display strings (also under
/// id `0x8000` for the title name, per-language).
#[allow(dead_code)]
#[repr(u16)]
pub(crate) enum XdbfSection {
    Metadata = 1,
    Image = 2,
    StringTable = 3,
}

/// The 24-byte XDBF header: six big-endian `u32`s - magic, version,
/// `entry_table_len`/`free_table_len` (table *capacities*),
/// `entry_used`/`free_used` (entries actually populated). The entry
/// table starts immediately after this header (no gap), so a sequential
/// read of `entry_used` [`XdbfEntry`] records right after this one lands
/// correctly without any explicit seek. The content/data region starts
/// after the table *capacities*, not the used counts - see
/// `find_xdbf_resource`'s `data_offset`. Each entry is `section(u16) +
/// id(u64) + offset(u32) + size(u32)` = 18 bytes; each free-list record
/// is `offset(u32) + size(u32)` = 8 bytes.
#[binread]
#[derive(Debug, Clone, Copy)]
#[br(big, magic = b"XDBF")]
struct XdbfHeader {
    #[br(temp)]
    version: u32,
    entry_table_len: u32,
    entry_used: u32,
    free_table_len: u32,
    #[br(temp)]
    free_used: u32,
}

/// One 18-byte entry-table record: `section(u16) + id(u64) +
/// offset(u32) + size(u32)`. `BinRead`-only - nothing in this crate
/// writes an XDBF blob, so a `BinWrite` derive would be dead code.
#[derive(BinRead, Debug, Clone, Copy)]
#[br(big)]
struct XdbfEntry {
    section: u16,
    id: u64,
    offset: u32,
    size: u32,
}

/// Locates one resource inside an XDBF blob by `(id, section)`. The
/// caller decides what id/section pair means (e.g. "the thumbnail" is
/// `(0x8000, XdbfSection::Image)`) and what fallback order to try them
/// in - this just does the lookup.
pub(crate) fn find_xdbf_resource(
    xdbf_bytes: &[u8],
    id: u64,
    section: u16,
) -> Result<Option<Vec<u8>>> {
    let mut r = Cursor::new(xdbf_bytes);
    let header = XdbfHeader::read(&mut r)
        .map_err(|e| anyhow::anyhow!("xdbf: missing 'XDBF' magic bytes or bad header: {e}"))?;

    // How many entries to actually read - capped separately from the
    // table *capacity*, which is still needed uncapped for data_offset
    // below.
    let entry_used = header.entry_used.min(MAX_XDBF_ENTRIES);

    // Cursor sits at byte 24 (right after the header) here, which is
    // exactly where the entry table starts - no seek needed between
    // records either, since they're contiguous.
    let mut entries = Vec::with_capacity(entry_used as usize);
    for _ in 0..entry_used {
        entries.push(XdbfEntry::read(&mut r)?);
    }

    // Uses the *full* table capacities, not entry_used - see module docs.
    let data_offset = XDBF_HEADER_SIZE
        + u64::from(header.entry_table_len) * XDBF_ENTRY_SIZE
        + u64::from(header.free_table_len) * XDBF_FREE_LOC_SIZE;
    let data_offset = usize::try_from(data_offset).context("XDBF data offset out of range")?;

    for entry in &entries {
        if entry.id == id && entry.section == section {
            let start = data_offset.saturating_add(entry.offset as usize);
            let end = start.saturating_add(entry.size as usize);
            return Ok(xdbf_bytes.get(start..end).map(<[u8]>::to_vec));
        }
    }

    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_synthetic_xdbf(section: u16) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(b"XDBF");
        buf.extend_from_slice(&0u32.to_be_bytes()); // version
        buf.extend_from_slice(&1u32.to_be_bytes()); // entry_table_len (capacity)
        buf.extend_from_slice(&1u32.to_be_bytes()); // entry_used
        buf.extend_from_slice(&0u32.to_be_bytes()); // free_table_len (capacity)
        buf.extend_from_slice(&0u32.to_be_bytes()); // free_used
        assert_eq!(buf.len() as u64, XDBF_HEADER_SIZE);

        buf.extend_from_slice(&section.to_be_bytes()); // entry.section
        buf.extend_from_slice(&0x8000u64.to_be_bytes()); // entry.id
        buf.extend_from_slice(&0u32.to_be_bytes()); // entry.offset (relative to data start)
        buf.extend_from_slice(&4u32.to_be_bytes()); // entry.size

        // data region starts right here (header + 1*18 + 0*8)
        buf.extend_from_slice(b"PNG!");
        buf
    }

    #[test]
    fn find_xdbf_resource_locates_entry_by_id_and_section() {
        let buf = build_synthetic_xdbf(2); // Image section
        let result = find_xdbf_resource(&buf, 0x8000, 2).unwrap();
        assert_eq!(result.as_deref(), Some(b"PNG!".as_slice()));
    }

    #[test]
    fn find_xdbf_resource_returns_none_when_no_matching_entry() {
        let buf = build_synthetic_xdbf(2);
        // Right id, wrong section: no StringTable (3) entry exists.
        assert!(find_xdbf_resource(&buf, 0x8000, 3).unwrap().is_none());
    }

    #[test]
    fn find_xdbf_resource_rejects_bad_magic() {
        assert!(find_xdbf_resource(&[0u8; 40], 0x8000, 2).is_err());
    }

    #[test]
    fn entry_table_starts_immediately_after_24_byte_header_no_padding() {
        let buf = build_synthetic_xdbf(2);
        assert_eq!(u16::from_be_bytes([buf[24], buf[25]]), 2);
    }

    #[test]
    fn data_offset_uses_entry_table_capacity_not_used_count() {
        let mut buf = Vec::new();
        buf.extend_from_slice(b"XDBF");
        buf.extend_from_slice(&0u32.to_be_bytes()); // version
        buf.extend_from_slice(&2u32.to_be_bytes()); // entry_table_len = 2 (capacity)
        buf.extend_from_slice(&1u32.to_be_bytes()); // entry_used = 1
        buf.extend_from_slice(&0u32.to_be_bytes()); // free_table_len
        buf.extend_from_slice(&0u32.to_be_bytes()); // free_used
        // entry 0 (the only used one)
        buf.extend_from_slice(&2u16.to_be_bytes()); // section
        buf.extend_from_slice(&0x8000u64.to_be_bytes()); // id
        buf.extend_from_slice(&0u32.to_be_bytes()); // offset
        buf.extend_from_slice(&4u32.to_be_bytes()); // size
        // padding for the unused second slot (18 bytes of don't-care)
        buf.resize(buf.len() + 18, 0xAA);
        // data region: starts at 24 + 2*18 = 60, not 24 + 1*18 = 42
        buf.extend_from_slice(b"PNG!");

        let result = find_xdbf_resource(&buf, 0x8000, 2).unwrap();
        assert_eq!(result.as_deref(), Some(b"PNG!".as_slice()));
    }

    fn build_fuzz_corpus_bytes(id: u64, section: u16, blob: &[u8]) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(&id.to_le_bytes());
        buf.extend_from_slice(&section.to_le_bytes());
        buf.extend_from_slice(blob);
        buf
    }

    /// Confirms the seed below actually reaches `find_xdbf_resource`.
    #[test]
    fn fuzz_corpus_bytes_resolve_via_find_xdbf_resource() {
        let xdbf_blob = build_synthetic_xdbf(2); // Image section
        let bytes = build_fuzz_corpus_bytes(0x8000, 2, &xdbf_blob);

        let (id_bytes, rest) = bytes.split_first_chunk::<8>().unwrap();
        let (section_bytes, blob) = rest.split_first_chunk::<2>().unwrap();
        let id = u64::from_le_bytes(*id_bytes);
        let section = u16::from_le_bytes(*section_bytes);

        let result = find_xdbf_resource(blob, id, section).unwrap();
        assert_eq!(result.as_deref(), Some(b"PNG!".as_slice()));
    }

    #[test]
    #[ignore = "writes fuzz corpus seed files; run explicitly with --ignored"]
    fn write_fuzz_corpus_seed_for_xdbf_resource() {
        let xdbf_blob = build_synthetic_xdbf(2);
        let bytes = build_fuzz_corpus_bytes(0x8000, 2, &xdbf_blob);
        let dir = "fuzz/corpus/xdbf_resource";
        std::fs::create_dir_all(dir).expect("corpus directory should be creatable");
        std::fs::write(format!("{dir}/seed-minimal-xdbf"), &bytes)
            .expect("seed file should be writable");
    }
}
