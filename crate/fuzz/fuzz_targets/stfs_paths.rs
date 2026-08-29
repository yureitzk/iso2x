#![no_main]

use iso2x::fuzz_targets::stfs_paths;
use libfuzzer_sys::fuzz_target;

// Exercises the path-traversal fix (`is_safe_path_component`, dangling-parent/cycle guards).
fuzz_target!(|data: &[u8]| {
    stfs_paths(data);
});
