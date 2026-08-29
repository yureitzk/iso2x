use super::format::{
    BLOCK_SIZE, ENTRIES_PER_OFFSET_RECORD, FOOTER_HASH_OFFSET, FOOTER_MAGIC, FOOTER_SIZE,
    FOOTER_VERSION, SectionInfo, ZarFooter, encode_windows_1252, output_file_name,
};
use crate::core::extracted_fs::ExtractedFilesystem;
use crate::core::iso;
use crate::core::iso::probe_source_over;
use crate::core::source::{ImageSource, OwnedSourceReader, ProbedDirectoryTable, SourceReader};
use crate::session::ChunkSource;
use binrw::BinWrite;
use ruzstd::encoding::CompressionLevel;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, VecDeque};
use std::io::{Cursor, Read, Seek, SeekFrom};
use std::mem;

/// Closest `ruzstd::encoding::CompressionLevel` to the reference zstd
/// encoder's default for this output.
const ZSTD_LEVEL: CompressionLevel = CompressionLevel::Fastest;

/// Where a packed file's bytes are read from.
enum ZarFileLocator {
    /// Byte offset within the source's own root, read via
    /// `OwnedSourceReader`.
    Image { source_offset: u64 },
    /// Index into `ExtractedFilesystem`'s parts, read via
    /// `ExtractedFilesystem::read_file_range`.
    Extracted { part_index: usize },
}

struct ZarFileEntry {
    path: String,
    size: u64,
    locator: ZarFileLocator,
}

enum ZarBackend {
    Image(OwnedSourceReader),
    Extracted(Box<ExtractedFilesystem>),
}

impl ZarBackend {
    /// `locator` must have come from the same backend variant this was
    /// built with - `build()` guarantees that.
    fn read_exact_at(
        &mut self,
        locator: &ZarFileLocator,
        pos_in_file: u64,
        buf: &mut [u8],
    ) -> Result<(), anyhow::Error> {
        match (self, locator) {
            (ZarBackend::Image(reader), ZarFileLocator::Image { source_offset }) => {
                reader.seek(SeekFrom::Start(source_offset + pos_in_file))?;
                reader.read_exact(buf)?;
                Ok(())
            }
            (ZarBackend::Extracted(fs), ZarFileLocator::Extracted { part_index }) => {
                fs.read_file_range(*part_index, pos_in_file, buf)
            }
            _ => unreachable!("ZarFileLocator variant must match ZarBackend variant"),
        }
    }
}

/// One node in the in-memory path tree, built once from the full file
/// list before streaming begins.
struct TreeNode {
    is_file: bool,
    /// Index into `names`/`name_offsets`. Unused for the root node, which
    /// serializes with the `0x7FFFFFFF` sentinel instead.
    name_index: u32,
    children: Vec<usize>,
    file_offset: u64,
    file_size: u64,
}

/// Interns `name` into `names`/`lookup`, returning its index. Dedups by
/// exact (case-sensitive) match, unlike the case-insensitive sibling
/// lookup in `insert_path` below.
fn intern_name(lookup: &mut HashMap<String, u32>, names: &mut Vec<String>, name: &str) -> u32 {
    if let Some(&idx) = lookup.get(name) {
        return idx;
    }
    let idx = u32::try_from(names.len()).expect("fewer than u32::MAX interned names");
    names.push(name.to_owned());
    lookup.insert(name.to_owned(), idx);
    idx
}

