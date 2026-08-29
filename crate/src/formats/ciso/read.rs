use super::format::{decode_lz4_sector, locate_part, part_boundaries};
use crate::core::reader::JsReader;
use crate::core::source::{ImageSource, SECTOR_SIZE as SOURCE_SECTOR_SIZE, SourcePart};
use anyhow::Context;
use ciso::layout::{CSOHeader, IndexTableEntry};
use std::io::{Read, Seek, SeekFrom};

/// Validates that a claimed index-table length (parsed from untrusted
/// header fields - `uncompressed_size`/`block_size`, both taken from the
/// file's own bytes) actually fits within the bytes available in part 0
/// after its fixed-size header, before that length is used to size an
/// allocation. Without this, a corrupted header claiming a huge
/// `uncompressed_size` could make `index_table_len()` (and thus
/// `vec![0u8; index_table_len() * 4]`) arbitrarily large.
///
/// Returns the validated byte length on success.
fn validate_index_table_size(
    index_table_len: usize,
    header_bytes: u64,
    part0_size: u64,
) -> Result<u64, anyhow::Error> {
    let index_table_bytes = index_table_len as u64 * 4;
    let available = part0_size.saturating_sub(header_bytes);
    anyhow::ensure!(
        index_table_bytes <= available,
        "ciso: header claims a {index_table_bytes}-byte index table, but part 0 \
         only has {available} bytes available after its {header_bytes}-byte header"
    );
    Ok(index_table_bytes)
}

/// Validates that a sector's stored `(position, length)` - parsed from
/// the file's own index table - actually falls inside the physical part
/// file it claims to live in, before `length` is used to size a read
/// buffer. Without this, a corrupted or malicious index entry could
/// claim an arbitrarily large slot and OOM the read.
fn validate_stored_range(
    sector_pos: u64,
    data_len: u32,
    part_size: u64,
) -> Result<(), anyhow::Error> {
    anyhow::ensure!(
        sector_pos
            .checked_add(u64::from(data_len))
            .is_some_and(|end| end <= part_size),
        "ciso: stored range (pos={sector_pos}, len={data_len}) exceeds part size ({part_size})"
    );
    Ok(())
}

/// Pure parse of the header + index table from part 0, generic over any
/// `Read + Seek` so `open()` can drive it with a live `JsReader` and a
/// fuzz harness can drive it directly with `Cursor<&[u8]>`.
fn parse_header_and_index<R: Read + Seek>(
    mut reader: R,
    part0_size: u64,
) -> Result<(CSOHeader, Vec<IndexTableEntry>), anyhow::Error> {
    let mut header_buf = [0u8; 24];
    reader.read_exact(&mut header_buf)?;
    let header = CSOHeader::deserialize::<anyhow::Error>(&header_buf)
        .map_err(|e| anyhow::anyhow!("ciso: invalid header: {e}"))?;
    let block_size = header.block_size;
    anyhow::ensure!(
        u64::from(block_size) == SOURCE_SECTOR_SIZE,
        "ciso: unsupported block size {block_size} (only {SOURCE_SECTOR_SIZE} is supported)"
    );

    let index_table_bytes = validate_index_table_size(header.index_table_len(), 24, part0_size)?;
    let mut index_bytes = vec![
        0u8;
        usize::try_from(index_table_bytes).map_err(|_| anyhow::anyhow!(
            "ciso: index table length does not fit in usize"
        ))?
    ];
    reader.read_exact(&mut index_bytes)?;
    let index_table: Vec<IndexTableEntry> = index_bytes
        .chunks_exact(4)
        .map(|c| IndexTableEntry::new_with_raw_value(u32::from_le_bytes(c.try_into().unwrap())))
        .collect();

    Ok((header, index_table))
}

/// Fuzz-only entry point: drives `parse_header_and_index` with an
/// in-memory `Cursor` standing in for part 0.
#[cfg(fuzzing)]
pub(crate) fn fuzz_parse_header_and_index(data: &[u8]) {
    let _ = parse_header_and_index(std::io::Cursor::new(data), data.len() as u64);
}

/// `ImageSource` backed by one or more CISO (`.cso`) part files. Header and
/// index table always live entirely in `parts[0]` - there's exactly one
/// shared index table. `image_offset()` is always 0, since a CISO file
/// already stores only the XDVDFS payload.
pub(crate) struct CisoSource {
    parts: Vec<SourcePart>,
    sequential_window: usize,
    header: CSOHeader,
    /// One entry per data sector plus one trailing boundary entry -
    /// `index_table[i+1].position() - index_table[i].position()` is how
    /// every sector's stored length is derived.
    index_table: Vec<IndexTableEntry>,
    /// Sector index at which each split part begins, from `part_boundaries`.
    part_start_sector: Vec<u64>,
    /// Only one part's `JsReader` held open at a time, rebuilt when a read
    /// crosses into a different part - same strategy as
    /// `core::source::MultiPartReader::reader_for`.
    active: Option<(usize, JsReader)>,
    /// Remembered so a `JsReader` rebuilt in `reader_for` starts in the
    /// right mode instead of reverting to `Cached`. CISO's index-table
    /// positions are validated non-decreasing per part, so reading
    /// sectors in ascending order visits ascending byte positions - safe
    /// to mark sequential.
    sequential_mode: bool,
}

