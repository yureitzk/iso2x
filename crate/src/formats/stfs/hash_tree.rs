//! Real (non-placeholder) STFS hash-tree computation, needed for signed
//! output since `topHashTableHash` in the header must be known up front.
//!
//! "Level 0" tables hash data/file-table blocks directly; "level 1"
//! tables hash level-0 tables; "level 2" (large packages only) hashes
//! level-1 tables. 0xAA blocks/level-0 table, 0x70E4 blocks/level-1
//! table, 0x4AF768 blocks/level-2 table
//! (<https://free60.org/System-Software/Formats/STFS>).
//!
//! Takes plain block content plus a caller-supplied chain-link
//! (status/nextBlock) per block rather than reaching into `StfsLayout`.

use super::format::{Level, TOP_RECORD_SIZE_USIZE};
use sha1::{Digest, Sha1};

const BLOCK_SIZE_USIZE: usize = 0x1000;
const RECORD_SIZE: usize = TOP_RECORD_SIZE_USIZE; // 0x18: 20-byte hash + 1-byte status + 3-byte nextBlock

/// Entries per hash table at every level - one `BLOCK_SIZE` block each.
pub(super) const ENTRIES_PER_TABLE: u32 = 0xAA;

/// Byte offset of the "blocks hashed" trailer, written into level-1/
/// level-2 tables only (not level-0, which has no separate count to add).
const BLOCKS_HASHED_TRAILER_OFFSET: usize = 0xFF0;

/// Status byte + nextBlock the caller already knows for a given block.
/// For level 1/2 entries (no chaining concept) this is always
/// `Allocated` (0x80) with `next_block = 0` - unconfirmed against a real
/// console-signed multi-level-table package.
#[derive(Clone, Copy)]
pub(crate) struct BlockLink {
    pub(crate) status: u8,
    pub(crate) next_block: u32,
}

type TableBuf = [u8; BLOCK_SIZE_USIZE];

fn new_table_buf() -> TableBuf {
    [0u8; BLOCK_SIZE_USIZE]
}

/// Writes one `TOP_RECORD_SIZE`-byte record at `local_index` within
/// `buf` - shared by every level, since all three levels use this same
/// record shape.
///
/// # Panics
///
/// Panics if `local_index >= ENTRIES_PER_TABLE` - a bug in the caller's
/// block-to-table math, not a runtime condition this module expects to
/// see in practice.
fn write_record(buf: &mut TableBuf, local_index: u32, hash: &[u8; 20], link: BlockLink) {
    assert!(
        local_index < ENTRIES_PER_TABLE,
        "hash_tree: local_index out of range"
    );
    let off = local_index as usize * RECORD_SIZE;
    buf[off..off + 20].copy_from_slice(hash);
    buf[off + 20] = link.status;
    let nb = link.next_block.to_be_bytes();
    buf[off + 21..off + 24].copy_from_slice(&nb[1..4]);
}

/// Writes the "blocks hashed" trailer - only ever called for level-1/2
/// tables (see `BLOCKS_HASHED_TRAILER_OFFSET`'s doc comment).
fn write_blocks_hashed_trailer(buf: &mut TableBuf, blocks_hashed: u32) {
    buf[BLOCKS_HASHED_TRAILER_OFFSET..BLOCKS_HASHED_TRAILER_OFFSET + 4]
        .copy_from_slice(&blocks_hashed.to_be_bytes());
}

fn hash_table(buf: &TableBuf) -> [u8; 20] {
    Sha1::digest(buf).into()
}

/// How many of a level-0 table's entries are real data/file-table
/// blocks rather than trailing unused space - every table is full
/// (`ENTRIES_PER_TABLE`) except possibly the very last one, which holds
/// whatever's left over from `total_blocks`.
fn blocks_in_level0_table(table_idx: usize, level0_count: usize, total_blocks: u32) -> u32 {
    if table_idx + 1 < level0_count {
        ENTRIES_PER_TABLE
    } else {
        let rem = total_blocks % ENTRIES_PER_TABLE;
        if rem == 0 { ENTRIES_PER_TABLE } else { rem }
    }
}

/// The fully-built tree, ready for the writer's emission phase to
/// stream out verbatim in physical-block order, and for `top_hash` to be
/// written into the volume descriptor before the header is signed.
pub(crate) struct HashTree {
    /// Level-0 tables, in table order (table `i` covers blocks
    /// `[i*0xAA, (i+1)*0xAA)`). Empty when `top_level == Level::Zero` -
    /// use `top` as the sole level-0 table in that case.
    pub(crate) level0: Vec<TableBuf>,
    /// Level-1 tables, in table order. Empty unless `top_level ==
    /// Level::Two` - for `Level::One` the level above level 0 *is* `top`.
    pub(crate) level1: Vec<TableBuf>,
    /// The top-level table's bytes - whichever level `top_level` names.
    pub(crate) top: TableBuf,
    /// SHA1 of `top` - goes straight into the volume descriptor's
    /// `topHashTableHash` field, and from there (transitively, as part
    /// of the header digest) into what gets signed.
    pub(crate) top_hash: [u8; 20],
}

