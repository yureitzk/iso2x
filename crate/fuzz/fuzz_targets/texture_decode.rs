#![no_main]

use arbitrary::Arbitrary;
use iso2x::fuzz_targets::texture_decode;
use libfuzzer_sys::fuzz_target;

#[derive(Debug, Arbitrary)]
struct Input {
    kind: u8,
    width: u32,
    height: u32,
    data: Vec<u8>,
}

// Covers the decoders `dxt1_decode` doesn't; `kind` selects which one.
fuzz_target!(|input: Input| {
    texture_decode(input.kind, input.width, input.height, &input.data);
});
