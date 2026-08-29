use std::cmp::Reverse;
use std::collections::HashMap;

/// Soft backstop on how many continuation fragments a candidate subset
/// may contain - not a correctness limit (real splits with more
/// fragments do happen). The real search-space control is
/// `find_matching_subsets`'s size-sum filter; this only guards the
/// pathological case where that filter can't narrow things down.
const MAX_SPLIT_FRAGMENTS_CEILING: usize = 8;

/// Ceiling on nodes the subset-sum search in `find_matching_subsets`
/// will visit. Each node is pure in-memory arithmetic, so this only
/// bounds the rare adversarial case, not everyday cost.
pub(crate) const MAX_SUBSET_SEARCH_STEPS: usize = 20_000;

/// Ceiling on candidate *orderings* `find_raw_split` will construct and
/// verify. Unlike the subset search, each of these is a real I/O round
/// trip, so this bounds wall-clock cost; in practice the size-sum filter
/// means very few orderings ever reach this stage.
pub(crate) const MAX_ORDERING_CANDIDATES_TRIED: usize = 2000;

/// One continuation fragment as seen by the subset-sum search: its index
/// into the original `continuations` slice, plus its byte size.
#[derive(Clone, Copy)]
pub(crate) struct SizedFragment {
    pub(crate) index: usize,
    pub(crate) size: u64,
}

/// One `k`'s worth of `find_matching_subsets`/`all_matching_subsets`:
/// every index-subset of size exactly `k` whose sizes sum to `target`
/// (`sum == target` if `exact`, else `sum >= target`), sorted tightest
/// fit first (smallest sum). A no-op for an exact match, but matters for
/// the `at least` tier so a same-or-larger bystander doesn't get tried
/// ahead of the genuine, closer-fitting fragment.
fn subsets_at_k(
    fragments: &[SizedFragment],
    sorted: &[SizedFragment],
    suffix_sum: &[u64],
    k: usize,
    target: u64,
    exact: bool,
    budget: &mut usize,
) -> Vec<Vec<usize>> {
    let mut chosen: Vec<usize> = Vec::with_capacity(k); // positions into `sorted`
    let mut found = Vec::new();
    search_subset_sum(
        sorted,
        suffix_sum,
        0,
        k,
        &mut chosen,
        0,
        target,
        exact,
        budget,
        &mut found,
    );
    // found holds original continuations indices, not positions into
    // fragments, so sizes must be looked up by index.
    let size_by_index: HashMap<usize, u64> = fragments.iter().map(|f| (f.index, f.size)).collect();
    found.sort_by_key(|subset| subset.iter().map(|idx| size_by_index[idx]).sum::<u64>());
    found
}

/// Depth-first subset-sum search: finds index-subsets of `fragments`
/// whose sizes sum to `target` (`sum == target` if `exact`, else
/// `sum >= target`), trying subset size `k` from 1 up to
/// `fragments.len().min(MAX_SPLIT_FRAGMENTS_CEILING)`, smallest first.
/// Every match at the first `k` that yields one is returned, tightest-fit
/// first (see `subsets_at_k`).
///
/// Pruned rather than enumerated: fragments are sorted descending by
/// size, so the next `slots_left` items at any point in the recursion
/// are the largest remaining, letting each step bound the best reachable
/// sum and prune whole subtrees.
///
/// `budget` is decremented per node visited and shared across every `k`
/// tried, so a pathological fragment pool still terminates.
#[cfg(test)]
fn find_matching_subsets(
    fragments: &[SizedFragment],
    target: u64,
    exact: bool,
    budget: &mut usize,
) -> Vec<Vec<usize>> {
    let max_k = fragments.len().min(MAX_SPLIT_FRAGMENTS_CEILING);
    let mut sorted = fragments.to_vec();
    sorted.sort_unstable_by_key(|f| Reverse(f.size));

    let mut suffix_sum = vec![0u64; sorted.len() + 1];
    for i in (0..sorted.len()).rev() {
        suffix_sum[i] = suffix_sum[i + 1] + sorted[i].size;
    }

    for k in 1..=max_k {
        let found = subsets_at_k(fragments, &sorted, &suffix_sum, k, target, exact, budget);
        if !found.is_empty() {
            return found; // smallest genuine match wins - never fold a
            // bystander into a larger subset once a smaller one verifies.
        }
        if *budget == 0 {
            break;
        }
    }
    Vec::new()
}