impl CisoSource {
    pub(crate) fn open(
        parts: Vec<SourcePart>,
        sequential_window: usize,
    ) -> Result<Self, anyhow::Error> {
        anyhow::ensure!(!parts.is_empty(), "ciso: at least one part is required");
        let first_part = &parts[0];
        let mut reader = JsReader::new(first_part.read_fn.clone(), first_part.size);
        // Tunes this reader's Sequential-mode readahead window, same as
        // every other read-fn constructed in this module.
        reader.set_sequential_window(sequential_window);
        let (header, index_table) = parse_header_and_index(&mut reader, first_part.size)?;

        let part_start_sector = part_boundaries(&index_table);
        anyhow::ensure!(
            parts.len() >= part_start_sector.len(),
            "ciso: index table implies {} part(s), but only {} were provided",
            part_start_sector.len(),
            parts.len(),
        );

        Ok(Self {
            parts,
            sequential_window,
            header,
            index_table,
            part_start_sector,
            // Reuse the reader already opened for the header/index read
            // instead of reopening part 0 on the first read_sector() call -
            // sectors near the start of the image are the common case.
            active: Some((0, reader)),
            sequential_mode: false,
        })
    }

    fn total_data_sectors(&self) -> u64 {
        (self.index_table.len() as u64).saturating_sub(1)
    }

    /// Stored byte offset and length of `sector`'s data, relative to
    /// whichever physical part file it lives in.
    fn stored_range(&self, sector: u64) -> (u64, u32) {
        let sector = usize::try_from(sector).unwrap_or(usize::MAX);
        let entry = self.index_table[sector];
        let next = self.index_table[sector + 1];
        let entry_pos: u32 = entry.position().into();
        let next_pos: u32 = next.position().into();
        let sector_pos = u64::from(entry_pos) << self.header.alignment;
        let data_len = (next_pos - entry_pos) << self.header.alignment;
        (sector_pos, data_len)
    }

    /// Returns the `JsReader` for `part_idx`, constructing (or replacing) it
    /// only if it isn't already the active one - repeated reads within the
    /// same part reuse it as-is and keep its internal read-ahead buffer
    /// warm.
    fn reader_for(&mut self, part_idx: usize) -> &mut JsReader {
        if !matches!(&self.active, Some((idx, _)) if *idx == part_idx) {
            let part = &self.parts[part_idx];
            let mut reader = JsReader::new(part.read_fn.clone(), part.size);
            reader.set_sequential_window(self.sequential_window);
            reader.set_sequential_mode(self.sequential_mode);
            self.active = Some((part_idx, reader));
        }
        &mut self
            .active
            .as_mut()
            .expect("just set to Some(...) above if it wasn't already the active entry")
            .1
    }
}

impl ImageSource for CisoSource {
    fn set_sequential_mode(&mut self, enabled: bool) {
        self.sequential_mode = enabled;
        if let Some((_, reader)) = &mut self.active {
            reader.set_sequential_mode(enabled);
        }
    }

    fn read_sector(&mut self, sector: u64, out: &mut [u8]) -> Result<(), anyhow::Error> {
        anyhow::ensure!(
            out.len() as u64 == SOURCE_SECTOR_SIZE,
            "CisoSource::read_sector: buffer must be exactly one sector"
        );
        anyhow::ensure!(
            sector < self.total_data_sectors(),
            "ciso: sector {sector} out of range ({} total)",
            self.total_data_sectors()
        );

        let sector_idx =
            usize::try_from(sector).context("ciso: sector index does not fit in usize")?;
        let entry = self.index_table[sector_idx];
        let (sector_pos, data_len) = self.stored_range(sector);
        let part_idx = locate_part(&self.part_start_sector, sector);
        validate_stored_range(sector_pos, data_len, self.parts[part_idx].size)?;
        let reader = self.reader_for(part_idx);
        reader.seek(SeekFrom::Start(sector_pos))?;

        let data_len_usize = usize::try_from(data_len)
            .context("ciso: stored sector length does not fit in usize")?;

        if !entry.compression_type() {
            anyhow::ensure!(
                data_len_usize == out.len(),
                "ciso: uncompressed sector {sector} has unexpected stored length {data_len}"
            );
            reader.read_exact(out)?;
            return Ok(());
        }

        let mut compressed = vec![0u8; data_len_usize];
        reader.read_exact(&mut compressed)?;
        let decompressed = decode_lz4_sector(&compressed)?;
        anyhow::ensure!(
            decompressed.len() == out.len(),
            "ciso: sector {sector} decompressed to {} bytes, expected {}",
            decompressed.len(),
            out.len()
        );
        out.copy_from_slice(&decompressed);
        Ok(())
    }

