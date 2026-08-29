#![no_main]

use iso2x::fuzz_targets::xdbf_resource;
use libfuzzer_sys::fuzz_target;

// Manual split keeps the wire format `id(8 LE) ++ section(2 LE) ++ data` so seed
// files stay hand-constructible; `arbitrary`'s Vec<u8> decoding wouldn't preserve that.
fuzz_target!(|data: &[u8]| {
    let Some((id_bytes, rest)) = data.split_first_chunk::<8>() else {
        return;
    };
    let Some((section_bytes, blob)) = rest.split_first_chunk::<2>() else {
        return;
    };
    let id = u64::from_le_bytes(*id_bytes);
    let section = u16::from_le_bytes(*section_bytes);
    xdbf_resource(blob, id, section);
});
