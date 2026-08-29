use super::format::{BLOCK_SIZE, CciHeader, HEADER_SIZE, INDEX_ALIGNMENT, SECTOR_SIZE, VERSION};
use crate::core::reader::JsReader;
use crate::core::source::{ImageSource, SourcePart};
use binrw::BinRead;
use std::io::{Read, Seek, SeekFrom};

/// One opened ".N.cci" file's header-derived read state.
struct CciFileIndex {
    reader: JsReader,
    /// N+1 byte positions into this file, unpacked from their on-disk form
    /// (`(raw & 0x7FFF_FFFF) << INDEX_ALIGNMENT`). Sector `i`'s slot is
    /// `positions[i]..positions[i+1]`; `positions[N]` is the sentinel
    /// `index_end`, not a sector.
    /// See `<https://consolemods.org/wiki/Xbox:Repackinator>` for the CCI
    /// ("Cerbios Compressed Image") format this mirrors.
    positions: Vec<u64>,
    /// Compressed-flag per real sector (`positions[..N]`); the sentinel's
    /// flag bit is already dropped.
    compressed: Vec<bool>,
}

/// Validates that a just-parsed position table never decreases and ends
/// exactly at `index_offset` (the boundary between data and the index
/// table itself). Positions are parsed straight from the file's own
/// trailing index and otherwise go completely unchecked before being
/// used to size a read: `read_sector`'s compressed path computes
/// `slot_len = positions[i+1] - positions[i]` and feeds it into
/// `vec![0u8; N]` with no other bound, so a corrupted or malicious file
/// could otherwise claim an arbitrarily large slot and OOM the read.
///
/// Called once, in `parse_header_and_index`, right after `positions` is
/// built - not on every `read_sector` call, since it validates the whole
/// table.
fn validate_index_positions(positions: &[u64], index_offset: u64) -> Result<(), anyhow::Error> {
    anyhow::ensure!(
        positions.windows(2).all(|w| w[0] <= w[1]),
        "cci: index positions are not non-decreasing"
    );
    anyhow::ensure!(
        positions.last().copied() == Some(index_offset),
        "cci: index doesn't end at its own index_offset ({index_offset})"
    );
    Ok(())
}

/// Pure parse of one part's header + index table, generic over any
/// `Read + Seek` so `open()` can drive it with a live `JsReader` and a
/// fuzz harness can drive it directly with `Cursor<&[u8]>`.
fn parse_header_and_index<R: Read + Seek>(
    mut reader: R,
    part_size: u64,
    part_name: &str,
) -> Result<(CciHeader, Vec<u64>, Vec<bool>), anyhow::Error> {
    reader.seek(SeekFrom::Start(0))?;
    let header = CciHeader::read(&mut reader)
        .map_err(|e| anyhow::anyhow!("cci: bad magic or header in part {part_name:?}: {e}"))?;

    anyhow::ensure!(
        header.header_size == u32::try_from(HEADER_SIZE).expect("HEADER_SIZE fits in u32")
            && header.block_size == BLOCK_SIZE
            && header.version == VERSION
            && header.index_alignment == INDEX_ALIGNMENT,
        "cci: unexpected header fields in part {part_name:?} \
         (header_size={}, block_size={}, version={}, index_alignment={})",
        header.header_size,
        header.block_size,
        header.version,
        header.index_alignment
    );
    anyhow::ensure!(
        header.index_offset >= HEADER_SIZE && header.index_offset <= part_size,
        "cci: index_offset {} out of range in part {part_name:?} ({part_size} bytes)",
        header.index_offset,
    );
    let index_offset = header.index_offset;

    let index_bytes_len = part_size - index_offset;
    anyhow::ensure!(
        index_bytes_len.is_multiple_of(4) && index_bytes_len >= 4,
        "cci: index table in part {part_name:?} isn't a whole number of u32 entries",
    );

    reader.seek(SeekFrom::Start(index_offset))?;
    let index_buf_len = usize::try_from(index_bytes_len).map_err(|_| {
        anyhow::anyhow!("cci: index table in part {part_name:?} too large for this platform")
    })?;
    let mut index_buf = vec![0u8; index_buf_len];
    reader.read_exact(&mut index_buf)?;

    let mut positions = Vec::with_capacity(index_buf.len() / 4);
    let mut compressed = Vec::with_capacity(positions.capacity().saturating_sub(1));
    for chunk in index_buf.chunks_exact(4) {
        let raw = u32::from_le_bytes(chunk.try_into().unwrap());
        compressed.push(raw & 0x8000_0000 != 0);
        positions.push(u64::from(raw & 0x7FFF_FFFF) << INDEX_ALIGNMENT);
    }
    compressed.pop();

    validate_index_positions(&positions, index_offset)
        .map_err(|e| anyhow::anyhow!("cci: part {part_name:?}: {e}"))?;

    let sector_count = positions.len() as u64 - 1;
    anyhow::ensure!(
        sector_count * SECTOR_SIZE == header.uncompressed_size,
        "cci: part {part_name:?} claims {} bytes uncompressed but index implies {sector_count} sectors",
        header.uncompressed_size,
    );

    Ok((header, positions, compressed))
}

