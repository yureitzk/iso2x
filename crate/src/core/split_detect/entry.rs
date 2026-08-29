use super::batch::HeaderCandidate;
use super::classify::{Classification, classify_parts};
use super::partition_xiso_candidates;
use super::raw_split::{RawSplitOutcome, find_raw_split};
use super::verify::SplitVerifyResult;
use crate::core::reader::DEFAULT_SEQ_WINDOW;
use crate::core::source::{
    FileType, SourcePart, SourcePartsRequiredExtern, detect, required_parts_from_js,
};
use crate::formats::cci::CciSource;
use crate::formats::ciso::CisoSource;
use crate::utils::JsErrExt;
use serde::Serialize;
use tsify::{Ts, Tsify};
use wasm_bindgen::prelude::*;

/// Unlike `resolve_raw_split_bucket`, only escalates on strong header
/// evidence (ambiguous, or found but unverified) - a bare continuation
/// with no header is too weak: `detect()` defaults any unrecognized magic
/// to `FileType::Xiso`, so CISO's part 2 or a GOD `Data####` fragment
/// land here too.
enum EntryRawSplitOutcome {
    /// Fall through to plain single-file detection.
    NoCandidates,
    Unresolvable {
        names: Vec<String>,
        reason: String,
        invalid_kind: InvalidKind,
    },
    Resolved {
        parts: Vec<String>,
        verify: SplitVerifyResult,
    },
}

fn raw_split_outcome_for_entry(parts: Vec<SourcePart>) -> Result<EntryRawSplitOutcome, JsError> {
    let (xiso_parts, _skipped) = partition_xiso_candidates(parts);
    let Classification {
        headers,
        continuations,
        ..
    } = classify_parts(xiso_parts, Vec::new(), None, false)?;

    Ok(match HeaderCandidate::from_headers(headers) {
        HeaderCandidate::None => EntryRawSplitOutcome::NoCandidates,
        HeaderCandidate::One(_) if continuations.is_empty() => EntryRawSplitOutcome::NoCandidates,
        HeaderCandidate::One(header) => match find_raw_split(&header, &continuations, false) {
            RawSplitOutcome::Resolved(parts, verify, _handle) => {
                EntryRawSplitOutcome::Resolved { parts, verify }
            }
            RawSplitOutcome::Ambiguous(orderings) => EntryRawSplitOutcome::Unresolvable {
                names: std::iter::once(header.name.clone())
                    .chain(continuations.iter().map(|p| p.name.clone()))
                    .collect(),
                reason: format!(
                    "{} different orderings of these parts each independently verified as \
                     a valid split - which is genuine can't be determined from content \
                     alone; resolve manually",
                    orderings.len()
                ),
                invalid_kind: InvalidKind::AmbiguousSplit,
            },
            RawSplitOutcome::Unresolved => EntryRawSplitOutcome::Unresolvable {
                names: std::iter::once(header.name.clone())
                    .chain(continuations.iter().map(|p| p.name.clone()))
                    .collect(),
                reason: "no ordering of these parts verified as a valid split".to_owned(),
                invalid_kind: InvalidKind::UnresolvedOrdering,
            },
        },
        HeaderCandidate::Ambiguous(headers) => EntryRawSplitOutcome::Unresolvable {
            names: headers
                .iter()
                .chain(continuations.iter())
                .map(|p| p.name.clone())
                .collect(),
            reason: "multiple ambiguous truncated-header candidates in this batch - \
                     resolve manually"
                .to_owned(),
            invalid_kind: InvalidKind::AmbiguousHeaders,
        },
    })
}

/// Why a `ResolvedEntry::Invalid` failed, so a caller can decide whether
/// to offer manual recovery without parsing `reason`. `Mismatch` never
/// recovers: the named-pair convention matched but the content didn't.
#[derive(Debug, Clone, Copy, Serialize, Tsify)]
#[serde(rename_all = "camelCase")]
pub enum InvalidKind {
    /// `.1./.2.` naming matched (CCI/CISO) but the parts don't form a
    /// valid split.
    Mismatch,
    /// Multiple ambiguous truncated-header candidates in the batch.
    AmbiguousHeaders,
    /// Exactly one truncated header, but no ordering of the
    /// continuation fragments verified as a valid split.
    UnresolvedOrdering,
    /// Exactly one truncated header, but more than one distinct ordering
    /// of the continuation fragments independently verified - see
    /// `RawSplitOutcome::Ambiguous`.
    AmbiguousSplit,
}

/// One batch-drop entry's resolution: a standalone file, a multi-part
/// split image, or a fragment set that couldn't be resolved.
#[derive(Debug, Clone, Serialize, Tsify)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ResolvedEntry {
    File {
        format: FileType,
        name: String,
    },
    Dir {
        format: FileType,
        names: Vec<String>,
    },
    Invalid {
        names: Vec<String>,
        reason: String,
        /// Explicit rename: tsify's tagged-enum codegen doesn't apply
        /// the enum-level `rename_all` to fields inside a struct
        /// variant.
        #[serde(rename = "invalidKind")]
        invalid_kind: InvalidKind,
    },
}

