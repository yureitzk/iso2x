use wasm_bindgen::prelude::*;

/// Builds a proper JS `Error` (stack, `.message`, `instanceof Error`) from
/// any displayable error.
pub(crate) fn js_err(msg: impl std::fmt::Display) -> JsError {
    JsError::new(&msg.to_string())
}

/// Maps this `Result`'s error to a `JsError`, formatted with `{:#}` - the
/// alternate form `anyhow::Error` uses to print its full `.context()`
/// chain, not just the top-level message.
pub(crate) trait JsErrExt<T> {
    fn js_err(self) -> Result<T, JsError>;
}

impl<T, E: std::fmt::Display> JsErrExt<T> for Result<T, E> {
    fn js_err(self) -> Result<T, JsError> {
        self.map_err(|e| js_err(format!("{e:#}")))
    }
}