/// Walks `path`'s components into the tree rooted at `arena[0]`, creating
/// directory nodes as needed and a file leaf at the end. Sibling lookups
/// are case-insensitive, per the `ZArchive` path-matching spec:
/// `<https://github.com/Exzap/ZArchive#features--specifications>`
fn insert_path(
    arena: &mut Vec<TreeNode>,
    lookup: &mut HashMap<String, u32>,
    names: &mut Vec<String>,
    path: &str,
    size: u64,
    running_offset: &mut u64,
) -> Result<(), anyhow::Error> {
    let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    anyhow::ensure!(!parts.is_empty(), "zar: empty path");
    let mut current = 0usize;
    for (i, part) in parts.iter().enumerate() {
        let is_last = i == parts.len() - 1;
        let existing = arena[current]
            .children
            .iter()
            .copied()
            .find(|&c| names[arena[c].name_index as usize].eq_ignore_ascii_case(part));
        current = if let Some(child_idx) = existing {
            if is_last {
                anyhow::bail!("zar: duplicate path {path:?}");
            }
            if arena[child_idx].is_file {
                anyhow::bail!("zar: {path:?} treats a file as a directory");
            }
            child_idx
        } else {
            let name_index = intern_name(lookup, names, part);
            let new_idx = arena.len();
            arena.push(TreeNode {
                is_file: is_last,
                name_index,
                children: Vec::new(),
                file_offset: 0,
                file_size: 0,
            });
            arena[current].children.push(new_idx);
            if is_last {
                arena[new_idx].file_offset = *running_offset;
                arena[new_idx].file_size = size;
                *running_offset += size;
            }
            new_idx
        };
    }
    Ok(())
}

/// Serializes the interned `names` into the length-prefixed name-table
/// format (1-byte header under 0x80, 2-byte otherwise), returning the raw
/// bytes alongside each name's byte offset into them.
fn build_name_table(names: &[String]) -> (Vec<u8>, Vec<u32>) {
    let mut name_table_bytes = Vec::new();
    let mut name_offsets = Vec::with_capacity(names.len());
    let mut cursor: u32 = 0;
    for name in names {
        name_offsets.push(cursor);
        let full = encode_windows_1252(name);
        let len = full.len().min(0x7FFF);
        let bytes = &full[..len];
        if len >= 0x80 {
            name_table_bytes.push(u8::try_from(len & 0x7F).expect("masked to 7 bits") | 0x80);
            name_table_bytes.push(
                u8::try_from(len >> 7).expect("len capped at 0x7FFF fits in 8 bits after >>7"),
            );
            cursor += 2;
        } else {
            name_table_bytes.push(u8::try_from(len).expect("len < 0x80 fits in u8"));
            cursor += 1;
        }
        name_table_bytes.extend_from_slice(bytes);
        cursor += u32::try_from(len).expect("len capped at 0x7FFF fits in u32");
    }
    (name_table_bytes, name_offsets)
}

/// Lays `arena` out in BFS order (so each directory's children end up
/// contiguous, matching `node.count` + `node.node_start_index`), sorting
/// each directory's children case-insensitively by name, and serializes
/// the result into the 16-byte-per-entry file-tree format.
fn build_file_tree(arena: &mut [TreeNode], names: &[String], name_offsets: &[u32]) -> Vec<u8> {
    let mut bfs_order: Vec<usize> = Vec::with_capacity(arena.len());
    let mut node_start_index = vec![0u32; arena.len()];
    let mut queue: VecDeque<usize> = VecDeque::new();
    queue.push_back(0);
    let mut next_index: u32 = 1;
    while let Some(idx) = queue.pop_front() {
        bfs_order.push(idx);
        if arena[idx].is_file {
            continue;
        }
        // `children` is taken out of `arena` before sorting since the
        // comparator also needs to index into `arena` to look up names.
        let mut children = mem::take(&mut arena[idx].children);
        children.sort_by(|&a, &b| {
            names[arena[a].name_index as usize]
                .to_ascii_lowercase()
                .cmp(&names[arena[b].name_index as usize].to_ascii_lowercase())
        });
        arena[idx].children = children;
        node_start_index[idx] = next_index;
        next_index +=
            u32::try_from(arena[idx].children.len()).expect("fewer than u32::MAX tree nodes");
        for &c in &arena[idx].children {
            queue.push_back(c);
        }
    }

    let mut file_tree_bytes = Vec::with_capacity(bfs_order.len() * 16);
    for &idx in &bfs_order {
        let node = &arena[idx];
        let name_offset: u32 = if idx == 0 {
            0x7FFF_FFFF
        } else {
            name_offsets[node.name_index as usize]
        };
        let mut entry = [0u8; 16];
        let flag = if node.is_file {
            0x8000_0000u32 | (name_offset & 0x7FFF_FFFF)
        } else {
            name_offset & 0x7FFF_FFFF
        };
        entry[0..4].copy_from_slice(&flag.to_be_bytes());
        if node.is_file {
            let offset_low = (node.file_offset & 0xFFFF_FFFF) as u32;
            let size_low = (node.file_size & 0xFFFF_FFFF) as u32;
            let high = (((node.file_offset >> 32) & 0xFFFF) as u32)
                | ((((node.file_size >> 32) & 0xFFFF) as u32) << 16);
            entry[4..8].copy_from_slice(&offset_low.to_be_bytes());
            entry[8..12].copy_from_slice(&size_low.to_be_bytes());
            entry[12..16].copy_from_slice(&high.to_be_bytes());
        } else {
            let count = u32::try_from(node.children.len()).expect("fewer than u32::MAX children");
            entry[4..8].copy_from_slice(&node_start_index[idx].to_be_bytes());
            entry[8..12].copy_from_slice(&count.to_be_bytes());
            entry[12..16].copy_from_slice(&0u32.to_be_bytes());
        }
        file_tree_bytes.extend_from_slice(&entry);
    }
    file_tree_bytes
}

