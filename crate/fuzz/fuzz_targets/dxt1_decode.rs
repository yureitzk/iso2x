#![no_main]

use arbitrary::Arbitrary;
use iso2x::fuzz_targets::dxt1_decode;
use libfuzzer_sys::fuzz_target;

#[derive(Debug, Arbitrary)]
struct Input {
    width: u16,
    height: u16,
    data: Vec<u8>,
}

fuzz_target!(|input: Input| {
    dxt1_decode(input.width, input.height, &input.data);
});
