use super::classify::classify_parts;
use super::raw_split::{RawSplitOutcome, find_raw_split};
use super::verify::SplitVerifyResult;
use super::{partition_xiso_candidates, report};
use crate::core::source::{self, SourcePart, SourcePartsRequiredExtern, required_parts_from_js};
use crate::utils::JsErrExt;
use js_sys::Function;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use tsify::{Ts, Tsify};
use wasm_bindgen::prelude::*;

/// One disc of a `MultiDiscSet`.
///
/// Not `Clone`: `handle` is a live, single-owner JS handle, so cloning
/// it should be a deliberate decision, not inherited from a derive.
#[derive(Debug, Serialize, Tsify)]
#[serde(rename_all = "camelCase")]
pub struct DiscInfo {
    pub name: String,
    /// Hex-formatted, uppercase, no "0x" prefix.
    pub media_id: String,
    pub disc_number: u8,
    /// Live, already-opened source for this disc, as an opaque
    /// round-trip token: built once from a real `OpenedSource` and handed
    /// straight back to JS. `JsValue::UNDEFINED` only if re-opening
    /// unexpectedly failed after classification.
    #[serde(with = "serde_wasm_bindgen::preserve")]
    #[tsify(type = "OpenedSource | undefined")]
    pub handle: JsValue,
}

/// One resolved group out of a batch of loose files sitting next to each
/// other.
///
/// Not `Clone`, for the same reason as `DiscInfo`.
#[derive(Debug, Serialize, Tsify)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum BatchResolution {
    /// One independently complete image, not sharing a title/disc-count
    /// with any other file in this batch.
    Standalone {
        names: Vec<String>,
        #[serde(rename = "titleId")]
        title_id: String,
        /// A live, already-probed `OpenedSource` for this exact image -
        /// pass it to `inspect()`/`generateAttachXbe()`/
        /// `openConversionSession()` instead of re-opening from raw
        /// bytes. `JsValue::UNDEFINED` only if re-opening unexpectedly failed.
        #[serde(with = "serde_wasm_bindgen::preserve")]
        #[tsify(type = "OpenedSource | undefined")]
        handle: JsValue,
    },
    /// Two or more independently complete images sharing `titleId` and
    /// `discCount`, with distinct `mediaId`/`discNumber` each.
    MultiDiscSet {
        #[serde(rename = "titleId")]
        title_id: String,
        #[serde(rename = "discCount")]
        disc_count: u8,
        discs: Vec<DiscInfo>,
    },
    /// A raw-split fragment set resolved to one content-verified
    /// ordering. `parts[0]` is the header ("part 1") file.
    RawSplit {
        parts: Vec<String>,
        verify: SplitVerifyResult,
        /// A live `OpenedSource` for the winning ordering, opaque
        /// round-trip token as above. `JsValue::UNDEFINED` if re-opening
        /// failed, or its launch executable didn't parse.
        #[serde(with = "serde_wasm_bindgen::preserve")]
        #[tsify(type = "OpenedSource | undefined")]
        handle: JsValue,
    },
    /// A GOD-shaped folder with nothing in the batch to group it
    /// against. No title/completeness info - not worth the cost unless
    /// the caller actually opens this source.
    GodFolder { names: Vec<String> },
    /// Couldn't confidently place this file (or these files) - see
    /// `reason`.
    Unresolved {
        names: Vec<String>,
        reason: String,
        #[serde(rename = "unresolvedKind")]
        unresolved_kind: UnresolvedKind,
    },
}

/// A specific, JS-actionable reason `BatchResolution::Unresolved`
/// occurred. Not exhaustive - most causes just report `Generic`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Tsify)]
#[serde(rename_all = "camelCase")]
pub enum UnresolvedKind {
    /// Two or more sources claim the same `disc_number` within a
    /// (titleId, discCount) group.
    DuplicateDiscClaim,
    /// Two or more distinct orderings of a raw-split candidate set, or
    /// distinct header-to-continuation pairings, each independently
    /// verified - see `RawSplitOutcome::Ambiguous` /
    /// `try_all_header_orderings`.
    AmbiguousSplit,
    /// A same-`(titleId, discCount)` group has more discs than its own
    /// claimed `discCount`, or a `disc_number` outside `1..=discCount` -
    /// evidence at least one member doesn't belong, even with no
    /// duplicate disc number.
    InvalidDiscCount,
    Generic,
}

