#![no_main]

use iso2x::fuzz_targets::xpr_decode;
use libfuzzer_sys::fuzz_target;

// Whole XPR0 container path: header, resource table, and decode dispatch together.
fuzz_target!(|data: &[u8]| {
    xpr_decode(data);
});
