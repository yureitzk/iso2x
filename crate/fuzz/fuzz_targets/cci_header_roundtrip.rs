#![no_main]

use iso2x::fuzz_targets::{CciHeader, cci_header_round_trips};
use libfuzzer_sys::fuzz_target;

// Structure-aware: checks the write/read round trip `cci_header` (raw bytes) doesn't exercise.
fuzz_target!(|header: CciHeader| {
    cci_header_round_trips(header);
});
