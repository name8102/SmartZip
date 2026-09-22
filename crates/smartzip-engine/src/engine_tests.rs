//! Unit tests moved out of lib.rs.

use crate::encoding_flow::*;
use crate::interactive::*;
use crate::nested::*;
use crate::policy::*;
use crate::types::*;
use async_trait::async_trait;
use rstest::*;
use smartzip_archive::*;
use smartzip_core::*;
use smartzip_db::{password::PasswordRepository, SmartZipDb};
use smartzip_passwords::*;
use smartzip_scanner::*;
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc, Mutex,
};
use std::time::Duration;

fn engine_with_test_recycler() -> SmartZipEngine {
    let recycler: ArchiveRecycleHandler = Arc::new(std::fs::remove_file);
    SmartZipEngine::default().with_archive_recycler(recycler)
}

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("tests")
        .join("fixtures")
        .join(name)
}

fn password_candidate(
    value: &str,
    source: smartzip_passwords::PasswordSource,
) -> PasswordCandidate {
    PasswordCandidate {
        id: None,
        value: value.to_string(),
        source,
    }
}

#[test]
fn password_order_is_explicit_then_known_then_batch_then_database() {
    use smartzip_passwords::PasswordSource;

    let base = vec![
        password_candidate("cli", PasswordSource::Manual),
        password_candidate("", PasswordSource::Empty),
        password_candidate("db", PasswordSource::Database),
    ];
    let known = password_candidate("known", PasswordSource::Database);
    let batch = vec![password_candidate("batch", PasswordSource::Recent)];

    let db = SmartZipDb::in_memory().unwrap();
    let service = PasswordService::new(PasswordRepository::new(db.connection()));
    let ordered = service.order_candidates(&base, Some(&known), &batch);
    assert_eq!(
        ordered
            .iter()
            .map(|candidate| candidate.value.as_str())
            .collect::<Vec<_>>(),
        vec!["cli", "known", "batch", "", "db"],
    );
}

#[test]
fn detects_empty_file_without_findings() {
    let path = std::env::temp_dir().join(format!("smartzip-engine-empty-{}", std::process::id()));
    std::fs::write(&path, []).unwrap();

    let engine = SmartZipEngine::default();
    let result = engine
        .detect(DetectRequest {
            path: path.clone(),
            scanner: ScannerConfig::default(),
        })
        .unwrap();
    let _ = std::fs::remove_file(path);

    assert!(result.findings.is_empty());
    assert!(matches!(
        result.events.first().unwrap().kind,
        TaskEventKind::Started
    ));
    assert!(matches!(
        result.events.last().unwrap().kind,
        TaskEventKind::Completed
    ));
}

#[test]
fn explicit_root_scan_is_full_while_nested_defaults_stay_bounded() {
    let nested = ScannerConfig::default();
    let root = full_root_scanner_config(&nested);

    assert_eq!(root.mode, ScanMode::Deep);
    assert_eq!(root.max_scan_bytes, None);
    assert_eq!(root.max_findings, usize::MAX);
    assert_eq!(nested.mode, ScanMode::Fast);
    assert_eq!(nested.max_scan_bytes, Some(64 * 1024 * 1024));
}

#[test]
fn root_scan_enqueues_every_eligible_finding_with_unique_output() {
    let root = ExtractionCandidate {
        path: PathBuf::from("/inputs/carrier.mp4"),
        relative_path: PathBuf::from("carrier"),
        depth: 0,
        source: CandidateSource::RootInput,
        detected_format: None,
        embedded_offset: None,
        embedded_size: None,
    };
    let policy = smartzip_core::EmbeddedScanPolicy::default();
    let findings = vec![
        EmbeddedArchiveFinding {
            offset: 100,
            size: Some(policy.min_finding_size_bytes - 1),
            format: ArchiveFormat::Zip,
            confidence: Confidence::High,
            description: String::new(),
        },
        EmbeddedArchiveFinding {
            offset: 200,
            size: Some(policy.min_finding_size_bytes),
            format: ArchiveFormat::Zip,
            confidence: Confidence::High,
            description: String::new(),
        },
        EmbeddedArchiveFinding {
            offset: 300,
            size: None,
            format: ArchiveFormat::SevenZip,
            confidence: Confidence::High,
            description: String::new(),
        },
    ];
    let eligible: Vec<_> = findings
        .into_iter()
        .filter(|finding| finding_meets_min_size(finding, &policy))
        .collect();
    let candidates = root_embedded_candidates(&root, &eligible);

    assert_eq!(candidates.len(), 2);
    assert_eq!(candidates[0].embedded_offset, Some(200));
    assert_eq!(candidates[1].embedded_offset, Some(300));
    assert_ne!(candidates[0].relative_path, candidates[1].relative_path);
    assert!(candidates
        .iter()
        .all(|candidate| candidate.source == CandidateSource::EmbeddedFinding));
}

#[test]
fn zip_encoding_assessment_skips_confirmation_for_ascii_names() {
    let assessment = build_zip_encoding_assessment(ArchiveListing {
        format: Some(ArchiveFormat::Zip),
        entries: vec![smartzip_archive::ArchiveEntry {
            path: PathBuf::from("docs/readme.txt"),
            raw_name: b"docs/readme.txt".to_vec(),
            compressed_size: None,
            uncompressed_size: None,
            is_dir: false,
        }],
    })
    .unwrap();

    assert!(!assessment.should_confirm);
    assert!(assessment.context.suspicious_reasons.is_empty());
}

#[tokio::test]
async fn embedded_ask_without_prompter_skips_archive() {
    let archive = fixture_path("video_7z_pass.mp4");
    let backend = BackendRouter::from_config(&smartzip_config::BackendConfig::default()).unwrap();
    let db = SmartZipDb::in_memory().unwrap();
    let service = PasswordService::new(PasswordRepository::new(db.connection()));
    let output = tempfile::tempdir().unwrap();

    let result = SmartZipEngine::default()
        .with_min_embedded_size_bytes(0)
        .extract_recursive(
            &backend,
            &service,
            ExtractWorkflowRequest {
                inputs: vec![archive],
                output_dir: output.path().to_path_buf(),
                recursion_limit: 1,
                encoding_mode: EncodingMode::Auto,
                scanner: ScannerConfig::default(),
                password_candidates: PasswordCandidateRequest {
                    manual: Vec::new(),
                    clipboard: None,
                    include_empty: false,
                    limit: 8,
                },
                layout_policy: Default::default(),
                single_root_name_policy: Default::default(),
                embedded_scan_mode: smartzip_core::EmbeddedScanMode::Ask,
                dominant_min_ratio: 0.70,
                confirm_large_scan: false,
                force: false,
                limits: Default::default(),
            },
            None,
            None,
        )
        .await
        .unwrap();

    assert!(result.processed.is_empty());
    assert_eq!(result.skipped.len(), 1);
    assert!(result.events.iter().any(|event| matches!(
        event.kind,
        TaskEventKind::EmbeddedArchiveSelectionRequired { .. }
    )));
}

#[test]
fn cbz_extension_is_not_a_business_container() {
    assert_eq!(ext_business_container_kind(Path::new("comic.cbz")), None);
}

#[test]
fn nested_candidate_output_uses_archive_parent_as_global_output_root() {
    let managed_output = PathBuf::from("/managed-output");
    let root = ExtractionCandidate {
        path: PathBuf::from("/inputs/outer.zip"),
        relative_path: PathBuf::from("outer"),
        depth: 0,
        source: CandidateSource::RootInput,
        detected_format: Some(ArchiveFormat::Zip),
        embedded_offset: None,
        embedded_size: None,
    };
    let nested = ExtractionCandidate {
        path: PathBuf::from("/managed-output/outer/inner.zip"),
        relative_path: PathBuf::from("outer/inner"),
        depth: 1,
        source: CandidateSource::ExtractedFile,
        detected_format: Some(ArchiveFormat::Zip),
        embedded_offset: None,
        embedded_size: None,
    };

    assert_eq!(
        output_dir_for_candidate(&managed_output, &root),
        PathBuf::from("/managed-output/outer")
    );
    assert_eq!(
        output_dir_for_candidate(&managed_output, &nested),
        PathBuf::from("/managed-output/outer/inner")
    );
}

#[test]
fn only_regular_extracted_archives_inside_output_are_recyclable() {
    let root = tempfile::tempdir().unwrap();
    let output = root.path().join("output");
    std::fs::create_dir_all(&output).unwrap();
    let archive = output.join("inner.zip");
    std::fs::write(&archive, b"archive").unwrap();

    let mut candidate = ExtractionCandidate {
        path: archive.clone(),
        relative_path: PathBuf::from("inner"),
        depth: 1,
        source: CandidateSource::ExtractedFile,
        detected_format: Some(ArchiveFormat::Zip),
        embedded_offset: None,
        embedded_size: None,
    };
    assert_eq!(
        recyclable_nested_archive_path(&candidate, &output),
        Some(archive.clone())
    );

    candidate.source = CandidateSource::RootInput;
    assert!(recyclable_nested_archive_path(&candidate, &output).is_none());

    candidate.source = CandidateSource::EmbeddedFinding;
    assert!(recyclable_nested_archive_path(&candidate, &output).is_none());

    candidate.source = CandidateSource::ExtractedFile;
    candidate.embedded_offset = Some(0);
    assert_eq!(
        recyclable_nested_archive_path(&candidate, &output),
        Some(archive.clone())
    );

    candidate.embedded_offset = Some(16);
    assert!(recyclable_nested_archive_path(&candidate, &output).is_none());

    candidate.embedded_offset = None;
    candidate.path = root.path().join("outside.zip");
    std::fs::write(&candidate.path, b"archive").unwrap();
    assert!(recyclable_nested_archive_path(&candidate, &output).is_none());
}

#[rstest]
#[case("a.zip", Some(ArchiveFormat::Zip))]
#[case("a.7z", Some(ArchiveFormat::SevenZip))]
#[case("a.rar", Some(ArchiveFormat::Rar))]
#[case("a.tar", Some(ArchiveFormat::Tar))]
#[case("a.gz", Some(ArchiveFormat::Gzip))]
#[case("a.gzip", Some(ArchiveFormat::Gzip))]
#[case("a.tgz", Some(ArchiveFormat::Gzip))]
#[case("a.bz2", Some(ArchiveFormat::Bzip2))]
#[case("a.xz", Some(ArchiveFormat::Xz))]
#[case("a.cab", Some(ArchiveFormat::Cab))]
#[case("a.iso", Some(ArchiveFormat::Iso))]
#[case("a.dmg", Some(ArchiveFormat::Dmg))]
#[case("a.zst", Some(ArchiveFormat::Zstd))]
#[case("a.zstd", Some(ArchiveFormat::Zstd))]
#[case("a.lz4", Some(ArchiveFormat::Lz4))]
#[case("a.lzma", Some(ArchiveFormat::Lzma))]
#[case("a.txt", None)]
#[case("a.bin", None)]
#[case("no-extension", None)]
#[case("a.ZIP", Some(ArchiveFormat::Zip))]
#[case("A.7Z", Some(ArchiveFormat::SevenZip))]
fn format_from_extension_parametrized(#[case] path: &str, #[case] expected: Option<ArchiveFormat>) {
    assert_eq!(format_from_extension(path), expected);
}

