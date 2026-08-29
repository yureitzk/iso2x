/// `u64::MAX` (2^64 - 1) isn't representable as `f64`; it rounds up to
/// 2^64, which is exactly the exclusive upper bound we want here.
#[allow(clippy::cast_precision_loss)]
const U64_MAX_P1: f64 = u64::MAX as f64;

/// Every integer up to and including this fits in an `f64`'s 53-bit
/// mantissa exactly - `2^53`. Past it, `as f64` still "succeeds" but
/// silently rounds to the nearest representable value instead of
/// erroring, which is exactly the failure mode `u64_to_js_number`
/// exists to catch before it reaches JS.
const MAX_EXACT_F64_INT: u64 = 1 << 53;

pub(crate) fn js_number_to_u64(v: f64, what: &str) -> Result<u64, anyhow::Error> {
    if v < 0.0 || v.fract() != 0.0 || v >= U64_MAX_P1 {
        anyhow::bail!("{what} is not a valid non-negative integer: {v}");
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // checked above
    Ok(v as u64)
}

/// The JS-bound mirror of `js_number_to_u64`: rejects a `u64` that can't
/// round-trip through a JS `number` exactly, rather than silently
/// handing JS a rounded value. Guards the Rust -> JS direction of the
/// same precision boundary `js_number_to_u64` guards JS -> Rust.
pub(crate) fn u64_to_js_number(v: u64, what: &str) -> Result<f64, anyhow::Error> {
    if v > MAX_EXACT_F64_INT {
        anyhow::bail!("{what} exceeds the range a JS number can represent exactly: {v}");
    }
    #[allow(clippy::cast_precision_loss)] // checked above
    Ok(v as f64)
}

#[cfg(test)]
mod tests {
    use super::{js_number_to_u64, u64_to_js_number};

    #[test]
    fn u64_to_js_number_round_trips_up_to_the_exact_boundary() {
        assert_eq!(u64_to_js_number(0, "x").unwrap(), 0.0);
        assert_eq!(u64_to_js_number(1 << 53, "x").unwrap(), (1u64 << 53) as f64);
    }

    #[test]
    fn u64_to_js_number_rejects_past_the_exact_boundary() {
        assert!(u64_to_js_number((1 << 53) + 1, "x").is_err());
        assert!(u64_to_js_number(u64::MAX, "x").is_err());
    }

    #[test]
    fn js_number_to_u64_and_u64_to_js_number_agree_on_the_boundary() {
        let n = (1u64 << 53) - 1;
        #[allow(clippy::cast_precision_loss)]
        let as_f64 = n as f64;
        assert_eq!(js_number_to_u64(as_f64, "x").unwrap(), n);
        assert_eq!(u64_to_js_number(n, "x").unwrap(), as_f64);
    }
}