pub(crate) fn all_matching_subsets(
    fragments: &[SizedFragment],
    target: u64,
    exact: bool,
    budget: &mut usize,
) -> Vec<Vec<usize>> {
    let max_k = fragments.len().min(MAX_SPLIT_FRAGMENTS_CEILING);
    let mut sorted = fragments.to_vec();
    sorted.sort_unstable_by_key(|f| Reverse(f.size));

    let mut suffix_sum = vec![0u64; sorted.len() + 1];
    for i in (0..sorted.len()).rev() {
        suffix_sum[i] = suffix_sum[i + 1] + sorted[i].size;
    }

    let mut all = Vec::new();
    for k in 1..=max_k {
        all.extend(subsets_at_k(
            fragments,
            &sorted,
            &suffix_sum,
            k,
            target,
            exact,
            budget,
        ));
        if *budget == 0 {
            break;
        }
    }
    all
}

#[allow(clippy::too_many_arguments)]
fn search_subset_sum(
    sorted: &[SizedFragment],
    suffix_sum: &[u64],
    start: usize,
    slots_left: usize,
    chosen: &mut Vec<usize>,
    running_sum: u64,
    target: u64,
    exact: bool,
    budget: &mut usize,
    found: &mut Vec<Vec<usize>>,
) {
    if *budget == 0 {
        return;
    }
    *budget -= 1;

    if slots_left == 0 {
        let matched = if exact {
            running_sum == target
        } else {
            running_sum >= target
        };
        if matched {
            found.push(chosen.iter().map(|&pos| sorted[pos].index).collect());
        }
        return;
    }
    let remaining = sorted.len() - start;
    if remaining < slots_left {
        return; // not enough fragments left to fill the remaining slots
    }

    let best_case = running_sum + (suffix_sum[start] - suffix_sum[start + slots_left]);
    if best_case < target {
        return; // even the largest remaining items can't reach target
    }
    if exact {
        let worst_case =
            running_sum + (suffix_sum[sorted.len() - slots_left] - suffix_sum[sorted.len()]);
        if worst_case > target {
            return; // even the smallest remaining items overshoot target
        }
    }

    for i in start..=sorted.len() - slots_left {
        chosen.push(i);
        search_subset_sum(
            sorted,
            suffix_sum,
            i + 1,
            slots_left - 1,
            chosen,
            running_sum + sorted[i].size,
            target,
            exact,
            budget,
            found,
        );
        chosen.pop();
        if *budget == 0 {
            return;
        }
    }
}