#[tokio::test]
async fn recursive_extract_enqueues_nested_archives_and_skips_non_first_volume() {
    let root =
        std::env::temp_dir().join(format!("smartzip-engine-recursive-{}", std::process::id()));
    let input = root.join("root.zip");
    let output = root.join("out");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(&input, b"not really a zip").unwrap();

    let backend = FakeBackend::default();
    let db = SmartZipDb::in_memory().unwrap();
    let service = PasswordService::new(PasswordRepository::new(db.connection()));
    service.add_password("secret", "manual", false).unwrap();

    let engine = engine_with_test_recycler();
    let result = engine
        .extract_recursive(
            &backend,
            &service,
            ExtractWorkflowRequest {
                inputs: vec![input.clone(), root.join("skip.part2.rar")],
                output_dir: output.clone(),
                recursion_limit: 2,
                encoding_mode: EncodingMode::Auto,
                scanner: ScannerConfig::default(),
                password_candidates: PasswordCandidateRequest {
                    include_empty: false,
                    limit: 10,
                    ..PasswordCandidateRequest::default()
                },
                layout_policy: crate::layout::OutputLayoutPolicy::default(),
                single_root_name_policy: crate::layout::SingleRootNamePolicy::default(),
                embedded_scan_mode: smartzip_core::EmbeddedScanMode::default(),
                dominant_min_ratio: 0.70,
                confirm_large_scan: false,
                force: false,
                limits: Default::default(),
            },
            None,
            None,
        )
        .await
        .unwrap();

    let calls = backend.calls.lock().unwrap().clone();
    assert!(calls.iter().any(|path| path.ends_with("root.zip")));
    assert!(calls.iter().any(|path| path.ends_with("nested.zip")));
    assert!(!calls.iter().any(|path| path.ends_with("skip.part2.rar")));
    assert!(result
        .processed
        .iter()
        .any(|candidate| candidate.path == input));
    assert!(
        output.join("root").exists(),
        "root archive should materialize without a depth suffix"
    );
    assert!(
        !output.join("root-d0").exists(),
        "depth is candidate state, not part of the output directory name"
    );
    assert!(result
        .enqueued
        .iter()
        .any(|candidate| candidate.path.ends_with("nested.zip")));
    assert!(result
        .skipped
        .iter()
        .any(|candidate| candidate.path.ends_with("skip.part2.rar")));

    let ranked = PasswordRepository::new(db.connection())
        .ranked_candidates(10)
        .unwrap();
    assert_eq!(ranked[0].success_count, 2);

    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn extract_fails_when_output_target_already_exists() {
    let root =
        std::env::temp_dir().join(format!("smartzip-engine-collision-{}", std::process::id()));
    let input = root.join("root.zip");
    let output = root.join("out");
    // The layout planner targets output_root/archive_stem = out/root.
    // Pre-create it to trigger a collision after layout planning.
    let target = output.join("root");
    std::fs::create_dir_all(&target).unwrap();
    std::fs::write(&input, b"not really a zip").unwrap();

    let backend = FakeBackend::default();
    let db = SmartZipDb::in_memory().unwrap();
    let service = PasswordService::new(PasswordRepository::new(db.connection()));

    let engine = SmartZipEngine::default();
    let result = engine
        .extract_recursive(
            &backend,
            &service,
            ExtractWorkflowRequest {
                inputs: vec![input.clone()],
                output_dir: output,
                recursion_limit: 1,
                encoding_mode: EncodingMode::Auto,
                scanner: ScannerConfig::default(),
                password_candidates: PasswordCandidateRequest::default(),
                layout_policy: crate::layout::OutputLayoutPolicy::default(),
                single_root_name_policy: crate::layout::SingleRootNamePolicy::default(),
                embedded_scan_mode: smartzip_core::EmbeddedScanMode::default(),
                dominant_min_ratio: 0.70,
                confirm_large_scan: false,
                force: false,
                limits: Default::default(),
            },
            None,
            None,
        )
        .await
        .unwrap();

    // With collision-after-layout, backend IS called (extraction happens first),
    // but the collision is detected after layout planning and the archive is skipped.
    assert!(result
        .skipped
        .iter()
        .any(|candidate| candidate.path == input));
    assert!(result.events.iter().any(|event| matches!(
        event.kind,
        TaskEventKind::Failed { ref error } if error.contains("output path already exists")
    )));

    let _ = std::fs::remove_dir_all(root);
}

#[derive(Clone)]
struct DelayedOutputPrompter {
    started: Arc<AtomicBool>,
}

#[async_trait]
impl InteractiveOutputPrompter for DelayedOutputPrompter {
    async fn prompt(
        &self,
        _archive_path: PathBuf,
        _output_path: PathBuf,
    ) -> OutputCollisionStrategy {
        let started = self.started.clone();
        tokio::task::spawn_blocking(move || {
            started.store(true, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(200));
            OutputCollisionStrategy::Skip
        })
        .await
        .unwrap()
    }
}

#[tokio::test]
async fn extract_keeps_other_archives_moving_while_prompt_waits() {
    let root = std::env::temp_dir().join(format!("smartzip-engine-prompt-{}", std::process::id()));
    let conflict = root.join("conflict.zip");
    let other = root.join("other.zip");
    let output = root.join("out");
    std::fs::create_dir_all(&output).unwrap();
    // The layout planner targets output_root/archive_stem = out/conflict.
    // Pre-create it to trigger a collision after layout planning.
    std::fs::create_dir_all(output.join("conflict")).unwrap();
    std::fs::write(&conflict, b"not really a zip").unwrap();
    std::fs::write(&other, b"not really a zip either").unwrap();

    let backend = FakeBackend::default();
    let db = SmartZipDb::in_memory().unwrap();
    let service = PasswordService::new(PasswordRepository::new(db.connection()));
    let prompt_started = Arc::new(AtomicBool::new(false));
    let output_prompter = DelayedOutputPrompter {
        started: prompt_started.clone(),
    };

    let engine = SmartZipEngine::default();
    let result = engine
        .extract_recursive(
            &backend,
            &service,
            ExtractWorkflowRequest {
                inputs: vec![conflict.clone(), other.clone()],
                output_dir: output,
                recursion_limit: 1,
                encoding_mode: EncodingMode::Auto,
                scanner: ScannerConfig::default(),
                password_candidates: PasswordCandidateRequest::default(),
                layout_policy: crate::layout::OutputLayoutPolicy::default(),
                single_root_name_policy: crate::layout::SingleRootNamePolicy::default(),
                embedded_scan_mode: smartzip_core::EmbeddedScanMode::default(),
                dominant_min_ratio: 0.70,
                confirm_large_scan: false,
                force: false,
                limits: Default::default(),
            },
            None,
            Some(&output_prompter),
        )
        .await
        .unwrap();

    // conflict.zip → layout target = out/conflict (exists) → collision → Skip
    let calls = backend.calls.lock().unwrap().clone();
    assert!(calls.iter().any(|path| path.ends_with("conflict.zip")));
    assert!(result
        .skipped
        .iter()
        .any(|candidate| candidate.path == conflict));

    let _ = std::fs::remove_dir_all(root);
}

#[derive(Clone, Default)]
struct EncodingAwareBackend {
    seen_test_encodings: Arc<Mutex<Vec<EncodingMode>>>,
    seen_extract_encodings: Arc<Mutex<Vec<EncodingMode>>>,
}

#[async_trait]
impl ArchiveExecutor for EncodingAwareBackend {
    async fn probe(&self, path: &std::path::Path) -> smartzip_core::Result<ArchiveProbe> {
        Ok(ArchiveProbe {
            path: path.to_path_buf(),
            format: Some(ArchiveFormat::Zip),
            encrypted: Some(false),
            supported: true,
        })
    }

    async fn list(&self, _request: ListRequest) -> smartzip_core::Result<ArchiveListing> {
        Ok(ArchiveListing {
            format: Some(ArchiveFormat::Zip),
            entries: Vec::new(),
        })
    }

    async fn test(&self, request: TestRequest) -> smartzip_core::Result<TestResult> {
        self.seen_test_encodings
            .lock()
            .unwrap()
            .push(request.encoding);
        Ok(TestResult {
            ok: true,
            encrypted: Some(false),
            ..TestResult::default()
        })
    }

    async fn extract(
        &self,
        request: ExtractArchiveRequest,
    ) -> smartzip_core::Result<ExtractArchiveResult> {
        self.seen_extract_encodings
            .lock()
            .unwrap()
            .push(request.encoding);
        std::fs::create_dir_all(&request.output_dir).map_err(|source| {
            smartzip_core::SmartZipError::io(Some(request.output_dir.clone()), source)
        })?;
        Ok(ExtractArchiveResult {
            output_dir: request.output_dir,
            encrypted: None,
        })
    }

    async fn compress(
        &self,
        request: CompressArchiveRequest,
    ) -> smartzip_core::Result<CompressArchiveResult> {
        Ok(CompressArchiveResult {
            output: request.output,
        })
    }
}

#[tokio::test]
async fn extraction_preserves_encoding_without_full_test_prepass() {
    let root =
        std::env::temp_dir().join(format!("smartzip-engine-encoding-{}", std::process::id()));
    let input = root.join("root.zip");
    let output = root.join("out");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(&input, b"not really a zip").unwrap();

    let backend = EncodingAwareBackend::default();
    let db = SmartZipDb::in_memory().unwrap();
    let service = PasswordService::new(PasswordRepository::new(db.connection()));

    let engine = SmartZipEngine::default();
    let result = engine
        .extract_recursive(
            &backend,
            &service,
            ExtractWorkflowRequest {
                inputs: vec![input.clone()],
                output_dir: output,
                recursion_limit: 0,
                encoding_mode: EncodingMode::Override("gbk".into()),
                scanner: ScannerConfig::default(),
                password_candidates: PasswordCandidateRequest::default(),
                layout_policy: crate::layout::OutputLayoutPolicy::default(),
                single_root_name_policy: crate::layout::SingleRootNamePolicy::default(),
                embedded_scan_mode: smartzip_core::EmbeddedScanMode::default(),
                dominant_min_ratio: 0.70,
                confirm_large_scan: false,
                force: false,
                limits: Default::default(),
            },
            None,
            None,
        )
        .await
        .unwrap();

    assert_eq!(result.processed.len(), 1);
    assert!(backend.seen_test_encodings.lock().unwrap().is_empty());
    assert_eq!(
        backend.seen_extract_encodings.lock().unwrap().as_slice(),
        &[EncodingMode::Override("gbk".into())]
    );

    let _ = std::fs::remove_dir_all(root);
}

#[derive(Clone, Default)]
struct FailingExtractBackend;

#[async_trait]
impl ArchiveExecutor for FailingExtractBackend {
    async fn probe(&self, path: &std::path::Path) -> smartzip_core::Result<ArchiveProbe> {
        Ok(ArchiveProbe {
            path: path.to_path_buf(),
            format: Some(ArchiveFormat::Zip),
            encrypted: Some(true),
            supported: true,
        })
    }

    async fn list(&self, _request: ListRequest) -> smartzip_core::Result<ArchiveListing> {
        Ok(ArchiveListing {
            format: Some(ArchiveFormat::Zip),
            entries: Vec::new(),
        })
    }

    async fn test(&self, request: TestRequest) -> smartzip_core::Result<TestResult> {
        Err(smartzip_core::SmartZipError::BackendFailed {
            backend: "test-backend".into(),
            exit_code: Some(2),
            stderr: format!("i/o failure while testing {}", request.archive.display()),
        })
    }

    async fn extract(
        &self,
        request: ExtractArchiveRequest,
    ) -> smartzip_core::Result<ExtractArchiveResult> {
        Err(smartzip_core::SmartZipError::BackendFailed {
            backend: "extract-backend".into(),
            exit_code: Some(2),
            stderr: format!("i/o failure while extracting {}", request.archive.display()),
        })
    }

    async fn compress(
        &self,
        request: CompressArchiveRequest,
    ) -> smartzip_core::Result<CompressArchiveResult> {
        Ok(CompressArchiveResult {
            output: request.output,
        })
    }
}

#[derive(Default)]
struct FailFirstExtractBackend {
    calls: AtomicUsize,
}

#[async_trait]
impl ArchiveExecutor for FailFirstExtractBackend {
    async fn probe(&self, path: &Path) -> smartzip_core::Result<ArchiveProbe> {
        Ok(ArchiveProbe {
            path: path.to_path_buf(),
            format: Some(ArchiveFormat::Zip),
            encrypted: Some(false),
            supported: true,
        })
    }

    async fn list(&self, _request: ListRequest) -> smartzip_core::Result<ArchiveListing> {
        Ok(ArchiveListing {
            format: Some(ArchiveFormat::Zip),
            entries: Vec::new(),
        })
    }

    async fn test(&self, _request: TestRequest) -> smartzip_core::Result<TestResult> {
        unreachable!("extraction does not run a full test pass")
    }

    async fn extract(
        &self,
        request: ExtractArchiveRequest,
    ) -> smartzip_core::Result<ExtractArchiveResult> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if call == 0 {
            tokio::time::sleep(Duration::from_millis(50)).await;
            return Err(smartzip_core::SmartZipError::BackendFailed {
                backend: "fail-first".into(),
                exit_code: Some(2),
                stderr: "first root failed".into(),
            });
        }
        std::fs::create_dir_all(&request.output_dir)
            .map_err(|error| smartzip_core::SmartZipError::io(None, error))?;
        std::fs::write(request.output_dir.join("unexpected"), b"unexpected")
            .map_err(|error| smartzip_core::SmartZipError::io(None, error))?;
        Ok(ExtractArchiveResult {
            output_dir: request.output_dir,
            encrypted: Some(false),
        })
    }

    async fn compress(
        &self,
        request: CompressArchiveRequest,
    ) -> smartzip_core::Result<CompressArchiveResult> {
        Ok(CompressArchiveResult {
            output: request.output,
        })
    }
}

