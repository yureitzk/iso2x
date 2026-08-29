#![no_main]

use iso2x::fuzz_targets::xiso_probe_root_offset;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    xiso_probe_root_offset(data);
});