/// Incremental builder - fed one block at a time, in order, then
/// finished once, so callers can interleave their own cancellation/
/// progress checks between calls.
pub(crate) struct HashTreeBuilder {
    top_level: Level,
    total_blocks: u32,
    level0_tables: Vec<TableBuf>,
    /// Only allocated (non-empty) for `Level::Two` - see `HashTree::level1`.
    level1_tables: Vec<TableBuf>,
    next_block: u32,
}

impl HashTreeBuilder {
    pub(crate) fn new(top_level: Level, total_blocks: u32) -> Self {
        let level0_count = total_blocks.div_ceil(ENTRIES_PER_TABLE) as usize;
        let level1_count = match top_level {
            Level::Two => {
                let level0_count_u32 = u32::try_from(level0_count)
                    .expect("level0_count is derived from a u32 total_blocks, so it fits back");
                level0_count_u32.div_ceil(ENTRIES_PER_TABLE) as usize
            }
            Level::Zero | Level::One => 0,
        };
        Self {
            top_level,
            total_blocks,
            level0_tables: vec![new_table_buf(); level0_count.max(1)],
            level1_tables: vec![new_table_buf(); level1_count],
            next_block: 0,
        }
    }

    /// Hashes block `block_num`'s real content into its level-0 entry.
    /// Must be called for every block `0..total_blocks`, strictly in
    /// increasing order.
    ///
    /// # Panics
    ///
    /// Panics if called out of order, or with content that isn't
    /// exactly `BLOCK_SIZE` bytes.
    pub(crate) fn hash_block(&mut self, block_num: u32, content: &[u8], link: BlockLink) {
        assert_eq!(
            block_num, self.next_block,
            "hash_tree: blocks must be hashed in order"
        );
        assert_eq!(
            content.len(),
            BLOCK_SIZE_USIZE,
            "hash_tree: block content must be exactly BLOCK_SIZE bytes"
        );
        let table_idx = (block_num / ENTRIES_PER_TABLE) as usize;
        let local = block_num % ENTRIES_PER_TABLE;
        let hash: [u8; 20] = Sha1::digest(content).into();
        write_record(&mut self.level0_tables[table_idx], local, &hash, link);
        self.next_block += 1;
    }