#[derive(Default)]
struct FooterSections {
    compressed_data: SectionInfo,
    offset_records: SectionInfo,
    names: SectionInfo,
    file_tree: SectionInfo,
    meta_directory: SectionInfo,
    meta_data: SectionInfo,
}

/// One offset record, built up as blocks are compressed and only serialized once
/// streaming reaches the offset-records section.
struct OffsetRecord {
    base_offset: u64,
    /// Compressed size minus one, per block. The last record may have fewer than
    /// `ENTRIES_PER_OFFSET_RECORD` entries - the reader derives block count from the
    /// uncompressed data size, not this vector's length.
    sizes: Vec<u16>,
}

enum Phase {
    /// Reading and compressing file data block by block, then flushing
    /// the final zero-padded partial block.
    Streaming,
    /// Zero-padding the compressed-data section to an 8-byte boundary.
    AlignPad,
    OffsetRecords,
    NameTable,
    FileTree,
    /// Emits the real, hash-populated footer.
    Footer,
    Done,
}

/// ZAR output session - see the module's format spec:
/// `<https://github.com/Exzap/ZArchive#features--specifications>`
pub(crate) struct ZarSession {
    backend: ZarBackend,
    files: Vec<ZarFileEntry>,
    /// Fully-serialized name-table bytes, precomputed at build time.
    name_table_bytes: Vec<u8>,
    /// Fully-serialized file-tree bytes, precomputed at build time.
    file_tree_bytes: Vec<u8>,
    total_input_bytes: u64,
    base_name: String,

    // Streaming state.
    phase: Phase,
    current_file: usize,
    current_file_pos: u64,
    /// Accumulates raw bytes until a full `BLOCK_SIZE` is ready to compress. Never
    /// holds more than `BLOCK_SIZE - 1` bytes across calls.
    block_buffer: Vec<u8>,
    /// Total bytes ever produced, independent of `pending_output` buffering/draining.
    total_emitted: u64,
    blocks_written: u64,
    offset_records: Vec<OffsetRecord>,
    hasher: Sha256,
    pending_output: Vec<u8>,
    sections: FooterSections,
}

impl ZarSession {
    /// `probed`, when present, is a directory-tree walk a caller already did on this
    /// exact `source` - reused instead of walking again.
    pub(crate) fn open(
        source: Box<dyn ImageSource>,
        base_name: String,
        probed: Option<ProbedDirectoryTable>,
    ) -> Result<Self, anyhow::Error> {
        let mut source = source;
        // Scoped so the borrow needed to probe the directory table ends
        // before `source` moves into `OwnedSourceReader` below.
        let directory_table = if let Some(p) = probed {
            p.directory_table
        } else {
            let probe_reader = SourceReader::new(source.as_mut());
            probe_source_over(probe_reader)
                .map_err(|e| anyhow::anyhow!("zar: {e:#}"))?
                .directory_table
        };
        let files = directory_table
            .entries
            .into_iter()
            .filter(|e| !e.is_directory())
            .map(|e| ZarFileEntry {
                path: e.path,
                size: u64::from(e.size),
                locator: ZarFileLocator::Image {
                    source_offset: u64::from(e.sector) * iso::SECTOR_SIZE,
                },
            })
            .collect();
        Self::build(
            ZarBackend::Image(OwnedSourceReader::new(source)),
            files,
            base_name,
        )
    }

