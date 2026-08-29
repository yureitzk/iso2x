use crate::core::iso;
use crate::core::reader::DEFAULT_SEQ_WINDOW;
use crate::core::source::{MultiPartReader, SourcePart, SourceReadFnExtern};
use crate::utils::{JsErrExt, js_number_to_u64};
use js_sys::Function;
use serde::Serialize;
use tsify::{Ts, Tsify};
use wasm_bindgen::prelude::*;

/// Whether a file is (a) not recognizable as XDVDFS, (b) recognizable
/// but missing bytes its own directory table references (a truncated
/// "part 1" candidate), or (c) independently complete.
pub(crate) struct Completeness {
    pub(crate) root_offset: u64,
    pub(crate) max_used_prefix_size: u64,
    pub(crate) is_complete: bool,
    pub(crate) root_directory_table_offset: u64,
}

impl From<Completeness> for IsoCompletenessInfo {
    fn from(c: Completeness) -> Self {
        Self {
            is_complete: c.is_complete,
            root_offset: c.root_offset,
            max_used_prefix_size: c.max_used_prefix_size,
            root_directory_table_offset: c.root_directory_table_offset,
        }
    }
}

/// Probes `part` as a raw XDVDFS image. `Ok(None)` when it doesn't parse
/// at all. `Ok(Some((_, detected)))` when the volume descriptor parses,
/// whether or not the directory-tree walk finished and whether or not
/// `part` alone holds every byte the directory table references.
/// `Completeness.is_complete` is `false` unconditionally when the walk
/// didn't finish, since the entry count is then a definite undercount.
pub(crate) fn probe_completeness(
    part: &SourcePart,
) -> Result<Option<(Completeness, iso::IsoReader<MultiPartReader>)>, anyhow::Error> {
    // Window size is inert here (Cached-mode probe, never Sequential),
    // but MultiPartReader::new still requires a value.
    let reader = MultiPartReader::new(vec![part.clone()], DEFAULT_SEQ_WINDOW)?;
    match iso::IsoReader::probe_staged(reader) {
        Some(detected) => {
            let root_offset = detected.volume_descriptor.root_offset;
            let root_directory_table_offset =
                root_offset + u64::from(detected.directory_table.root_sector) * iso::SECTOR_SIZE;
            let max_used_prefix_size = detected.max_used_prefix_size();
            let is_complete =
                detected.tree_fully_walked && part.size >= root_offset + max_used_prefix_size;
            Ok(Some((
                Completeness {
                    root_offset,
                    max_used_prefix_size,
                    is_complete,
                    root_directory_table_offset,
                },
                detected,
            )))
        }
        None => Ok(None),
    }
}

/// `checkIsoCompleteness`'s JS-facing result: `undefined` when the file
/// doesn't parse as XDVDFS at all; otherwise whether it alone already
/// holds every byte its own directory table references.
#[derive(Debug, Clone, Serialize, Tsify)]
#[serde(rename_all = "camelCase")]
pub struct IsoCompletenessInfo {
    pub is_complete: bool,
    pub root_offset: u64,
    pub max_used_prefix_size: u64,
    pub root_directory_table_offset: u64,
}

/// `#[serde(transparent)]` keeps the JS shape `IsoCompletenessInfo |
/// undefined`, not a wrapper object.
#[derive(Debug, Clone, Serialize, Tsify)]
#[serde(transparent)]
pub struct IsoCompletenessResult(pub Option<IsoCompletenessInfo>);

/// Single-file completeness probe, exposed so JS split-detection can
/// classify one dropped file at a time without a whole batch.
///
/// # Errors
///
/// Returns an error if reading from `read_fn` fails at the I/O level.
#[wasm_bindgen(js_name = checkIsoCompleteness)]
pub fn check_iso_completeness(
    read_fn: &SourceReadFnExtern,
    file_size: f64,
) -> Result<Ts<IsoCompletenessResult>, JsError> {
    let read_fn: &Function = read_fn.unchecked_ref();
    let part = SourcePart {
        name: String::new(),
        read_fn: read_fn.clone(),
        size: js_number_to_u64(file_size, "fileSize").js_err()?,
    };
    let result = probe_completeness(&part).js_err()?;
    let info: Option<IsoCompletenessInfo> = result.map(|(c, _)| c.into());
    Ok(IsoCompletenessResult(info).into_ts()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::source::SourcePart;
    use js_sys::Uint8Array;
    use wasm_bindgen_test::*;

    fn make_source_part(name: &str, bytes: &'static [u8]) -> SourcePart {
        let closure = wasm_bindgen::closure::Closure::wrap(Box::new(
            move |offset: f64, length: f64| -> Uint8Array {
                let offset = offset as usize;
                let length = length as usize;
                let end = (offset + length).min(bytes.len());
                let slice = if offset < bytes.len() {
                    &bytes[offset..end]
                } else {
                    &[]
                };
                Uint8Array::from(slice)
            },
        )
            as Box<dyn FnMut(f64, f64) -> Uint8Array>);
        let read_fn: Function = closure.as_ref().clone().unchecked_into();
        closure.forget();
        SourcePart {
            name: name.to_owned(),
            read_fn,
            size: bytes.len() as u64,
        }
    }

    #[wasm_bindgen_test]
    fn garbage_bytes_do_not_parse_as_xdvdfs() {
        let part = make_source_part("garbage.iso", &[0xAAu8; 4096]);
        let result = probe_completeness(&part).expect("must not error on non-XDVDFS bytes");
        assert!(result.is_none());
    }

    #[wasm_bindgen_test]
    fn empty_file_does_not_parse_as_xdvdfs() {
        let part = make_source_part("empty.iso", &[]);
        let result = probe_completeness(&part).expect("must not error on an empty file");
        assert!(result.is_none());
    }
}
