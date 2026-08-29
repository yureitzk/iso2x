use super::batch::{BatchResolution, DiscInfo, UnresolvedKind, resolve_raw_split_bucket};
use super::probe::probe_completeness;
use super::report;
use crate::core::iso;
use crate::core::reader::DEFAULT_SEQ_WINDOW;
use crate::core::source::{self, GodCandidate, SourcePart};
use crate::core::title::TitleInfo;
use crate::formats::god::GodSource;
use crate::formats::xiso::XisoSource;
use crate::utils::JsErrExt;
use anyhow::Context;
use js_sys::Function;
use std::collections::{HashMap, HashSet};
use wasm_bindgen::prelude::*;

/// The raw container an independently complete image was found in. Folded
/// into `resolve_complete_images`'s grouping key alongside `(titleId,
/// discCount)`, since a real multi-disc release never mixes raw images and
/// GOD folders in one set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum ContainerShape {
    /// A standalone file (raw XISO magic).
    File,
    /// A GOD-shaped folder.
    God,
}

/// One independently complete image found in a batch, format-erased down
/// to what `resolve_complete_images` needs.
struct CompleteImage {
    /// One name for a standalone file; every `Data####` part's name for a
    /// GOD folder.
    names: Vec<String>,
    title_info: TitleInfo,
    shape: ContainerShape,
    /// Already-opened, ready to hand to JS as a live `OpenedSource` handle.
    /// `None` if re-opening failed, or already reported via `on_item`.
    source_handle: Option<source::SourceInner>,
}

/// GOD counterpart to `probe_completeness`: opens `candidate` as a
/// `GodSource` and extracts `TitleInfo`. `Ok(None)` means the naming
/// matched but the content didn't (gap in the sequence, open error, or an
/// unparseable directory table).
fn verify_god_candidate(
    candidate: GodCandidate,
    build_handle: bool,
) -> Result<Option<CompleteImage>, anyhow::Error> {
    if !source::god_candidate_is_contiguous(&candidate) {
        return Ok(None);
    }
    let data_dir = candidate.data_dir.clone();
    let names: Vec<String> = candidate
        .parts
        .iter()
        .map(|(_, p)| p.name.clone())
        .collect();
    let parts: Vec<SourcePart> = candidate.parts.into_iter().map(|(_, p)| p).collect();

    let Ok(mut god_source) = GodSource::open(parts, DEFAULT_SEQ_WINDOW, None) else {
        return Ok(None);
    };
    let reader = source::SourceReader::new(&mut god_source);
    let Ok(mut iso) = iso::probe_source_over(reader) else {
        return Ok(None);
    };
    match TitleInfo::from_image(&mut iso) {
        Ok(title_info) => {
            // Drop `iso` (which borrows `god_source`) after cloning the
            // directory table, so `god_source` can move into the handle.
            let source_handle = if build_handle {
                let directory_table = iso.directory_table.clone();
                drop(iso);
                Some(source::SourceInner::Image {
                    source: Box::new(god_source),
                    probed: Some(source::ProbedDirectoryTable {
                        directory_table,
                        title_info: title_info.clone(),
                    }),
                })
            } else {
                None
            };
            Ok(Some(CompleteImage {
                names,
                title_info,
                shape: ContainerShape::God,
                source_handle,
            }))
        }
        // Valid volume, no launch executable - report rather than drop.
        Err(e) => Err(e).context(format!(
            "\"{data_dir}\": valid XDVDFS image but no launch executable ({})",
            names.join(", ")
        )),
    }
}

/// Result of the cost gate `classify_god_candidates` applies.
enum GodClassification {
    /// Zero or one GOD-shaped candidate - reported as-is, unverified.
    Passthrough(Option<Vec<String>>),
    /// Two or more candidates - grouping is possible, so verification cost
    /// is justified.
    Verified {
        complete: Vec<CompleteImage>,
        /// (names, reason) for GOD-named candidates that failed to verify.
        invalid: Vec<(Vec<String>, String)>,
    },
}

