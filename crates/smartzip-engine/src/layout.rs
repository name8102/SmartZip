use crate::name_score::{classify_similarity, name_similarity, score_name, SimilarityLevel};
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
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum OutputLayoutPolicy {
    Smart,
    Raw,
    #[default]
    Conservative,
    /// Collapse a single top-level item directly into the output directory
    /// without wrapping it in an archive-name subdirectory.
    FlatSingle,
}

/// Policy for naming when there's a single root item.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SingleRootNamePolicy {
    PreferArchiveName,
    PreferInnerName,
    #[default]
    Auto,
    PreserveBoth,
    AskWhenAmbiguous,
}

/// What was found at the top level of the extraction temp directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TopLevelShape {
    Empty,
    SingleFile(TopLevelItemSummary),
    SingleDir(TopLevelItemSummary),
    Multiple {
        items: Vec<TopLevelItemSummary>,
        count: usize,
    },
}

/// Summary of a single item found during top-level scan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TopLevelItemSummary {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
}

/// Input to the layout planning function.
#[derive(Debug, Clone)]
pub struct LayoutRequest {
    pub shape: TopLevelShape,
    pub archive_path: PathBuf,
    pub archive_stem: String,
    pub output_root: PathBuf,
    pub layout_policy: OutputLayoutPolicy,
    pub single_root_name_policy: SingleRootNamePolicy,
}

/// What source the layout plan operates on inside the temp directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanSource {
    WholeTempDir,
    SingleDir(PathBuf),
    SingleDirContents(PathBuf),
    SingleFile(PathBuf),
}

/// The decided output layout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayoutPlan {
    pub source: PlanSource,
    pub kind: LayoutPlanKind,
    pub target: PathBuf,
    pub reason: LayoutDecisionReason,
    pub warnings: Vec<String>,
}

/// What layout strategy to use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayoutPlanKind {
    CommitWholeTempAsArchiveDir { name: String },
    CommitSingleDirContentsAsArchiveName,
    CommitSingleDirAsInnerName,
    CommitSingleFileAsArchiveName,
    CommitSingleFileAsInnerName,
    PreserveBothSingleDir,
    PreserveBothSingleFile,
    RawArchiveDir { name: String },
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
    SingleFileSimilarToArchive,
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
        .map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            TopLevelItemSummary {
                name,
                path: e.path(),
                is_dir: e.file_type().map(|ft| ft.is_dir()).unwrap_or(false),
            }
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
        n => TopLevelShape::Multiple { items, count: n },
    }
}