/// Fuzz-only entry point: drives `parse_header_and_index` with an
/// in-memory `Cursor` standing in for a single opened part.
#[cfg(fuzzing)]
pub(crate) fn fuzz_parse_header_and_index(data: &[u8]) {
    let _ = parse_header_and_index(std::io::Cursor::new(data), data.len() as u64, "fuzz");
}

pub(crate) struct CciSource {
    files: Vec<CciFileIndex>,
    sector_offsets: Vec<u64>,
    total_sectors: u64,
}

impl CciSource {
    pub(crate) fn open(
        parts: Vec<SourcePart>,
        sequential_window: usize,
    ) -> Result<Self, anyhow::Error> {
        anyhow::ensure!(
            !parts.is_empty(),
            "cci: expected at least 1 part, got {}",
            parts.len()
        );

        let mut files = Vec::with_capacity(parts.len());
        let mut sector_offsets = Vec::with_capacity(parts.len());
        let mut running_sectors = 0u64;

        for part in parts {
            let mut reader = JsReader::new(part.read_fn, part.size);
            // Tunes how big a Sequential-mode readahead window this
            // reader would use if/when set_sequential_mode(true) is
            // called on it later (e.g. a Direct-backend bulk pass over
            // this source). The scattered-read cache uses a fixed
            // internal block size regardless.
            reader.set_sequential_window(sequential_window);
            let (_header, positions, compressed) =
                parse_header_and_index(&mut reader, part.size, &part.name)?;

            sector_offsets.push(running_sectors);
            running_sectors += positions.len() as u64 - 1;
            files.push(CciFileIndex {
                reader,
                positions,
                compressed,
            });
        }

        Ok(Self {
            files,
            sector_offsets,
            total_sectors: running_sectors,
        })
    }

    fn locate(&self, sector: u64) -> Result<(usize, u64), anyhow::Error> {
        for (i, &offset) in self.sector_offsets.iter().enumerate().rev() {
            if sector >= offset {
                let sector_in_file = sector - offset;
                anyhow::ensure!(
                    sector_in_file < self.files[i].compressed.len() as u64,
                    "cci: sector {sector} out of range ({} sectors total)",
                    self.total_sectors
                );
                return Ok((i, sector_in_file));
            }
        }
        anyhow::bail!("cci: sector {sector} out of range")
    }
}

impl ImageSource for CciSource {
    // Unlike GodSource/CisoSource, every part's JsReader is already open
    // (see `open()` - no lazy `active` swap), so there's no "current"
    // reader to remember a mode for: apply it to all of them up front.
    // Safe for the same reason as CisoSource - `validate_index_positions`
    // guarantees non-decreasing byte positions per part, so ascending
    // sector reads (a Direct-backend bulk pass) are ascending byte reads.
    fn set_sequential_mode(&mut self, enabled: bool) {
        for file in &mut self.files {
            file.reader.set_sequential_mode(enabled);
        }
    }

    fn read_sector(&mut self, sector: u64, out: &mut [u8]) -> Result<(), anyhow::Error> {
        anyhow::ensure!(
            out.len() == usize::try_from(SECTOR_SIZE).expect("SECTOR_SIZE fits in usize"),
            "cci: read_sector buffer must be exactly one sector"
        );

        let (file_idx, s) = self.locate(sector)?;
        let file = &mut self.files[file_idx];
        let s = usize::try_from(s).expect("sector index fits in usize");
        let (start, end) = (file.positions[s], file.positions[s + 1]);
        let is_compressed = file.compressed[s];
        let slot_len = end - start;

        file.reader.seek(SeekFrom::Start(start))?;

        if !is_compressed && slot_len == SECTOR_SIZE {
            file.reader.read_exact(out)?;
            return Ok(());
        }

        anyhow::ensure!(
            slot_len >= 1,
            "cci: sector {sector} has an empty slot (0 bytes)"
        );

        // Compressed slots are prefixed with a 1-byte padding length, used
        // to round the slot up to INDEX_ALIGNMENT.
        let mut padding_len_buf = [0u8; 1];
        file.reader.read_exact(&mut padding_len_buf)?;
        let padding_len = u64::from(padding_len_buf[0]);

        let compressed_size = slot_len.checked_sub(1 + padding_len).ok_or_else(|| {
            anyhow::anyhow!("cci: sector {sector} padding_len exceeds its own slot")
        })?;
        let compressed_size = usize::try_from(compressed_size).map_err(|_| {
            anyhow::anyhow!("cci: sector {sector} compressed size too large for this platform")
        })?;
        let mut compressed_buf = vec![0u8; compressed_size];
        file.reader.read_exact(&mut compressed_buf)?;

        // CCI sectors are compressed as independent LZ4 blocks (no frame
        // header), so this needs the raw block decoder, not the LZ4 frame
        // format. https://github.com/lz4/lz4/blob/dev/doc/lz4_Block_format.md
        let n = lz4_flex::block::decompress_into(&compressed_buf, out).map_err(|e| {
            anyhow::anyhow!("cci: LZ4 block decompress failed for sector {sector}: {e}")
        })?;
        anyhow::ensure!(
            n == usize::try_from(SECTOR_SIZE).expect("SECTOR_SIZE fits in usize"),
            "cci: sector {sector} decompressed to {n} bytes, expected {SECTOR_SIZE}"
        );
        Ok(())
    }

