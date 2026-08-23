//! Unit tests moved out of lib.rs.

use crate::encoding_flow::*;
use crate::interactive::*;
use crate::nested::*;
use crate::password_order::*;
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

    let ordered = order_password_candidates(&base, Some(&known), &batch);
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
fn root_scan_is_full_while_default_nested_scan_stays_fast() {
    let nested = ScannerConfig::default();
    let root = full_root_scanner_config(&nested);

    assert_eq!(root.mode, ScanMode::Deep);
    assert_eq!(root.max_scan_bytes, None);
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

#[test]
fn maps_common_extensions() {
    assert_eq!(format_from_extension("a.7z"), Some(ArchiveFormat::SevenZip));
    assert_eq!(format_from_extension("a.tgz"), Some(ArchiveFormat::Gzip));
    assert_eq!(format_from_extension("a.bin"), None);
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

#[test]
fn engine_accepts_custom_scanner_config() {
    let engine = SmartZipEngine::with_scanner_config(ScannerConfig {
        mode: ScanMode::Deep,
        ..ScannerConfig::default()
    });
    assert_eq!(engine.scanner.config().mode, ScanMode::Deep);
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
async fn explicit_encoding_override_is_preserved_for_test_and_extract() {
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
            },
            None,
            None,
        )
        .await
        .unwrap();

    assert_eq!(result.processed.len(), 1);
    assert_eq!(
        backend.seen_test_encodings.lock().unwrap().as_slice(),
        &[EncodingMode::Override("gbk".into())]
    );
    assert_eq!(
        backend.seen_extract_encodings.lock().unwrap().as_slice(),
        &[EncodingMode::Override("gbk".into())]
    );

    let _ = std::fs::remove_dir_all(root);
}

#[derive(Clone, Default)]
struct FailingTestBackend;

#[async_trait]
impl ArchiveExecutor for FailingTestBackend {
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
        Ok(ExtractArchiveResult {
            output_dir: request.output_dir,
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
async fn backend_failures_do_not_record_password_failures() {
    let root = std::env::temp_dir().join(format!(
        "smartzip-engine-backend-fail-{}",
        std::process::id()
    ));
    let input = root.join("root.zip");
    let output = root.join("out");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(&input, b"not really a zip").unwrap();

    let backend = FailingTestBackend;
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

    async fn test(&self, request: TestRequest) -> smartzip_core::Result<TestResult> {
        self.attempted_passwords
            .lock()
            .unwrap()
            .push(request.password.clone());
        if request.password.as_deref() == Some("batch-secret") {
            Ok(TestResult {
                ok: true,
                encrypted: Some(true),
            })
        } else {
            Err(smartzip_core::SmartZipError::WrongPassword {
                path: request.archive,
            })
        }
    }

    async fn extract(
        &self,
        request: ExtractArchiveRequest,
    ) -> smartzip_core::Result<ExtractArchiveResult> {
        std::fs::create_dir_all(&request.output_dir).map_err(|source| {
            smartzip_core::SmartZipError::io(Some(request.output_dir.clone()), source)
        })?;
        std::fs::write(request.output_dir.join("content.txt"), b"content").map_err(|source| {
            smartzip_core::SmartZipError::io(Some(request.output_dir.clone()), source)
        })?;
        Ok(ExtractArchiveResult {
            output_dir: request.output_dir,
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
        self.calls.fetch_add(1, Ordering::SeqCst);
        Some("batch-secret".to_string())
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
        1,
        "the password accepted for the first file should be reused in-memory for the second",
    );
    assert_eq!(
        backend.attempted_passwords.lock().unwrap().as_slice(),
        &[
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