/// Finds a two-file split named "<stem>.1.<ext>" / "<stem>.2.<ext>"
/// (case-insensitive) that `entry` is itself one half of. Naming only -
/// the caller checks content.
///
/// Keyed off `entry` rather than "the first `.1.<ext>` found anywhere in
/// `parts`", since a batch can hold more than one `.1./.2.` pair and
/// scanning the whole array for any match would let an unrelated pair
/// hijack this entry's resolution.
fn find_named_split<'a>(
    entry: &'a SourcePart,
    parts: &'a [SourcePart],
    ext: &str,
) -> Option<(&'a SourcePart, &'a SourcePart)> {
    let lower_name = entry.name.to_ascii_lowercase();
    let suffix1 = format!(".1.{ext}");
    let suffix2 = format!(".2.{ext}");

    if let Some(stem) = lower_name.strip_suffix(suffix1.as_str()) {
        let partner_name = format!("{stem}.2.{ext}");
        let second = parts
            .iter()
            .find(|p| p.name.to_ascii_lowercase() == partner_name)?;
        return Some((entry, second));
    }
    if let Some(stem) = lower_name.strip_suffix(suffix2.as_str()) {
        let partner_name = format!("{stem}.1.{ext}");
        let first = parts
            .iter()
            .find(|p| p.name.to_ascii_lowercase() == partner_name)?;
        return Some((first, entry));
    }
    None
}

/// Named split-pair detection for CCI or CISO (`.cso`/`.ciso`). Naming
/// alone tells us the grouping and order, so verification means opening
/// the real format reader over the two parts and seeing whether it
/// parses, rather than a byte-search like the raw-XISO path.
///
/// Not a magic-byte check on each part: CCI's parts are each
/// self-contained, but CISO's header/index table live only in part 1,
/// so part 2 has no magic of its own to check.
///
/// A matched-but-invalid pair resolves as `Invalid` rather than falling
/// through to the next detector - the naming convention is unambiguous
/// evidence of intent.
fn detect_named_split(
    entry: &SourcePart,
    parts: &[SourcePart],
    ext: &str,
    format: FileType,
) -> Option<ResolvedEntry> {
    let (first, second) = find_named_split(entry, parts, ext)?;
    let names = vec![first.name.clone(), second.name.clone()];
    let pair = vec![first.clone(), second.clone()];

    let open_err = match format {
        FileType::Cci => CciSource::open(pair, DEFAULT_SEQ_WINDOW).err(),
        FileType::Ciso => CisoSource::open(pair, DEFAULT_SEQ_WINDOW).err(),
        _ => unreachable!("detect_named_split is only ever called with Cci or Ciso"),
    };

    Some(match open_err {
        None => ResolvedEntry::Dir { format, names },
        Some(e) => ResolvedEntry::Invalid {
            names,
            reason: format!(
                "found \"{}\"/\"{}\" but they don't form a valid {format:?} split: {e:#}",
                first.name, second.name
            ),
            invalid_kind: InvalidKind::Mismatch,
        },
    })
}

/// Resolves one batch-drop entry, trying every split-capable format in
/// order (named CCI pair, named CISO pair, content-verified raw XISO)
/// before falling back to plain single-file detection.
///
/// `entries[0]` is the file being resolved; the rest are candidate
/// siblings for split detection. Order beyond `entries[0]` doesn't
/// matter.
///
/// # Errors
///
/// Returns an error if `entries` is empty or if reading from any part
/// fails at the I/O level.
#[wasm_bindgen(js_name = resolveBatchEntry)]
pub fn resolve_batch_entry(
    entries: &SourcePartsRequiredExtern,
) -> Result<Ts<ResolvedEntry>, JsError> {
    let parts = required_parts_from_js(entries).js_err()?;
    let first = &parts[0];

    for (ext, format) in [
        ("cci", FileType::Cci),
        ("cso", FileType::Ciso),
        ("ciso", FileType::Ciso),
    ] {
        if let Some(resolved) = detect_named_split(first, &parts, ext, format) {
            return Ok(resolved.into_ts()?);
        }
    }

    // Raw XISO has no naming convention or header past part 1, so it's
    // only found by content search.
    //
    // raw_split_outcome_for_entry classifies every part in the batch, so
    // its result may not involve entries[0] at all - only accept it when
    // entries[0] is actually named, otherwise fall through to
    // single-file detection.
    match raw_split_outcome_for_entry(parts.clone())? {
        EntryRawSplitOutcome::Resolved { parts: names, .. }
            if names.iter().any(|n| n == &first.name) =>
        {
            return Ok(ResolvedEntry::Dir {
                format: FileType::Xiso,
                names,
            }
            .into_ts()?);
        }
        EntryRawSplitOutcome::Unresolvable {
            names,
            reason,
            invalid_kind,
        } if names.iter().any(|n| n == &first.name) => {
            return Ok(ResolvedEntry::Invalid {
                names,
                reason,
                invalid_kind,
            }
            .into_ts()?);
        }
        EntryRawSplitOutcome::Resolved { .. }
        | EntryRawSplitOutcome::Unresolvable { .. }
        | EntryRawSplitOutcome::NoCandidates => {}
    }

    let format = detect(first.read_fn.clone(), first.size).js_err()?;
    Ok(ResolvedEntry::File {
        format,
        name: first.name.clone(),
    }
    .into_ts()?)
}