    /// Rolls every level-0 table's hash up into level-1 (if present),
    /// then level-1 into the top, and returns the finished tree.
    ///
    /// # Panics
    ///
    /// Panics if fewer than `total_blocks` blocks were hashed.
    pub(crate) fn finish(self) -> HashTree {
        assert_eq!(
            self.next_block, self.total_blocks,
            "hash_tree: not all blocks were hashed before finish()"
        );

        match self.top_level {
            Level::Zero => {
                let top = self
                    .level0_tables
                    .into_iter()
                    .next()
                    .unwrap_or_else(new_table_buf);
                let top_hash = hash_table(&top);
                HashTree {
                    level0: Vec::new(),
                    level1: Vec::new(),
                    top,
                    top_hash,
                }
            }

            Level::One => {
                let level0 = self.level0_tables;
                let mut top = new_table_buf();
                for (i, table) in level0.iter().enumerate() {
                    let h = hash_table(table);
                    let index = u32::try_from(i)
                        .expect("level0 table count fits in a single hash table's u32 index");
                    write_record(
                        &mut top,
                        index,
                        &h,
                        BlockLink {
                            status: 0x80,
                            next_block: 0,
                        },
                    );
                }
                write_blocks_hashed_trailer(&mut top, self.total_blocks);
                let top_hash = hash_table(&top);
                HashTree {
                    level0,
                    level1: Vec::new(),
                    top,
                    top_hash,
                }
            }

            Level::Two => {
                let level0 = self.level0_tables;
                let mut level1 = self.level1_tables;
                let level0_count = level0.len();
                for (l1_idx, l1_table) in level1.iter_mut().enumerate() {
                    let l0_start = l1_idx * ENTRIES_PER_TABLE as usize;
                    let l0_end = (l0_start + ENTRIES_PER_TABLE as usize).min(level0_count);
                    let mut blocks_under_this_l1 = 0u32;
                    for (local, l0_idx) in (l0_start..l0_end).enumerate() {
                        let h = hash_table(&level0[l0_idx]);
                        let local_index = u32::try_from(local)
                            .expect("local index is bounded by ENTRIES_PER_TABLE (170)");
                        write_record(
                            l1_table,
                            local_index,
                            &h,
                            BlockLink {
                                status: 0x80,
                                next_block: 0,
                            },
                        );
                        blocks_under_this_l1 +=
                            blocks_in_level0_table(l0_idx, level0_count, self.total_blocks);
                    }
                    write_blocks_hashed_trailer(l1_table, blocks_under_this_l1);
                }

                let mut top = new_table_buf();
                for (i, table) in level1.iter().enumerate() {
                    let h = hash_table(table);
                    let index = u32::try_from(i)
                        .expect("level1 table count is bounded by ENTRIES_PER_TABLE (170)");
                    write_record(
                        &mut top,
                        index,
                        &h,
                        BlockLink {
                            status: 0x80,
                            next_block: 0,
                        },
                    );
                }
                write_blocks_hashed_trailer(&mut top, self.total_blocks);
                let top_hash = hash_table(&top);
                HashTree {
                    level0,
                    level1,
                    top,
                    top_hash,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const END_OF_CHAIN: u32 = 0x00FF_FFFF;

    fn block(fill: u8) -> Vec<u8> {
        vec![fill; BLOCK_SIZE_USIZE]
    }

    #[test]
    fn level_zero_hashes_blocks_directly_into_the_top_table() {
        let mut builder = HashTreeBuilder::new(Level::Zero, 3);
        for i in 0..3u32 {
            builder.hash_block(
                i,
                &block(i as u8),
                BlockLink {
                    status: 0x80,
                    next_block: if i == 2 { END_OF_CHAIN } else { i + 1 },
                },
            );
        }
        let tree = builder.finish();

        assert!(tree.level0.is_empty());
        assert!(tree.level1.is_empty());

        for i in 0..3u32 {
            let expected_hash: [u8; 20] = Sha1::digest(block(i as u8)).into();
            let off = i as usize * RECORD_SIZE;
            assert_eq!(&tree.top[off..off + 20], &expected_hash);
            assert_eq!(tree.top[off + 20], 0x80);
        }
        // block 2's nextBlock == END_OF_CHAIN, big-endian 3 bytes
        let off2 = 2 * RECORD_SIZE;
        assert_eq!(&tree.top[off2 + 21..off2 + 24], &[0xFF, 0xFF, 0xFF]);
        // block 0's nextBlock == 1
        assert_eq!(&tree.top[0 + 21..0 + 24], &[0x00, 0x00, 0x01]);

        // No trailer for a level-0 top table.
        assert_eq!(
            &tree.top[BLOCKS_HASHED_TRAILER_OFFSET..BLOCKS_HASHED_TRAILER_OFFSET + 4],
            &[0, 0, 0, 0]
        );

        assert_eq!(tree.top_hash, hash_table(&tree.top));
    }

    #[test]
    fn level_one_chains_level0_table_hashes_into_the_top() {
        let total_blocks = ENTRIES_PER_TABLE + 5; // forces a second, partial level-0 table
        let mut builder = HashTreeBuilder::new(Level::One, total_blocks);
        for i in 0..total_blocks {
            builder.hash_block(
                i,
                &block((i % 256) as u8),
                BlockLink {
                    status: 0x80,
                    next_block: i + 1,
                },
            );
        }
        let tree = builder.finish();

        assert_eq!(tree.level0.len(), 2);
        assert!(tree.level1.is_empty());

        for (i, l0_table) in tree.level0.iter().enumerate() {
            let expected = hash_table(l0_table);
            let off = i * RECORD_SIZE;
            assert_eq!(&tree.top[off..off + 20], &expected);
            assert_eq!(tree.top[off + 20], 0x80);
        }

        assert_eq!(
            &tree.top[BLOCKS_HASHED_TRAILER_OFFSET..BLOCKS_HASHED_TRAILER_OFFSET + 4],
            &total_blocks.to_be_bytes()
        );
        assert_eq!(tree.top_hash, hash_table(&tree.top));
    }

    #[test]
    fn level_two_nests_level1_tables_under_the_top() {
        let total_blocks = ENTRIES_PER_TABLE * 2 + 3; // 2 full + 1 partial level-0 table, all under one level-1 table
        let mut builder = HashTreeBuilder::new(Level::Two, total_blocks);
        for i in 0..total_blocks {
            builder.hash_block(
                i,
                &block(0xAB),
                BlockLink {
                    status: 0x80,
                    next_block: i + 1,
                },
            );
        }
        let tree = builder.finish();

        assert_eq!(tree.level0.len(), 3);
        assert_eq!(tree.level1.len(), 1);

        let expected_l1_hash = hash_table(&tree.level1[0]);
        assert_eq!(&tree.top[0..20], &expected_l1_hash);

        assert_eq!(
            &tree.level1[0][BLOCKS_HASHED_TRAILER_OFFSET..BLOCKS_HASHED_TRAILER_OFFSET + 4],
            &total_blocks.to_be_bytes()
        );
        assert_eq!(
            &tree.top[BLOCKS_HASHED_TRAILER_OFFSET..BLOCKS_HASHED_TRAILER_OFFSET + 4],
            &total_blocks.to_be_bytes()
        );
        assert_eq!(tree.top_hash, hash_table(&tree.top));
    }

    #[test]
    #[should_panic(expected = "must be hashed in order")]
    fn hashing_out_of_order_panics() {
        let mut builder = HashTreeBuilder::new(Level::Zero, 2);
        builder.hash_block(
            1,
            &block(0),
            BlockLink {
                status: 0x80,
                next_block: END_OF_CHAIN,
            },
        );
    }

    #[test]
    #[should_panic(expected = "not all blocks were hashed")]
    fn finishing_early_panics() {
        let mut builder = HashTreeBuilder::new(Level::Zero, 2);
        builder.hash_block(
            0,
            &block(0),
            BlockLink {
                status: 0x80,
                next_block: 1,
            },
        );
        let _ = builder.finish();
    }
}