    /// Extracted-source counterpart to `open()`. No "mode" parameter here:
    /// an extracted-files source has no padding/junk to scrub, and ZAR
    /// has nothing equivalent to scrub even for image sources.
    pub(crate) fn open_from_extracted(
        fs: ExtractedFilesystem,
        base_name: String,
    ) -> Result<Self, anyhow::Error> {
        let entries = fs.file_entries();
        let files = entries
            .into_iter()
            .enumerate()
            .map(|(part_index, (path, size))| ZarFileEntry {
                path,
                size,
                locator: ZarFileLocator::Extracted { part_index },
            })
            .collect();
        Self::build(ZarBackend::Extracted(Box::new(fs)), files, base_name)
    }

    fn build(
        backend: ZarBackend,
        files: Vec<ZarFileEntry>,
        base_name: String,
    ) -> Result<Self, anyhow::Error> {
        let mut arena = vec![TreeNode {
            is_file: false,
            name_index: 0,
            children: Vec::new(),
            file_offset: 0,
            file_size: 0,
        }];
        let mut lookup = HashMap::new();
        let mut names: Vec<String> = Vec::new();
        let mut running_offset = 0u64;
        for file in &files {
            insert_path(
                &mut arena,
                &mut lookup,
                &mut names,
                &file.path,
                file.size,
                &mut running_offset,
            )?;
        }
        let total_input_bytes = running_offset;

        let (name_table_bytes, name_offsets) = build_name_table(&names);
        let file_tree_bytes = build_file_tree(&mut arena, &names, &name_offsets);

        Ok(Self {
            backend,
            files,
            name_table_bytes,
            file_tree_bytes,
            total_input_bytes,
            base_name,
            phase: Phase::Streaming,
            current_file: 0,
            current_file_pos: 0,
            block_buffer: Vec::with_capacity(BLOCK_SIZE),
            total_emitted: 0,
            blocks_written: 0,
            offset_records: Vec::new(),
            hasher: Sha256::new(),
            pending_output: Vec::new(),
            sections: FooterSections::default(),
        })
    }

    /// Appends `bytes` to the output stream: hashes it and queues it for
    /// `next_chunk` to drain.
    fn emit(&mut self, bytes: &[u8]) {
        self.hasher.update(bytes);
        self.pending_output.extend_from_slice(bytes);
        self.total_emitted += bytes.len() as u64;
    }

    /// Compresses one full `BLOCK_SIZE` block and emits it, recording an
    /// offset-table entry. Falls back to storing the block uncompressed
    /// if compression didn't actually shrink it.
    fn store_block(&mut self, data: &[u8]) {
        debug_assert_eq!(data.len(), BLOCK_SIZE);
        let base_offset = self.total_emitted;
        let compressed = ruzstd::encoding::compress_to_vec(data, ZSTD_LEVEL);
        let out_len = if compressed.len() < BLOCK_SIZE {
            self.emit(&compressed);
            compressed.len()
        } else {
            self.emit(data);
            BLOCK_SIZE
        };
        if self
            .blocks_written
            .is_multiple_of(ENTRIES_PER_OFFSET_RECORD as u64)
        {
            self.offset_records.push(OffsetRecord {
                base_offset,
                sizes: Vec::with_capacity(ENTRIES_PER_OFFSET_RECORD),
            });
        }
        self.offset_records
            .last_mut()
            .expect("just pushed a record if needed")
            .sizes
            .push(u16::try_from(out_len - 1).expect("out_len is at most BLOCK_SIZE (64 KiB)"));
        self.blocks_written += 1;
    }

