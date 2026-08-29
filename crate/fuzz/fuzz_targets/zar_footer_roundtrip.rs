#![no_main]

use iso2x::fuzz_targets::{ZarFooter, zar_footer_round_trips};
use libfuzzer_sys::fuzz_target;

// Covers the write/read round trip for the fixed-size footer; `zar_tree` covers the file-tree walk.
fuzz_target!(|footer: ZarFooter| {
    zar_footer_round_trips(footer);
});
