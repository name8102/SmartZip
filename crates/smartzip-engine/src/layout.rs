use crate::name_score::{
    classify_similarity, name_similarity, score_name, SimilarityLevel,
};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Metadata entries to ignore when scanning top-level contents.
pub const METADATA_ENTRIES: &[&str] = &[
    "__MACOSX",
    ".DS_Store",
    "Thumbs.db",
    "desktop.ini",
    ".AppleDouble",
    ".LSOverride",
];

/// Minimum score margin for the inner name to beat the archive name
/// when deciding whether to collapse a single item.
const SCORE_MARGIN_THRESHOLD: f32 = 1.0;

/// How to place extracted content into the output directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OutputLayoutPolicy {
    /// Smart layout: analyze temp dir contents and decide.
    SmartArchive,
    /// Raw: always create an archive-named directory, no smart decisions.
    RawArchiveDir,
}

/// Policy for naming when there's a single root item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SingleRootNamePolicy {
    /// Use the archive file's name as the output directory name.
    UseArchiveName,
    /// Use the inner item's name when it's more informative.
    UseInnerName,
    /// Collapse the single item directly into the output.
    Collapse,
}

/// What was found at the top level of the extraction temp directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TopLevelShape {
    /// Temp directory is empty or contains only metadata.
    Empty,
    /// Exactly one file (non-metadata) at the top level.
    SingleFile(TopLevelItemSummary),
    /// Exactly one directory (non-metadata) at the top level.
    SingleDir(TopLevelItemSummary),
    /// Two or more non-metadata items at the top level.
    Multiple(Vec<TopLevelItemSummary>),
}

/// Summary of a single item found during top-level scan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TopLevelItemSummary {
    /// File name of the item.
    pub name: String,
    /// Full path to the item.
    pub path: PathBuf,
    /// Whether the item is a directory.
    pub is_dir: bool,
}

/// Input to the layout planning function.
#[derive(Debug, Clone)]
pub struct LayoutRequest {
    /// The shape of the temp directory contents.
    pub shape: TopLevelShape,
    /// Name of the original archive file (stem, no extension).
    pub archive_name: String,
    /// The output layout policy.
    pub policy: OutputLayoutPolicy,
    /// Single root naming policy (applies when shape is SingleDir or SingleFile).
    pub single_root_policy: SingleRootNamePolicy,
}

/// The decided output layout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayoutPlan {
    /// What kind of layout was decided.
    pub kind: LayoutPlanKind,
    /// Human-readable reason for this decision.
    pub reason: LayoutDecisionReason,
}

/// What layout strategy to use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayoutPlanKind {
    /// Commit the entire temp directory under a wrapper directory.
    /// The `name` field is the wrapper directory name.
    CommitWholeTempAsArchiveDir { name: String },
    /// Collapse a single item directly into the output root.
    CollapseSingleItem,
    /// Place content in a raw archive-named directory (no smart decisions).
    RawArchiveDir { name: String },
    /// The temp directory was empty; nothing to commit.
    Empty,
}

/// Why this layout was chosen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayoutDecisionReason {
    MultipleTopLevelItems,
    SingleDirGoodName,
    SingleDirGenericName,
    SingleDirSimilarToArchive,
    SingleFileGoodName,
    SingleFileGenericName,
    SingleFileArchiveExtension,
    RawPolicyForced,
    EmptyTempDir,
    DefaultConservative,
}

/// Scan a temp directory and categorize its visible top-level contents.
///
/// Ignores common metadata entries (macOS, Windows). Returns the shape
/// of what remains after filtering.
pub fn scan_visible_top_level(temp_dir: &Path) -> TopLevelShape {
    let entries = match std::fs::read_dir(temp_dir) {
        Ok(entries) => entries,
        Err(_) => return TopLevelShape::Empty,
    };

    let items: Vec<TopLevelItemSummary> = entries
        .filter_map(|e| e.ok())
        .filter(|e| {
            let name = e.file_name();
            let name_str = name.to_string_lossy();
            !METADATA_ENTRIES.contains(&name_str.as_ref())
        })
        .map(|e| TopLevelItemSummary {
            name: e.file_name().to_string_lossy().to_string(),
            path: e.path(),
            is_dir: e.file_type().map(|ft| ft.is_dir()).unwrap_or(false),
        })
        .collect();

    match items.len() {
        0 => TopLevelShape::Empty,
        1 => {
            let item = items.into_iter().next().unwrap();
            if item.is_dir {
                TopLevelShape::SingleDir(item)
            } else {
                TopLevelShape::SingleFile(item)
            }
        }
        _ => TopLevelShape::Multiple(items),
    }
}