/// Plan the output layout for extracted content.
pub fn plan_layout(req: &LayoutRequest) -> LayoutPlan {
    if req.layout_policy == OutputLayoutPolicy::Raw {
        return LayoutPlan {
            source: PlanSource::WholeTempDir,
            kind: LayoutPlanKind::RawArchiveDir {
                name: req.archive_stem.clone(),
            },
            target: req.output_root.join(&req.archive_stem),
            reason: LayoutDecisionReason::RawPolicyForced,
            warnings: vec![],
        };
    }

    if req.layout_policy == OutputLayoutPolicy::FlatSingle {
        return match &req.shape {
            TopLevelShape::Empty => LayoutPlan {
                source: PlanSource::WholeTempDir,
                kind: LayoutPlanKind::Empty,
                target: req.output_root.clone(),
                reason: LayoutDecisionReason::EmptyTempDir,
                warnings: vec![],
            },
            TopLevelShape::SingleDir(item) => LayoutPlan {
                source: PlanSource::SingleDir(item.path.clone()),
                kind: LayoutPlanKind::CommitSingleDirAsInnerName,
                target: req.output_root.join(&item.name),
                reason: LayoutDecisionReason::SingleDirGoodName,
                warnings: vec![],
            },
            TopLevelShape::SingleFile(item) => LayoutPlan {
                source: PlanSource::SingleFile(item.path.clone()),
                kind: LayoutPlanKind::CommitSingleFileAsInnerName,
                target: req.output_root.join(&item.name),
                reason: LayoutDecisionReason::SingleFileGoodName,
                warnings: vec![],
            },
            TopLevelShape::Multiple { .. } => LayoutPlan {
                source: PlanSource::WholeTempDir,
                kind: LayoutPlanKind::CommitWholeTempAsArchiveDir {
                    name: req.archive_stem.clone(),
                },
                target: req.output_root.join(&req.archive_stem),
                reason: LayoutDecisionReason::MultipleTopLevelItems,
                warnings: vec![],
            },
        };
    }

    match &req.shape {
        TopLevelShape::Empty => LayoutPlan {
            source: PlanSource::WholeTempDir,
            kind: LayoutPlanKind::Empty,
            target: req.output_root.clone(),
            reason: LayoutDecisionReason::EmptyTempDir,
            warnings: vec![],
        },
        TopLevelShape::Multiple { .. } => LayoutPlan {
            source: PlanSource::WholeTempDir,
            kind: LayoutPlanKind::CommitWholeTempAsArchiveDir {
                name: req.archive_stem.clone(),
            },
            target: req.output_root.join(&req.archive_stem),
            reason: LayoutDecisionReason::MultipleTopLevelItems,
            warnings: vec![],
        },
        TopLevelShape::SingleDir(item) => decide_single_dir(req, item),
        TopLevelShape::SingleFile(item) => decide_single_file(req, item),
    }
}

fn decide_single_dir(req: &LayoutRequest, item: &TopLevelItemSummary) -> LayoutPlan {
    // 1. Raw policy → always raw
    if req.layout_policy == OutputLayoutPolicy::Raw {
        return LayoutPlan {
            source: PlanSource::WholeTempDir,
            kind: LayoutPlanKind::RawArchiveDir {
                name: req.archive_stem.clone(),
            },
            target: req.output_root.join(&req.archive_stem),
            reason: LayoutDecisionReason::RawPolicyForced,
            warnings: vec![],
        };
    }

    // 2. Explicit name policies → first priority
    match req.single_root_name_policy {
        SingleRootNamePolicy::PreferArchiveName => {
            return LayoutPlan {
                source: PlanSource::SingleDirContents(item.path.clone()),
                kind: LayoutPlanKind::CommitSingleDirContentsAsArchiveName,
                target: req.output_root.join(&req.archive_stem),
                reason: LayoutDecisionReason::DefaultConservative,
                warnings: vec![],
            };
        }
        SingleRootNamePolicy::PreferInnerName => {
            return LayoutPlan {
                source: PlanSource::SingleDir(item.path.clone()),
                kind: LayoutPlanKind::CommitSingleDirAsInnerName,
                target: req.output_root.join(&item.name),
                reason: LayoutDecisionReason::SingleDirGoodName,
                warnings: vec![],
            };
        }
        SingleRootNamePolicy::PreserveBoth => {
            return LayoutPlan {
                source: PlanSource::SingleDir(item.path.clone()),
                kind: LayoutPlanKind::PreserveBothSingleDir,
                target: req.output_root.join(&req.archive_stem),
                reason: LayoutDecisionReason::SingleDirGoodName,
                warnings: vec![],
            };
        }
        _ => {} // fall through to heuristics
    }

    // 3. Heuristics (only for Auto/AskWhenAmbiguous)
    let dir_score = score_name(&item.name);
    let archive_score = score_name(&req.archive_stem);

    let sim = name_similarity(&item.name, &req.archive_stem);
    let similarity = classify_similarity(sim);

    if similarity == SimilarityLevel::Equivalent {
        return LayoutPlan {
            source: PlanSource::SingleDir(item.path.clone()),
            kind: LayoutPlanKind::CommitSingleDirAsInnerName,
            target: req.output_root.join(&item.name),
            reason: LayoutDecisionReason::SingleDirSimilarToArchive,
            warnings: vec![],
        };
    }

    if archive_score.is_generic && !dir_score.is_generic {
        return LayoutPlan {
            source: PlanSource::SingleDir(item.path.clone()),
            kind: LayoutPlanKind::CommitSingleDirAsInnerName,
            target: req.output_root.join(&item.name),
            reason: LayoutDecisionReason::SingleDirGoodName,
            warnings: vec![],
        };
    }

    if dir_score.is_generic {
        return LayoutPlan {
            source: PlanSource::SingleDirContents(item.path.clone()),
            kind: LayoutPlanKind::CommitSingleDirContentsAsArchiveName,
            target: req.output_root.join(&req.archive_stem),
            reason: LayoutDecisionReason::SingleDirGenericName,
            warnings: vec![],
        };
    }

    if dir_score.total > archive_score.total + SCORE_MARGIN_THRESHOLD {
        return LayoutPlan {
            source: PlanSource::SingleDir(item.path.clone()),
            kind: LayoutPlanKind::CommitSingleDirAsInnerName,
            target: req.output_root.join(&item.name),
            reason: LayoutDecisionReason::SingleDirGoodName,
            warnings: vec![],
        };
    }

    // 4. Default conservative
    LayoutPlan {
        source: PlanSource::WholeTempDir,
        kind: LayoutPlanKind::CommitWholeTempAsArchiveDir {
            name: req.archive_stem.clone(),
        },
        target: req.output_root.join(&req.archive_stem),
        reason: LayoutDecisionReason::DefaultConservative,
        warnings: vec![],
    }
}

