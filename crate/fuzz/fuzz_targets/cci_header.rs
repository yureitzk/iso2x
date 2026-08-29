#![no_main]

use iso2x::fuzz_targets::cci_header;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    cci_header(data);
});
