#![no_main]

use iso2x::fuzz_targets::ciso_header;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    ciso_header(data);
});
