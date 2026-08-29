use crate::core::executable::{xbe::XbeHeader, xex::XexHeader};
use crate::core::iso;
use crate::core::reader::DEFAULT_SEQ_WINDOW;
use crate::core::source::{
    MultiPartReader, SourcePart, SourcePartsRequiredExtern, required_parts_from_js,
};
use crate::core::title::TitleInfo;
use crate::utils::JsErrExt;
use serde::Serialize;
use std::io::{Read, Seek, SeekFrom};
use tsify::{Ts, Tsify};
use wasm_bindgen::prelude::*;

/// Spot-check evidence: a directory entry with a checkable magic past a
/// candidate split boundary.
pub(crate) struct VerifiableEntry {
    pub path: String,
    /// Absolute offset within the full logical image.
    pub absolute_offset: u64,
    pub expected_magic: &'static [u8],
}

/// XEX2: `<https://free60.org/System-Software/Formats/XEX/>`
/// XBEH: `<https://xboxdevwiki.net/Xbe>`
/// MZ (DLL/PE): standard DOS/PE magic.
fn expected_magic_for(path: &str) -> Option<&'static [u8]> {
    let ext = std::path::Path::new(path)
        .extension()?
        .to_str()?
        .to_ascii_lowercase();
    match ext.as_str() {
        "xex" => Some(b"XEX2"),
        "xbe" => Some(b"XBEH"),
        "dll" => Some(b"MZ"),
        _ => None,
    }
}

/// Whether `part` is itself a complete `.xex`/`.xbe` file, so it can be
/// excluded from being treated as a headerless continuation fragment.
/// `.dll` is excluded - no structural parser for it exists here.
pub(crate) fn is_self_contained_known_file(part: &SourcePart) -> bool {
    let Some(ext) = std::path::Path::new(&part.name)
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
    else {
        return false;
    };
    let Ok(reader) = MultiPartReader::new(vec![part.clone()], DEFAULT_SEQ_WINDOW) else {
        return false;
    };
    match ext.as_str() {
        "xex" => XexHeader::read(reader).is_ok(),
        "xbe" => XbeHeader::read(reader).is_ok(),
        _ => false,
    }
}

impl<R> iso::IsoReader<R> {
    /// Entries with a checkable magic at or past `boundary_in_volume`
    /// (volume-relative); earlier entries sit in part 1, already covered.
    fn verifiable_entries_past(&self, boundary_in_volume: u64) -> Vec<VerifiableEntry> {
        self.directory_table
            .entries
            .iter()
            .filter(|e| !e.is_directory())
            .filter_map(|e| {
                let magic = expected_magic_for(&e.path)?;
                let start_in_volume = u64::from(e.sector) * iso::SECTOR_SIZE;
                if start_in_volume < boundary_in_volume {
                    return None;
                }
                Some(VerifiableEntry {
                    path: e.path.clone(),
                    absolute_offset: self.volume_descriptor.root_offset + start_in_volume,
                    expected_magic: magic,
                })
            })
            .collect()
    }
}

/// One entry a verified split ordering spot-checked.
#[derive(Debug, Clone, Serialize, Tsify)]
#[serde(rename_all = "camelCase")]
pub struct CheckedEntry {
    pub path: String,
    pub matched: bool,
}

/// Result of verifying one candidate ordering of split parts.
#[derive(Debug, Clone, Serialize, Tsify)]
#[serde(rename_all = "camelCase")]
pub struct SplitVerifyResult {
    pub root_offset: u64,
    /// `true` only if the header/directory table parsed, the combined
    /// size covers every referenced byte, and every spot-checked entry
    /// matched its expected magic.
    pub ok: bool,
    /// Entries actually read back and compared, in check order.
    pub checked_entries: Vec<CheckedEntry>,
    /// Set when `ok` is `false`, or when `ok` is `true` but nothing was
    /// spot-checked (size/header only).
    pub reason: Option<String>,
}

impl SplitVerifyResult {
    fn fail(root_offset: u64, reason: impl Into<String>) -> Self {
        Self {
            root_offset,
            ok: false,
            checked_entries: Vec::new(),
            reason: Some(reason.into()),
        }
    }
}

/// JS-facing `verify_ordering` for one specific candidate ordering.
///
/// # Errors
///
/// Returns an error if `parts` isn't a non-empty array of source parts,
/// or if reading from any part fails at the I/O level.
#[wasm_bindgen(js_name = verifySplitCandidate)]
pub fn verify_split_candidate(
    parts: &SourcePartsRequiredExtern,
) -> Result<Ts<SplitVerifyResult>, JsError> {
    let parts = required_parts_from_js(parts).js_err()?;
    Ok(verify_ordering(parts)?.into_ts()?)
}

