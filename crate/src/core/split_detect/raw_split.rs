use super::batch::HeaderCandidate;
use super::classify::{Classification, classify_parts};
use super::partition_xiso_candidates;
use super::probe::probe_completeness;
use super::subset_search::{
    MAX_ORDERING_CANDIDATES_TRIED, MAX_SUBSET_SEARCH_STEPS, SizedFragment, all_matching_subsets,
    index_permutations,
};
use super::verify::{
    OrderingProbe, SplitVerifyResult, is_self_contained_known_file, verify_ordering_impl,
};
use crate::core::reader::DEFAULT_SEQ_WINDOW;
use crate::core::source::{self, SourcePart, SourcePartsRequiredExtern, required_parts_from_js};
use crate::formats::xiso::XisoSource;
use crate::utils::JsErrExt;
use serde::Serialize;
use tsify::{Ts, Tsify};
use wasm_bindgen::prelude::*;

/// What `find_raw_split` concluded after searching every candidate ordering.
pub(crate) enum RawSplitOutcome {
    /// One ordering verified: part names (header first), verify detail, and
    /// an `OpenedSource` handle if `build_handle` was set and it parsed.
    Resolved(Vec<String>, SplitVerifyResult, Option<crate::OpenedSource>),
    /// Multiple same-size orderings verified; content alone can't disambiguate.
    Ambiguous(Vec<Vec<String>>),
    /// No ordering verified within budget.
    Unresolved,
}

/// Searches for a genuine raw XDVDFS split among `continuations`, placed after
/// `header`. Subsets are narrowed by exact byte count from the header's
/// directory table, then each size-matching subset is tried in every order.
/// Falls back to sums of *at least* the required count if nothing matches
/// exactly, to tolerate a trailing-padding split tool.
///
/// Keeps searching same-size subsets after a first winner in case a second,
/// differently-named one also verifies (`Ambiguous`); same multiset of names
/// collapses to one `Resolved`.
pub(crate) fn find_raw_split(
    header: &SourcePart,
    continuations: &[SourcePart],
    build_handle: bool,
) -> RawSplitOutcome {
    // Bytes still needed per the header's declared total size.
    let Ok(Some((completeness, _))) = probe_completeness(header) else {
        return RawSplitOutcome::Unresolved;
    };
    let required_total = completeness.root_offset + completeness.max_used_prefix_size;
    let needed = required_total.saturating_sub(header.size);

    // Exclude candidates that are already complete files of a known format.
    let fragments: Vec<SizedFragment> = continuations
        .iter()
        .enumerate()
        .filter(|(_, p)| !is_self_contained_known_file(p))
        .map(|(index, p)| SizedFragment {
            index,
            size: p.size,
        })
        .collect();

    let mut subset_budget = MAX_SUBSET_SEARCH_STEPS;
    let mut subsets = all_matching_subsets(&fragments, needed, true, &mut subset_budget);
    // Fallback: tolerate a split tool that pads the last part.
    subsets.extend(all_matching_subsets(
        &fragments,
        needed,
        false,
        &mut subset_budget,
    ));

    // Kept rather than returned immediately, so a same-size second winner
    // can still turn this into `Ambiguous`.
    let mut winners: Vec<(Vec<String>, Vec<usize>, SplitVerifyResult)> = Vec::new();
    let mut winning_k: Option<usize> = None;

    let mut tried = 0usize;
    'search: for subset in &subsets {
        if winning_k.is_some_and(|k| subset.len() != k) {
            break;
        }
        for ordering in index_permutations(subset) {
            if tried >= MAX_ORDERING_CANDIDATES_TRIED {
                break 'search;
            }
            tried += 1;

            let mut candidate = vec![header.clone()];
            candidate.extend(ordering.iter().map(|&i| continuations[i].clone()));
            let names: Vec<String> = candidate.iter().map(|p| p.name.clone()).collect();
            if let Ok((verify, _probe)) = verify_ordering_impl(candidate, false)
                && verify.ok
            {
                winning_k.get_or_insert(subset.len());
                let mut sorted_names: Vec<String> =
                    names.iter().map(|n| n.to_ascii_lowercase()).collect();
                sorted_names.sort_unstable();
                let already_seen = winners.iter().any(|(existing_names, ..)| {
                    let mut existing_sorted: Vec<String> = existing_names
                        .iter()
                        .map(|n| n.to_ascii_lowercase())
                        .collect();
                    existing_sorted.sort_unstable();
                    existing_sorted == sorted_names
                });
                if !already_seen {
                    winners.push((names, ordering, verify));
                    if winners.len() >= 2 {
                        break 'search;
                    }
                }
            }
        }
    }

    match winners.len() {
        0 => RawSplitOutcome::Unresolved,
        1 => {
            let (names, ordering, verify) = winners.into_iter().next().expect("len checked above");
            let handle = if build_handle {
                let mut candidate = vec![header.clone()];
                candidate.extend(ordering.iter().map(|&i| continuations[i].clone()));
                verify_ordering_impl(candidate, true)
                    .ok()
                    .and_then(|(_, probe)| probe)
                    .and_then(|probe| {
                        build_raw_split_handle(header, continuations, &ordering, probe)
                    })
            } else {
                None
            };
            RawSplitOutcome::Resolved(names, verify, handle)
        }
        _ => RawSplitOutcome::Ambiguous(winners.into_iter().map(|(names, ..)| names).collect()),
    }
}

