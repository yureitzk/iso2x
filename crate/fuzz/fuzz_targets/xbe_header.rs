#![no_main]

use iso2x::fuzz_targets::xbe_header;
use libfuzzer_sys::fuzz_target;

// Generalizes xbe.rs's truncation/single-byte-corruption unit tests to the whole input space.
fuzz_target!(|data: &[u8]| {
    xbe_header(data);
});