    /// Reads more file data into `block_buffer` (bounded to roughly one
    /// block per call, so a multi-gigabyte file can't block the event
    /// loop for longer than that), compressing and emitting whenever a
    /// full block accumulates. Advances to `AlignPad` once every file is
    /// exhausted and the trailing partial block is flushed.
    fn advance_streaming(&mut self) -> Result<(), anyhow::Error> {
        if self.current_file >= self.files.len() {
            if !self.block_buffer.is_empty() {
                self.block_buffer.resize(BLOCK_SIZE, 0);
                let block = mem::replace(&mut self.block_buffer, Vec::with_capacity(BLOCK_SIZE));
                self.store_block(&block);
            }
            self.phase = Phase::AlignPad;
            return Ok(());
        }

        let file_size = self.files[self.current_file].size;
        let remaining_in_file = file_size - self.current_file_pos;
        if remaining_in_file == 0 {
            self.current_file += 1;
            self.current_file_pos = 0;
            return Ok(());
        }
        let want = (BLOCK_SIZE - self.block_buffer.len())
            .min(usize::try_from(remaining_in_file).unwrap_or(usize::MAX))
            .clamp(1, BLOCK_SIZE);
        let old_len = self.block_buffer.len();
        self.block_buffer.resize(old_len + want, 0);
        if let Err(e) = self.backend.read_exact_at(
            &self.files[self.current_file].locator,
            self.current_file_pos,
            &mut self.block_buffer[old_len..],
        ) {
            // Matches GodBackend::Direct's read-error handling - don't
            // leave a partial/stale read sitting in the reused buffer.
            self.block_buffer.truncate(old_len);
            return Err(anyhow::anyhow!(
                "zar: reading {:?}: {e:#}",
                self.files[self.current_file].path
            ));
        }
        self.current_file_pos += want as u64;

        if self.current_file_pos >= file_size {
            self.current_file += 1;
            self.current_file_pos = 0;
        }
        if self.block_buffer.len() == BLOCK_SIZE {
            let block = mem::replace(&mut self.block_buffer, Vec::with_capacity(BLOCK_SIZE));
            self.store_block(&block);
        }
        Ok(())
    }

    /// Drives one bounded step of output generation, queuing bytes into
    /// `pending_output` (or moving to the next phase). Called in a loop
    /// by `next_chunk` until there's something to drain or the session
    /// is fully done.
    fn advance(&mut self) -> Result<(), anyhow::Error> {
        match self.phase {
            Phase::Streaming => self.advance_streaming(),
            Phase::AlignPad => {
                self.sections.compressed_data = SectionInfo {
                    offset: 0,
                    size: self.total_emitted,
                };
                let pad = (8 - (self.total_emitted % 8)) % 8;
                if pad > 0 {
                    self.emit(&vec![0u8; pad as usize]);
                }
                self.phase = Phase::OffsetRecords;
                Ok(())
            }
            Phase::OffsetRecords => {
                let start = self.total_emitted;
                let mut bytes = Vec::with_capacity(self.offset_records.len() * 40);
                for record in &self.offset_records {
                    bytes.extend_from_slice(&record.base_offset.to_be_bytes());
                    for i in 0..ENTRIES_PER_OFFSET_RECORD {
                        let size = record.sizes.get(i).copied().unwrap_or(0);
                        bytes.extend_from_slice(&size.to_be_bytes());
                    }
                }
                self.emit(&bytes);
                self.sections.offset_records = SectionInfo {
                    offset: start,
                    size: self.total_emitted - start,
                };
                self.phase = Phase::NameTable;
                Ok(())
            }
            Phase::NameTable => {
                let start = self.total_emitted;
                let bytes = mem::take(&mut self.name_table_bytes);
                self.emit(&bytes);
                self.sections.names = SectionInfo {
                    offset: start,
                    size: self.total_emitted - start,
                };
                self.phase = Phase::FileTree;
                Ok(())
            }
            Phase::FileTree => {
                let start = self.total_emitted;
                let bytes = mem::take(&mut self.file_tree_bytes);
                self.emit(&bytes);
                self.sections.file_tree = SectionInfo {
                    offset: start,
                    size: self.total_emitted - start,
                };
                // No metadata support yet - always a zero-length section.
                self.sections.meta_directory = SectionInfo {
                    offset: self.total_emitted,
                    size: 0,
                };
                self.sections.meta_data = SectionInfo {
                    offset: self.total_emitted,
                    size: 0,
                };
                self.phase = Phase::Footer;
                Ok(())
            }
            Phase::Footer => {
                let footer = ZarFooter {
                    compressed_data: self.sections.compressed_data,
                    offset_records: self.sections.offset_records,
                    names: self.sections.names,
                    file_tree: self.sections.file_tree,
                    meta_directory: self.sections.meta_directory,
                    meta_data: self.sections.meta_data,
                    hash: [0u8; 32],
                    total_size: self.total_emitted + FOOTER_SIZE as u64,
                    version: FOOTER_VERSION,
                    magic: FOOTER_MAGIC,
                };

                // One binrw write with the hash still zeroed - both what gets hashed (a
                // self-referential digest needs *some* placeholder for its own field) and,
                // after the patch below, the exact bytes emitted. `FOOTER_HASH_OFFSET` is
                // cross-checked against `ZarFooter`'s layout by format.rs's own test.
                let mut footer_bytes = Vec::with_capacity(FOOTER_SIZE);
                footer
                    .write(&mut Cursor::new(&mut footer_bytes))
                    .expect("writing a fixed-size footer into an in-memory Vec<u8> cannot fail");
                debug_assert_eq!(footer_bytes.len(), FOOTER_SIZE);

                let hash: [u8; 32] = self
                    .hasher
                    .clone()
                    .chain_update(&footer_bytes)
                    .finalize()
                    .into();

                footer_bytes[FOOTER_HASH_OFFSET..FOOTER_HASH_OFFSET + 32].copy_from_slice(&hash);

                self.pending_output.extend_from_slice(&footer_bytes);
                self.total_emitted += footer_bytes.len() as u64;
                self.phase = Phase::Done;
                Ok(())
            }
            Phase::Done => Ok(()),
        }
    }
}

