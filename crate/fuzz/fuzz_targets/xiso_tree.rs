#![no_main]

use iso2x::fuzz_targets::xiso_tree;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    xiso_tree(data);
});