/// `#[serde(transparent)]`: plain array on the JS side, not a wrapper.
/// Not `Clone`: wraps `Vec<BatchResolution>`, which isn't - see
/// `BatchResolution`'s doc comment.
#[derive(Debug, Serialize, Tsify)]
#[serde(transparent)]
pub struct BatchResolutions(pub Vec<BatchResolution>);

/// How many truncated-header candidates a fragment set contains.
pub(crate) enum HeaderCandidate {
    /// No truncated header found - nothing to reassemble. Includes the
    /// case where every part is independently complete (never fragments
    /// of one another).
    None,
    /// Exactly one - the only case a split can be resolved from.
    One(SourcePart),
    /// More than one - which fragment set belongs to which header is
    /// ambiguous.
    Ambiguous(Vec<SourcePart>),
}

impl HeaderCandidate {
    pub(crate) fn from_headers(headers: Vec<SourcePart>) -> Self {
        match <[SourcePart; 1]>::try_from(headers) {
            Ok([header]) => Self::One(header),
            Err(headers) if headers.is_empty() => Self::None,
            Err(headers) => Self::Ambiguous(headers),
        }
    }
}

/// Resolves a single header against `continuations`. Factored out so both
/// the single-header path and the multi-header search below can share it.
fn resolve_one_header(
    header: SourcePart,
    continuations: &[SourcePart],
    build_handles: bool,
) -> (
    Option<BatchResolution>,
    Vec<SourcePart>, /* claimed */
    Option<BatchResolution>,
) {
    if continuations.is_empty() {
        return (
            None,
            Vec::new(),
            Some(BatchResolution::Unresolved {
                names: vec![header.name],
                reason: "truncated XDVDFS header but no continuation fragments found in this \
                         batch"
                    .to_owned(),
                unresolved_kind: UnresolvedKind::Generic,
            }),
        );
    }
    match find_raw_split(&header, continuations, build_handles) {
        RawSplitOutcome::Resolved(parts, verify, handle) => {
            // Two continuations can share a name, so track remaining
            // uses per name rather than a plain set.
            let mut remaining_uses: HashMap<String, usize> = HashMap::new();
            for name in parts.iter().skip(1) {
                *remaining_uses.entry(name.clone()).or_insert(0) += 1;
            }
            let claimed: Vec<SourcePart> = continuations
                .iter()
                .filter(|c| match remaining_uses.get_mut(c.name.as_str()) {
                    Some(count) if *count > 0 => {
                        *count -= 1;
                        true
                    }
                    _ => false,
                })
                .cloned()
                .collect();
            (
                Some(BatchResolution::RawSplit {
                    parts,
                    verify,
                    handle: handle.map_or(JsValue::UNDEFINED, Into::into),
                }),
                claimed,
                None,
            )
        }
        RawSplitOutcome::Ambiguous(orderings) => (
            None,
            Vec::new(),
            Some(BatchResolution::Unresolved {
                names: std::iter::once(header.name.clone())
                    .chain(continuations.iter().map(|p| p.name.clone()))
                    .collect(),
                reason: format!(
                    "{} different orderings of these parts each independently verified as \
                     a valid split - which is genuine can't be determined from content \
                     alone; resolve manually",
                    orderings.len()
                ),
                unresolved_kind: UnresolvedKind::AmbiguousSplit,
            }),
        ),
        RawSplitOutcome::Unresolved => (
            None,
            Vec::new(),
            Some(BatchResolution::Unresolved {
                names: std::iter::once(header.name.clone())
                    .chain(continuations.iter().map(|p| p.name.clone()))
                    .collect(),
                reason: "no ordering of these parts verified as a valid split".to_owned(),
                unresolved_kind: UnresolvedKind::Generic,
            }),
        ),
    }
}