struct NestedThenFailBackend {
    calls: Mutex<Vec<String>>,
    stage_entered: Arc<tokio::sync::Notify>,
}

#[async_trait]
impl ArchiveExecutor for NestedThenFailBackend {
    async fn probe(&self, path: &Path) -> smartzip_core::Result<ArchiveProbe> {
        Ok(ArchiveProbe {
            path: path.to_path_buf(),
            format: Some(ArchiveFormat::Zip),
            encrypted: Some(false),
            supported: true,
        })
    }

    async fn list(&self, _request: ListRequest) -> smartzip_core::Result<ArchiveListing> {
        Ok(ArchiveListing {
            format: Some(ArchiveFormat::Zip),
            entries: Vec::new(),
        })
    }

    async fn test(&self, _request: TestRequest) -> smartzip_core::Result<TestResult> {
        unreachable!("extraction does not run a full test pass")
    }

    async fn extract(
        &self,
        request: ExtractArchiveRequest,
    ) -> smartzip_core::Result<ExtractArchiveResult> {
        let name = request
            .archive
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        self.calls.lock().unwrap().push(name.clone());
        match name.as_str() {
            "first.zip" => {
                tokio::task::yield_now().await;
                std::fs::create_dir_all(&request.output_dir)
                    .map_err(|error| smartzip_core::SmartZipError::io(None, error))?;
                std::fs::write(request.output_dir.join("nested.zip"), b"nested")
                    .map_err(|error| smartzip_core::SmartZipError::io(None, error))?;
                Ok(ExtractArchiveResult {
                    output_dir: request.output_dir,
                    encrypted: Some(false),
                })
            }
            "second.zip" => {
                tokio::time::timeout(Duration::from_secs(2), self.stage_entered.notified())
                    .await
                    .expect("nested node should enter its execution stage");
                Err(smartzip_core::SmartZipError::BackendFailed {
                    backend: "nested-then-fail".into(),
                    exit_code: Some(2),
                    stderr: "second root failed".into(),
                })
            }
            "nested.zip" => {
                std::fs::create_dir_all(&request.output_dir)
                    .map_err(|error| smartzip_core::SmartZipError::io(None, error))?;
                std::fs::write(request.output_dir.join("unexpected"), b"unexpected")
                    .map_err(|error| smartzip_core::SmartZipError::io(None, error))?;
                Ok(ExtractArchiveResult {
                    output_dir: request.output_dir,
                    encrypted: Some(false),
                })
            }
            other => panic!("unexpected archive {other}"),
        }
    }

    async fn compress(
        &self,
        request: CompressArchiveRequest,
    ) -> smartzip_core::Result<CompressArchiveResult> {
        Ok(CompressArchiveResult {
            output: request.output,
        })
    }
}

struct ScanThenFailBackend {
    stage_entered: Arc<tokio::sync::Notify>,
    calls: AtomicUsize,
}

#[async_trait]
impl ArchiveExecutor for ScanThenFailBackend {
    async fn probe(&self, path: &Path) -> smartzip_core::Result<ArchiveProbe> {
        Ok(ArchiveProbe {
            path: path.to_path_buf(),
            format: Some(ArchiveFormat::Zip),
            encrypted: Some(false),
            supported: true,
        })
    }

    async fn list(&self, _request: ListRequest) -> smartzip_core::Result<ArchiveListing> {
        Ok(ArchiveListing {
            format: Some(ArchiveFormat::Zip),
            entries: Vec::new(),
        })
    }

    async fn test(&self, _request: TestRequest) -> smartzip_core::Result<TestResult> {
        unreachable!("extraction does not run a full test pass")
    }

    async fn extract(
        &self,
        request: ExtractArchiveRequest,
    ) -> smartzip_core::Result<ExtractArchiveResult> {
        assert_eq!(request.archive.file_name().unwrap(), "fail.zip");
        self.calls.fetch_add(1, Ordering::SeqCst);
        tokio::time::timeout(Duration::from_secs(2), self.stage_entered.notified())
            .await
            .expect("the other root should start its scan");
        Err(smartzip_core::SmartZipError::BackendFailed {
            backend: "scan-then-fail".into(),
            exit_code: Some(2),
            stderr: "root failed while its sibling was scanning".into(),
        })
    }

    async fn compress(
        &self,
        request: CompressArchiveRequest,
    ) -> smartzip_core::Result<CompressArchiveResult> {
        Ok(CompressArchiveResult {
            output: request.output,
        })
    }
}

struct StageWaitRecorder {
    wait_node: Mutex<Option<NodeId>>,
    wait_stage: crate::coordinator::Stage,
    block_at_stage: bool,
    stage_entered: Arc<tokio::sync::Notify>,
    outcomes: Mutex<Vec<(NodeId, crate::NodeOutcome)>>,
    transitions: Arc<Mutex<Vec<(NodeId, crate::coordinator::Stage)>>>,
}

#[async_trait(?Send)]
impl crate::ExecutionStateRecorder for StageWaitRecorder {
    async fn enqueue_child(
        &self,
        _task_id: &TaskId,
        node_id: &NodeId,
        _parent_id: &NodeId,
        _root_id: &NodeId,
        _candidate: &crate::ExtractionCandidate,
        _generation: u64,
    ) -> smartzip_core::Result<bool> {
        let mut wait_node = self.wait_node.lock().unwrap();
        if wait_node.is_none() {
            *wait_node = Some(node_id.clone());
        }
        Ok(true)
    }

    async fn transition(
        &self,
        _task_id: &TaskId,
        node_id: &NodeId,
        _generation: u64,
        _from: &str,
        _to: &str,
        stage: crate::coordinator::Stage,
        _attempt_id: Option<&AttemptId>,
        cancellation: &tokio_util::sync::CancellationToken,
    ) -> smartzip_core::Result<bool> {
        self.transitions
            .lock()
            .unwrap()
            .push((node_id.clone(), stage));
        let is_wait_node = self.wait_node.lock().unwrap().as_ref() == Some(node_id);
        if is_wait_node && stage == self.wait_stage {
            self.stage_entered.notify_one();
            if self.block_at_stage {
                cancellation.cancelled().await;
                return Err(smartzip_core::SmartZipError::Cancelled);
            }
        }
        Ok(true)
    }

    fn release_stage(&self, _task_id: &TaskId, _node_id: &NodeId) {}

    async fn record_staging(
        &self,
        _task_id: &TaskId,
        _node_id: &NodeId,
        _generation: u64,
        _path: &Path,
    ) -> smartzip_core::Result<bool> {
        Ok(true)
    }

    async fn wait_for_decision(
        &self,
        _task_id: &TaskId,
        _node_id: &NodeId,
        _generation: u64,
        _stage: crate::coordinator::Stage,
        _decision_id: &DecisionId,
        _kind: &str,
        _evidence: &str,
    ) -> smartzip_core::Result<bool> {
        Ok(true)
    }

    async fn accept_decision(
        &self,
        _task_id: &TaskId,
        _node_id: &NodeId,
        _generation: u64,
        _decision_id: &DecisionId,
    ) -> smartzip_core::Result<bool> {
        Ok(true)
    }

    async fn begin_commit(
        &self,
        _task_id: &TaskId,
        _node_id: &NodeId,
        _generation: u64,
        _intent: &crate::CommitIntent,
    ) -> smartzip_core::Result<bool> {
        Ok(true)
    }

