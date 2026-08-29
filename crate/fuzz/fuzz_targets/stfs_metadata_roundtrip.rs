#![no_main]

use iso2x::fuzz_targets::{StfsMetadata, stfs_metadata_round_trips};
use libfuzzer_sys::fuzz_target;

// Mixed-endian fields, a hand-mapped 24-bit int, and `pad_before` gaps make this the
// richest round-trip candidate. See `stfs_metadata_round_trips` for the 24-bit masking.
fuzz_target!(|metadata: StfsMetadata| {
    stfs_metadata_round_trips(metadata);
});
