use anyhow::Error;
use sha1::{Digest, Sha1};
use std::io::{Read, Write};

/// A GOD hash block (master or subhash table): up to 204 concatenated
/// 20-byte SHA1 hashes, zero-padded to fill 4096 bytes.
pub struct HashList {
    buffer: [u8; 4096],
    len: usize,
}

impl Default for HashList {
    fn default() -> Self {
        Self::new()
    }
}

impl HashList {
    pub fn bytes(&self) -> &[u8; 4096] {
        &self.buffer
    }

    pub fn new() -> HashList {
        HashList {
            buffer: [0u8; 4096],
            len: 0,
        }
    }

    pub fn read<R: Read>(mut reader: R) -> Result<HashList, Error> {
        let mut buffer = [0u8; 4096];
        reader.read_exact(&mut buffer)?;

        // Matches any all-zero chunk, not just a full 20-byte `[0u8; 20]`,
        // so the final 16-byte remainder (4096 % 20) still counts as
        // padding when the buffer is completely full (204 hashes).
        let len = buffer
            .chunks(20)
            .position(|c| c.iter().all(|&b| b == 0))
            .map_or(buffer.len(), |p| p * 20);

        Ok(HashList { buffer, len })
    }

    pub fn add_hash(&mut self, hash: &[u8; 20]) {
        self.buffer[self.len..self.len + 20].copy_from_slice(hash);
        self.len += 20;
    }

    pub fn add_block_hash(&mut self, block: &[u8]) {
        self.add_hash(&Sha1::digest(block).into());
    }

    pub fn digest(&self) -> [u8; 20] {
        Sha1::digest(self.buffer).into()
    }

    pub fn write<W: Write>(&self, mut writer: W) -> Result<(), Error> {
        writer.write_all(&self.buffer)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trips_at(hash_count: usize) {
        let mut list = HashList::new();
        for i in 0..hash_count {
            let mut hash = [0u8; 20];
            hash[0] = (i % 256) as u8;
            hash[1] = (i / 256) as u8;
            hash[19] = 0xAA;
            list.add_hash(&hash);
        }

        let mut buf = Vec::new();
        list.write(&mut buf).expect("write is infallible");

        let parsed = HashList::read(buf.as_slice()).expect("read must succeed");
        assert_eq!(parsed.len, hash_count * 20, "hash_count = {hash_count}");
        assert_eq!(parsed.buffer, list.buffer, "hash_count = {hash_count}");
    }

    #[test]
    fn round_trips_empty() {
        round_trips_at(0);
    }

    #[test]
    fn round_trips_single_hash() {
        round_trips_at(1);
    }

    #[test]
    fn round_trips_one_below_capacity() {
        round_trips_at(203);
    }

    /// Fully-packed boundary: no all-zero 20-byte chunk exists anywhere.
    #[test]
    fn round_trips_at_full_capacity() {
        round_trips_at(204);
    }

    #[test]
    #[ignore = "writes fuzz corpus seed files; run explicitly with --ignored"]
    fn write_fuzz_corpus_seed_for_hash_list() {
        let mut list = HashList::new();
        for i in 0..204u8 {
            let mut hash = [0u8; 20];
            hash[0] = i;
            hash[19] = 0xAA; // never all-zero, even at i == 0
            list.add_hash(&hash);
        }
        let mut bytes = Vec::new();
        list.write(&mut bytes).expect("write is infallible");

        let dir = "fuzz/corpus/hash_list";
        std::fs::create_dir_all(dir).expect("corpus directory should be creatable");
        std::fs::write(format!("{dir}/seed-full-hash-list"), &bytes)
            .expect("seed file should be writable");
    }
}