    async fn commit_published(
        &self,
        _task_id: &TaskId,
        _node_id: &NodeId,
        _generation: u64,
        _intent: &crate::CommitIntent,
    ) -> smartzip_core::Result<bool> {
        Ok(true)
    }

    async fn abort_commit(
        &self,
        _task_id: &TaskId,
        _node_id: &NodeId,
        _generation: u64,
    ) -> smartzip_core::Result<bool> {
        Ok(true)
    }

    async fn finish_node(
        &self,
        _task_id: &TaskId,
        node_id: &NodeId,
        _generation: u64,
        outcome: crate::NodeOutcome,
    ) -> smartzip_core::Result<bool> {
        self.outcomes
            .lock()
            .unwrap()
            .push((node_id.clone(), outcome));
        Ok(true)
    }
}

#[tokio::test(flavor = "current_thread")]
async fn stop_on_error_cancels_other_roots_waiting_for_backend_capacity() {
    let root = tempfile::tempdir().unwrap();
    let inputs = vec![
        root.path().join("first.zip"),
        root.path().join("second.zip"),
        root.path().join("third.zip"),
        root.path().join("fourth.zip"),
    ];
    for input in &inputs {
        std::fs::write(input, b"archive").unwrap();
    }
    let mut config = smartzip_config::SmartZipConfig::default();
    config.extraction.on_error = smartzip_config::OnError::Stop;
    config.extraction.recursion.enabled = false;
    config.extraction.embedded.root = smartzip_config::RootScan::Off;
    config.extraction.embedded.nested = smartzip_config::NestedScan::Off;
    let policy = crate::CompiledRunPolicy::compile(smartzip_config::ResolvedConfig {
        values: config,
        origins: Default::default(),
        path: None,
        diagnostics: Vec::new(),
    })
    .unwrap();
    let request = policy
        .resolve_request(ExtractWorkflowRequest {
            inputs: inputs.clone(),
            output_dir: root.path().join("out"),
            recursion_limit: 0,
            encoding_mode: EncodingMode::Auto,
            scanner: ScannerConfig::default(),
            password_candidates: PasswordCandidateRequest {
                manual: Vec::new(),
                clipboard: None,
                include_empty: true,
                limit: 8,
            },
            layout_policy: Default::default(),
            single_root_name_policy: Default::default(),
            embedded_scan_mode: smartzip_core::EmbeddedScanMode::default(),
            dominant_min_ratio: 0.7,
            confirm_large_scan: false,
            force: false,
            limits: Default::default(),
        })
        .unwrap();
    let identity = crate::ExtractTaskIdentity::new(&inputs);
    let execution_db = root.path().join("execution.db");
    let store = Arc::new(crate::state_store::StateStore::start(&execution_db).unwrap());
    let execution = crate::execution_runtime::ExecutionCoordinator::new(
        store,
        crate::coordinator::ResourceCapacity {
            cpu_units: 2,
            backend_processes: 1,
            ..Default::default()
        },
    )
    .unwrap();
    execution
        .submit(crate::state_store::TaskSubmission {
            task_id: identity.task_id.clone(),
            kind: "extract".into(),
            output_path: Some(request.output_dir.clone()),
            started_at: smartzip_db::timestamp::now_utc_iso8601(),
            inputs_json: serde_json::to_string(&inputs).unwrap(),
            config_snapshot_json: serde_json::to_string(
                &crate::state_store::PersistedExtractPlan::new(
                    policy.values().clone(),
                    request.clone(),
                ),
            )
            .unwrap(),
            priority: smartzip_db::task_execution::Priority::Normal,
            queue_position: 0,
            recoverable: true,
            roots: identity
                .roots
                .iter()
                .map(|root| crate::state_store::NodeSubmission {
                    node_id: root.node_id.clone(),
                    parent_id: None,
                    root_id: root.root_id.clone(),
                    input_path: root.candidate.path.clone(),
                    input_ref_json: serde_json::to_string(&root.candidate).unwrap(),
                    config_revision: 0,
                    generation: 0,
                })
                .collect(),
        })
        .await
        .unwrap();
    let backend = FailFirstExtractBackend::default();
    let db = SmartZipDb::in_memory().unwrap();
    let passwords = PasswordService::new(PasswordRepository::new(db.connection()));

    let result = SmartZipEngine::default()
        .with_run_policy(policy)
        .extract_task(
            identity.clone(),
            &backend,
            &passwords,
            request,
            crate::ExtractInteraction::default(),
            crate::ExtractObserver {
                listener: None,
                history: None,
                execution: Some(&execution),
            },
        )
        .await
        .unwrap();

    assert_eq!(backend.calls.load(Ordering::SeqCst), 1);
    assert_eq!(result.status, crate::history::TaskCompletionStatus::Failed);
    assert_eq!(result.status.exit_code(), 1);
    assert_eq!(result.failed_count, 1);
    assert!(!root.path().join("out").join("unexpected").exists());

    let persisted = SmartZipDb::open_read_only(&execution_db).unwrap();
    let task_status: String = persisted
        .connection()
        .query_row(
            "SELECT status FROM tasks WHERE id=?1",
            [&identity.task_id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    let failed: i64 = persisted
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM file_extractions WHERE task_id=?1 AND status='failed'",
            [&identity.task_id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    let skipped: i64 = persisted
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM file_extractions WHERE task_id=?1 AND status='skipped'",
            [&identity.task_id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(task_status, "failed");
    assert_eq!((failed, skipped), (1, 3));
    execution.release_task(&identity.task_id);
}

#[tokio::test(flavor = "current_thread")]
async fn stop_on_error_finishes_waiting_nested_node_without_rewriting_its_root() {
    let root = tempfile::tempdir().unwrap();
    let inputs = vec![
        root.path().join("first.zip"),
        root.path().join("second.zip"),
    ];
    for input in &inputs {
        std::fs::write(input, b"archive").unwrap();
    }
    let mut config = smartzip_config::SmartZipConfig::default();
    config.extraction.on_error = smartzip_config::OnError::Stop;
    config.extraction.recursion.enabled = true;
    config.extraction.recursion.max_depth = 1;
    config.extraction.embedded.root = smartzip_config::RootScan::Off;
    config.extraction.embedded.nested = smartzip_config::NestedScan::Off;
    let policy = crate::CompiledRunPolicy::compile(smartzip_config::ResolvedConfig {
        values: config,
        origins: Default::default(),
        path: None,
        diagnostics: Vec::new(),
    })
    .unwrap();
    let request = policy
        .resolve_request(ExtractWorkflowRequest {
            inputs: inputs.clone(),
            output_dir: root.path().join("out"),
            recursion_limit: 1,
            encoding_mode: EncodingMode::Auto,
            scanner: ScannerConfig::default(),
            password_candidates: PasswordCandidateRequest {
                manual: Vec::new(),
                clipboard: None,
                include_empty: true,
                limit: 8,
            },
            layout_policy: Default::default(),
            single_root_name_policy: Default::default(),
            embedded_scan_mode: smartzip_core::EmbeddedScanMode::default(),
            dominant_min_ratio: 0.7,
            confirm_large_scan: false,
            force: false,
            limits: Default::default(),
        })
        .unwrap();
    let identity = crate::ExtractTaskIdentity::new(&inputs);
    let stage_entered = Arc::new(tokio::sync::Notify::new());
    let execution = StageWaitRecorder {
        wait_node: Mutex::new(None),
        wait_stage: crate::coordinator::Stage::ResolveInputs,
        block_at_stage: true,
        stage_entered: stage_entered.clone(),
        outcomes: Mutex::new(Vec::new()),
        transitions: Default::default(),
    };
    let backend = NestedThenFailBackend {
        calls: Mutex::new(Vec::new()),
        stage_entered,
    };
    let db = SmartZipDb::in_memory().unwrap();
    let passwords = PasswordService::new(PasswordRepository::new(db.connection()));

    let result = SmartZipEngine::default()
        .with_run_policy(policy)
        .extract_task(
            identity.clone(),
            &backend,
            &passwords,
            request,
            crate::ExtractInteraction::default(),
            crate::ExtractObserver {
                listener: None,
                history: None,
                execution: Some(&execution),
            },
        )
        .await
        .unwrap();

    assert_eq!(result.status, crate::history::TaskCompletionStatus::Partial);
    assert_eq!(result.failed_count, 1);
    assert_eq!(
        backend.calls.lock().unwrap().as_slice(),
        ["first.zip", "second.zip"]
    );
    let wait_node = execution.wait_node.lock().unwrap().clone().unwrap();
    let outcomes = execution.outcomes.lock().unwrap();
    assert_eq!(outcomes.len(), 3);
    let first_root = outcomes
        .iter()
        .find(|(node_id, _)| node_id == &identity.roots[0].node_id)
        .unwrap();
    let second_root = outcomes
        .iter()
        .find(|(node_id, _)| node_id == &identity.roots[1].node_id)
        .unwrap();
    let nested = outcomes
        .iter()
        .find(|(node_id, _)| node_id == &wait_node)
        .unwrap();
    assert_eq!(first_root.1.status, "extracted");
    assert!(first_root.1.committed);
    assert_eq!(second_root.1.status, "failed");
    assert_eq!(nested.1.status, "skipped");
    assert_eq!(nested.1.reason.as_deref(), Some("task_stopped"));
}

#[tokio::test(flavor = "current_thread")]
async fn stop_on_error_finishes_a_root_whose_scan_is_running() {
    let root = tempfile::tempdir().unwrap();
    let scanning = root.path().join("scan.bin");
    let failing = root.path().join("fail.zip");
    std::fs::File::create(&scanning)
        .unwrap()
        .set_len(256 * 1024 * 1024)
        .unwrap();
    std::fs::write(&failing, b"archive").unwrap();
    let inputs = vec![scanning, failing];
    let mut config = smartzip_config::SmartZipConfig::default();
    config.extraction.on_error = smartzip_config::OnError::Stop;
    config.extraction.recursion.enabled = false;
    config.extraction.embedded.root = smartzip_config::RootScan::All;
    config.extraction.embedded.nested = smartzip_config::NestedScan::Off;
    let policy = crate::CompiledRunPolicy::compile(smartzip_config::ResolvedConfig {
        values: config,
        origins: Default::default(),
        path: None,
        diagnostics: Vec::new(),
    })
    .unwrap();
    let request = policy
        .resolve_request(ExtractWorkflowRequest {
            inputs: inputs.clone(),
            output_dir: root.path().join("out"),
            recursion_limit: 0,
            encoding_mode: EncodingMode::Auto,
            scanner: ScannerConfig::default(),
            password_candidates: PasswordCandidateRequest {
                manual: Vec::new(),
                clipboard: None,
                include_empty: true,
                limit: 8,
            },
            layout_policy: Default::default(),
            single_root_name_policy: Default::default(),
            embedded_scan_mode: smartzip_core::EmbeddedScanMode::default(),
            dominant_min_ratio: 0.7,
            confirm_large_scan: false,
            force: false,
            limits: Default::default(),
        })
        .unwrap();
    let identity = crate::ExtractTaskIdentity::new(&inputs);
    let stage_entered = Arc::new(tokio::sync::Notify::new());
    let execution = StageWaitRecorder {
        wait_node: Mutex::new(Some(identity.roots[0].node_id.clone())),
        wait_stage: crate::coordinator::Stage::ScanEmbedded,
        block_at_stage: false,
        stage_entered: stage_entered.clone(),
        outcomes: Mutex::new(Vec::new()),
        transitions: Default::default(),
    };
    let backend = ScanThenFailBackend {
        stage_entered,
        calls: AtomicUsize::new(0),
    };
    let db = SmartZipDb::in_memory().unwrap();
    let passwords = PasswordService::new(PasswordRepository::new(db.connection()));

    let result = SmartZipEngine::default()
        .with_run_policy(policy)
        .extract_task(
            identity.clone(),
            &backend,
            &passwords,
            request,
            crate::ExtractInteraction::default(),
            crate::ExtractObserver {
                listener: None,
                history: None,
                execution: Some(&execution),
            },
        )
        .await
        .unwrap();

    assert_eq!(backend.calls.load(Ordering::SeqCst), 1);
    assert_eq!(result.status, crate::history::TaskCompletionStatus::Failed);
    assert_eq!(result.failed_count, 1);
    let outcomes = execution.outcomes.lock().unwrap();
    assert_eq!(outcomes.len(), 2);
    let scan = outcomes
        .iter()
        .find(|(node_id, _)| node_id == &identity.roots[0].node_id)
        .unwrap();
    let failure = outcomes
        .iter()
        .find(|(node_id, _)| node_id == &identity.roots[1].node_id)
        .unwrap();
    assert_eq!(scan.1.status, "skipped");
    assert_eq!(scan.1.reason.as_deref(), Some("task_stopped"));
    assert_eq!(failure.1.status, "failed");
}

#[tokio::test]
async fn backend_failures_do_not_record_password_failures() {
    let root = std::env::temp_dir().join(format!(
        "smartzip-engine-backend-fail-{}",
        std::process::id()
    ));
    let input = root.join("root.zip");
    let output = root.join("out");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(&input, b"not really a zip").unwrap();

    let backend = FailingExtractBackend;
    let db = SmartZipDb::in_memory().unwrap();
    let repo = PasswordRepository::new(db.connection());
    let password_id = repo
        .upsert(smartzip_db::password::NewPassword {
            value: "candidate-password",
            source: "test",
            pinned: false,
        })
        .unwrap();
    let service = PasswordService::new(PasswordRepository::new(db.connection()));

    let engine = SmartZipEngine::default();
    let result = engine
        .extract_recursive(
            &backend,
            &service,
            ExtractWorkflowRequest {
                inputs: vec![input],
                output_dir: output,
                recursion_limit: 0,
                encoding_mode: EncodingMode::Auto,
                scanner: ScannerConfig::default(),
                password_candidates: PasswordCandidateRequest {
                    manual: Vec::new(),
                    clipboard: None,
                    include_empty: false,
                    limit: 8,
                },
                layout_policy: crate::layout::OutputLayoutPolicy::default(),
                single_root_name_policy: crate::layout::SingleRootNamePolicy::default(),
                embedded_scan_mode: smartzip_core::EmbeddedScanMode::default(),
                dominant_min_ratio: 0.70,
                confirm_large_scan: false,
                force: false,
                limits: Default::default(),
            },
            None,
            None,
        )
        .await
        .unwrap();

    assert!(result.processed.is_empty());
    let stored = PasswordRepository::new(db.connection())
        .ranked_candidates(10)
        .unwrap()
        .into_iter()
        .find(|record| record.id == password_id)
        .unwrap();
    assert_eq!(stored.failure_count, 0);

    let _ = std::fs::remove_dir_all(root);
}

#[derive(Default, Clone)]
struct FakeBackend {
    calls: Arc<Mutex<Vec<String>>>,
}

struct CancelAfterPublish {
    cancellation: tokio_util::sync::CancellationToken,
    intents: Mutex<Vec<crate::CommitIntent>>,
    outcomes: Mutex<Vec<crate::NodeOutcome>>,
}

#[async_trait(?Send)]
impl crate::ExecutionStateRecorder for CancelAfterPublish {
    async fn enqueue_child(
        &self,
        _task_id: &TaskId,
        _node_id: &NodeId,
        _parent_id: &NodeId,
        _root_id: &NodeId,
        _candidate: &crate::ExtractionCandidate,
        _generation: u64,
    ) -> smartzip_core::Result<bool> {
        Ok(true)
    }

    async fn transition(
        &self,
        _task_id: &TaskId,
        _node_id: &NodeId,
        _generation: u64,
        _from: &str,
        _to: &str,
        stage: crate::coordinator::Stage,
        _attempt_id: Option<&AttemptId>,
        cancellation: &tokio_util::sync::CancellationToken,
    ) -> smartzip_core::Result<bool> {
        if stage == crate::coordinator::Stage::DiscoverChildren && cancellation.is_cancelled() {
            return Err(smartzip_core::SmartZipError::Cancelled);
        }
        Ok(true)
    }

    fn release_stage(&self, _task_id: &TaskId, _node_id: &NodeId) {}

    async fn record_staging(
        &self,
        _task_id: &TaskId,
        _node_id: &NodeId,
        _generation: u64,
        _path: &Path,
    ) -> smartzip_core::Result<bool> {
        Ok(true)
    }

    async fn wait_for_decision(
        &self,
        _task_id: &TaskId,
        _node_id: &NodeId,
        _generation: u64,
        _stage: crate::coordinator::Stage,
        _decision_id: &DecisionId,
        _kind: &str,
        _evidence: &str,
    ) -> smartzip_core::Result<bool> {
        Ok(true)
    }

    async fn accept_decision(
        &self,
        _task_id: &TaskId,
        _node_id: &NodeId,
        _generation: u64,
        _decision_id: &DecisionId,
    ) -> smartzip_core::Result<bool> {
        Ok(true)
    }

    async fn begin_commit(
        &self,
        _task_id: &TaskId,
        _node_id: &NodeId,
        _generation: u64,
        intent: &crate::CommitIntent,
    ) -> smartzip_core::Result<bool> {
        self.intents.lock().unwrap().push(intent.clone());
        Ok(true)
    }

    async fn commit_published(
        &self,
        _task_id: &TaskId,
        _node_id: &NodeId,
        _generation: u64,
        _intent: &crate::CommitIntent,
    ) -> smartzip_core::Result<bool> {
        self.cancellation.cancel();
        Ok(true)
    }

    async fn abort_commit(
        &self,
        _task_id: &TaskId,
        _node_id: &NodeId,
        _generation: u64,
    ) -> smartzip_core::Result<bool> {
        Ok(true)
    }

    async fn finish_node(
        &self,
        _task_id: &TaskId,
        _node_id: &NodeId,
        _generation: u64,
        outcome: crate::NodeOutcome,
    ) -> smartzip_core::Result<bool> {
        self.outcomes.lock().unwrap().push(outcome);
        Ok(true)
    }
}

#[derive(Default, Clone)]
struct BatchPasswordBackend {
    attempted_passwords: Arc<Mutex<Vec<Option<String>>>>,
}

#[async_trait]
impl ArchiveExecutor for BatchPasswordBackend {
    async fn probe(&self, path: &Path) -> smartzip_core::Result<ArchiveProbe> {
        Ok(ArchiveProbe {
            path: path.to_path_buf(),
            format: Some(ArchiveFormat::Zip),
            encrypted: Some(true),
            supported: true,
        })
    }

    async fn list(&self, _request: ListRequest) -> smartzip_core::Result<ArchiveListing> {
        Ok(ArchiveListing {
            format: Some(ArchiveFormat::Zip),
            entries: Vec::new(),
        })
    }

    async fn test(&self, _request: TestRequest) -> smartzip_core::Result<TestResult> {
        panic!("password attempts must extract directly, never run a full test");
    }

    async fn extract(
        &self,
        request: ExtractArchiveRequest,
    ) -> smartzip_core::Result<ExtractArchiveResult> {
        self.attempted_passwords
            .lock()
            .unwrap()
            .push(request.password.clone());
        if request.password.as_deref() != Some("batch-secret") {
            std::fs::write(
                request.output_dir.join("wrong-partial.txt"),
                b"discard this",
            )
            .unwrap();
            return Err(smartzip_core::SmartZipError::PasswordIndeterminate {
                path: request.archive,
            });
        }
        std::fs::create_dir_all(&request.output_dir).map_err(|source| {
            smartzip_core::SmartZipError::io(Some(request.output_dir.clone()), source)
        })?;
        std::fs::write(request.output_dir.join("content.txt"), b"content").map_err(|source| {
            smartzip_core::SmartZipError::io(Some(request.output_dir.clone()), source)
        })?;
        Ok(ExtractArchiveResult {
            output_dir: request.output_dir,
            encrypted: Some(true),
        })
    }

    async fn compress(
        &self,
        request: CompressArchiveRequest,
    ) -> smartzip_core::Result<CompressArchiveResult> {
        Ok(CompressArchiveResult {
            output: request.output,
        })
    }
}

struct CountingPasswordPrompter {
    calls: AtomicUsize,
}

#[async_trait]
impl InteractivePasswordPrompter for CountingPasswordPrompter {
    async fn prompt(&self, _archive_path: &Path) -> Option<String> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        Some(
            if call == 0 {
                "wrong-first"
            } else {
                "batch-secret"
            }
            .to_string(),
        )
    }
}

#[tokio::test]
async fn interactive_password_is_reused_for_later_files_in_same_batch() {
    let root = tempfile::tempdir().unwrap();
    let first = root.path().join("first.zip");
    let second = root.path().join("second.zip");
    std::fs::write(&first, b"first archive").unwrap();
    std::fs::write(&second, b"second archive").unwrap();

    let backend = BatchPasswordBackend::default();
    let db = SmartZipDb::in_memory().unwrap();
    let service = PasswordService::new(PasswordRepository::new(db.connection()));
    let prompter = CountingPasswordPrompter {
        calls: AtomicUsize::new(0),
    };

    let result = SmartZipEngine::default()
        .extract_recursive_interactive(
            &backend,
            &service,
            ExtractWorkflowRequest {
                inputs: vec![first, second],
                output_dir: root.path().join("out"),
                recursion_limit: 0,
                encoding_mode: EncodingMode::Auto,
                scanner: ScannerConfig::default(),
                password_candidates: PasswordCandidateRequest {
                    manual: Vec::new(),
                    clipboard: None,
                    include_empty: false,
                    limit: 8,
                },
                layout_policy: Default::default(),
                single_root_name_policy: Default::default(),
                embedded_scan_mode: smartzip_core::EmbeddedScanMode::default(),
                dominant_min_ratio: 0.70,
                confirm_large_scan: false,
                force: false,
                limits: Default::default(),
            },
            Some(&prompter),
            None,
            None,
            None,
        )
        .await
        .unwrap();

    assert_eq!(result.processed.len(), 2);
    assert_eq!(
        prompter.calls.load(Ordering::SeqCst),
        2,
        "the password accepted for the first file should be reused in-memory for the second",
    );
    assert_eq!(
        backend.attempted_passwords.lock().unwrap().as_slice(),
        &[
            Some("wrong-first".to_string()),
            Some("batch-secret".to_string()),
            Some("batch-secret".to_string())
        ],
    );
    let stored = PasswordRepository::new(db.connection())
        .get_by_value("batch-secret")
        .unwrap()
        .expect("the first interactive success should be persisted immediately");
    assert_eq!(stored.success_count, 2);
}

#[async_trait]
impl ArchiveExecutor for FakeBackend {
    async fn probe(&self, path: &std::path::Path) -> smartzip_core::Result<ArchiveProbe> {
        Ok(ArchiveProbe {
            path: path.to_path_buf(),
            format: format_from_extension(path),
            encrypted: Some(true),
            supported: true,
        })
    }

    async fn list(&self, _request: ListRequest) -> smartzip_core::Result<ArchiveListing> {
        Ok(ArchiveListing {
            format: Some(ArchiveFormat::Zip),
            entries: Vec::new(),
        })
    }

    async fn test(&self, _request: TestRequest) -> smartzip_core::Result<TestResult> {
        Ok(TestResult {
            ok: true,
            encrypted: Some(true),
            ..TestResult::default()
        })
    }

    async fn extract(
        &self,
        request: ExtractArchiveRequest,
    ) -> smartzip_core::Result<ExtractArchiveResult> {
        self.calls
            .lock()
            .unwrap()
            .push(request.archive.display().to_string());
        std::fs::create_dir_all(&request.output_dir).map_err(|source| {
            smartzip_core::SmartZipError::io(Some(request.output_dir.clone()), source)
        })?;
        // Always create a file so the layout planner sees a non-Empty shape
        std::fs::write(request.output_dir.join("extracted.txt"), b"content").map_err(|source| {
            smartzip_core::SmartZipError::io(Some(request.output_dir.clone()), source)
        })?;
        if request.archive.file_name().and_then(|name| name.to_str()) == Some("root.zip") {
            std::fs::write(request.output_dir.join("nested.zip"), b"nested").map_err(|source| {
                smartzip_core::SmartZipError::io(Some(request.output_dir.clone()), source)
            })?;
        }
        Ok(ExtractArchiveResult {
            output_dir: request.output_dir,
            encrypted: Some(true),
        })
    }

    async fn compress(
        &self,
        request: CompressArchiveRequest,
    ) -> smartzip_core::Result<CompressArchiveResult> {
        Ok(CompressArchiveResult {
            output: request.output,
        })
    }
}

#[tokio::test]
async fn cancellation_after_publish_keeps_committed_success() {
    let root = tempfile::tempdir().unwrap();
    let archive = root.path().join("archive.zip");
    let output = root.path().join("out");
    std::fs::write(&archive, b"archive").unwrap();
    let cancellation = tokio_util::sync::CancellationToken::new();
    let recorder = CancelAfterPublish {
        cancellation: cancellation.clone(),
        intents: Mutex::new(Vec::new()),
        outcomes: Mutex::new(Vec::new()),
    };
    let db = SmartZipDb::in_memory().unwrap();
    let passwords = PasswordService::new(PasswordRepository::new(db.connection()));
    let request = ExtractWorkflowRequest {
        inputs: vec![archive],
        output_dir: output,
        recursion_limit: 0,
        encoding_mode: EncodingMode::Auto,
        scanner: ScannerConfig::default(),
        password_candidates: PasswordCandidateRequest {
            manual: vec!["secret".into()],
            ..PasswordCandidateRequest::default()
        },
        layout_policy: Default::default(),
        single_root_name_policy: Default::default(),
        embedded_scan_mode: smartzip_core::EmbeddedScanMode::default(),
        dominant_min_ratio: 0.70,
        confirm_large_scan: false,
        force: false,
        limits: Default::default(),
    };
    let identity = crate::ExtractTaskIdentity::new(&request.inputs);
    let result = SmartZipEngine::default()
        .with_cancellation_token(cancellation)
        .extract_task(
            identity,
            &FakeBackend::default(),
            &passwords,
            request,
            crate::ExtractInteraction::default(),
            crate::ExtractObserver {
                listener: None,
                history: None,
                execution: Some(&recorder),
            },
        )
        .await
        .unwrap();

    assert_eq!(result.processed.len(), 1);
    let outcomes = recorder.outcomes.lock().unwrap();
    assert_eq!(outcomes.len(), 1);
    assert_eq!(outcomes[0].status, "extracted");
    assert!(outcomes[0].committed);
    assert!(outcomes[0].output_path.as_ref().unwrap().exists());
    let intents = recorder.intents.lock().unwrap();
    assert_eq!(intents.len(), 1);
    assert!(intents[0].success.has_password);
}

#[tokio::test]
async fn extract_via_real_seven_zip_with_smart_output() {
    let root = std::env::temp_dir().join(format!("smartzip-int-{}", std::process::id()));
    let archive = root.join("test.zip");
    let extracted_file = root.join("hello.txt");
    let output = root.join("out");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(&extracted_file, b"hello world").unwrap();

    let status = std::process::Command::new("7z")
        .arg("a")
        .arg(&archive)
        .arg(&extracted_file)
        .current_dir(&root)
        .status()
        .unwrap();
    assert!(status.success(), "7z must be available in PATH");
    std::fs::remove_file(&extracted_file).unwrap();

    let seven_zip = SevenZipBackend::locate(&smartzip_archive::SevenZipLocator::default())
        .expect("7z/7zz must be available");
    let backend =
        BackendRouter::from_adapters(vec![AdapterRegistration::from_adapter(seven_zip, 10)]);
    let db = SmartZipDb::in_memory().unwrap();
    let service = PasswordService::new(PasswordRepository::new(db.connection()));

    let engine = SmartZipEngine::default();
    let result = engine
        .extract_recursive(
            &backend,
            &service,
            ExtractWorkflowRequest {
                inputs: vec![archive.clone()],
                output_dir: output.clone(),
                recursion_limit: 1,
                encoding_mode: EncodingMode::Auto,
                scanner: ScannerConfig::default(),
                password_candidates: PasswordCandidateRequest::default(),
                layout_policy: crate::layout::OutputLayoutPolicy::default(),
                single_root_name_policy: crate::layout::SingleRootNamePolicy::default(),
                embedded_scan_mode: smartzip_core::EmbeddedScanMode::default(),
                dominant_min_ratio: 0.70,
                confirm_large_scan: false,
                force: false,
                limits: Default::default(),
            },
            None,
            None,
        )
        .await
        .unwrap();

    assert_eq!(result.processed.len(), 1);

    // Verify the extracted content exists somewhere under output.
    let candidates = [
        output.join("hello.txt"),
        output.join("test").join("hello.txt"),
        output.join("test.txt"),
    ];
    assert!(
        candidates.iter().any(|p| p.exists()),
        "expected hello.txt in one of {:?}",
        candidates
    );

    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn embedded_archive_is_carved_before_extraction_and_recurses() {
    let root = std::env::temp_dir().join(format!("smartzip-embedded-{}", std::process::id()));
    let archive = root.join("payload.zip");
    let disguised = root.join("photo.jpg");
    let output = root.join("out");
    std::fs::create_dir_all(&root).unwrap();

    let payload = root.join("payload.txt");
    std::fs::write(&payload, b"payload").unwrap();
    let status = std::process::Command::new("7z")
        .arg("a")
        .arg(&archive)
        .arg(&payload)
        .current_dir(&root)
        .status()
        .unwrap();
    assert!(status.success(), "7z must be available in PATH");

    let mut composite = Vec::from(&b"JPEG-HEADER"[..]);
    composite.extend_from_slice(&std::fs::read(&archive).unwrap());
    std::fs::write(&disguised, composite).unwrap();

    let backend = EmbeddedAwareFakeBackend::default();
    let db = SmartZipDb::in_memory().unwrap();
    let service = PasswordService::new(PasswordRepository::new(db.connection()));

    let engine = engine_with_test_recycler().with_min_embedded_size_bytes(0);
    let result = engine
        .extract_recursive(
            &backend,
            &service,
            ExtractWorkflowRequest {
                inputs: vec![disguised.clone()],
                output_dir: output.clone(),
                recursion_limit: 2,
                encoding_mode: EncodingMode::Auto,
                scanner: ScannerConfig::default(),
                password_candidates: PasswordCandidateRequest::default(),
                layout_policy: crate::layout::OutputLayoutPolicy::default(),
                single_root_name_policy: crate::layout::SingleRootNamePolicy::default(),
                embedded_scan_mode: smartzip_core::EmbeddedScanMode::Aggressive,
                dominant_min_ratio: 0.70,
                confirm_large_scan: false,
                force: false,
                limits: Default::default(),
            },
            None,
            None,
        )
        .await
        .unwrap();

    let calls = backend.calls.lock().unwrap().clone();
    assert_eq!(
        calls.len(),
        2,
        "expected root archive and nested archive calls"
    );
    assert_ne!(calls[0].0, disguised.display().to_string());
    assert!(calls[0].1, "carved archive should start with zip magic");
    assert!(calls[1].0.ends_with("nested.zip"));
    assert!(result
        .enqueued
        .iter()
        .any(|candidate| candidate.path.ends_with("nested.zip")));

    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn list_embedded_archive_is_carved_before_backend() {
    let root = std::env::temp_dir().join(format!("smartzip-list-embedded-{}", std::process::id()));
    let archive = root.join("payload.zip");
    let carrier = root.join("photo.jpg");
    std::fs::create_dir_all(&root).unwrap();
    let payload = root.join("payload.txt");
    std::fs::write(&payload, b"payload").unwrap();
    let status = std::process::Command::new("7z")
        .arg("a")
        .arg(&archive)
        .arg(&payload)
        .current_dir(&root)
        .status()
        .unwrap();
    assert!(status.success(), "7z must be available in PATH");

    let mut composite = Vec::from(&b"JPEG-HEADER"[..]);
    composite.extend_from_slice(&std::fs::read(&archive).unwrap());
    std::fs::write(&carrier, composite).unwrap();

    let backend = EmbeddedAwareFakeBackend::default();
    let db = SmartZipDb::in_memory().unwrap();
    let service = PasswordService::new(PasswordRepository::new(db.connection()));
    SmartZipEngine::default()
        .with_min_embedded_size_bytes(0)
        .list_archive_with_listener_interactive(
            &backend,
            &service,
            ListArchiveRequest {
                path: carrier.clone(),
                scanner: ScannerConfig::default(),
                encoding_mode: EncodingMode::Auto,
                password_candidates: PasswordCandidateRequest::default(),
            },
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap();

    let calls = backend.list_calls.lock().unwrap().clone();
    assert!(!calls.is_empty());
    assert!(
        calls.iter().all(
            |(path, starts_with_zip)| path != &carrier.display().to_string() && *starts_with_zip
        ),
        "list backend should receive only carved archives: {calls:?}"
    );
    let _ = std::fs::remove_dir_all(root);
}

#[derive(Default, Clone)]
struct EmbeddedAwareFakeBackend {
    calls: Arc<Mutex<Vec<(String, bool)>>>,
    list_calls: Arc<Mutex<Vec<(String, bool)>>>,
}

#[async_trait]
impl ArchiveExecutor for EmbeddedAwareFakeBackend {
    async fn probe(&self, path: &std::path::Path) -> smartzip_core::Result<ArchiveProbe> {
        Ok(ArchiveProbe {
            path: path.to_path_buf(),
            format: format_from_extension(path),
            encrypted: Some(true),
            supported: true,
        })
    }

    async fn list(&self, request: ListRequest) -> smartzip_core::Result<ArchiveListing> {
        let starts_with_zip = std::fs::read(&request.archive)
            .map(|bytes| bytes.starts_with(b"PK"))
            .unwrap_or(false);
        self.list_calls
            .lock()
            .unwrap()
            .push((request.archive.display().to_string(), starts_with_zip));
        Ok(ArchiveListing {
            format: Some(ArchiveFormat::Zip),
            entries: Vec::new(),
        })
    }

    async fn test(&self, _request: TestRequest) -> smartzip_core::Result<TestResult> {
        Ok(TestResult {
            ok: true,
            encrypted: Some(true),
            ..TestResult::default()
        })
    }

    async fn extract(
        &self,
        request: ExtractArchiveRequest,
    ) -> smartzip_core::Result<ExtractArchiveResult> {
        let starts_with_zip = std::fs::read(&request.archive)
            .map(|bytes| bytes.starts_with(b"PK"))
            .unwrap_or(false);
        self.calls
            .lock()
            .unwrap()
            .push((request.archive.display().to_string(), starts_with_zip));
        std::fs::create_dir_all(&request.output_dir).map_err(|source| {
            smartzip_core::SmartZipError::io(Some(request.output_dir.clone()), source)
        })?;
        if self.calls.lock().unwrap().len() == 1 {
            std::fs::write(request.output_dir.join("nested.zip"), b"nested").map_err(|source| {
                smartzip_core::SmartZipError::io(Some(request.output_dir.clone()), source)
            })?;
            std::fs::write(request.output_dir.join("readme.txt"), b"readme").map_err(|source| {
                smartzip_core::SmartZipError::io(Some(request.output_dir.clone()), source)
            })?;
        }
        Ok(ExtractArchiveResult {
            output_dir: request.output_dir,
            encrypted: None,
        })
    }

    async fn compress(
        &self,
        request: CompressArchiveRequest,
    ) -> smartzip_core::Result<CompressArchiveResult> {
        Ok(CompressArchiveResult {
            output: request.output,
        })
    }
}

struct PasswordListingBackend {
    requests: Mutex<Vec<ListRequest>>,
}

#[async_trait]
impl ArchiveExecutor for PasswordListingBackend {
    async fn probe(&self, path: &Path) -> smartzip_core::Result<ArchiveProbe> {
        Ok(ArchiveProbe {
            path: path.into(),
            format: Some(ArchiveFormat::Zip),
            encrypted: Some(true),
            supported: true,
        })
    }

    async fn list(&self, request: ListRequest) -> smartzip_core::Result<ArchiveListing> {
        let accepted = request.password.as_deref() == Some("correct");
        let path = request.archive.clone();
        self.requests.lock().unwrap().push(request);
        if accepted {
            Ok(ArchiveListing {
                format: Some(ArchiveFormat::Zip),
                entries: Vec::new(),
            })
        } else {
            Err(SmartZipError::WrongPassword { path })
        }
    }

    async fn test(&self, _: TestRequest) -> smartzip_core::Result<TestResult> {
        panic!("listing must not test data")
    }
    async fn extract(
        &self,
        _: ExtractArchiveRequest,
    ) -> smartzip_core::Result<ExtractArchiveResult> {
        panic!("listing must not extract")
    }
    async fn compress(
        &self,
        _: CompressArchiveRequest,
    ) -> smartzip_core::Result<CompressArchiveResult> {
        panic!("listing must not compress")
    }
}

struct ListingPasswordPrompt(Mutex<Option<&'static str>>);

#[async_trait]
impl InteractivePasswordPrompter for ListingPasswordPrompt {
    async fn prompt(&self, _: &Path) -> Option<String> {
        self.0.lock().unwrap().take().map(str::to_string)
    }
}

#[rstest]
#[case(true, None, None)]
#[case(false, Some("correct"), None)]
#[case(false, None, Some("password_required"))]
#[case(false, Some("still-wrong"), Some("password_required"))]
#[tokio::test]
async fn listing_retries_in_order_and_preserves_prompt_outcomes(
    #[case] stored_success: bool,
    #[case] prompted: Option<&'static str>,
    #[case] expected_error: Option<&str>,
) {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("encrypted.zip");
    std::fs::write(&path, b"listing backend fixture").unwrap();
    let backend = PasswordListingBackend {
        requests: Mutex::new(Vec::new()),
    };
    let db = SmartZipDb::in_memory().unwrap();
    let passwords = PasswordService::new(PasswordRepository::new(db.connection()));
    let mut manual = vec!["wrong".into()];
    if stored_success {
        manual.extend(["correct".into(), "unused".into()]);
    }
    let observed = Arc::new(Mutex::new(Vec::new()));
    let listener_events = observed.clone();
    let result = SmartZipEngine::default()
        .list_archive_with_listener_interactive(
            &backend,
            &passwords,
            ListArchiveRequest {
                path: path.clone(),
                scanner: ScannerConfig::default(),
                encoding_mode: EncodingMode::Override("UTF-8".into()),
                password_candidates: PasswordCandidateRequest {
                    manual,
                    include_empty: false,
                    ..Default::default()
                },
            },
            Some(&ListingPasswordPrompt(Mutex::new(prompted))),
            None,
            Some(Arc::new(move |event| {
                listener_events.lock().unwrap().push(event.clone())
            })),
            None,
        )
        .await;
    match expected_error {
        None => {
            let result = result.unwrap();
            assert!(!result.used_password);
            assert!(result.password_id.is_none());
            assert_eq!(result.encoding, "UTF-8");
            assert_eq!(result.events, *observed.lock().unwrap());
        }
        Some("password_required") => {
            assert!(matches!(result, Err(SmartZipError::PasswordRequired { path: p }) if p == path))
        }
        Some("wrong_password") => {
            assert!(matches!(result, Err(SmartZipError::WrongPassword { path: p }) if p == path))
        }
        _ => unreachable!(),
    }
    let requests = backend.requests.lock().unwrap();
    let attempts: Vec<_> = requests
        .iter()
        .map(|r| r.password.as_deref().unwrap())
        .collect();
    let mut expected = vec!["wrong"];
    if stored_success {
        expected.push("correct");
    } else if let Some(password) = prompted {
        expected.push(password);
    }
    assert_eq!(attempts, expected);
    let events = observed.lock().unwrap();
    let progress: Vec<_> = events
        .iter()
        .filter_map(|e| match &e.kind {
            TaskEventKind::Progress(p) if p.message.starts_with("Trying password") => {
                Some(p.message.as_str())
            }
            _ => None,
        })
        .collect();
    assert_eq!(progress.len(), if stored_success { 2 } else { 1 });
    for (index, message) in progress.iter().enumerate() {
        assert!(message.contains(&format!(
            "[{}/{}]",
            index + 1,
            if stored_success { 3 } else { 1 }
        )));
        assert!(!message.contains("correct") && !message.contains("wrong"));
    }
}

#[tokio::test]
async fn configured_cleanup_keep_never_calls_recycler_and_policy_is_a_snapshot() {
    let root = tempfile::tempdir().unwrap();
    let input = root.path().join("root.zip");
    std::fs::write(&input, b"fake archive").unwrap();
    let mut resolved = smartzip_config::ResolvedConfig::load(None).unwrap();
    let c = &mut resolved.values;
    c.state.mode = smartzip_config::StateMode::Off;
    c.extraction.recursion.max_depth = 1;
    c.extraction.embedded.root = smartzip_config::RootScan::Off;
    c.extraction.embedded.nested = smartzip_config::NestedScan::Off;
    c.extraction.volumes.auto_discover = false;
    c.extraction.encoding.mode = "backend".into();
    c.extraction.output.layout = smartzip_config::Layout::Raw;
    c.extraction.cleanup.nested_archives = smartzip_config::Cleanup::Keep;
    let policy = crate::CompiledRunPolicy::compile(resolved).unwrap();
    let service = PasswordService::configured(
        None,
        policy.values().passwords.clone(),
        smartzip_config::StateMode::Off,
    );
    let recycled = Arc::new(AtomicUsize::new(0));
    let calls = recycled.clone();
    let engine = SmartZipEngine::default()
        .with_run_policy(policy.clone())
        .with_archive_recycler(Arc::new(move |_| {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }));
    let mut next_task_policy = policy.resolved().clone();
    next_task_policy.values.extraction.cleanup.nested_archives = smartzip_config::Cleanup::Delete;
    next_task_policy.values.extraction.recursion.enabled = false;
    let backend = FakeBackend::default();
    let result = engine
        .extract_recursive(
            &backend,
            &service,
            ExtractWorkflowRequest {
                inputs: vec![input.clone()],
                output_dir: root.path().join("out"),
                recursion_limit: 0,
                encoding_mode: EncodingMode::Auto,
                scanner: ScannerConfig::default(),
                password_candidates: PasswordCandidateRequest::default(),
                layout_policy: crate::layout::OutputLayoutPolicy::default(),
                single_root_name_policy: crate::layout::SingleRootNamePolicy::default(),
                embedded_scan_mode: smartzip_core::EmbeddedScanMode::Auto,
                dominant_min_ratio: 0.7,
                confirm_large_scan: false,
                force: false,
                limits: Default::default(),
            },
            None,
            None,
        )
        .await
        .unwrap();
    assert_eq!(result.processed.len(), 2);
    assert_eq!(backend.calls.lock().unwrap().len(), 2);
    assert_eq!(recycled.load(Ordering::SeqCst), 0);
    assert!(input.exists());
    assert!(root.path().join("out/root/nested.zip").exists());
}

#[test]
fn nested_classification_keeps_header_precedence_and_single_output_scan_boundary() {
    let root = tempfile::tempdir().unwrap();
    let scanner = EmbeddedScanner::new(ScannerConfig::default());
    let policy = EmbeddedScanPolicy {
        min_finding_size_bytes: 0,
        mode: EmbeddedScanMode::All,
        ..Default::default()
    };
    let zip = std::fs::read(fixture_path("enc_utf8.zip")).unwrap();
    for (name, scan, expected) in [
        ("mislabeled.7z", true, Some(ArchiveFormat::Zip)),
        ("mislabeled.7z", false, Some(ArchiveFormat::SevenZip)),
        ("document.docx", true, None),
    ] {
        let dir = root.path().join(format!("case-{name}-{scan}"));
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, &zip).unwrap();
        let single = discover_nested_candidates(
            &scanner,
            &path,
            2,
            Path::new("parent"),
            &policy,
            scan,
            true,
            &tokio_util::sync::CancellationToken::new(),
        );
        let walked = discover_nested_candidates(
            &scanner,
            &dir,
            2,
            Path::new("parent"),
            &policy,
            scan,
            true,
            &tokio_util::sync::CancellationToken::new(),
        );
        assert_eq!(single, walked);
        assert_eq!(
            single.first().and_then(|c| c.detected_format.clone()),
            expected
        );
    }
    let dir = root.path().join("carrier");
    std::fs::create_dir(&dir).unwrap();
    let path = dir.join("image.bin");
    let mut payload = vec![0; 512];
    payload.extend_from_slice(&zip);
    std::fs::write(&path, payload).unwrap();
    // A collapsed single output only uses header/extension recognition in the baseline.
    assert!(discover_nested_candidates(
        &scanner,
        &path,
        2,
        Path::new("parent"),
        &policy,
        true,
        true,
        &tokio_util::sync::CancellationToken::new()
    )
    .is_empty());
    let walked = discover_nested_candidates(
        &scanner,
        &dir,
        2,
        Path::new("parent"),
        &policy,
        true,
        true,
        &tokio_util::sync::CancellationToken::new(),
    );
    assert_eq!(walked.len(), 1);
    assert_eq!(walked[0].embedded_offset, Some(512));
    assert_eq!(walked[0].embedded_size, Some(zip.len() as u64));
    #[cfg(unix)]
    {
        let links = root.path().join("links");
        std::fs::create_dir(&links).unwrap();
        std::os::unix::fs::symlink(&path, links.join("linked.zip")).unwrap();
        assert!(discover_nested_candidates(
            &scanner,
            &links,
            2,
            Path::new("parent"),
            &policy,
            true,
            true,
            &tokio_util::sync::CancellationToken::new()
        )
        .is_empty());
    }
}

#[test]
fn nested_discovery_uses_the_retained_output_inventory() {
    let root = tempfile::tempdir().unwrap();
    let included = root.path().join("included.zip");
    let excluded = root.path().join("excluded.zip");
    let zip = std::fs::read(fixture_path("enc_utf8.zip")).unwrap();
    std::fs::write(&included, &zip).unwrap();
    std::fs::write(&excluded, &zip).unwrap();
    let files = vec![crate::budget::InventoryFile {
        relative_path: PathBuf::from("included.zip"),
        size: zip.len() as u64,
    }];

    let candidates = discover_nested_candidates_from_inventory(
        &EmbeddedScanner::new(ScannerConfig::default()),
        root.path(),
        &files,
        1,
        Path::new("parent"),
        &EmbeddedScanPolicy::default(),
        true,
        false,
        &tokio_util::sync::CancellationToken::new(),
    );

    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].path, included);
}

#[test]
fn small_unrecognized_files_skip_header_probe_but_archive_suffixes_do_not() {
    let root = tempfile::tempdir().unwrap();
    let unrecognized = root.path().join("payload.bin");
    let archive_named = root.path().join("payload.zip");
    let zip = std::fs::read(fixture_path("enc_utf8.zip")).unwrap();
    std::fs::write(&unrecognized, &zip).unwrap();
    std::fs::write(&archive_named, &zip).unwrap();
    let files = vec![
        crate::budget::InventoryFile {
            relative_path: PathBuf::from("payload.bin"),
            size: zip.len() as u64,
        },
        crate::budget::InventoryFile {
            relative_path: PathBuf::from("payload.zip"),
            size: zip.len() as u64,
        },
    ];
    let policy = EmbeddedScanPolicy {
        min_finding_size_bytes: zip.len() as u64 + 1,
        mode: EmbeddedScanMode::All,
        ..Default::default()
    };

    let candidates = discover_nested_candidates_from_inventory(
        &EmbeddedScanner::new(ScannerConfig::default()),
        root.path(),
        &files,
        1,
        Path::new("parent"),
        &policy,
        true,
        true,
        &tokio_util::sync::CancellationToken::new(),
    );

    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].path, archive_named);
}

#[tokio::test(flavor = "current_thread")]
async fn multi_input_starts_extracting_before_scanning_the_entire_batch() {
    let root = tempfile::tempdir().unwrap();
    let inputs: Vec<_> = (0..8)
        .map(|i| root.path().join(format!("input-{i}.zip")))
        .collect();
    for input in &inputs {
        let mut writer = zip::ZipWriter::new(std::fs::File::create(input).unwrap());
        writer
            .start_file("data.txt", zip::write::SimpleFileOptions::default())
            .unwrap();
        std::io::Write::write_all(&mut writer, b"payload").unwrap();
        writer.finish().unwrap();
    }
    let transitions = Arc::new(Mutex::new(Vec::new()));
    let execution = StageWaitRecorder {
        wait_node: Mutex::new(None),
        wait_stage: crate::coordinator::Stage::ScanEmbedded,
        block_at_stage: false,
        stage_entered: Default::default(),
        outcomes: Default::default(),
        transitions: transitions.clone(),
    };
    let observed = Arc::new(Mutex::new(None));
    let first_extract = observed.clone();
    let listener: crate::events::TaskEventListener = Arc::new(move |event| {
        if matches!(event.kind, TaskEventKind::PasswordTried { .. }) {
            let mut first = first_extract.lock().unwrap();
            if first.is_none() {
                *first = Some(
                    transitions
                        .lock()
                        .unwrap()
                        .iter()
                        .filter(|(_, stage)| *stage == crate::coordinator::Stage::ScanEmbedded)
                        .count(),
                );
            }
        }
    });
    let backend = FailFirstExtractBackend {
        calls: AtomicUsize::new(1),
    };
    let db = SmartZipDb::in_memory().unwrap();
    let passwords = PasswordService::new(PasswordRepository::new(db.connection()));
    let expected_inputs = inputs.clone();
    let result = SmartZipEngine::default()
        .extract(
            &backend,
            &passwords,
            ExtractWorkflowRequest {
                inputs,
                output_dir: root.path().join("out"),
                recursion_limit: 0,
                encoding_mode: EncodingMode::Override("UTF-8".into()),
                scanner: ScannerConfig::default(),
                password_candidates: PasswordCandidateRequest {
                    include_empty: true,
                    limit: 0,
                    ..Default::default()
                },
                layout_policy: Default::default(),
                single_root_name_policy: Default::default(),
                embedded_scan_mode: Default::default(),
                dominant_min_ratio: 0.7,
                confirm_large_scan: false,
                force: false,
                limits: Default::default(),
            },
            crate::ExtractInteraction::default(),
            crate::ExtractObserver {
                listener: Some(listener),
                history: None,
                execution: Some(&execution),
            },
        )
        .await
        .unwrap();
    assert_eq!(
        result
            .processed
            .iter()
            .map(|candidate| candidate.path.clone())
            .collect::<Vec<_>>(),
        expected_inputs
    );
    assert!(
        observed.lock().unwrap().is_some_and(|scanned| scanned <= 2),
        "all roots were pre-scanned before extraction: {:?}",
        observed.lock().unwrap()
    );
}

#[tokio::test(flavor = "current_thread")]
async fn stateless_embedded_inputs_extract_before_scanning_later_carriers() {
    let root = tempfile::tempdir().unwrap();
    let inputs: Vec<_> = (0..8)
        .map(|i| root.path().join(format!("carrier-{i}.bin")))
        .collect();
    for input in &inputs {
        let mut writer = zip::ZipWriter::new(std::fs::File::create(input).unwrap());
        writer
            .start_file("data.txt", zip::write::SimpleFileOptions::default())
            .unwrap();
        std::io::Write::write_all(&mut writer, b"payload").unwrap();
        writer.finish().unwrap();
        let zip = std::fs::read(input).unwrap();
        let mut carrier = b"carrier prefix".to_vec();
        carrier.extend(zip);
        std::fs::write(input, carrier).unwrap();
    }
    let findings = Arc::new(AtomicUsize::new(0));
    let observed = Arc::new(Mutex::new(None));
    let first_extract = observed.clone();
    let listener: crate::events::TaskEventListener = Arc::new(move |event| {
        if matches!(event.kind, TaskEventKind::EmbeddedArchiveFound { .. }) {
            findings.fetch_add(1, Ordering::SeqCst);
        }
        if matches!(event.kind, TaskEventKind::PasswordTried { .. }) {
            first_extract
                .lock()
                .unwrap()
                .get_or_insert(findings.load(Ordering::SeqCst));
        }
    });
    let backend = FailFirstExtractBackend {
        calls: AtomicUsize::new(1),
    };
    let db = SmartZipDb::in_memory().unwrap();
    let passwords = PasswordService::new(PasswordRepository::new(db.connection()));
    let result = SmartZipEngine::default()
        .extract(
            &backend,
            &passwords,
            ExtractWorkflowRequest {
                inputs,
                output_dir: root.path().join("out"),
                recursion_limit: 0,
                encoding_mode: EncodingMode::Override("UTF-8".into()),
                scanner: ScannerConfig::default(),
                password_candidates: PasswordCandidateRequest {
                    include_empty: true,
                    limit: 0,
                    ..Default::default()
                },
                layout_policy: Default::default(),
                single_root_name_policy: Default::default(),
                embedded_scan_mode: Default::default(),
                dominant_min_ratio: 0.7,
                confirm_large_scan: false,
                force: false,
                limits: Default::default(),
            },
            crate::ExtractInteraction::default(),
            crate::ExtractObserver {
                listener: Some(listener),
                history: None,
                execution: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(result.processed.len(), 8);
    assert!(
        observed.lock().unwrap().is_some_and(|scanned| scanned == 1),
        "all roots were pre-scanned before extraction: {:?}",
        observed.lock().unwrap()
    );
}

mod execution_cleanup;