/// Gates GOD verification on candidate count, before any I/O: a lone GOD
/// folder with nothing to group against skips the full parse.
fn classify_god_candidates(
    god_candidates: Vec<GodCandidate>,
    build_handles: bool,
) -> GodClassification {
    if god_candidates.len() <= 1 {
        let names = god_candidates
            .into_iter()
            .next()
            .map(|c| c.parts.into_iter().map(|(_, p)| p.name).collect());
        return GodClassification::Passthrough(names);
    }
    let mut complete = Vec::new();
    let mut invalid = Vec::new();
    for candidate in god_candidates {
        let names: Vec<String> = candidate
            .parts
            .iter()
            .map(|(_, p)| p.name.clone())
            .collect();
        match verify_god_candidate(candidate, build_handles) {
            Ok(Some(image)) => complete.push(image),
            Ok(None) => invalid.push((
                names,
                "GOD folder has gaps, wrong ordering, or an invalid layout".to_owned(),
            )),
            Err(e) => invalid.push((names, format!("{e:#}"))),
        }
    }
    GodClassification::Verified { complete, invalid }
}

/// Every part in a batch, sorted into buckets. `complete` is format-erased
/// so grouping never distinguishes a raw ISO from a GOD folder.
pub(crate) struct Classification {
    complete: Vec<CompleteImage>,
    /// Recognizable XDVDFS header missing bytes its own directory table
    /// references - a truncated "part 1" candidate.
    pub(crate) headers: Vec<SourcePart>,
    /// Doesn't parse as XDVDFS at all: a headerless continuation fragment,
    /// or not an XISO.
    pub(crate) continuations: Vec<SourcePart>,
    unresolved: Vec<(Vec<String>, String)>,
    /// A single GOD-shaped folder with nothing to group against.
    god_passthrough: Option<Vec<String>>,
    /// Names already pushed through `on_item` mid-loop, so `resolve_batch`
    /// can filter them out of the final result.
    pub(crate) already_reported: Vec<String>,
}

/// `god_candidates` must already be partitioned out of `parts` by the
/// caller, before the XISO-magic filter runs, or a `Data0000` fragment
/// would route straight to `Unresolved`.
pub(crate) fn classify_parts(
    parts: Vec<SourcePart>,
    god_candidates: Vec<GodCandidate>,
    on_item: Option<&Function>,
    build_handles: bool,
) -> Result<Classification, JsError> {
    let mut classification = Classification {
        complete: Vec::new(),
        headers: Vec::new(),
        continuations: Vec::new(),
        unresolved: Vec::new(),
        god_passthrough: None,
        already_reported: Vec::new(),
    };

    match classify_god_candidates(god_candidates, build_handles) {
        GodClassification::Passthrough(names) => classification.god_passthrough = names,
        GodClassification::Verified { complete, invalid } => {
            classification.complete.extend(complete);
            classification.unresolved.extend(invalid);
        }
    }

    for part in parts {
        match probe_completeness(&part).js_err()? {
            Some((c, mut detected)) if c.is_complete => {
                match TitleInfo::from_image(&mut detected) {
                    Ok(title_info) => {
                        let disc_count = title_info.execution_info.disc_count;
                        let source_handle = if build_handles {
                            let directory_table = detected.directory_table.clone();
                            // Cheap: a root-offset probe, not another directory-table walk.
                            XisoSource::open_multi_part(vec![part.clone()], DEFAULT_SEQ_WINDOW)
                                .ok()
                                .map(|source| source::SourceInner::Image {
                                    source: Box::new(source),
                                    probed: Some(source::ProbedDirectoryTable {
                                        directory_table,
                                        title_info: title_info.clone(),
                                    }),
                                })
                        } else {
                            None
                        };
                        // A single-disc title can never join a MultiDiscSet, so report
                        // it immediately rather than carrying it forward.
                        if disc_count <= 1 && on_item.is_some() {
                            let handle = source_handle.map_or(JsValue::UNDEFINED, |inner| {
                                crate::OpenedSource::from_inner(inner).into()
                            });
                            let resolution = BatchResolution::Standalone {
                                names: vec![part.name.clone()],
                                title_id: format!("{:08X}", title_info.execution_info.title_id),
                                handle,
                            };
                            report(on_item, &resolution);
                            classification.already_reported.push(part.name.clone());
                            classification.complete.push(CompleteImage {
                                names: vec![part.name.clone()],
                                title_info,
                                shape: ContainerShape::File,
                                source_handle: None,
                            });
                        } else {
                            classification.complete.push(CompleteImage {
                                names: vec![part.name.clone()],
                                title_info,
                                shape: ContainerShape::File,
                                source_handle,
                            });
                        }
                    }
                    Err(e) => classification.unresolved.push((
                        vec![part.name.clone()],
                        format!("valid XDVDFS image but no launch executable: {e:#}"),
                    )),
                }
            }
            Some(_) => classification.headers.push(part),
            None => classification.continuations.push(part),
        }
    }
    Ok(classification)
}