impl ChunkSource for ZarSession {
    fn next_chunk(&mut self, max_bytes: usize) -> Result<Option<Vec<u8>>, anyhow::Error> {
        // Keeps advancing until `pending_output` reaches `max_bytes`
        // (rather than stopping as soon as it's non-empty): each
        // `advance_streaming()` call emits at most one compressed block,
        // which would otherwise cap chunk size well below `max_bytes`.
        while self.pending_output.len() < max_bytes && !matches!(self.phase, Phase::Done) {
            self.advance()?;
        }
        if self.pending_output.is_empty() {
            return Ok(None);
        }
        let n = max_bytes.min(self.pending_output.len());
        Ok(Some(self.pending_output.drain(..n).collect()))
    }

    fn is_done(&self) -> bool {
        matches!(self.phase, Phase::Done) && self.pending_output.is_empty()
    }

    fn total_units(&self) -> u64 {
        self.total_input_bytes
    }

    /// `next_chunk()` returns compressed-output bytes, but `total_units()`
    /// is raw input bytes, so progress is tracked separately here instead
    /// of via the default "sum received bytes" heuristic. Capped at
    /// `total_input_bytes` since the last block's zero-padding would
    /// otherwise overshoot it.
    fn units_done(&self) -> Option<u64> {
        Some(
            (self.blocks_written * BLOCK_SIZE as u64 + self.block_buffer.len() as u64)
                .min(self.total_input_bytes),
        )
    }

    fn current_entry_name(&self) -> Option<&str> {
        Some(&self.base_name)
    }

    fn output_manifest(&self) -> Vec<(String, u64)> {
        // Final size depends on every block's actual compressed size, so
        // it isn't known until the footer phase.
        if matches!(self.phase, Phase::Done) {
            vec![(output_file_name(&self.base_name), self.total_emitted)]
        } else {
            Vec::new()
        }
    }
}

#[cfg(test)]
mod name_table_tests {
    use super::*;

    #[test]
    fn non_ascii_name_is_encoded_as_windows_1252_not_utf8() {
        let (table, offsets) = build_name_table(&["café.txt".to_owned()]);
        assert_eq!(table[0], 8); // length prefix: 8 one-byte chars, not UTF-8's 9
        assert_eq!(&table[1..9], b"caf\xE9.txt");
        assert_eq!(offsets, vec![0]);
    }
}