/// Plan the output layout for extracted content.
///
/// Takes a `LayoutRequest` containing the temp directory shape, archive name,
/// and policy preferences. Returns a `LayoutPlan` describing what to do.
pub fn plan_layout(req: &LayoutRequest) -> LayoutPlan {
    if req.policy == OutputLayoutPolicy::RawArchiveDir {
        return LayoutPlan {
            kind: LayoutPlanKind::RawArchiveDir {
                name: req.archive_name.clone(),
            },
            reason: LayoutDecisionReason::RawPolicyForced,
        };
    }

    match &req.shape {
        TopLevelShape::Empty => LayoutPlan {
            kind: LayoutPlanKind::Empty,
            reason: LayoutDecisionReason::EmptyTempDir,
        },
        TopLevelShape::Multiple(_) => LayoutPlan {
            kind: LayoutPlanKind::CommitWholeTempAsArchiveDir {
                name: req.archive_name.clone(),
            },
            reason: LayoutDecisionReason::MultipleTopLevelItems,
        },
        TopLevelShape::SingleDir(item) => decide_single_dir(req, item),
        TopLevelShape::SingleFile(item) => decide_single_file(req, item),
    }
}

fn decide_single_dir(req: &LayoutRequest, item: &TopLevelItemSummary) -> LayoutPlan {
    let dir_score = score_name(&item.name);
    let archive_score = score_name(&req.archive_name);

    let sim = name_similarity(&item.name, &req.archive_name);
    let similarity = classify_similarity(sim);

    if similarity == SimilarityLevel::Equivalent {
        return LayoutPlan {
            kind: LayoutPlanKind::CollapseSingleItem,
            reason: LayoutDecisionReason::SingleDirSimilarToArchive,
        };
    }

    if archive_score.is_generic && !dir_score.is_generic {
        return LayoutPlan {
            kind: LayoutPlanKind::CollapseSingleItem,
            reason: LayoutDecisionReason::SingleDirGoodName,
        };
    }

    if dir_score.is_generic {
        return LayoutPlan {
            kind: LayoutPlanKind::CommitWholeTempAsArchiveDir {
                name: req.archive_name.clone(),
            },
            reason: LayoutDecisionReason::SingleDirGenericName,
        };
    }

    if dir_score.total > archive_score.total + SCORE_MARGIN_THRESHOLD {
        return LayoutPlan {
            kind: LayoutPlanKind::CollapseSingleItem,
            reason: LayoutDecisionReason::SingleDirGoodName,
        };
    }

    match req.single_root_policy {
        SingleRootNamePolicy::Collapse => LayoutPlan {
            kind: LayoutPlanKind::CollapseSingleItem,
            reason: LayoutDecisionReason::SingleDirGoodName,
        },
        SingleRootNamePolicy::UseInnerName => {
            if dir_score.total >= archive_score.total {
                LayoutPlan {
                    kind: LayoutPlanKind::CollapseSingleItem,
                    reason: LayoutDecisionReason::SingleDirGoodName,
                }
            } else {
                LayoutPlan {
                    kind: LayoutPlanKind::CommitWholeTempAsArchiveDir {
                        name: req.archive_name.clone(),
                    },
                    reason: LayoutDecisionReason::DefaultConservative,
                }
            }
        }
        SingleRootNamePolicy::UseArchiveName => LayoutPlan {
            kind: LayoutPlanKind::CommitWholeTempAsArchiveDir {
                name: req.archive_name.clone(),
            },
            reason: LayoutDecisionReason::DefaultConservative,
        },
    }
}

