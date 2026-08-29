//! `ZarArchiveReader` - parses a `.zar` footer/offset-records/name-table/file-tree
//! (`<https://github.com/Exzap/ZArchive#features--specifications>`) and serves
//! individual files' bytes by decompressing the block(s) they fall in.
//!
//! Exposed to the rest of the crate as an `ExtractedFilesystem`, so
//! `formats::cci`/`ciso`/`xiso`/`god` don't need to know `.zar` exists.
//!
//! Known limitations: the footer's SHA-256 is parsed but never checked
//! against the archive's bytes. `ensure_block` caches only the single
//! most-recently-decoded block (not a full LRU set), so it helps the common
//! case - a `read_file_range` call landing back on the block it just used,
//! e.g. because a caller's request straddles a `next_chunk` boundary - but
//! not a genuinely random access pattern across many blocks.

use super::format::{
    BLOCK_SIZE, ENTRIES_PER_OFFSET_RECORD, FOOTER_MAGIC, FOOTER_SIZE, FOOTER_VERSION, ZarFooter,
    decode_windows_1252,
};
use crate::core::reader::JsReader;
use crate::utils::is_safe_path_component;
use binrw::BinRead;
use js_sys::Function;
use std::collections::{HashSet, VecDeque};
use std::io::{Cursor, Read, Seek, SeekFrom};

/// One parsed offset record on the read side - unlike the writer's `OffsetRecord`, this
/// is always fully known up front as a fixed-size array, not built incrementally.
struct ReadOffsetRecord {
    base_offset: u64,
    /// Compressed size minus one, per block.
    sizes: [u16; ENTRIES_PER_OFFSET_RECORD],
}

/// One parsed 16-byte file-tree entry, before path reconstruction.
///
/// `pub(crate)`, along with `parse_tree_entry`/`build_files` below: the only parsing
/// steps in this file that don't touch `JsReader`.
pub(crate) enum TreeNodeRaw {
    File {
        name_offset: u32,
        offset: u64,
        size: u64,
    },
    Dir {
        name_offset: u32,
        node_start_index: u32,
        count: u32,
    },
}

pub(crate) fn parse_tree_entry(bytes: &[u8; 16]) -> TreeNodeRaw {
    let flag = u32::from_be_bytes(bytes[0..4].try_into().unwrap());
    let is_file = flag & 0x8000_0000 != 0;
    let name_offset = flag & 0x7FFF_FFFF;
    if is_file {
        let offset_low = u64::from(u32::from_be_bytes(bytes[4..8].try_into().unwrap()));
        let size_low = u64::from(u32::from_be_bytes(bytes[8..12].try_into().unwrap()));
        let high = u32::from_be_bytes(bytes[12..16].try_into().unwrap());
        TreeNodeRaw::File {
            name_offset,
            offset: offset_low | (u64::from(high & 0xFFFF) << 32),
            size: size_low | (u64::from((high >> 16) & 0xFFFF) << 32),
        }
    } else {
        TreeNodeRaw::Dir {
            name_offset,
            node_start_index: u32::from_be_bytes(bytes[4..8].try_into().unwrap()),
            count: u32::from_be_bytes(bytes[8..12].try_into().unwrap()),
        }
    }
}

/// Decodes the length-prefixed name at byte offset `offset` in the raw
/// name-table section (1-byte header, or 2-byte for names >= 0x80 bytes).
fn read_name(name_table: &[u8], offset: u32) -> Result<String, anyhow::Error> {
    let off = offset as usize;
    anyhow::ensure!(off < name_table.len(), "zar: name offset out of range");
    let b0 = name_table[off];
    let (len, data_start) = if b0 & 0x80 != 0 {
        anyhow::ensure!(off + 1 < name_table.len(), "zar: truncated name header");
        let b1 = name_table[off + 1];
        (((b0 & 0x7F) as usize) | ((b1 as usize) << 7), off + 2)
    } else {
        (b0 as usize, off + 1)
    };
    anyhow::ensure!(
        data_start + len <= name_table.len(),
        "zar: name extends past the end of the name table"
    );
    let name = decode_windows_1252(&name_table[data_start..data_start + len]);
    anyhow::ensure!(
        is_safe_path_component(&name),
        "zar: unsafe path component in name table: {name:?}"
    );
    Ok(name)
}

