#![no_main]

use iso2x::fuzz_targets::stfs_header;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    stfs_header(data);
});