/// Every entry in `continuations` not accounted for in `claimed`, one
/// name per instance (names can repeat, so this counts remaining uses
/// rather than testing set membership).
fn unclaimed_leftovers(
    continuations: &[SourcePart],
    claimed: &[SourcePart],
) -> Vec<BatchResolution> {
    let mut remaining_uses: HashMap<String, usize> = HashMap::new();
    for name in claimed.iter().map(|p| &p.name) {
        *remaining_uses.entry(name.clone()).or_insert(0) += 1;
    }
    continuations
        .iter()
        .filter(|c| match remaining_uses.get_mut(c.name.as_str()) {
            Some(count) if *count > 0 => {
                *count -= 1;
                false
            }
            _ => true,
        })
        .map(|leftover| BatchResolution::Unresolved {
            names: vec![leftover.name.clone()],
            reason: "did not fit into the raw XISO split verified elsewhere in this batch"
                .to_owned(),
            unresolved_kind: UnresolvedKind::Generic,
        })
        .collect()
}

/// Cap on how many header candidates get a full permutation search - `n!`
/// orderings are tried, bounding worst-case work to a small constant
/// (6! = 720). Realistic batches have 1-3; above the cap, callers fall
/// back to a single greedy pass in name-sorted order.
const MAX_PERMUTED_HEADERS: usize = 6;

/// All permutations of `0..n`, order arbitrary. Only called with
/// `n <= MAX_PERMUTED_HEADERS`, so an eager `Vec<Vec<usize>>` is fine.
fn all_permutations(n: usize) -> Vec<Vec<usize>> {
    fn permute(current: &mut Vec<usize>, remaining: &mut Vec<usize>, out: &mut Vec<Vec<usize>>) {
        if remaining.is_empty() {
            out.push(current.clone());
            return;
        }
        for i in 0..remaining.len() {
            let val = remaining.remove(i);
            current.push(val);
            permute(current, remaining, out);
            current.pop();
            remaining.insert(i, val);
        }
    }
    let mut out = Vec::new();
    permute(&mut Vec::new(), &mut (0..n).collect(), &mut out);
    out
}

/// Tries every ordering of `headers` against `continuations`, looking for
/// one where a single greedy pass (`resolve_one_header`, against a
/// shrinking pool) resolves *every* header. A fixed-order pass can strand
/// a header a different order would have resolved: header A verifies
/// against continuation set X or Y, header B only against Y - order
/// `[A, B]` lets A claim Y first and strands B, `[B, A]` resolves both.
///
/// Returns `None` (caller falls back to one greedy pass, sorted by name,
/// for its per-header diagnostic) when `headers.len()` exceeds
/// `MAX_PERMUTED_HEADERS`, or no ordering resolves every header.
///
/// When two or more orderings each resolve every header but disagree on
/// the pairing, that's a genuine assignment-level ambiguity in the
/// content, reported as its own `Unresolved`. In practice this only
/// arises from literal duplicate content, since verification checks real
/// magic bytes at real offsets, not just size.
fn try_all_header_orderings(
    headers: &[SourcePart],
    continuations: &[SourcePart],
    build_handles: bool,
) -> Option<Vec<BatchResolution>> {
    if headers.is_empty() || headers.len() > MAX_PERMUTED_HEADERS {
        return None;
    }

    // (fingerprint, results) per distinct fully-resolving ordering.
    // Fingerprint = sorted winning `parts` lists, so orderings landing on
    // the same pairing collapse into one instead of a false ambiguity.
    let mut solutions: Vec<(Vec<Vec<String>>, Vec<BatchResolution>)> = Vec::new();

    for perm in all_permutations(headers.len()) {
        let mut pool: Vec<SourcePart> = continuations.to_vec();
        let mut claimed_all: Vec<SourcePart> = Vec::new();
        let mut results = Vec::with_capacity(headers.len());
        let mut all_resolved = true;

        for &i in &perm {
            let (resolved, claimed, _unresolved) =
                resolve_one_header(headers[i].clone(), &pool, build_handles);
            let Some(resolution) = resolved else {
                all_resolved = false;
                break;
            };
            for c in &claimed {
                if let Some(pos) = pool.iter().position(|p| p.name == c.name) {
                    pool.remove(pos);
                }
            }
            claimed_all.extend(claimed);
            results.push(resolution);
        }

        if !all_resolved {
            continue;
        }

        // Whatever's left in the pool after every header claimed its
        // share is genuine leftover for this ordering.
        results.extend(unclaimed_leftovers(continuations, &claimed_all));

        let mut fingerprint: Vec<Vec<String>> = results
            .iter()
            .filter_map(|r| match r {
                BatchResolution::RawSplit { parts, .. } => Some(parts.clone()),
                _ => None,
            })
            .collect();
        fingerprint.sort();

        if !solutions.iter().any(|(fp, _)| fp == &fingerprint) {
            solutions.push((fingerprint, results));
        }
    }

    match solutions.len() {
        0 => None,
        1 => solutions.into_iter().next().map(|(_, results)| results),
        _ => Some(vec![BatchResolution::Unresolved {
            names: headers
                .iter()
                .map(|h| h.name.clone())
                .chain(continuations.iter().map(|c| c.name.clone()))
                .collect(),
            reason: format!(
                "{} different header-to-continuation pairings each independently resolve \
                 every header in this batch - which pairing is genuine can't be determined \
                 from content alone; resolve manually",
                solutions.len()
            ),
            unresolved_kind: UnresolvedKind::AmbiguousSplit,
        }]),
    }
}