/// Walks the file tree from the root (entry 0), reconstructing each file's full
/// `/`-joined path. Expects a BFS-contiguous `node_start_index`/`count` per directory,
/// as the writer produces.
pub(crate) fn build_files(
    name_table: &[u8],
    entries: &[TreeNodeRaw],
) -> Result<Vec<(String, u64, u64)>, anyhow::Error> {
    anyhow::ensure!(!entries.is_empty(), "zar: empty file tree");
    anyhow::ensure!(
        matches!(entries[0], TreeNodeRaw::Dir { .. }),
        "zar: first file-tree entry must be the root directory"
    );
    let mut files = Vec::new();
    let mut queue: VecDeque<(usize, String)> = VecDeque::new();
    queue.push_back((0, String::new()));
    // Tracks already-queued directory indices so a crafted entry can't point
    // its child range at itself or an ancestor and get re-enqueued forever -
    // same shape as `xiso::directory_table::read_root`'s `visited_sectors`.
    let mut visited: HashSet<usize> = HashSet::new();
    visited.insert(0);
    while let Some((idx, prefix)) = queue.pop_front() {
        let (node_start, count) = match entries[idx] {
            TreeNodeRaw::Dir {
                node_start_index,
                count,
                ..
            } => (node_start_index as usize, count as usize),
            TreeNodeRaw::File { .. } => continue,
        };
        for i in node_start
            ..node_start
                .checked_add(count)
                .ok_or_else(|| anyhow::anyhow!("zar: directory child range overflowed"))?
        {
            let child = entries
                .get(i)
                .ok_or_else(|| anyhow::anyhow!("zar: file-tree index {i} out of range"))?;
            match child {
                TreeNodeRaw::File {
                    name_offset,
                    offset,
                    size,
                } => {
                    let name = read_name(name_table, *name_offset)?;
                    let path = if prefix.is_empty() {
                        name
                    } else {
                        format!("{prefix}/{name}")
                    };
                    files.push((path, *offset, *size));
                }
                TreeNodeRaw::Dir { name_offset, .. } => {
                    anyhow::ensure!(
                        visited.insert(i),
                        "zar: file-tree index {i} revisited (cyclic directory range)"
                    );
                    let name = read_name(name_table, *name_offset)?;
                    let path = if prefix.is_empty() {
                        name
                    } else {
                        format!("{prefix}/{name}")
                    };
                    queue.push_back((i, path));
                }
            }
        }
    }
    Ok(files)
}

/// A parsed `.zar` archive, ready to serve individual files' bytes. `open()` parses the
/// footer/offset-records/name-table/file-tree up front; no file data is decompressed
/// until `read_file_range` is called.
pub(crate) struct ZarArchiveReader {
    reader: JsReader,
    compressed_data_offset: u64,
    offset_records: Vec<ReadOffsetRecord>,
    /// (path, uncompressed-stream offset, size), one per file.
    files: Vec<(String, u64, u64)>,
    /// Reused across `ensure_block` calls for the compressed bytes read off
    /// `reader`, instead of allocating a fresh `Vec` per block.
    raw_scratch: Vec<u8>,
    /// (block_index, decoded bytes) for whichever block `ensure_block` decoded
    /// most recently - checked on entry so a repeat request for the same block
    /// doesn't pay to decompress it again, and its `Vec` allocation is reused
    /// for the next block regardless of whether that block hits the cache.
    cached_block: Option<(u64, Vec<u8>)>,
}

