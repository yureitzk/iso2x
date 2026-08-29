use crate::core::scrub::SECTOR_SIZE;
use wasm_bindgen::prelude::*;

/// Exact multiple of `SECTOR_SIZE`, so every split lands between two
/// sectors - `next_chunk` can clamp the sector range per call with no
/// mid-sector byte splitting.
pub(super) const SPLIT_MARGIN: u64 = 0xFF00_0000;
pub(super) const SPLIT_MARGIN_SECTORS: u64 = SPLIT_MARGIN / SECTOR_SIZE;

/// Threshold at which xiso output splits across "name.1.xiso.iso",
/// "name.2.xiso.iso", etc. Same boundary as `cciFileSplitPoint` - both
/// keep parts under the ~4 GiB FATX/FAT32 single-file limit - but xiso
/// applies it to a raw sector stream with no per-part header.
#[wasm_bindgen(js_name = xisoSplitMargin)]
pub fn xiso_split_margin() -> f64 {
    f64::from(u32::try_from(SPLIT_MARGIN).expect("SPLIT_MARGIN is a small compile-time constant"))
}
