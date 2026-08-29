//! GOD splits a title into `Data%04d` parts. Each part holds one master
//! hash block, `SUBPARTS_PER_PART` subhash blocks, then up to
//! `BLOCKS_PER_PART` data blocks split across those subparts
//! (`BLOCKS_PER_SUBPART` each).

pub use super::hash_list::HashList;
pub(crate) use crate::core::signing::ConHeaderBuilder;
use wasm_bindgen::prelude::*;

pub const BLOCKS_PER_PART: u64 = 0xa1c4;
pub const BLOCKS_PER_SUBPART: u64 = 0xcc;
pub const BLOCK_SIZE: u64 = 0x1000;
pub const SUBPARTS_PER_PART: u32 = 0xcb;
pub const SUBPART_SIZE: u64 = BLOCK_SIZE * BLOCKS_PER_SUBPART;

pub const MHT_SIZE: usize = 4096;
/// Size of one master hash table block - same as `BLOCK_SIZE`, since
/// every Nth block in an STFS-family package is itself a hash block.
/// `<https://free60.org/System-Software/Formats/STFS>`
#[wasm_bindgen(js_name = mhtSize)]
pub fn mht_size() -> u32 {
    u32::try_from(MHT_SIZE).expect("MHT_SIZE is a small compile-time constant")
}