impl ZarArchiveReader {
    /// No `sequential_window` parameter: nothing here ever calls `set_sequential_mode`
    /// on `reader` - every access is a scattered Cached-mode read against `JsReader`'s
    /// fixed internal cache block size, so there's no Sequential-mode window to size.
    pub(crate) fn open(read_fn: Function, file_size: u64) -> Result<Self, anyhow::Error> {
        anyhow::ensure!(
            file_size > FOOTER_SIZE as u64,
            "zar: file too small to contain a footer"
        );
        let mut reader = JsReader::new(read_fn, file_size);
        let mut footer_bytes = [0u8; FOOTER_SIZE];
        reader.seek(SeekFrom::Start(file_size - FOOTER_SIZE as u64))?;
        reader.read_exact(&mut footer_bytes)?;

        let footer = ZarFooter::read(&mut Cursor::new(&footer_bytes[..]))
            .map_err(|e| anyhow::anyhow!("zar: failed to parse footer: {e}"))?;

        anyhow::ensure!(footer.magic == FOOTER_MAGIC, "zar: bad footer magic");
        anyhow::ensure!(
            footer.version == FOOTER_VERSION,
            "zar: unsupported footer version"
        );
        anyhow::ensure!(
            footer.total_size == file_size,
            "zar: footer total size {} doesn't match actual file size {file_size}",
            footer.total_size
        );

        for section in [
            &footer.compressed_data,
            &footer.offset_records,
            &footer.names,
            &footer.file_tree,
        ] {
            anyhow::ensure!(
                section.in_range(file_size),
                "zar: footer section out of range"
            );
        }

        anyhow::ensure!(
            footer.offset_records.size.is_multiple_of(40),
            "zar: offset-records section isn't a whole number of records"
        );
        reader.seek(SeekFrom::Start(footer.offset_records.offset))?;
        let mut offset_records = Vec::with_capacity((footer.offset_records.size / 40) as usize);
        let mut record_buf = [0u8; 40];
        for _ in 0..offset_records.capacity() {
            reader.read_exact(&mut record_buf)?;
            let base_offset = u64::from_be_bytes(record_buf[0..8].try_into().unwrap());
            let mut sizes = [0u16; ENTRIES_PER_OFFSET_RECORD];
            for (i, size) in sizes.iter_mut().enumerate() {
                let start = 8 + i * 2;
                *size = u16::from_be_bytes(record_buf[start..start + 2].try_into().unwrap());
            }
            offset_records.push(ReadOffsetRecord { base_offset, sizes });
        }

        reader.seek(SeekFrom::Start(footer.names.offset))?;
        let mut name_table = vec![0u8; usize::try_from(footer.names.size)?];
        reader.read_exact(&mut name_table)?;

        anyhow::ensure!(
            footer.file_tree.size.is_multiple_of(16),
            "zar: file-tree section isn't a whole number of entries"
        );
        reader.seek(SeekFrom::Start(footer.file_tree.offset))?;
        let mut tree_bytes = vec![0u8; usize::try_from(footer.file_tree.size)?];
        reader.read_exact(&mut tree_bytes)?;
        let entries: Vec<TreeNodeRaw> = tree_bytes
            .chunks_exact(16)
            .map(|c| parse_tree_entry(c.try_into().unwrap()))
            .collect();
        let files = build_files(&name_table, &entries)?;

        Ok(Self {
            reader,
            compressed_data_offset: footer.compressed_data.offset,
            offset_records,
            files,
            raw_scratch: Vec::new(),
            cached_block: None,
        })
    }

    /// (path, size) per file, in file-tree traversal order.
    pub(crate) fn file_entries(&self) -> Vec<(String, u64)> {
        self.files.iter().map(|(p, _, s)| (p.clone(), *s)).collect()
    }

    /// Reads exactly `buf.len()` bytes starting at `offset_in_file` within
    /// `self.files[idx]`, decompressing whichever block(s) that range falls in.
    pub(crate) fn read_file_range(
        &mut self,
        idx: usize,
        offset_in_file: u64,
        buf: &mut [u8],
    ) -> Result<(), anyhow::Error> {
        let &(_, file_offset, file_size) = self
            .files
            .get(idx)
            .ok_or_else(|| anyhow::anyhow!("zar: file index {idx} out of range"))?;
        anyhow::ensure!(
            offset_in_file + buf.len() as u64 <= file_size,
            "zar: read past end of file"
        );
        let mut global_offset = file_offset + offset_in_file;
        let mut written = 0usize;
        while written < buf.len() {
            let block_index = global_offset / BLOCK_SIZE as u64;
            let pos_in_block = usize::try_from(global_offset % BLOCK_SIZE as u64)
                .expect("modulo by BLOCK_SIZE always fits in usize");
            self.ensure_block(block_index)?;
            let block = &self
                .cached_block
                .as_ref()
                .expect("ensure_block just populated this")
                .1;
            let n = (BLOCK_SIZE - pos_in_block).min(buf.len() - written);
            buf[written..written + n].copy_from_slice(&block[pos_in_block..pos_in_block + n]);
            written += n;
            global_offset += n as u64;
        }
        Ok(())
    }

