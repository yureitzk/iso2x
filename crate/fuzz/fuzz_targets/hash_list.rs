#![no_main]

use iso2x::fuzz_targets::hash_list;
use libfuzzer_sys::fuzz_target;

// Exercises `HashList::read`'s zero-padding scan against arbitrary bytes.
fuzz_target!(|data: &[u8]| {
    hash_list(data);
});