    fn read_bytes(&mut self, offset: u64, out: &mut [u8]) -> Result<(), anyhow::Error> {
        if out.is_empty() {
            return Ok(());
        }
        let start_sector = offset / SECTOR_SIZE;
        let end_sector = (offset + out.len() as u64 - 1) / SECTOR_SIZE;
        let mut sector_buf =
            vec![0u8; usize::try_from(SECTOR_SIZE).expect("SECTOR_SIZE fits in usize")];
        let mut written = 0usize;
        for sector in start_sector..=end_sector {
            self.read_sector(sector, &mut sector_buf)?;
            let sector_start = sector * SECTOR_SIZE;
            let copy_start = offset.max(sector_start) - sector_start;
            let copy_end =
                (offset + out.len() as u64).min(sector_start + SECTOR_SIZE) - sector_start;
            let n = usize::try_from(copy_end - copy_start).expect("chunk length fits in usize");
            let copy_start =
                usize::try_from(copy_start).expect("offset within sector fits in usize");
            out[written..written + n].copy_from_slice(&sector_buf[copy_start..copy_start + n]);
            written += n;
        }
        Ok(())
    }

    fn total_sectors(&self) -> u64 {
        self.total_sectors
    }

    fn image_offset(&self) -> u64 {
        0
    }
}

#[cfg(test)]
mod validate_index_positions_tests {
    use super::*;

    #[test]
    fn accepts_monotonic_positions_ending_at_index_offset() {
        let positions = vec![32, 36, 44, 2092];
        assert!(validate_index_positions(&positions, 2092).is_ok());
    }

    #[test]
    fn rejects_non_monotonic_positions() {
        let positions = vec![32, 36, 20, 2092];
        assert!(validate_index_positions(&positions, 2092).is_err());
    }

    #[test]
    fn rejects_positions_not_ending_at_index_offset() {
        let positions = vec![32, 36, 44, 999_999_999];
        assert!(validate_index_positions(&positions, 2092).is_err());
    }

    #[test]
    fn rejects_empty_positions() {
        let positions: Vec<u64> = vec![];
        assert!(validate_index_positions(&positions, 0).is_err());
    }
}

#[cfg(test)]
mod corpus_seed_tests {
    use super::super::format::serialize_cci_header;
    use super::*;
    use std::io::Cursor;

    fn valid_cci_bytes() -> Vec<u8> {
        let sector_data_start = HEADER_SIZE;
        let index_offset = sector_data_start + SECTOR_SIZE;

        let mut buf = serialize_cci_header(SECTOR_SIZE, index_offset);
        buf.extend(std::iter::repeat_n(0u8, SECTOR_SIZE as usize)); // one raw sector

        let raw0 = u32::try_from(sector_data_start >> INDEX_ALIGNMENT).unwrap();
        let raw1 = u32::try_from(index_offset >> INDEX_ALIGNMENT).unwrap();
        buf.extend_from_slice(&raw0.to_le_bytes());
        buf.extend_from_slice(&raw1.to_le_bytes());

        buf
    }

    #[test]
    fn valid_cci_bytes_parses_header_and_index() {
        let bytes = valid_cci_bytes();
        let (header, positions, compressed) =
            parse_header_and_index(Cursor::new(bytes.as_slice()), bytes.len() as u64, "test")
                .expect("a freshly-built part should parse");
        assert_eq!(header.uncompressed_size, SECTOR_SIZE);
        assert_eq!(positions, vec![HEADER_SIZE, HEADER_SIZE + SECTOR_SIZE]);
        assert_eq!(compressed, vec![false]);
    }

    #[test]
    #[ignore = "writes fuzz corpus seed files; run explicitly with --ignored"]
    fn write_fuzz_corpus_seed_for_cci_header() {
        let bytes = valid_cci_bytes();
        let dir = "fuzz/corpus/cci_header";
        std::fs::create_dir_all(dir).expect("corpus directory should be creatable");
        std::fs::write(format!("{dir}/seed-minimal-cci"), &bytes)
            .expect("seed file should be writable");
    }
}