    /// Ensures `self.cached_block` holds the decompressed bytes for `block_index`,
    /// short-circuiting if it's already the cached block. A block stored at exactly
    /// `BLOCK_SIZE` bytes was kept raw (compression didn't shrink it), so it's moved
    /// into the cache as-is rather than run through the decoder.
    fn ensure_block(&mut self, block_index: u64) -> Result<(), anyhow::Error> {
        if matches!(&self.cached_block, Some((idx, _)) if *idx == block_index) {
            return Ok(());
        }

        let record_idx = usize::try_from(block_index / ENTRIES_PER_OFFSET_RECORD as u64)
            .map_err(|_| anyhow::anyhow!("zar: block index too large"))?;
        let slot = usize::try_from(block_index % ENTRIES_PER_OFFSET_RECORD as u64)
            .expect("modulo by ENTRIES_PER_OFFSET_RECORD always fits in usize");
        let record = self
            .offset_records
            .get(record_idx)
            .ok_or_else(|| anyhow::anyhow!("zar: block {block_index} out of range"))?;
        let mut compressed_offset = record.base_offset;
        for &size in &record.sizes[..slot] {
            compressed_offset += u64::from(size) + 1;
        }
        let stored_len = record.sizes[slot] as usize + 1;

        self.reader.seek(SeekFrom::Start(
            self.compressed_data_offset + compressed_offset,
        ))?;
        self.raw_scratch.clear();
        self.raw_scratch.resize(stored_len, 0);
        self.reader.read_exact(&mut self.raw_scratch)?;

        // Reuse the previously-cached block's Vec (whichever block it was) as the
        // destination for this one, so the cache doesn't cost a fresh allocation
        // even on a miss.
        let mut out = self.cached_block.take().map_or_else(
            || Vec::with_capacity(BLOCK_SIZE),
            |(_, mut v)| {
                v.clear();
                v
            },
        );

        if stored_len == BLOCK_SIZE {
            out.extend_from_slice(&self.raw_scratch);
            self.cached_block = Some((block_index, out));
            return Ok(());
        }

        let mut decoder =
            ruzstd::decoding::StreamingDecoder::new(Cursor::new(&self.raw_scratch[..])).map_err(
                |e| anyhow::anyhow!("zar: zstd stream init failed for block {block_index}: {e}"),
            )?;
        decoder
            .read_to_end(&mut out)
            .map_err(|e| anyhow::anyhow!("zar: zstd decode failed for block {block_index}: {e}"))?;
        anyhow::ensure!(
            out.len() == BLOCK_SIZE,
            "zar: block {block_index} decompressed to {} bytes, expected {BLOCK_SIZE}",
            out.len()
        );
        self.cached_block = Some((block_index, out));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_name_decodes_short_form() {
        let table = [3u8, b'f', b'o', b'o'];
        assert_eq!(read_name(&table, 0).unwrap(), "foo");
    }

    #[test]
    fn read_name_decodes_long_form() {
        let len = 200usize;
        let b0 = 0x80 | (len & 0x7F) as u8;
        let b1 = (len >> 7) as u8;
        let mut table = vec![b0, b1];
        table.extend(std::iter::repeat_n(b'a', len));
        let name = read_name(&table, 0).unwrap();
        assert_eq!(name.len(), len);
    }

    #[test]
    fn read_name_rejects_offset_past_end_of_table() {
        let table = [1u8, b'x'];
        assert!(read_name(&table, 10).is_err());
    }

    #[test]
    fn read_name_rejects_length_extending_past_table() {
        // Claims 50 bytes follow, but only 1 is actually present.
        let table = [50u8, b'x'];
        assert!(read_name(&table, 0).is_err());
    }

    #[test]
    fn read_name_rejects_truncated_long_form_header() {
        // 0x80 set (long form) but the second header byte is missing.
        let table = [0x80u8];
        assert!(read_name(&table, 0).is_err());
    }

    #[test]
    fn read_name_decodes_windows_1252_not_utf8() {
        let table = [1u8, 0xE9];
        assert_eq!(read_name(&table, 0).unwrap(), "é");
    }

    #[test]
    fn read_name_rejects_traversal_component() {
        let table = [2u8, b'.', b'.'];
        assert!(read_name(&table, 0).is_err());
    }

    #[test]
    fn read_name_rejects_embedded_separator() {
        // "a/b" - a single name-table entry smuggling an extra path segment.
        let table = [3u8, b'a', b'/', b'b'];
        assert!(read_name(&table, 0).is_err());
    }

    fn dir(name_offset: u32, node_start_index: u32, count: u32) -> TreeNodeRaw {
        TreeNodeRaw::Dir {
            name_offset,
            node_start_index,
            count,
        }
    }

    fn file(name_offset: u32, offset: u64, size: u64) -> TreeNodeRaw {
        TreeNodeRaw::File {
            name_offset,
            offset,
            size,
        }
    }

    #[test]
    fn build_files_rejects_empty_tree() {
        assert!(build_files(&[], &[]).is_err());
    }

    #[test]
    fn build_files_rejects_root_that_is_not_a_directory() {
        let names = [3u8, b'f', b'o', b'o'];
        assert!(build_files(&names, &[file(0, 0, 0)]).is_err());
    }

    #[test]
    fn build_files_rejects_child_range_overflow() {
        let names = [0u8];
        let entries = [dir(0, u32::MAX, 2)];
        assert!(build_files(&names, &entries).is_err());
    }

    #[test]
    fn build_files_rejects_child_index_out_of_range() {
        let names = [0u8];
        let entries = [dir(0, 5, 1)];
        assert!(build_files(&names, &entries).is_err());
    }

    #[test]
    fn build_files_rejects_traversal_child_name() {
        // name-table: [0]="" (root, unused), then ".." at offset 0.
        let names = [2u8, b'.', b'.'];
        // root dir -> one child, a file named "..".
        let entries = [dir(0, 1, 1), file(0, 0, 0)];
        let err = build_files(&names, &entries).unwrap_err();
        assert!(err.to_string().contains("unsafe path component"));
    }

    #[test]
    fn build_files_rejects_self_referencing_directory() {
        // Root directory's own child range includes index 0, i.e. itself.
        let names = [0u8];
        let entries = [dir(0, 0, 1)];
        let err = build_files(&names, &entries).unwrap_err();
        assert!(err.to_string().contains("revisited"));
    }

    #[test]
    fn build_files_rejects_ancestor_cycle() {
        let names = [1u8, b'd'];
        let entries = [dir(0, 1, 1), dir(0, 0, 1)];
        let err = build_files(&names, &entries).unwrap_err();
        assert!(err.to_string().contains("revisited"));
    }

    #[test]
    fn build_files_resolves_nested_paths() {
        // name-table: [0]="" (unused), then "sub" at 0, "a.bin" at 4.
        let mut names = Vec::new();
        let sub_off = names.len() as u32;
        names.push(3u8);
        names.extend_from_slice(b"sub");
        let file_off = names.len() as u32;
        names.push(5u8);
        names.extend_from_slice(b"a.bin");

        let entries = [dir(0, 1, 1), dir(sub_off, 2, 1), file(file_off, 100, 10)];
        let files = build_files(&names, &entries).unwrap();
        assert_eq!(files, vec![("sub/a.bin".to_string(), 100, 10)]);
    }

    fn encode_tree_entry(node: &TreeNodeRaw) -> [u8; 16] {
        let mut out = [0u8; 16];
        match *node {
            TreeNodeRaw::Dir {
                name_offset,
                node_start_index,
                count,
            } => {
                out[0..4].copy_from_slice(&name_offset.to_be_bytes());
                out[4..8].copy_from_slice(&node_start_index.to_be_bytes());
                out[8..12].copy_from_slice(&count.to_be_bytes());
            }
            TreeNodeRaw::File {
                name_offset,
                offset,
                size,
            } => {
                let flag = name_offset | 0x8000_0000;
                out[0..4].copy_from_slice(&flag.to_be_bytes());
                out[4..8].copy_from_slice(&(offset as u32).to_be_bytes());
                out[8..12].copy_from_slice(&(size as u32).to_be_bytes());
                let high =
                    (((size >> 32) & 0xFFFF) << 16) as u32 | ((offset >> 32) & 0xFFFF) as u32;
                out[12..16].copy_from_slice(&high.to_be_bytes());
            }
        }
        out
    }

    fn valid_zar_tree_bytes() -> (Vec<u8>, Vec<u8>) {
        let mut names = Vec::new();
        let sub_off = names.len() as u32;
        names.push(3u8);
        names.extend_from_slice(b"sub");
        let file_off = names.len() as u32;
        names.push(5u8);
        names.extend_from_slice(b"a.bin");

        let entries = [dir(0, 1, 1), dir(sub_off, 2, 1), file(file_off, 100, 10)];
        let tree_bytes = entries.iter().flat_map(encode_tree_entry).collect();
        (names, tree_bytes)
    }

    #[test]
    fn valid_zar_tree_bytes_resolves_via_build_files() {
        let (names, tree_bytes) = valid_zar_tree_bytes();
        let entries: Vec<_> = tree_bytes
            .chunks_exact(16)
            .map(|c| parse_tree_entry(c.try_into().unwrap()))
            .collect();
        let files = build_files(&names, &entries).expect("hand-built tree should resolve");
        assert_eq!(files, vec![("sub/a.bin".to_string(), 100, 10)]);
    }
}
