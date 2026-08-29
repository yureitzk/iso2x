mod batch;
mod classify;
mod entry;
mod probe;
mod raw_split;
mod subset_search;
mod verify;

use crate::core::source::{FileType, SourcePart, detect};
use batch::{BatchResolution, UnresolvedKind};
use js_sys::Function;
use wasm_bindgen::prelude::*;

/// Best-effort mid-batch notification; callback/serialization errors are swallowed.
fn report(on_item: Option<&Function>, resolution: &BatchResolution) {
    let Some(cb) = on_item else { return };
    if let Ok(value) = serde_wasm_bindgen::to_value(resolution) {
        let _ = cb.call1(&JsValue::NULL, &value);
    }
}

/// Splits `parts` into raw-XISO candidates and everything else (reported as `Unresolved`).
fn partition_xiso_candidates(parts: Vec<SourcePart>) -> (Vec<SourcePart>, Vec<BatchResolution>) {
    let mut xiso = Vec::new();
    let mut skipped = Vec::new();
    for part in parts {
        match detect(part.read_fn.clone(), part.size) {
            Ok(FileType::Xiso) => xiso.push(part),
            Ok(other) => skipped.push(BatchResolution::Unresolved {
                names: vec![part.name],
                reason: format!(
                    "detected as {other:?}, not a raw XISO candidate - route it through its \
                     own format's classification instead"
                ),
                unresolved_kind: UnresolvedKind::Generic,
            }),
            Err(e) => skipped.push(BatchResolution::Unresolved {
                names: vec![part.name],
                reason: format!("failed to detect format: {e:#}"),
                unresolved_kind: UnresolvedKind::Generic,
            }),
        }
    }
    (xiso, skipped)
}
