// Rejects a single untrusted path *component* (a ZAR/STFS name-table entry,
// or one JS-supplied relative-path segment) before it's joined into a path.

/// True if `component` is safe to use as a single segment of a `/`-joined
/// path: non-empty, not `.`/`..`, and free of embedded separators or NUL.
pub(crate) fn is_safe_path_component(component: &str) -> bool {
    if component.is_empty() || component == "." || component == ".." {
        return false;
    }
    // `\` rejected on every target, not just Windows: a caller who later
    // joins with `\`-aware APIs must still be covered.
    !component.contains(['/', '\\', '\0'])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_ordinary_names() {
        for name in ["foo.txt", "default.xbe", "café.txt", "€uro", "a b"] {
            assert!(is_safe_path_component(name), "{name:?} should be safe");
        }
    }

    #[test]
    fn rejects_empty() {
        assert!(!is_safe_path_component(""));
    }

    #[test]
    fn rejects_dot_and_dotdot() {
        assert!(!is_safe_path_component("."));
        assert!(!is_safe_path_component(".."));
    }

    #[test]
    fn accepts_names_that_merely_start_or_end_with_dots() {
        for name in ["...", "..foo", "foo..", "..foo..", ".gitignore", ".hidden"] {
            assert!(is_safe_path_component(name), "{name:?} should be safe");
        }
    }

    #[test]
    fn rejects_embedded_separators() {
        assert!(!is_safe_path_component("a/b"));
        assert!(!is_safe_path_component("a\\b"));
        assert!(!is_safe_path_component("../evil"));
        assert!(!is_safe_path_component("..\\evil"));
    }

    #[test]
    fn rejects_embedded_nul() {
        assert!(!is_safe_path_component("evil\0.txt"));
    }

    #[test]
    fn never_panics_on_arbitrary_bytes() {
        // Must be total over any `&str` - sits on the untrusted-read path.
        for b in 0u8..=255 {
            if let Ok(s) = std::str::from_utf8(&[b]) {
                let _ = is_safe_path_component(s);
            }
        }
    }
}
