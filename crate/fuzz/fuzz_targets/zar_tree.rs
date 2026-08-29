#![no_main]

use arbitrary::Arbitrary;
use iso2x::fuzz_targets::zar_tree;
use libfuzzer_sys::fuzz_target;

#[derive(Debug, Arbitrary)]
struct Input {
    name_table: Vec<u8>,
    tree_bytes: Vec<u8>,
}

// Fuzzes tree_bytes and name_table together; both come from the same untrusted .zar archive.
fuzz_target!(|input: Input| {
    zar_tree(&input.name_table, &input.tree_bytes);
});