/// Re-opens the winning ordering as a real `XisoSource`, paired with the
/// directory table/title info `verify_ordering_impl` already walked.
/// `None` if re-opening failed - never a placeholder `JsValue`, since the
/// caller now carries this as `Option<OpenedSource>` end to end.
fn build_raw_split_handle(
    header: &SourcePart,
    continuations: &[SourcePart],
    ordering: &[usize],
    probe: OrderingProbe,
) -> Option<crate::OpenedSource> {
    let mut winning_parts = vec![header.clone()];
    winning_parts.extend(ordering.iter().map(|&i| continuations[i].clone()));
    XisoSource::open_multi_part(winning_parts, DEFAULT_SEQ_WINDOW)
        .ok()
        .map(|source| {
            crate::OpenedSource::from_inner(source::SourceInner::Image {
                source: Box::new(source),
                probed: Some(source::ProbedDirectoryTable {
                    directory_table: probe.directory_table,
                    title_info: probe.title_info,
                }),
            })
        })
}

/// `resolveArbitraryXisoSplit`'s result: winning part names (header first)
/// and its verification detail.
#[derive(Debug, Clone, Serialize, Tsify)]
#[serde(rename_all = "camelCase")]
pub struct RawXisoSplit {
    pub parts: Vec<String>,
    pub verify: SplitVerifyResult,
}

/// `#[serde(transparent)]`: `RawXisoSplit | undefined` on the JS side.
#[derive(Debug, Clone, Serialize, Tsify)]
#[serde(transparent)]
pub struct XisoSplitResolution(pub Option<RawXisoSplit>);

/// Content-verified raw XISO split detection over an arbitrary set of
/// filenames - no naming convention required. Tries groupings/orderings and
/// checks whether the bytes line up, since parts 2+ have no self-describing
/// header.
///
/// Returns `None` unless exactly one entry is a truncated XDVDFS header with
/// at least one headerless continuation fragment to pair it with.
///
/// # Errors
///
/// Returns an error if `entries` isn't a non-empty array of source parts,
/// or if reading from any part fails at the I/O level.
#[wasm_bindgen(js_name = resolveArbitraryXisoSplit)]
pub fn resolve_arbitrary_xiso_split(
    entries: &SourcePartsRequiredExtern,
) -> Result<Ts<XisoSplitResolution>, JsError> {
    let parts = required_parts_from_js(entries).js_err()?;
    Ok(XisoSplitResolution(resolve_arbitrary_xiso_split_over(parts)?).into_ts()?)
}

/// `resolve_arbitrary_xiso_split`'s body, factored out so
/// `resolve_batch_entry` can reuse it without a JS round-trip.
fn resolve_arbitrary_xiso_split_over(
    parts: Vec<SourcePart>,
) -> Result<Option<RawXisoSplit>, JsError> {
    let (xiso_parts, _skipped) = partition_xiso_candidates(parts);
    let Classification {
        headers,
        continuations,
        ..
    } = classify_parts(xiso_parts, Vec::new(), None, false)?;

    let (HeaderCandidate::One(header), false) = (
        HeaderCandidate::from_headers(headers),
        continuations.is_empty(),
    ) else {
        return Ok(None);
    };

    Ok(match find_raw_split(&header, &continuations, false) {
        RawSplitOutcome::Resolved(parts, verify, _handle) => Some(RawXisoSplit { parts, verify }),
        RawSplitOutcome::Ambiguous(_) | RawSplitOutcome::Unresolved => None,
    })
}