/// Parses `parts` (in order) as one XDVDFS volume, confirms the directory
/// table fits within the total size, then spot-checks entries past part 1.
/// Thin wrapper over `verify_ordering_impl` that discards the probe capture.
fn verify_ordering(parts: Vec<SourcePart>) -> Result<SplitVerifyResult, JsError> {
    verify_ordering_impl(parts, false).map(|(result, _probe)| result)
}

/// What `verify_ordering_impl` hands back for a winning ordering when
/// `capture_probe` is set: everything `source::ProbedDirectoryTable` needs.
pub(crate) struct OrderingProbe {
    pub(crate) directory_table: iso::DirectoryTable,
    pub(crate) title_info: TitleInfo,
}

/// `verify_ordering`'s shared body. `capture_probe` gates the extra read
/// needed to build a reusable handle, only paid on the `ok: true` path.
pub(crate) fn verify_ordering_impl(
    parts: Vec<SourcePart>,
    capture_probe: bool,
) -> Result<(SplitVerifyResult, Option<OrderingProbe>), JsError> {
    if parts.is_empty() {
        return Ok((SplitVerifyResult::fail(0, "no parts given to verify"), None));
    }
    let total_size: u64 = parts.iter().map(|p| p.size).sum();
    let part1_size = parts[0].size;

    let probe_reader = MultiPartReader::new(parts.clone(), DEFAULT_SEQ_WINDOW).js_err()?;
    let mut detected = match iso::probe_source_over(probe_reader) {
        Ok(d) => d,
        Err(e) => {
            return Ok((
                SplitVerifyResult::fail(0, format!("header/directory parse failed: {e:#}")),
                None,
            ));
        }
    };

    let root_offset = detected.volume_descriptor.root_offset;
    let max_used_prefix_size = detected.max_used_prefix_size();
    if total_size < root_offset + max_used_prefix_size {
        return Ok((
            SplitVerifyResult::fail(
                root_offset,
                format!(
                    "parts sum to {total_size} bytes but the directory table \
                     references data up to {} bytes",
                    root_offset + max_used_prefix_size
                ),
            ),
            None,
        ));
    }
    if part1_size < root_offset {
        return Ok((
            SplitVerifyResult::fail(
                root_offset,
                "part 1 is smaller than the detected root offset",
            ),
            None,
        ));
    }

    let boundary_in_volume = part1_size - root_offset;
    let verifiable = detected.verifiable_entries_past(boundary_in_volume);
    if verifiable.is_empty() {
        // Nothing to spot-check, but size/header checks passed.
        let probe = capture_probe
            .then(|| build_ordering_probe(&mut detected))
            .flatten();
        return Ok((
            SplitVerifyResult {
                root_offset,
                ok: true,
                checked_entries: Vec::new(),
                reason: Some(
                    "no executable entries land past part 1 - size and header checks \
                     passed but content was not spot-checked"
                        .to_owned(),
                ),
            },
            probe,
        ));
    }

    // Fresh reader: the first one was consumed parsing the directory table.
    let mut read_reader = MultiPartReader::new(parts, DEFAULT_SEQ_WINDOW).js_err()?;

    let mut checked_entries = Vec::with_capacity(verifiable.len());
    let mut all_matched = true;
    for entry in &verifiable {
        let mut buf = vec![0u8; entry.expected_magic.len()];
        let matched = read_reader
            .seek(SeekFrom::Start(entry.absolute_offset))
            .and_then(|_| read_reader.read_exact(&mut buf))
            .is_ok_and(|()| buf.as_slice() == entry.expected_magic);
        if !matched {
            all_matched = false;
        }
        checked_entries.push(CheckedEntry {
            path: entry.path.clone(),
            matched,
        });
    }

    // detected is still intact (only directory_table was read).
    let probe = (capture_probe && all_matched)
        .then(|| build_ordering_probe(&mut detected))
        .flatten();

    Ok((
        SplitVerifyResult {
            root_offset,
            ok: all_matched,
            reason: if all_matched {
                None
            } else {
                Some(
                    "one or more executable entries past part 1 did not match their \
                     expected magic at the expected offset - wrong part, or parts in \
                     the wrong order"
                        .to_owned(),
                )
            },
            checked_entries,
        },
        probe,
    ))
}

/// Parses the launch executable off an already-walked `detected`, paired
/// with its directory table. `None` if no launch executable parses - the
/// ordering is still reported verified, just without a reuse handle.
fn build_ordering_probe(detected: &mut iso::IsoReader<MultiPartReader>) -> Option<OrderingProbe> {
    TitleInfo::from_image(detected)
        .ok()
        .map(|title_info| OrderingProbe {
            directory_table: detected.directory_table.clone(),
            title_info,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Empty parts must not panic.
    #[test]
    fn verify_ordering_impl_reports_a_failed_result_instead_of_panicking_on_empty_parts() {
        let (result, probe) = verify_ordering_impl(Vec::new(), false)
            .expect("empty parts must not panic, and must not need a JsError");
        assert!(!result.ok);
        assert!(probe.is_none());
    }
}