impl Classification {
    /// Turns a batch's classification into caller-facing results.
    /// `resolve_complete_images` handles multi-disc/packaging;
    /// `resolve_raw_split_bucket` handles physical image structure.
    pub(crate) fn into_resolutions(self, build_handles: bool) -> Vec<BatchResolution> {
        let mut results = resolve_complete_images(self.complete);
        results.extend(self.unresolved.into_iter().map(|(names, reason)| {
            BatchResolution::Unresolved {
                names,
                reason,
                unresolved_kind: UnresolvedKind::Generic,
            }
        }));
        if let Some(names) = self.god_passthrough {
            results.push(BatchResolution::GodFolder { names });
        }
        results.extend(resolve_raw_split_bucket(
            self.headers,
            &self.continuations,
            build_handles,
        ));
        results
    }
}

/// Groups images sharing `(titleId, discCount, shape)` into a
/// `MultiDiscSet`; everything else resolves as `Standalone`.
fn resolve_complete_images(complete: Vec<CompleteImage>) -> Vec<BatchResolution> {
    let mut results = Vec::new();
    let mut groups: HashMap<(u32, u8, ContainerShape), Vec<CompleteImage>> = HashMap::new();
    for image in complete {
        let key = (
            image.title_info.execution_info.title_id,
            image.title_info.execution_info.disc_count,
            image.shape,
        );
        groups.entry(key).or_default().push(image);
    }
    for ((title_id, disc_count, _shape), group) in groups {
        if disc_count > 1 && group.len() > 1 {
            // Two candidates claiming the same disc_number are duplicates, not siblings.
            let mut seen_disc_numbers = HashSet::new();
            let has_duplicate_disc_number = group.iter().any(|image| {
                !seen_disc_numbers.insert(image.title_info.execution_info.disc_number)
            });
            if has_duplicate_disc_number {
                results.extend(group.into_iter().map(|image| BatchResolution::Unresolved {
                    reason: format!(
                        "multiple sources claim disc {} of a {disc_count}-disc {title_id:08X} set",
                        image.title_info.execution_info.disc_number
                    ),
                    names: image.names,
                    unresolved_kind: UnresolvedKind::DuplicateDiscClaim,
                }));
                continue;
            }
            // No duplicate, but the group can still be bogus: too many
            // members, or a disc_number outside 1..=disc_count.
            let disc_count_usize = usize::from(disc_count);
            let has_out_of_range_disc_number = group.iter().any(|image| {
                let n = image.title_info.execution_info.disc_number;
                n == 0 || usize::from(n) > disc_count_usize
            });
            if group.len() > disc_count_usize || has_out_of_range_disc_number {
                let group_len = group.len();
                results.extend(group.into_iter().map(|image| BatchResolution::Unresolved {
                    reason: format!(
                        "{group_len} sources claim membership in a {disc_count}-disc \
                         {title_id:08X} set, with disc numbers outside 1..={disc_count} or \
                         exceeding its own disc_count"
                    ),
                    names: image.names,
                    unresolved_kind: UnresolvedKind::InvalidDiscCount,
                }));
                continue;
            }
            results.push(BatchResolution::MultiDiscSet {
                title_id: format!("{title_id:08X}"),
                disc_count,
                discs: group
                    .into_iter()
                    .map(|image| DiscInfo {
                        name: image.names[0].clone(),
                        media_id: format!("{:08X}", image.title_info.execution_info.media_id),
                        disc_number: image.title_info.execution_info.disc_number,
                        handle: image.source_handle.map_or(JsValue::UNDEFINED, |inner| {
                            crate::OpenedSource::from_inner(inner).into()
                        }),
                    })
                    .collect(),
            });
        } else {
            results.extend(group.into_iter().map(|image| BatchResolution::Standalone {
                title_id: format!("{:08X}", image.title_info.execution_info.title_id),
                handle: image.source_handle.map_or(JsValue::UNDEFINED, |inner| {
                    crate::OpenedSource::from_inner(inner).into()
                }),
                names: image.names,
            }));
        }
    }
    results
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::executable::TitleExecutionInfo;
    use crate::core::title::ContentType;

    fn title_info(title_id: u32, disc_number: u8, disc_count: u8, media_id: u32) -> TitleInfo {
        TitleInfo {
            content_type: ContentType::GamesOnDemand,
            execution_info: TitleExecutionInfo {
                media_id,
                version: 0,
                base_version: 0,
                title_id,
                platform: 0,
                executable_type: 0,
                disc_number,
                disc_count,
                save_game_id: 0,
            },
        }
    }

    #[test]
    fn multi_disc_disc_info_name_is_first_part_only_not_every_part_joined() {
        let disc1 = CompleteImage {
            names: vec![
                "Set/Disc1.data/Data0000".to_owned(),
                "Set/Disc1.data/Data0001".to_owned(),
                "Set/Disc1.data/Data0002".to_owned(),
            ],
            title_info: title_info(0x4744_0134, 1, 2, 0x1),
            shape: ContainerShape::God,
            source_handle: None,
        };
        let disc2 = CompleteImage {
            names: vec!["Set/Disc2.data/Data0000".to_owned()],
            title_info: title_info(0x4744_0134, 2, 2, 0x2),
            shape: ContainerShape::God,
            source_handle: None,
        };

        let results = resolve_complete_images(vec![disc1, disc2]);
        assert_eq!(results.len(), 1);
        let BatchResolution::MultiDiscSet { discs, .. } = &results[0] else {
            panic!("expected a MultiDiscSet, got {results:?}");
        };
        let disc1_info = discs.iter().find(|d| d.disc_number == 1).unwrap();
        assert_eq!(disc1_info.name, "Set/Disc1.data/Data0000");
    }

    #[test]
    fn duplicate_disc_number_never_forms_a_multi_disc_set() {
        let disc1a = CompleteImage {
            names: vec!["Set/DiscA.iso".to_owned()],
            title_info: title_info(0x4744_0134, 1, 2, 0x1),
            shape: ContainerShape::File,
            source_handle: None,
        };
        let disc1b = CompleteImage {
            names: vec!["Set/DiscB.iso".to_owned()],
            title_info: title_info(0x4744_0134, 1, 2, 0x2),
            shape: ContainerShape::File,
            source_handle: None,
        };

        let results = resolve_complete_images(vec![disc1a, disc1b]);

        assert_eq!(results.len(), 2);
        assert!(
            !results
                .iter()
                .any(|r| matches!(r, BatchResolution::MultiDiscSet { .. })),
            "expected no MultiDiscSet from a duplicate disc_number, got {results:?}"
        );
        for result in &results {
            let BatchResolution::Unresolved {
                names,
                reason,
                unresolved_kind,
            } = result
            else {
                panic!("expected Unresolved, got {result:?}");
            };
            assert_eq!(names.len(), 1);
            assert!(reason.contains("disc 1"));
            assert!(reason.contains("47440134"));
            assert!(matches!(
                unresolved_kind,
                UnresolvedKind::DuplicateDiscClaim
            ));
        }
    }

    #[test]
    fn cross_shape_disc_number_collision_does_not_poison_a_genuine_same_shape_set() {
        let god_disc1 = CompleteImage {
            names: vec![
                "Set/Game (GoD)/Data0000".to_owned(),
                "Set/Game (GoD)/Data0001".to_owned(),
            ],
            title_info: title_info(0x4B4E_0809, 1, 2, 0x1),
            shape: ContainerShape::God,
            source_handle: None,
        };
        let iso_disc1 = CompleteImage {
            names: vec!["Set/Game (Disc 1).iso".to_owned()],
            title_info: title_info(0x4B4E_0809, 1, 2, 0x1),
            shape: ContainerShape::File,
            source_handle: None,
        };
        let iso_disc2 = CompleteImage {
            names: vec!["Set/Game (Disc 2).iso".to_owned()],
            title_info: title_info(0x4B4E_0809, 2, 2, 0x2),
            shape: ContainerShape::File,
            source_handle: None,
        };

        let results = resolve_complete_images(vec![god_disc1, iso_disc1, iso_disc2]);

        assert!(
            !results
                .iter()
                .any(|r| matches!(r, BatchResolution::Unresolved { .. })),
            "expected no Unresolved/DuplicateDiscClaim result, got {results:?}"
        );

        let multi_disc_sets: Vec<_> = results
            .iter()
            .filter(|r| matches!(r, BatchResolution::MultiDiscSet { .. }))
            .collect();
        assert_eq!(
            multi_disc_sets.len(),
            1,
            "expected exactly one MultiDiscSet (the two same-shape ISO discs), got {results:?}"
        );
        let BatchResolution::MultiDiscSet { discs, .. } = multi_disc_sets[0] else {
            unreachable!()
        };
        assert_eq!(discs.len(), 2);

        let standalones: Vec<_> = results
            .iter()
            .filter(|r| matches!(r, BatchResolution::Standalone { .. }))
            .collect();
        assert_eq!(
            standalones.len(),
            1,
            "expected the unpaired GOD folder to resolve as its own Standalone, got {results:?}"
        );
        let BatchResolution::Standalone { names, .. } = standalones[0] else {
            unreachable!()
        };
        assert_eq!(
            names,
            &vec![
                "Set/Game (GoD)/Data0000".to_owned(),
                "Set/Game (GoD)/Data0001".to_owned(),
            ]
        );
    }

    #[test]
    fn three_distinct_disc_numbers_exceeding_the_claimed_disc_count_is_not_silently_accepted() {
        let disc1 = CompleteImage {
            names: vec!["Set/Disc1.iso".to_owned()],
            title_info: title_info(0x4E4F_0201, 1, 2, 0x1),
            shape: ContainerShape::File,
            source_handle: None,
        };
        let disc2 = CompleteImage {
            names: vec!["Set/Disc2.iso".to_owned()],
            title_info: title_info(0x4E4F_0201, 2, 2, 0x2),
            shape: ContainerShape::File,
            source_handle: None,
        };
        // disc_count: 2, disc_number 3 - out of range, not a duplicate.
        let disc3 = CompleteImage {
            names: vec!["Set/Disc3.iso".to_owned()],
            title_info: title_info(0x4E4F_0201, 3, 2, 0x3),
            shape: ContainerShape::File,
            source_handle: None,
        };

        let results = resolve_complete_images(vec![disc1, disc2, disc3]);

        let multi_disc_summary: Vec<(u8, usize)> = results
            .iter()
            .filter_map(|r| match r {
                BatchResolution::MultiDiscSet {
                    disc_count, discs, ..
                } => Some((*disc_count, discs.len())),
                _ => None,
            })
            .collect();
        assert!(
            multi_disc_summary
                .iter()
                .all(|&(disc_count, disc_len)| disc_len <= usize::from(disc_count)),
            "expected no MultiDiscSet with more discs than its own claimed disc_count, \
             got (disc_count, discs.len()) pairs: {multi_disc_summary:?}"
        );

        let unresolved_kinds: Vec<_> = results
            .iter()
            .filter_map(|r| match r {
                BatchResolution::Unresolved {
                    unresolved_kind, ..
                } => Some(*unresolved_kind),
                _ => None,
            })
            .collect();
        assert_eq!(
            unresolved_kinds,
            vec![
                UnresolvedKind::InvalidDiscCount,
                UnresolvedKind::InvalidDiscCount,
                UnresolvedKind::InvalidDiscCount,
            ],
            "expected all three sources flagged Unresolved/InvalidDiscCount, got: {unresolved_kinds:?}"
        );
    }
}