    fn read_bytes(&mut self, offset: u64, out: &mut [u8]) -> Result<(), anyhow::Error> {
        let mut sector = offset / SOURCE_SECTOR_SIZE;
        let mut pos_in_sector = usize::try_from(offset % SOURCE_SECTOR_SIZE).expect(
            "offset % SOURCE_SECTOR_SIZE is always < SOURCE_SECTOR_SIZE, which fits in usize",
        );
        let mut written = 0usize;
        let mut buf = vec![
            0u8;
            usize::try_from(SOURCE_SECTOR_SIZE)
                .expect("SOURCE_SECTOR_SIZE is a small compile-time constant")
        ];
        while written < out.len() {
            self.read_sector(sector, &mut buf)?;
            let n = (buf.len() - pos_in_sector).min(out.len() - written);
            out[written..written + n].copy_from_slice(&buf[pos_in_sector..pos_in_sector + n]);
            written += n;
            pos_in_sector = 0;
            sector += 1;
        }
        Ok(())
    }

    fn total_sectors(&self) -> u64 {
        self.total_data_sectors()
    }

    fn image_offset(&self) -> u64 {
        0
    }
}

#[cfg(test)]
mod source_tests {
    use super::*;
    use std::io::Write;

    fn compress_for_test(data: &[u8]) -> Vec<u8> {
        let cfg = lz4_flex::frame::FrameInfo::new()
            .block_mode(lz4_flex::frame::BlockMode::Independent)
            .block_size(lz4_flex::frame::BlockSize::Max64KB)
            .content_checksum(false)
            .block_checksums(false)
            .legacy_frame(true)
            .content_size(None);
        let mut encoder = lz4_flex::frame::FrameEncoder::with_frame_info(cfg, Vec::new());
        encoder.write_all(data).unwrap();
        let compressed = encoder.finish().unwrap();
        compressed[7..compressed.len() - 4].to_vec()
    }

    #[test]
    fn decode_round_trips_compress_sector_output() {
        let mut original = vec![0u8; usize::try_from(SOURCE_SECTOR_SIZE).unwrap()];
        for (i, b) in original.iter_mut().enumerate() {
            *b = u8::try_from(i % 256).unwrap();
        }
        let compressed = compress_for_test(&original);
        assert!(compressed.len() < original.len());
        let decoded = decode_lz4_sector(&compressed).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn decode_round_trips_all_zero_sector() {
        let original = vec![0u8; usize::try_from(SOURCE_SECTOR_SIZE).unwrap()];
        let compressed = compress_for_test(&original);
        let decoded = decode_lz4_sector(&compressed).unwrap();
        assert_eq!(decoded, original);
    }

    fn stored_range(
        header: &CSOHeader,
        index_table: &[IndexTableEntry],
        sector: u64,
    ) -> (u64, u32) {
        let sector = usize::try_from(sector).unwrap();
        let entry = index_table[sector];
        let next = index_table[sector + 1];
        let entry_pos: u32 = entry.position().into();
        let next_pos: u32 = next.position().into();
        let sector_pos = u64::from(entry_pos) << header.alignment;
        let data_len = (next_pos - entry_pos) << header.alignment;
        (sector_pos, data_len)
    }

    #[test]
    fn validate_index_table_size_accepts_a_table_that_fits() {
        assert!(validate_index_table_size(100, 24, 24 + 400).is_ok());
    }

    #[test]
    fn validate_index_table_size_rejects_a_table_claimed_larger_than_the_part() {
        assert!(validate_index_table_size(usize::MAX / 8, 24, 1000).is_err());
    }

    #[test]
    fn validate_index_table_size_rejects_when_the_part_is_smaller_than_its_own_header() {
        assert!(validate_index_table_size(1, 24, 10).is_err());
    }

    #[test]
    fn validate_stored_range_accepts_a_range_within_the_part() {
        assert!(validate_stored_range(100, 50, 1000).is_ok());
    }

    #[test]
    fn validate_stored_range_rejects_a_range_that_overruns_the_part() {
        assert!(validate_stored_range(900, 500, 1000).is_err());
    }

    #[test]
    fn validate_stored_range_rejects_a_range_whose_end_overflows_u64() {
        assert!(validate_stored_range(u64::MAX - 10, u32::MAX, 1000).is_err());
    }

    #[test]
    fn stored_range_applies_alignment_shift() {
        use arbitrary_int::u31;
        let header = {
            let mut h = CSOHeader::new();
            h.alignment = 2;
            h
        };
        let index_table = vec![
            IndexTableEntry::default().with_position(u31::new(9)),
            IndexTableEntry::default().with_position(u31::new(9 + 100)),
            IndexTableEntry::default().with_position(u31::new(9 + 100 + 50)),
        ];
        let (pos0, len0) = stored_range(&header, &index_table, 0);
        assert_eq!(pos0, 36);
        assert_eq!(len0, 400);
        let (pos1, len1) = stored_range(&header, &index_table, 1);
        assert_eq!(pos1, 436);
        assert_eq!(len1, 200);
    }
}