/// Classifies the header/continuation fragments left after complete
/// images have been pulled out, into the `BatchResolution`s they
/// deserve. Returns an empty `Vec` when there's nothing left to report.
/// A batch can legitimately contain more than one independent raw split
/// at once; ordering/fallback behavior is `try_all_header_orderings`'s
/// (see its doc comment).
pub(crate) fn resolve_raw_split_bucket(
    headers: Vec<SourcePart>,
    continuations: &[SourcePart],
    build_handles: bool,
) -> Vec<BatchResolution> {
    match HeaderCandidate::from_headers(headers) {
        HeaderCandidate::None if continuations.is_empty() => Vec::new(),
        HeaderCandidate::None => vec![BatchResolution::Unresolved {
            names: continuations.iter().map(|p| p.name.clone()).collect(),
            reason: "fragment(s) with no matching XDVDFS header found in this batch".to_owned(),
            unresolved_kind: UnresolvedKind::Generic,
        }],
        // Exactly one header: resolve directly against the full pool -
        // cheaper than the general multi-header search for no benefit.
        HeaderCandidate::One(header) => {
            let (resolved, claimed, unresolved) =
                resolve_one_header(header, continuations, build_handles);
            let mut results = Vec::new();
            if let Some(resolution) = resolved {
                results.push(resolution);
                // Only a *resolved* split can leave genuine leftovers.
                // A failed resolution's `Unresolved` entry already
                // embeds every continuation's name, so reporting them
                // again here would double-count.
                results.extend(unclaimed_leftovers(continuations, &claimed));
            } else if let Some(resolution) = unresolved {
                results.push(resolution);
            }
            results
        }
        // Two or more headers: search for a processing order that
        // resolves every one, falling back to a greedy pass otherwise.
        HeaderCandidate::Ambiguous(mut headers) => {
            if let Some(results) = try_all_header_orderings(&headers, continuations, build_handles)
            {
                return results;
            }

            // Sorted by name so the fallback is at least deterministic.
            headers.sort_by(|a, b| a.name.cmp(&b.name));

            let mut pool: Vec<SourcePart> = continuations.to_vec();
            let mut results = Vec::new();
            let mut failed_headers = Vec::new();
            for header in headers {
                let (resolved, claimed, unresolved) =
                    resolve_one_header(header, &pool, build_handles);
                if let Some(resolution) = resolved {
                    results.push(resolution);
                    // Remove exactly the claimed instances, one-for-one
                    // by name - an unclaimed duplicate stays available.
                    for c in &claimed {
                        if let Some(pos) = pool.iter().position(|p| p.name == c.name) {
                            pool.remove(pos);
                        }
                    }
                } else if let Some(BatchResolution::Unresolved { names, .. }) = unresolved {
                    // The header itself is always names[0].
                    failed_headers.push(names.into_iter().next().expect("header name present"));
                }
            }
            if failed_headers.is_empty() {
                // Every header resolved cleanly - whatever's left is
                // unrelated noise, not batch-level ambiguity.
                results.extend(pool.iter().map(|leftover| {
                    BatchResolution::Unresolved {
                        names: vec![leftover.name.clone()],
                        reason: "did not fit into the raw XISO split verified elsewhere in this \
                             batch"
                            .to_owned(),
                        unresolved_kind: UnresolvedKind::Generic,
                    }
                }));
            } else {
                // At least one header couldn't be placed even with every
                // other header's claims subtracted - that plus the pool
                // remainder is the real ambiguous residue.
                results.push(BatchResolution::Unresolved {
                    names: failed_headers
                        .into_iter()
                        .chain(pool.iter().map(|p| p.name.clone()))
                        .collect(),
                    reason: "multiple ambiguous truncated-header candidates in this batch - \
                             resolve manually"
                        .to_owned(),
                    unresolved_kind: UnresolvedKind::Generic,
                });
            }
            results
        }
    }
}