fn decide_single_file(req: &LayoutRequest, item: &TopLevelItemSummary) -> LayoutPlan {
    // 1. Raw policy → always raw
    if req.layout_policy == OutputLayoutPolicy::Raw {
        return LayoutPlan {
            source: PlanSource::WholeTempDir,
            kind: LayoutPlanKind::RawArchiveDir {
                name: req.archive_stem.clone(),
            },
            target: req.output_root.join(&req.archive_stem),
            reason: LayoutDecisionReason::RawPolicyForced,
            warnings: vec![],
        };
    }

    // 2. Explicit name policies → first priority
    match req.single_root_name_policy {
        SingleRootNamePolicy::PreferArchiveName => {
            let ext = item
                .path
                .extension()
                .map(|e| format!(".{}", e.to_string_lossy()))
                .unwrap_or_default();
            return LayoutPlan {
                source: PlanSource::SingleFile(item.path.clone()),
                kind: LayoutPlanKind::CommitSingleFileAsArchiveName,
                target: req.output_root.join(format!("{}{}", req.archive_stem, ext)),
                reason: LayoutDecisionReason::DefaultConservative,
                warnings: vec![],
            };
        }
        SingleRootNamePolicy::PreferInnerName => {
            return LayoutPlan {
                source: PlanSource::SingleFile(item.path.clone()),
                kind: LayoutPlanKind::CommitSingleFileAsInnerName,
                target: req.output_root.join(&item.name),
                reason: LayoutDecisionReason::SingleFileGoodName,
                warnings: vec![],
            };
        }
        SingleRootNamePolicy::PreserveBoth => {
            return LayoutPlan {
                source: PlanSource::SingleFile(item.path.clone()),
                kind: LayoutPlanKind::PreserveBothSingleFile,
                target: req.output_root.join(&req.archive_stem),
                reason: LayoutDecisionReason::SingleFileGoodName,
                warnings: vec![],
            };
        }
        _ => {} // fall through to heuristics
    }

    // 3. Heuristics (only for Auto/AskWhenAmbiguous)
    let file_score = score_name(&item.name);
    let archive_score = score_name(&req.archive_stem);

    let has_archive_ext = crate::format_from_extension(&item.path).is_some();

    let sim = name_similarity(&item.name, &req.archive_stem);
    let similarity = classify_similarity(sim);

    if similarity == SimilarityLevel::Equivalent {
        return LayoutPlan {
            source: PlanSource::SingleFile(item.path.clone()),
            kind: LayoutPlanKind::CommitSingleFileAsInnerName,
            target: req.output_root.join(&item.name),
            reason: LayoutDecisionReason::SingleFileSimilarToArchive,
            warnings: vec![],
        };
    }

    if archive_score.is_generic && !file_score.is_generic {
        return LayoutPlan {
            source: PlanSource::SingleFile(item.path.clone()),
            kind: LayoutPlanKind::CommitSingleFileAsInnerName,
            target: req.output_root.join(&item.name),
            reason: LayoutDecisionReason::SingleFileGoodName,
            warnings: vec![],
        };
    }

    if file_score.is_generic {
        let ext = item
            .path
            .extension()
            .map(|e| format!(".{}", e.to_string_lossy()))
            .unwrap_or_default();
        return LayoutPlan {
            source: PlanSource::SingleFile(item.path.clone()),
            kind: LayoutPlanKind::CommitSingleFileAsArchiveName,
            target: req.output_root.join(format!("{}{}", req.archive_stem, ext)),
            reason: LayoutDecisionReason::SingleFileGenericName,
            warnings: vec![],
        };
    }

    if has_archive_ext && file_score.total > archive_score.total {
        return LayoutPlan {
            source: PlanSource::SingleFile(item.path.clone()),
            kind: LayoutPlanKind::CommitSingleFileAsInnerName,
            target: req.output_root.join(&item.name),
            reason: LayoutDecisionReason::SingleFileArchiveExtension,
            warnings: vec![],
        };
    }

    if file_score.total > archive_score.total + SCORE_MARGIN_THRESHOLD {
        return LayoutPlan {
            source: PlanSource::SingleFile(item.path.clone()),
            kind: LayoutPlanKind::CommitSingleFileAsInnerName,
            target: req.output_root.join(&item.name),
            reason: LayoutDecisionReason::SingleFileGoodName,
            warnings: vec![],
        };
    }

    // 4. Default conservative
    LayoutPlan {
        source: PlanSource::WholeTempDir,
        kind: LayoutPlanKind::CommitWholeTempAsArchiveDir {
            name: req.archive_stem.clone(),
        },
        target: req.output_root.join(&req.archive_stem),
        reason: LayoutDecisionReason::DefaultConservative,
        warnings: vec![],
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
        archive_stem: &str,
        policy: OutputLayoutPolicy,
        single_policy: SingleRootNamePolicy,
    ) -> LayoutRequest {
        let output = temp_dir("output");
        LayoutRequest {
            shape,
            archive_path: output.join(format!("{archive_stem}.zip")),
            archive_stem: archive_stem.to_string(),
            output_root: output,
            layout_policy: policy,
            single_root_name_policy: single_policy,
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
            TopLevelShape::Multiple { items, count } => {
                assert_eq!(*count, 2);
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
            TopLevelShape::Multiple {
                items: items.clone(),
                count: items.len(),
            },
            "my-archive",
            OutputLayoutPolicy::Smart,
            SingleRootNamePolicy::PreferArchiveName,
        );
        let plan = plan_layout(&req);
        assert_eq!(plan.source, PlanSource::WholeTempDir);
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
            TopLevelShape::Multiple {
                items: items.clone(),
                count: items.len(),
            },
            "archive",
            OutputLayoutPolicy::Raw,
            SingleRootNamePolicy::PreferArchiveName,
        );
        let plan = plan_layout(&req);
        assert_eq!(plan.source, PlanSource::WholeTempDir);
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
            OutputLayoutPolicy::Smart,
            SingleRootNamePolicy::PreferArchiveName,
        );
        let plan = plan_layout(&req);
        assert_eq!(plan.source, PlanSource::WholeTempDir);
        assert_eq!(plan.kind, LayoutPlanKind::Empty);
        assert_eq!(plan.reason, LayoutDecisionReason::EmptyTempDir);
    }

    #[test]
    fn plan_single_generic_dir_flattens_contents_with_auto() {
        let item = make_item("files", true, &PathBuf::new());
        let req = make_request(
            TopLevelShape::SingleDir(item),
            "my-project-v2.0",
            OutputLayoutPolicy::Smart,
            SingleRootNamePolicy::Auto,
        );
        let plan = plan_layout(&req);
        assert_eq!(
            plan.source,
            PlanSource::SingleDirContents(PathBuf::from("files"))
        );
        assert_eq!(
            plan.kind,
            LayoutPlanKind::CommitSingleDirContentsAsArchiveName
        );
        assert_eq!(plan.target, req.output_root.join("my-project-v2.0"));
        assert_eq!(plan.reason, LayoutDecisionReason::SingleDirGenericName);
    }

    #[test]
    fn plan_prefer_archive_name_wraps_single_dir() {
        let item = make_item("MyProject", true, &PathBuf::new());
        let req = make_request(
            TopLevelShape::SingleDir(item),
            "my-archive",
            OutputLayoutPolicy::Smart,
            SingleRootNamePolicy::PreferArchiveName,
        );
        let plan = plan_layout(&req);
        assert_eq!(
            plan.kind,
            LayoutPlanKind::CommitSingleDirContentsAsArchiveName
        );
        assert_eq!(
            plan.source,
            PlanSource::SingleDirContents(PathBuf::from("MyProject"))
        );
        assert_eq!(plan.target, req.output_root.join("my-archive"));
        assert_eq!(plan.reason, LayoutDecisionReason::DefaultConservative);
    }

    #[test]
    fn plan_prefer_inner_name_collapses_single_dir() {
        let item = make_item("MyProject", true, &PathBuf::new());
        let req = make_request(
            TopLevelShape::SingleDir(item),
            "my-archive",
            OutputLayoutPolicy::Smart,
            SingleRootNamePolicy::PreferInnerName,
        );
        let plan = plan_layout(&req);
        assert_eq!(
            plan.source,
            PlanSource::SingleDir(PathBuf::from("MyProject"))
        );
        assert_eq!(plan.kind, LayoutPlanKind::CommitSingleDirAsInnerName);
        assert_eq!(plan.target, req.output_root.join("MyProject"));
        assert_eq!(plan.reason, LayoutDecisionReason::SingleDirGoodName);
    }

    #[test]
    fn plan_preserve_both_single_dir() {
        let item = make_item("MyProject", true, &PathBuf::new());
        let req = make_request(
            TopLevelShape::SingleDir(item),
            "my-archive",
            OutputLayoutPolicy::Smart,
            SingleRootNamePolicy::PreserveBoth,
        );
        let plan = plan_layout(&req);
        assert_eq!(
            plan.source,
            PlanSource::SingleDir(PathBuf::from("MyProject"))
        );
        assert_eq!(plan.kind, LayoutPlanKind::PreserveBothSingleDir);
        assert_eq!(plan.target, req.output_root.join("my-archive"));
        assert_eq!(plan.reason, LayoutDecisionReason::SingleDirGoodName);
    }

    #[test]
    fn plan_single_generic_archive_uses_inner_name() {
        let item = make_item("The Great Gatsby", true, &PathBuf::new());
        let req = make_request(
            TopLevelShape::SingleDir(item),
            "archive",
            OutputLayoutPolicy::Smart,
            SingleRootNamePolicy::Auto,
        );
        let plan = plan_layout(&req);
        assert_eq!(plan.kind, LayoutPlanKind::CommitSingleDirAsInnerName);
        assert_eq!(plan.reason, LayoutDecisionReason::SingleDirGoodName);
    }

    #[test]
    fn plan_single_dir_similar_to_archive_collapses() {
        let item = make_item("project-release", true, &PathBuf::new());
        let req = make_request(
            TopLevelShape::SingleDir(item),
            "project-release",
            OutputLayoutPolicy::Smart,
            SingleRootNamePolicy::Auto,
        );
        let plan = plan_layout(&req);
        assert_eq!(plan.kind, LayoutPlanKind::CommitSingleDirAsInnerName);
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
            OutputLayoutPolicy::Smart,
            SingleRootNamePolicy::Auto,
        );
        let plan = plan_layout(&req);
        assert_eq!(plan.kind, LayoutPlanKind::CommitSingleDirAsInnerName);
        assert_eq!(plan.reason, LayoutDecisionReason::SingleDirGoodName);
    }

    #[test]
    fn plan_single_file_generic_name_uses_archive() {
        let item = make_item("downloads", false, &PathBuf::new());
        let req = make_request(
            TopLevelShape::SingleFile(item),
            "my-project",
            OutputLayoutPolicy::Smart,
            SingleRootNamePolicy::Auto,
        );
        let plan = plan_layout(&req);
        assert_eq!(plan.kind, LayoutPlanKind::CommitSingleFileAsArchiveName);
        assert_eq!(plan.target, req.output_root.join("my-project"));
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
            OutputLayoutPolicy::Smart,
            SingleRootNamePolicy::Auto,
        );
        let plan = plan_layout(&req);
        assert_eq!(plan.kind, LayoutPlanKind::CommitSingleFileAsInnerName);
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
            OutputLayoutPolicy::Smart,
            SingleRootNamePolicy::Auto,
        );
        let plan = plan_layout(&req);
        assert_eq!(plan.kind, LayoutPlanKind::CommitSingleFileAsInnerName);
        assert_eq!(
            plan.target,
            req.output_root
                .join("The Great Gatsby - F. Scott Fitzgerald.zip")
        );
        assert_eq!(
            plan.reason,
            LayoutDecisionReason::SingleFileArchiveExtension
        );
    }

    #[test]
    fn plan_prefer_archive_name_renames_single_file() {
        let item = make_item("document.pdf", false, &PathBuf::new());
        let req = make_request(
            TopLevelShape::SingleFile(item),
            "download",
            OutputLayoutPolicy::Smart,
            SingleRootNamePolicy::PreferArchiveName,
        );
        let plan = plan_layout(&req);
        assert_eq!(
            plan.source,
            PlanSource::SingleFile(PathBuf::from("document.pdf"))
        );
        assert_eq!(plan.kind, LayoutPlanKind::CommitSingleFileAsArchiveName);
        assert_eq!(plan.target, req.output_root.join("download.pdf"));
        assert_eq!(plan.reason, LayoutDecisionReason::DefaultConservative);
    }

    #[test]
    fn plan_prefer_inner_name_keeps_single_file() {
        let item = make_item("document.pdf", false, &PathBuf::new());
        let req = make_request(
            TopLevelShape::SingleFile(item),
            "download",
            OutputLayoutPolicy::Smart,
            SingleRootNamePolicy::PreferInnerName,
        );
        let plan = plan_layout(&req);
        assert_eq!(plan.kind, LayoutPlanKind::CommitSingleFileAsInnerName);
        assert_eq!(plan.target, req.output_root.join("document.pdf"));
        assert_eq!(plan.reason, LayoutDecisionReason::SingleFileGoodName);
    }

    #[test]
    fn plan_preserve_both_single_file() {
        let item = make_item("document.pdf", false, &PathBuf::new());
        let req = make_request(
            TopLevelShape::SingleFile(item),
            "download",
            OutputLayoutPolicy::Smart,
            SingleRootNamePolicy::PreserveBoth,
        );
        let plan = plan_layout(&req);
        assert_eq!(plan.kind, LayoutPlanKind::PreserveBothSingleFile);
        assert_eq!(plan.target, req.output_root.join("download"));
        assert_eq!(plan.reason, LayoutDecisionReason::SingleFileGoodName);
    }

    #[test]
    fn plan_auto_policy_prefers_inner_name() {
        let item = make_item("The Great Gatsby", true, &PathBuf::new());
        let req = make_request(
            TopLevelShape::SingleDir(item),
            "archive",
            OutputLayoutPolicy::Smart,
            SingleRootNamePolicy::Auto,
        );
        let plan = plan_layout(&req);
        assert_eq!(plan.kind, LayoutPlanKind::CommitSingleDirAsInnerName);
        assert_eq!(plan.reason, LayoutDecisionReason::SingleDirGoodName);
    }

    #[test]
    fn plan_flat_single_policy_empty() {
        let req = make_request(
            TopLevelShape::Empty,
            "archive",
            OutputLayoutPolicy::FlatSingle,
            SingleRootNamePolicy::Auto,
        );
        let plan = plan_layout(&req);
        assert_eq!(plan.kind, LayoutPlanKind::Empty);
        assert_eq!(plan.reason, LayoutDecisionReason::EmptyTempDir);
    }

    #[test]
    fn plan_flat_single_collapses_single_dir() {
        let item = make_item("MyProject", true, &PathBuf::new());
        let req = make_request(
            TopLevelShape::SingleDir(item),
            "archive",
            OutputLayoutPolicy::FlatSingle,
            SingleRootNamePolicy::Auto,
        );
        let plan = plan_layout(&req);
        assert_eq!(plan.kind, LayoutPlanKind::CommitSingleDirAsInnerName);
        assert_eq!(
            plan.source,
            PlanSource::SingleDir(PathBuf::from("MyProject"))
        );
        assert_eq!(plan.target, req.output_root.join("MyProject"));
    }

    #[test]
    fn plan_flat_single_collapses_single_file() {
        let item = make_item("document.pdf", false, &PathBuf::new());
        let req = make_request(
            TopLevelShape::SingleFile(item),
            "archive",
            OutputLayoutPolicy::FlatSingle,
            SingleRootNamePolicy::Auto,
        );
        let plan = plan_layout(&req);
        assert_eq!(plan.kind, LayoutPlanKind::CommitSingleFileAsInnerName);
        assert_eq!(
            plan.source,
            PlanSource::SingleFile(PathBuf::from("document.pdf"))
        );
        assert_eq!(plan.target, req.output_root.join("document.pdf"));
    }

    #[test]
    fn plan_flat_single_wraps_multiple() {
        let items = vec![
            make_item("a.txt", false, &PathBuf::new()),
            make_item("b.txt", false, &PathBuf::new()),
        ];
        let req = make_request(
            TopLevelShape::Multiple {
                items: items.clone(),
                count: items.len(),
            },
            "archive",
            OutputLayoutPolicy::FlatSingle,
            SingleRootNamePolicy::Auto,
        );
        let plan = plan_layout(&req);
        assert_eq!(
            plan.kind,
            LayoutPlanKind::CommitWholeTempAsArchiveDir {
                name: "archive".to_string()
            }
        );
        assert_eq!(plan.reason, LayoutDecisionReason::MultipleTopLevelItems);
    }

    #[test]
    fn plan_prefer_archive_name_always_wraps_single_dir() {
        let item = make_item(
            "The Great Gatsby - F. Scott Fitzgerald",
            true,
            &PathBuf::new(),
        );
        let req = make_request(
            TopLevelShape::SingleDir(item),
            "downloads",
            OutputLayoutPolicy::Smart,
            SingleRootNamePolicy::PreferArchiveName,
        );
        let plan = plan_layout(&req);
        assert_eq!(
            plan.kind,
            LayoutPlanKind::CommitSingleDirContentsAsArchiveName
        );
        assert_eq!(
            plan.source,
            PlanSource::SingleDirContents(PathBuf::from("The Great Gatsby - F. Scott Fitzgerald"))
        );
        assert_eq!(plan.target, req.output_root.join("downloads"));
    }

    #[test]
    fn plan_prefer_inner_name_always_collapses_dir() {
        let item = make_item("downloads", true, &PathBuf::new());
        let req = make_request(
            TopLevelShape::SingleDir(item),
            "The Great Gatsby",
            OutputLayoutPolicy::Smart,
            SingleRootNamePolicy::PreferInnerName,
        );
        let plan = plan_layout(&req);
        assert_eq!(plan.kind, LayoutPlanKind::CommitSingleDirAsInnerName);
        assert_eq!(
            plan.source,
            PlanSource::SingleDir(PathBuf::from("downloads"))
        );
        assert_eq!(plan.target, req.output_root.join("downloads"));
    }

    #[test]
    fn plan_prefer_archive_name_always_renames_single_file() {
        let item = make_item("The Great Gatsby.pdf", false, &PathBuf::new());
        let req = make_request(
            TopLevelShape::SingleFile(item),
            "downloads",
            OutputLayoutPolicy::Smart,
            SingleRootNamePolicy::PreferArchiveName,
        );
        let plan = plan_layout(&req);
        assert_eq!(plan.kind, LayoutPlanKind::CommitSingleFileAsArchiveName);
        assert_eq!(plan.target, req.output_root.join("downloads.pdf"));
    }

    #[test]
    fn plan_prefer_inner_name_always_keeps_single_file() {
        let item = make_item("downloads.pdf", false, &PathBuf::new());
        let req = make_request(
            TopLevelShape::SingleFile(item),
            "The Great Gatsby",
            OutputLayoutPolicy::Smart,
            SingleRootNamePolicy::PreferInnerName,
        );
        let plan = plan_layout(&req);
        assert_eq!(plan.kind, LayoutPlanKind::CommitSingleFileAsInnerName);
        assert_eq!(plan.target, req.output_root.join("downloads.pdf"));
    }

    #[test]
    fn plan_layout_populates_warnings() {
        let req = make_request(
            TopLevelShape::Empty,
            "archive",
            OutputLayoutPolicy::Smart,
            SingleRootNamePolicy::Auto,
        );
        let plan = plan_layout(&req);
        assert!(plan.warnings.is_empty());
    }

    #[test]
    fn layout_flat_single_dir_targets_output_root_inner_dir() {
        let item = make_item("ProjectName", true, &PathBuf::new());
        let req = make_request(
            TopLevelShape::SingleDir(item),
            "archive",
            OutputLayoutPolicy::FlatSingle,
            SingleRootNamePolicy::Auto,
        );
        let plan = plan_layout(&req);
        assert_eq!(plan.kind, LayoutPlanKind::CommitSingleDirAsInnerName);
        assert_eq!(plan.target, req.output_root.join("ProjectName"));
    }

    #[test]
    fn layout_flat_single_file_targets_output_root_file() {
        let item = make_item("doc.pdf", false, &PathBuf::new());
        let req = make_request(
            TopLevelShape::SingleFile(item),
            "archive",
            OutputLayoutPolicy::FlatSingle,
            SingleRootNamePolicy::Auto,
        );
        let plan = plan_layout(&req);
        assert_eq!(plan.kind, LayoutPlanKind::CommitSingleFileAsInnerName);
        assert_eq!(plan.target, req.output_root.join("doc.pdf"));
    }

    #[test]
    fn layout_prefer_archive_name_single_dir_flattens_contents() {
        let item = make_item("images", true, &PathBuf::new());
        let req = make_request(
            TopLevelShape::SingleDir(item),
            "archive",
            OutputLayoutPolicy::Smart,
            SingleRootNamePolicy::PreferArchiveName,
        );
        let plan = plan_layout(&req);
        assert_eq!(
            plan.kind,
            LayoutPlanKind::CommitSingleDirContentsAsArchiveName
        );
        assert_eq!(plan.target, req.output_root.join("archive"));
        assert_eq!(
            plan.source,
            PlanSource::SingleDirContents(PathBuf::from("images"))
        );
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
