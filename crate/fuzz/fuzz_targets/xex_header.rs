#![no_main]

use iso2x::fuzz_targets::xex_header;
use libfuzzer_sys::fuzz_target;

// Generalizes xex.rs's hand-picked malformed-field-table unit tests to the whole input space.
fuzz_target!(|data: &[u8]| {
    xex_header(data);
});