fn decide_single_file(req: &LayoutRequest, item: &TopLevelItemSummary) -> LayoutPlan {
    let file_score = score_name(&item.name);
    let archive_score = score_name(&req.archive_name);

    let has_archive_ext = crate::format_from_extension(&item.path).is_some();

    let sim = name_similarity(&item.name, &req.archive_name);
    let similarity = classify_similarity(sim);

    if similarity == SimilarityLevel::Equivalent {
        return LayoutPlan {
            kind: LayoutPlanKind::CollapseSingleItem,
            reason: LayoutDecisionReason::SingleDirSimilarToArchive,
        };
    }

    if archive_score.is_generic && !file_score.is_generic {
        return LayoutPlan {
            kind: LayoutPlanKind::CollapseSingleItem,
            reason: LayoutDecisionReason::SingleFileGoodName,
        };
    }

    if file_score.is_generic {
        return LayoutPlan {
            kind: LayoutPlanKind::CommitWholeTempAsArchiveDir {
                name: req.archive_name.clone(),
            },
            reason: LayoutDecisionReason::SingleFileGenericName,
        };
    }

    if has_archive_ext && file_score.total > archive_score.total {
        return LayoutPlan {
            kind: LayoutPlanKind::CollapseSingleItem,
            reason: LayoutDecisionReason::SingleFileArchiveExtension,
        };
    }

    if file_score.total > archive_score.total + SCORE_MARGIN_THRESHOLD {
        return LayoutPlan {
            kind: LayoutPlanKind::CollapseSingleItem,
            reason: LayoutDecisionReason::SingleFileGoodName,
        };
    }

    match req.single_root_policy {
        SingleRootNamePolicy::Collapse => LayoutPlan {
            kind: LayoutPlanKind::CollapseSingleItem,
            reason: LayoutDecisionReason::SingleFileGoodName,
        },
        SingleRootNamePolicy::UseInnerName => {
            if file_score.total >= archive_score.total {
                LayoutPlan {
                    kind: LayoutPlanKind::CollapseSingleItem,
                    reason: LayoutDecisionReason::SingleFileGoodName,
                }
            } else {
                LayoutPlan {
                    kind: LayoutPlanKind::CommitWholeTempAsArchiveDir {
                        name: req.archive_name.clone(),
                    },
                    reason: LayoutDecisionReason::DefaultConservative,
                }
            }
        }
        SingleRootNamePolicy::UseArchiveName => LayoutPlan {
            kind: LayoutPlanKind::CommitWholeTempAsArchiveDir {
                name: req.archive_name.clone(),
            },
            reason: LayoutDecisionReason::DefaultConservative,
        },
    }
}