/// Classifies every file in a batch dir in one pass: independently
/// complete images (grouped into a `MultiDiscSet` when several share
/// `titleId`+`discCount`), fragments of a raw XISO/ISO split, and
/// anything that couldn't be confidently placed. Non-XISO-magic entries
/// are reported back as `Unresolved` rather than silently dropped.
///
/// # Errors
///
/// Returns an error if `entries` is empty or if reading from any part
/// fails at the I/O level. A file that doesn't resolve is reported as
/// `Unresolved`, not an `Err`. `on_item`, when given, is called
/// synchronously as soon as each result not needing whole-batch
/// correlation is known.
// wasm-bindgen has no OptionFromWasmAbi for Option<&Function>, only
// owned Option<Function> - converted to Option<&Function> below.
#[allow(clippy::needless_pass_by_value)]
#[wasm_bindgen(js_name = resolveBatch)]
pub fn resolve_batch(
    entries: &SourcePartsRequiredExtern,
    on_item: Option<Function>,
) -> Result<Ts<BatchResolutions>, JsError> {
    let on_item = on_item.as_ref();
    let parts = required_parts_from_js(entries).js_err()?;
    let (god_candidates, remaining) = source::partition_god_candidates(parts);
    let (xiso_parts, skipped) = partition_xiso_candidates(remaining);

    for resolution in &skipped {
        report(on_item, resolution);
    }

    // `resolveBatch` is the only caller that hands live handles back to
    // JS - other callers pass `false` and skip the extra opens.
    let classification = classify_parts(xiso_parts, god_candidates, on_item, true)?;
    let already_reported: HashSet<String> =
        classification.already_reported.iter().cloned().collect();
    let mut results = classification.into_resolutions(true);

    if on_item.is_none() {
        results.extend(skipped);
    } else if !already_reported.is_empty() {
        results.retain(|r| {
            !matches!(
                r,
                BatchResolution::Standalone { names, .. }
                    if names.iter().all(|n| already_reported.contains(n))
            )
        });
    }

    Ok(BatchResolutions(results).into_ts()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet as StdHashSet;

    #[test]
    fn permutation_count_and_uniqueness_for_small_n() {
        assert_eq!(all_permutations(0), vec![Vec::<usize>::new()]);
        assert_eq!(all_permutations(1), vec![vec![0]]);

        for n in [2, 3, 4] {
            let perms = all_permutations(n);
            assert_eq!(
                perms.len(),
                (1..=n).product::<usize>(),
                "expected n! permutations for n={n}"
            );

            let unique: StdHashSet<Vec<usize>> = perms.iter().cloned().collect();
            assert_eq!(
                unique.len(),
                perms.len(),
                "expected no duplicate orderings for n={n}"
            );

            for p in &perms {
                let mut sorted = p.clone();
                sorted.sort_unstable();
                assert_eq!(
                    sorted,
                    (0..n).collect::<Vec<_>>(),
                    "each permutation must be a rearrangement of 0..n"
                );
            }
        }
    }

    #[test]
    fn max_permuted_headers_cap_keeps_permutation_count_small() {
        let perms = all_permutations(MAX_PERMUTED_HEADERS);
        assert_eq!(perms.len(), 720);
    }
}