pub(crate) fn index_permutations(indices: &[usize]) -> Vec<Vec<usize>> {
    if indices.len() <= 1 {
        return vec![indices.to_vec()];
    }
    let mut result = Vec::new();
    for i in 0..indices.len() {
        let mut rest = indices.to_vec();
        let item = rest.remove(i);
        for mut perm in index_permutations(&rest) {
            perm.insert(0, item);
            result.push(perm);
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fragments(sizes: &[u64]) -> Vec<SizedFragment> {
        sizes
            .iter()
            .enumerate()
            .map(|(index, &size)| SizedFragment { index, size })
            .collect()
    }

    fn sorted_matches(mut matches: Vec<Vec<usize>>) -> Vec<Vec<usize>> {
        for m in &mut matches {
            m.sort_unstable();
        }
        matches.sort();
        matches
    }

    #[test]
    fn subset_sum_finds_the_only_exact_two_item_match() {
        // sizes: idx0=10, idx1=3, idx2=5 - only {idx1, idx2} sums to 8.
        let frags = fragments(&[10, 3, 5]);
        let mut budget = MAX_SUBSET_SEARCH_STEPS;
        let found = find_matching_subsets(&frags, 8, true, &mut budget);
        assert_eq!(sorted_matches(found), vec![vec![1, 2]]);
    }

    #[test]
    fn subset_sum_prefers_a_single_exact_match_over_a_larger_alternative() {
        let frags = fragments(&[8, 3, 5]);
        let mut budget = MAX_SUBSET_SEARCH_STEPS;
        let found = find_matching_subsets(&frags, 8, true, &mut budget);
        assert_eq!(sorted_matches(found), vec![vec![0]]);
    }

    #[test]
    fn subset_sum_exact_tier_returns_nothing_when_no_combination_matches() {
        let frags = fragments(&[10, 20, 30]);
        let mut budget = MAX_SUBSET_SEARCH_STEPS;
        let found = find_matching_subsets(&frags, 25, true, &mut budget);
        assert!(found.is_empty());
    }

    #[test]
    fn subset_sum_at_least_tier_accepts_a_covering_superset() {
        // No subset sums to exactly 12, but {idx0, idx1} sums to 15 >= 12.
        let frags = fragments(&[10, 5]);
        let mut budget = MAX_SUBSET_SEARCH_STEPS;
        assert!(find_matching_subsets(&frags, 12, true, &mut budget).is_empty());
        let found = find_matching_subsets(&frags, 12, false, &mut budget);
        assert_eq!(sorted_matches(found), vec![vec![0, 1]]);
    }

    #[test]
    fn subset_sum_respects_an_exhausted_budget_without_panicking() {
        let frags = fragments(&[10, 3, 5]);
        let mut budget = 0usize;
        let found = find_matching_subsets(&frags, 8, true, &mut budget);
        assert!(found.is_empty());
    }

    #[test]
    fn subset_sum_zero_size_fragment_trivially_matches_a_target_of_zero() {
        let frags = fragments(&[0, 4096, 8192]);
        let mut budget = MAX_SUBSET_SEARCH_STEPS;
        let found = find_matching_subsets(&frags, 0, true, &mut budget);
        assert_eq!(sorted_matches(found), vec![vec![0]]);
    }

    #[test]
    fn subset_sum_several_zero_size_bystanders_all_match_a_target_of_zero() {
        let frags = fragments(&[0, 0, 4096]);
        let mut budget = MAX_SUBSET_SEARCH_STEPS;
        let found = find_matching_subsets(&frags, 0, true, &mut budget);
        assert_eq!(sorted_matches(found), vec![vec![0], vec![1]]);
    }

    #[test]
    fn subset_sum_stays_cheap_over_a_batch_sized_pool_of_identical_bystanders() {
        let sizes = vec![0x8000u64; 300];
        let frags = fragments(&sizes);
        let target = 0x8000 * 300 + 1;
        let mut budget = MAX_SUBSET_SEARCH_STEPS;
        let found = find_matching_subsets(&frags, target, true, &mut budget);
        assert!(found.is_empty());
        assert!(
            MAX_SUBSET_SEARCH_STEPS - budget < 100,
            "expected the size-bound prune to reject every k almost immediately, \
             used {} of {} budget steps",
            MAX_SUBSET_SEARCH_STEPS - budget,
            MAX_SUBSET_SEARCH_STEPS
        );
    }

    #[test]
    fn subset_sum_finds_a_genuine_match_at_exactly_the_fragment_count_ceiling() {
        let sizes = vec![1u64; MAX_SPLIT_FRAGMENTS_CEILING];
        let frags = fragments(&sizes);
        let target = MAX_SPLIT_FRAGMENTS_CEILING as u64;
        let mut budget = MAX_SUBSET_SEARCH_STEPS;
        let found = find_matching_subsets(&frags, target, true, &mut budget);
        assert_eq!(
            sorted_matches(found),
            vec![(0..MAX_SPLIT_FRAGMENTS_CEILING).collect::<Vec<_>>()]
        );
    }

    #[test]
    fn subset_sum_does_not_search_past_the_fragment_count_ceiling() {
        let over_ceiling = MAX_SPLIT_FRAGMENTS_CEILING + 1;
        let sizes = vec![1u64; over_ceiling];
        let frags = fragments(&sizes);
        let target = over_ceiling as u64;
        let mut budget = MAX_SUBSET_SEARCH_STEPS;
        let found = find_matching_subsets(&frags, target, true, &mut budget);
        assert!(
            found.is_empty(),
            "expected no match: the only combination that sums to {target} needs all \
             {over_ceiling} fragments, one more than MAX_SPLIT_FRAGMENTS_CEILING allows, \
             got {found:?}"
        );
    }
}