/// Pick the best name from a list of duplicates based on name score.
pub fn pick_best_duplicate_name<'a>(names: &[&'a str]) -> Option<&'a str> {
    names
        .iter()
        .max_by(|a, b| {
            let sa = score_name(a);
            let sb = score_name(b);
            sa.total
                .partial_cmp(&sb.total)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(suffix: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "smartzip-layout-test-{}-{}",
            std::process::id(),
            suffix
        ))
    }

    fn make_item(name: &str, is_dir: bool, parent: &Path) -> TopLevelItemSummary {
        TopLevelItemSummary {
            name: name.to_string(),
            path: parent.join(name),
            is_dir,
        }
    }

    fn make_request(
        shape: TopLevelShape,
        archive_name: &str,
        policy: OutputLayoutPolicy,
        single_policy: SingleRootNamePolicy,
    ) -> LayoutRequest {
        LayoutRequest {
            shape,
            archive_name: archive_name.to_string(),
            policy,
            single_root_policy: single_policy,
        }
    }

    // ── scan_visible_top_level tests ──────────────────────────────────────

    #[test]
    fn scan_empty_dir() {
        let dir = temp_dir("empty");
        std::fs::create_dir_all(&dir).unwrap();
        let shape = scan_visible_top_level(&dir);
        assert_eq!(shape, TopLevelShape::Empty);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn scan_nonexistent_dir() {
        let dir = temp_dir("nonexistent");
        let shape = scan_visible_top_level(&dir);
        assert_eq!(shape, TopLevelShape::Empty);
    }

    #[test]
    fn scan_filters_metadata_entries() {
        let dir = temp_dir("metadata");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("__MACOSX"), []).unwrap();
        std::fs::write(dir.join(".DS_Store"), []).unwrap();
        std::fs::write(dir.join("Thumbs.db"), []).unwrap();
        std::fs::write(dir.join("desktop.ini"), []).unwrap();
        std::fs::write(dir.join(".AppleDouble"), []).unwrap();
        std::fs::write(dir.join(".LSOverride"), []).unwrap();
        std::fs::write(dir.join("real_file.txt"), b"content").unwrap();

        let shape = scan_visible_top_level(&dir);
        match &shape {
            TopLevelShape::SingleFile(item) => {
                assert_eq!(item.name, "real_file.txt");
                assert!(!item.is_dir);
            }
            other => panic!("expected SingleFile, got {:?}", other),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn scan_multiple_items() {
        let dir = temp_dir("multiple");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), []).unwrap();
        std::fs::write(dir.join("b.txt"), []).unwrap();
        std::fs::write(dir.join("__MACOSX"), []).unwrap();

        let shape = scan_visible_top_level(&dir);
        match &shape {
            TopLevelShape::Multiple(items) => {
                assert_eq!(items.len(), 2);
                let names: Vec<&str> = items.iter().map(|i| i.name.as_str()).collect();
                assert!(names.contains(&"a.txt"));
                assert!(names.contains(&"b.txt"));
            }
            other => panic!("expected Multiple, got {:?}", other),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn scan_single_dir() {
        let dir = temp_dir("singledir");
        std::fs::create_dir_all(dir.join("MyProject")).unwrap();

        let shape = scan_visible_top_level(&dir);
        match &shape {
            TopLevelShape::SingleDir(item) => {
                assert_eq!(item.name, "MyProject");
                assert!(item.is_dir);
            }
            other => panic!("expected SingleDir, got {:?}", other),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── plan_layout tests ─────────────────────────────────────────────────

    #[test]
    fn plan_multiple_items_commit_archive_dir() {
        let items = vec![
            make_item("a.txt", false, &PathBuf::new()),
            make_item("b.txt", false, &PathBuf::new()),
        ];
        let req = make_request(
            TopLevelShape::Multiple(items),
            "my-archive",
            OutputLayoutPolicy::SmartArchive,
            SingleRootNamePolicy::UseArchiveName,
        );
        let plan = plan_layout(&req);
        assert_eq!(
            plan.kind,
            LayoutPlanKind::CommitWholeTempAsArchiveDir {
                name: "my-archive".to_string()
            }
        );
        assert_eq!(plan.reason, LayoutDecisionReason::MultipleTopLevelItems);
    }

    #[test]
    fn plan_raw_policy_always_raw_archive_dir() {
        let items = vec![
            make_item("a.txt", false, &PathBuf::new()),
            make_item("b.txt", false, &PathBuf::new()),
        ];
        let req = make_request(
            TopLevelShape::Multiple(items),
            "archive",
            OutputLayoutPolicy::RawArchiveDir,
            SingleRootNamePolicy::UseArchiveName,
        );
        let plan = plan_layout(&req);
        assert_eq!(
            plan.kind,
            LayoutPlanKind::RawArchiveDir {
                name: "archive".to_string()
            }
        );
        assert_eq!(plan.reason, LayoutDecisionReason::RawPolicyForced);
    }

    #[test]
    fn plan_empty_shape() {
        let req = make_request(
            TopLevelShape::Empty,
            "archive",
            OutputLayoutPolicy::SmartArchive,
            SingleRootNamePolicy::UseArchiveName,
        );
        let plan = plan_layout(&req);
        assert_eq!(plan.kind, LayoutPlanKind::Empty);
        assert_eq!(plan.reason, LayoutDecisionReason::EmptyTempDir);
    }

    #[test]
    fn plan_single_generic_dir_uses_archive_name() {
        let item = make_item("files", true, &PathBuf::new());
        let req = make_request(
            TopLevelShape::SingleDir(item),
            "my-project-v2.0",
            OutputLayoutPolicy::SmartArchive,
            SingleRootNamePolicy::UseArchiveName,
        );
        let plan = plan_layout(&req);
        assert_eq!(
            plan.kind,
            LayoutPlanKind::CommitWholeTempAsArchiveDir {
                name: "my-project-v2.0".to_string()
            }
        );
        assert_eq!(plan.reason, LayoutDecisionReason::SingleDirGenericName);
    }

    #[test]
    fn plan_single_generic_archive_uses_inner_name() {
        let item = make_item("The Great Gatsby", true, &PathBuf::new());
        let req = make_request(
            TopLevelShape::SingleDir(item),
            "archive",
            OutputLayoutPolicy::SmartArchive,
            SingleRootNamePolicy::UseArchiveName,
        );
        let plan = plan_layout(&req);
        assert_eq!(plan.kind, LayoutPlanKind::CollapseSingleItem);
        assert_eq!(plan.reason, LayoutDecisionReason::SingleDirGoodName);
    }

    #[test]
    fn plan_single_dir_similar_to_archive_collapses() {
        let item = make_item("project-release", true, &PathBuf::new());
        let req = make_request(
            TopLevelShape::SingleDir(item),
            "project-release",
            OutputLayoutPolicy::SmartArchive,
            SingleRootNamePolicy::UseArchiveName,
        );
        let plan = plan_layout(&req);
        assert_eq!(plan.kind, LayoutPlanKind::CollapseSingleItem);
        assert_eq!(plan.reason, LayoutDecisionReason::SingleDirSimilarToArchive);
    }

    #[test]
    fn plan_single_dir_good_name_beats_archive() {
        let item = make_item(
            "The Great Gatsby - F. Scott Fitzgerald (2024 Edition)",
            true,
            &PathBuf::new(),
        );
        let req = make_request(
            TopLevelShape::SingleDir(item),
            "book",
            OutputLayoutPolicy::SmartArchive,
            SingleRootNamePolicy::UseArchiveName,
        );
        let plan = plan_layout(&req);
        assert_eq!(plan.kind, LayoutPlanKind::CollapseSingleItem);
        assert_eq!(plan.reason, LayoutDecisionReason::SingleDirGoodName);
    }

    #[test]
    fn plan_single_file_generic_name_uses_archive() {
        let item = make_item("downloads", false, &PathBuf::new());
        let req = make_request(
            TopLevelShape::SingleFile(item),
            "my-project",
            OutputLayoutPolicy::SmartArchive,
            SingleRootNamePolicy::UseArchiveName,
        );
        let plan = plan_layout(&req);
        assert_eq!(
            plan.kind,
            LayoutPlanKind::CommitWholeTempAsArchiveDir {
                name: "my-project".to_string()
            }
        );
        assert_eq!(plan.reason, LayoutDecisionReason::SingleFileGenericName);
    }

    #[test]
    fn plan_single_file_good_name_collapses() {
        let item = make_item(
            "The Great Gatsby - F. Scott Fitzgerald.pdf",
            false,
            &PathBuf::new(),
        );
        let req = make_request(
            TopLevelShape::SingleFile(item),
            "archive",
            OutputLayoutPolicy::SmartArchive,
            SingleRootNamePolicy::UseArchiveName,
        );
        let plan = plan_layout(&req);
        assert_eq!(plan.kind, LayoutPlanKind::CollapseSingleItem);
        assert_eq!(plan.reason, LayoutDecisionReason::SingleFileGoodName);
    }

    #[test]
    fn plan_single_file_archive_ext_beats_generic_archive() {
        let item = make_item(
            "The Great Gatsby - F. Scott Fitzgerald.zip",
            false,
            &PathBuf::new(),
        );
        let req = make_request(
            TopLevelShape::SingleFile(item),
            "abc",
            OutputLayoutPolicy::SmartArchive,
            SingleRootNamePolicy::UseArchiveName,
        );
        let plan = plan_layout(&req);
        assert_eq!(plan.kind, LayoutPlanKind::CollapseSingleItem);
        assert_eq!(plan.reason, LayoutDecisionReason::SingleFileArchiveExtension);
    }

    #[test]
    fn plan_collapse_policy_collapses_single_dir() {
        let item = make_item("MyProject", true, &PathBuf::new());
        let req = make_request(
            TopLevelShape::SingleDir(item),
            "archive",
            OutputLayoutPolicy::SmartArchive,
            SingleRootNamePolicy::Collapse,
        );
        let plan = plan_layout(&req);
        assert_eq!(plan.kind, LayoutPlanKind::CollapseSingleItem);
        assert_eq!(plan.reason, LayoutDecisionReason::SingleDirGoodName);
    }

    #[test]
    fn plan_use_archive_name_policy_wraps() {
        let item = make_item("MyProject", true, &PathBuf::new());
        let req = make_request(
            TopLevelShape::SingleDir(item),
            "my-archive",
            OutputLayoutPolicy::SmartArchive,
            SingleRootNamePolicy::UseArchiveName,
        );
        let plan = plan_layout(&req);
        assert_eq!(
            plan.kind,
            LayoutPlanKind::CommitWholeTempAsArchiveDir {
                name: "my-archive".to_string()
            }
        );
        assert_eq!(plan.reason, LayoutDecisionReason::DefaultConservative);
    }

    // ── pick_best_duplicate_name tests ────────────────────────────────────

    #[test]
    fn pick_best_duplicate_prefers_scored_name() {
        let names = vec!["files", "The Great Gatsby"];
        let best = pick_best_duplicate_name(&names).unwrap();
        assert_eq!(best, "The Great Gatsby");
    }

    #[test]
    fn pick_best_duplicate_empty_list() {
        let names: Vec<&str> = vec![];
        assert!(pick_best_duplicate_name(&names).is_none());
    }

    #[test]
    fn pick_best_duplicate_single_item() {
        let names = vec!["only-one"];
        let best = pick_best_duplicate_name(&names).unwrap();
        assert_eq!(best, "only-one");
    }
}
