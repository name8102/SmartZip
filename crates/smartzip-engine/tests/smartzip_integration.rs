//! SmartZip integration tests using archive fixtures.
//!
//! Requires `7z` or `7zz` in PATH. Generate fixtures first:
//!   cd tests/fixtures && python3 generate.py && cd ../..
//! Run:
//!   cargo test -p smartzip-engine --test smartzip_integration -- --test-threads=1

use async_trait::async_trait;
use rstest::*;
use smartzip_archive::{
    AdapterRegistration, ArchiveExecutor, BackendRouter, ExtractArchiveRequest, ListRequest,
    SevenZipBackend, SevenZipLocator, TestRequest,
};
use smartzip_core::{ArchiveFormat, EncodingMode};
use smartzip_db::{password::PasswordRepository, SmartZipDb};
use smartzip_encoding::ArchiveEncodingDetector;
use smartzip_engine::{
    format_from_extension, ArchiveRecycleHandler, EmbeddedSelectionChoice, ExtractWorkflowRequest,
    InteractiveEmbeddedPrompter, InteractivePasswordPrompter, SmartZipEngine,
};
use smartzip_passwords::{PasswordCandidateRequest, PasswordService};
use smartzip_scanner::{EmbeddedScanner, ScannerConfig};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tempfile::TempDir;

// ── Helpers ───────────────────────────────────────────────────────────────

/// Resolve path to the workspace-level `tests/fixtures/` directory.
fn fixture_dir() -> PathBuf {
    // CARGO_MANIFEST_DIR is .../SmartZip/crates/smartzip-engine
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent() // crates/
        .unwrap()
        .parent() // project root
        .unwrap()
        .join("tests")
        .join("fixtures")
}

fn fixture_path(name: &str) -> PathBuf {
    fixture_dir().join(name)
}

fn backend() -> BackendRouter {
    let seven_zip = SevenZipBackend::locate(&SevenZipLocator::default())
        .expect("7z/7zz must be available in PATH to run integration tests");
    BackendRouter::from_adapters(vec![AdapterRegistration::from_adapter(seven_zip, 10)])
}

fn engine_with_test_recycler() -> (SmartZipEngine, Arc<Mutex<Vec<PathBuf>>>) {
    let recycled = Arc::new(Mutex::new(Vec::new()));
    let recycled_for_handler = Arc::clone(&recycled);
    let handler: ArchiveRecycleHandler = Arc::new(move |path| {
        std::fs::remove_file(&path)?;
        recycled_for_handler.lock().unwrap().push(path.clone());
        Ok(())
    });
    (
        SmartZipEngine::default().with_archive_recycler(handler),
        recycled,
    )
}

fn router() -> BackendRouter {
    BackendRouter::from_config(&smartzip_config::BackendConfig::default())
        .expect("backend router should initialize")
}

fn create_split_encrypted_zip(root: &Path, password: &str) -> PathBuf {
    let input = root.join("big.bin");
    let first_volume = root.join("split.zip");
    let mut state = 0x1234_5678_9ABC_DEF0_u64;
    let payload: Vec<u8> = (0..200_000)
        .map(|_| {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            (state >> 24) as u8
        })
        .collect();
    std::fs::write(&input, payload).unwrap();

    let status = std::process::Command::new("zip")
        .arg("-s")
        .arg("64k")
        .arg("-P")
        .arg(password)
        .arg("split.zip")
        .arg("big.bin")
        .current_dir(root)
        .status()
        .expect("zip must be available in PATH");
    assert!(status.success(), "zip should create split encrypted zip");
    assert!(
        root.join("split.z01").exists(),
        "first split sidecar should exist"
    );
    assert!(first_volume.exists(), "split zip entrypoint should exist");
    first_volume
}

// ── Basic extraction (no password) ────────────────────────────────────────

#[rstest]
#[case::utf8_zip("enc_utf8.zip")]
#[case::nested_zip_in_zip("nested_zip_in_zip.zip")]
#[case::nested_7z_in_zip("nested_7z_in_zip.zip")]
#[case::nested_mixed_formats("nested_mixed_formats.zip")]
#[tokio::test]
async fn test_extract_archive_no_password(#[case] fixture_name: &str) {
    let archive = fixture_path(fixture_name);
    assert!(archive.exists(), "fixture missing: {fixture_name}");

    let backend = backend();
    let output = TempDir::new().unwrap();

    backend
        .extract(ExtractArchiveRequest {
            archive: archive.clone(),
            format: None,
            output_dir: output.path().to_path_buf(),
            password: None,
            encoding: EncodingMode::Auto,
        })
        .await
        .expect("extract should succeed");

    // Verify output is non-empty (at least one file/dir extracted)
    let entries: Vec<_> = std::fs::read_dir(output.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    assert!(
        !entries.is_empty(),
        "extracted directory should contain files"
    );
}

// ── Password-protected extraction ─────────────────────────────────────────

#[rstest]
#[case::cn("pass_cn.zip", "中文密码123", "文档.txt")]
#[case::kr("pass_kr.zip", "한국어비밀번호", "문서.txt")]
#[case::emoji("pass_emoji.zip", "🔒Secret!密码", "readme.txt")]
#[case::rtl("pass_rtl.zip", "עברית-123", "readme.txt")]
#[tokio::test]
async fn test_extract_with_unicode_password(
    #[case] fixture_name: &str,
    #[case] password: &str,
    #[case] expected_file: &str,
) {
    let archive = fixture_path(fixture_name);
    assert!(archive.exists(), "fixture missing: {fixture_name}");

    let backend = backend();
    let output = TempDir::new().unwrap();

    let result = backend
        .extract(ExtractArchiveRequest {
            archive: archive.clone(),
            format: None,
            output_dir: output.path().to_path_buf(),
            password: Some(password.to_string()),
            encoding: EncodingMode::Auto,
        })
        .await;

    assert!(
        result.is_ok(),
        "extract with password '{password}' failed: {result:?}"
    );

    let file_path = find_file(output.path(), expected_file);
    assert!(
        file_path.is_some(),
        "expected file '{expected_file}' not found in {:?}",
        output.path()
    );
}

#[rstest]
#[case::jp("pass_jp.7z", "日本語パスワード", "ファイル.txt")]
#[tokio::test]
async fn test_extract_7z_unicode_password(
    #[case] fixture_name: &str,
    #[case] password: &str,
    #[case] expected_file: &str,
) {
    let archive = fixture_path(fixture_name);
    assert!(archive.exists(), "fixture missing: {fixture_name}");

    let backend = backend();
    let output = TempDir::new().unwrap();

    let result = backend
        .extract(ExtractArchiveRequest {
            archive: archive.clone(),
            format: None,
            output_dir: output.path().to_path_buf(),
            password: Some(password.to_string()),
            encoding: EncodingMode::Auto,
        })
        .await;

    assert!(
        result.is_ok(),
        "extract of 7z with password '{password}' failed: {result:?}"
    );

    let file_path = find_file(output.path(), expected_file);
    assert!(
        file_path.is_some(),
        "expected file '{expected_file}' not found in {:?}",
        output.path()
    );
}

// ── Wrong password handling ───────────────────────────────────────────────

#[rstest]
#[case::wrong_cn("pass_cn.zip", "wrong-password")]
#[case::wrong_kr("pass_kr.zip", "틀린비밀번호")]
#[tokio::test]
async fn test_extract_wrong_password_fails(
    #[case] fixture_name: &str,
    #[case] wrong_password: &str,
) {
    let archive = fixture_path(fixture_name);
    assert!(archive.exists(), "fixture missing: {fixture_name}");

    let backend = backend();
    let output = TempDir::new().unwrap();

    let result = backend
        .extract(ExtractArchiveRequest {
            archive: archive.clone(),
            format: None,
            output_dir: output.path().to_path_buf(),
            password: Some(wrong_password.to_string()),
            encoding: EncodingMode::Auto,
        })
        .await;

    assert!(
        result.is_err(),
        "expected failure with wrong password, got: {result:?}"
    );
}

// ── Archive listing ───────────────────────────────────────────────────────

#[rstest]
#[case::utf8("enc_utf8.zip", &["中文文件名测试.txt", "日本語テスト.txt", "한국어테스트.txt", "English_File.txt"])]
#[case::nested_zip("nested_zip_in_zip.zip", &["inner.zip"])]
#[tokio::test]
async fn test_list_archive_entries(
    #[case] fixture_name: &str,
    #[case] expected_filenames: &[&str],
) {
    let archive = fixture_path(fixture_name);
    assert!(archive.exists(), "fixture missing: {fixture_name}");

    let backend = backend();
    let listing = backend
        .list(ListRequest {
            archive: archive.clone(),
            format: None,
            password: None,
            encoding: EncodingMode::Auto,
        })
        .await
        .expect("list should succeed");

    for expected in expected_filenames {
        let found = listing
            .entries
            .iter()
            .any(|entry| entry.path.file_name().map_or(false, |n| n == *expected));
        assert!(
            found,
            "expected entry '{}' not found; entries: {:?}",
            expected,
            listing
                .entries
                .iter()
                .map(|e| e.path.display().to_string())
                .collect::<Vec<_>>()
        );
    }
}

#[rstest]
#[case::gbk("enc_gbk.zip", "gbk", &["中文文件名测试.txt", "压缩包说明文档.doc"])]
#[case::sjis("enc_sjis.zip", "Shift_JIS", &["日本語ファイル名テスト.txt", "資料/会議メモ.docx"])]
#[case::euckr("enc_euckr.zip", "euc-kr", &["한글파일이름.txt", "보고서_2024.hwp"])]
#[case::big5("enc_big5.zip", "Big5", &["繁體中文檔案名稱.txt", "會議記錄.doc"])]
#[tokio::test]
async fn test_list_archive_entries_with_explicit_encoding_override(
    #[case] fixture_name: &str,
    #[case] encoding: &str,
    #[case] expected_filenames: &[&str],
) {
    let archive = fixture_path(fixture_name);
    assert!(archive.exists(), "fixture missing: {fixture_name}");

    let backend = backend();
    let listing = backend
        .list(ListRequest {
            archive,
            format: None,
            password: None,
            encoding: EncodingMode::Override(encoding.to_string()),
        })
        .await
        .expect("list with explicit encoding override should succeed");

    assert_eq!(
        listing.entries.len(),
        expected_filenames.len(),
        "explicit encoding override should keep the archive listable"
    );
    assert!(
        listing
            .entries
            .iter()
            .all(|entry| !entry.path.as_os_str().is_empty()),
        "explicit encoding override should still return non-empty entry paths"
    );
}

// ── Archive testing (integrity check) ─────────────────────────────────────

#[rstest]
#[case::plain("enc_utf8.zip", None)]
#[case::encrypted("pass_cn.zip", Some("中文密码123"))]
#[tokio::test]
async fn test_archive_integrity_check(#[case] fixture_name: &str, #[case] password: Option<&str>) {
    let archive = fixture_path(fixture_name);
    assert!(archive.exists(), "fixture missing: {fixture_name}");

    let backend = backend();
    let result = backend
        .test(TestRequest {
            archive: archive.clone(),
            format: None,
            password: password.map(str::to_string),
            encoding: EncodingMode::Auto,
        })
        .await;

    assert!(result.is_ok(), "archive integrity test failed: {result:?}");
    assert!(result.unwrap().ok);
}

// ── Nested archive detection via scanner ──────────────────────────────────

#[rstest]
#[case("nested_zip_in_zip.zip")]
#[case("nested_7z_in_zip.zip")]
#[case("nested_mixed_formats.zip")]
fn test_scanner_does_not_panic_on_archive_fixtures(#[case] fixture_name: &str) {
    let archive = fixture_path(fixture_name);
    assert!(archive.exists(), "fixture missing: {fixture_name}");

    let scanner_config = ScannerConfig {
        min_confidence: smartzip_scanner::Confidence::Low,
        ..ScannerConfig::default()
    };
    let scanner = EmbeddedScanner::new(scanner_config);
    let result = scanner.scan_path(&archive);

    // Scanner should not error on these fixtures
    assert!(
        result.is_ok(),
        "scanner panicked on {fixture_name}: {result:?}"
    );
}

// ── Nested archive path collision (TDD: red before P0-2 fix) ──────────────

/// TDD regression: `.tar.gz -> .tar -> leaf` path collision.
///
/// Current failure (P0-1): when `CommitSingleFileAsInnerName` commits the
/// extracted `.tar` as a single file, `candidate.relative_path` is updated to
/// the file path.  The nested tar candidate then uses that file path as a
/// directory prefix → `create_dir_all` fails with `File exists`.
///
/// Regression fixed by P0-2 nested output-root separation.
#[tokio::test]
#[allow(non_snake_case)]
async fn test_TDD_tar_gz_name_equivalent_to_inner_tar_collision() {
    // `real_tar.tar.gz`: gzip → `real_tar.tar` → `leaf_rt.txt`
    // archive_stem("real_tar.tar.gz") = "real_tar"
    // inner file name = "real_tar.tar" → Equivalent similarity
    // → CommitSingleFileAsInnerName → file path used as dir prefix → collision
    let archive = fixture_path("real_tar.tar.gz");
    assert!(archive.exists(), "fixture missing: real_tar.tar.gz");

    let backend = backend();
    let db = SmartZipDb::in_memory().unwrap();
    let service = PasswordService::new(PasswordRepository::new(db.connection()));
    let (engine, recycled) = engine_with_test_recycler();
    let output = TempDir::new().unwrap();

    let result = engine
        .extract_recursive(
            &backend,
            &service,
            ExtractWorkflowRequest {
                inputs: vec![archive.clone()],
                output_dir: output.path().to_path_buf(),
                recursion_limit: 2,
                encoding_mode: EncodingMode::Auto,
                scanner: ScannerConfig::default(),
                password_candidates: PasswordCandidateRequest::default(),
                layout_policy: Default::default(),
                single_root_name_policy: Default::default(),
                embedded_scan_mode: smartzip_core::EmbeddedScanMode::default(),
                dominant_min_ratio: 0.70,
                confirm_large_scan: false,
                force: false,
                limits: Default::default(),
            },
            None,
            None,
        )
        .await;

    // Target behavior (post-fix): extraction succeeds, leaf is found
    assert!(
        result.is_ok(),
        "tar.gz -> tar -> leaf extraction failed (expected after P0-2 fix): {:?}",
        result
    );
    let workflow = result.unwrap();

    // Both gzip and tar levels should be processed
    assert!(
        workflow.processed.len() >= 2,
        "expected >=2 processed (gzip + tar), got {:?}",
        workflow
            .processed
            .iter()
            .map(|c| c.path.display().to_string())
            .collect::<Vec<_>>()
    );

    assert!(
        find_file(output.path(), "leaf_rt.txt").is_some(),
        "expected leaf_rt.txt in {:?}",
        output.path()
    );
    assert!(
        recycled
            .lock()
            .unwrap()
            .iter()
            .any(|path| path.file_name().and_then(|name| name.to_str()) == Some("real_tar.tar")),
        "processed inner tar should be recycled; processed={:?}, recycled={:?}",
        workflow.processed,
        recycled.lock().unwrap()
    );
}

/// TDD regression: `zip -> tar.gz -> tar -> leaf` three-level path collision.
///
/// The outermost zip extraction discovers `real_tar.tar.gz` as a nested
/// archive.  The gzip→tar step hits the same `CommitSingleFileAsInnerName`
/// path collision described above.
///
/// Regression fixed by P0-2 nested output-root separation.
#[tokio::test]
#[allow(non_snake_case)]
async fn test_TDD_zip_containing_tar_gz_three_level_collision() {
    let archive = fixture_path("zip_containing_real_tar_gz.zip");
    assert!(archive.exists(), "fixture missing");

    let backend = backend();
    let db = SmartZipDb::in_memory().unwrap();
    let service = PasswordService::new(PasswordRepository::new(db.connection()));
    let (engine, recycled) = engine_with_test_recycler();
    let output = TempDir::new().unwrap();

    let result = engine
        .extract_recursive(
            &backend,
            &service,
            ExtractWorkflowRequest {
                inputs: vec![archive.clone()],
                output_dir: output.path().to_path_buf(),
                recursion_limit: 3,
                encoding_mode: EncodingMode::Auto,
                scanner: ScannerConfig::default(),
                password_candidates: PasswordCandidateRequest::default(),
                layout_policy: Default::default(),
                single_root_name_policy: Default::default(),
                embedded_scan_mode: smartzip_core::EmbeddedScanMode::default(),
                dominant_min_ratio: 0.70,
                confirm_large_scan: false,
                force: false,
                limits: Default::default(),
            },
            None,
            None,
        )
        .await;

    // Target behavior (post-fix): leaf found at the end of zip→gzip→tar chain
    assert!(
        result.is_ok(),
        "zip -> tar.gz -> tar extraction failed (expected after P0-2 fix): {:?}",
        result
    );
    let workflow = result.unwrap();

    assert!(
        workflow.processed.len() >= 2,
        "expected >=2 processed, got {:?}",
        workflow.processed.len()
    );

    assert!(
        find_file(output.path(), "leaf_rt.txt").is_some(),
        "expected leaf_rt.txt from nested tar.gz chain in {:?}",
        output.path()
    );
    let recycled_names = recycled
        .lock()
        .unwrap()
        .iter()
        .filter_map(|path| path.file_name().and_then(|name| name.to_str()))
        .map(str::to_owned)
        .collect::<Vec<_>>();
    assert!(recycled_names.iter().any(|name| name == "real_tar.tar.gz"));
    assert!(recycled_names.iter().any(|name| name == "real_tar.tar"));
}

/// TDD regression: `zip -> inner.zip -> leaf` path collision.
///
/// `zip_inner_zip.zip` extracts a single file named `zip_inner_zip.zip`, which
/// is equivalent to the outer archive stem. That forces
/// `CommitSingleFileAsInnerName` for an archive file; the nested inner-zip
/// candidate must not treat that file path as a directory prefix.
///
/// Regression fixed by P0-2 nested output-root separation.
#[tokio::test]
#[allow(non_snake_case)]
async fn test_TDD_zip_inner_zip_single_file_path_collision() {
    let archive = fixture_path("zip_inner_zip.zip");
    assert!(archive.exists(), "fixture missing: zip_inner_zip.zip");

    let backend = backend();
    let db = SmartZipDb::in_memory().unwrap();
    let service = PasswordService::new(PasswordRepository::new(db.connection()));
    let (engine, recycled) = engine_with_test_recycler();
    let output = TempDir::new().unwrap();

    let result = engine
        .extract_recursive(
            &backend,
            &service,
            ExtractWorkflowRequest {
                inputs: vec![archive.clone()],
                output_dir: output.path().to_path_buf(),
                recursion_limit: 2,
                encoding_mode: EncodingMode::Auto,
                scanner: ScannerConfig::default(),
                password_candidates: PasswordCandidateRequest::default(),
                layout_policy: Default::default(),
                single_root_name_policy: Default::default(),
                embedded_scan_mode: smartzip_core::EmbeddedScanMode::default(),
                dominant_min_ratio: 0.70,
                confirm_large_scan: false,
                force: false,
                limits: Default::default(),
            },
            None,
            None,
        )
        .await;

    assert!(
        result.is_ok(),
        "zip -> inner.zip extraction failed (expected after P0-2 fix): {:?}",
        result
    );
    let workflow = result.unwrap();

    assert!(
        workflow.processed.len() >= 2,
        "expected outer + inner zip processed, got {:?}",
        workflow
            .processed
            .iter()
            .map(|c| c.path.display().to_string())
            .collect::<Vec<_>>()
    );

    assert!(
        find_file(output.path(), "zip_inner_leaf.txt").is_some(),
        "expected zip_inner_leaf.txt in {:?}",
        output.path()
    );
    assert!(
        recycled.lock().unwrap().iter().any(|path| {
            path.file_name().and_then(|name| name.to_str()) == Some("zip_inner_zip.zip")
        }),
        "processed inner zip should be recycled"
    );
}

/// TDD regression: alternate naming variant of the `.tar.gz` path collision.
///
/// `matching.tar.gz` → `matching.tar` → `leaf_m.txt`
/// Same root cause as `real_tar.tar.gz` — different name confirms the
/// trigger is the Equivalent similarity, not a specific name string.
///
/// Regression fixed by P0-2 nested output-root separation.
#[tokio::test]
#[allow(non_snake_case)]
async fn test_TDD_tar_gz_name_equivalent_variant_collision() {
    let archive = fixture_path("matching.tar.gz");
    assert!(archive.exists(), "fixture missing: matching.tar.gz");

    let backend = backend();
    let db = SmartZipDb::in_memory().unwrap();
    let service = PasswordService::new(PasswordRepository::new(db.connection()));
    let (engine, recycled) = engine_with_test_recycler();
    let output = TempDir::new().unwrap();

    let result = engine
        .extract_recursive(
            &backend,
            &service,
            ExtractWorkflowRequest {
                inputs: vec![archive.clone()],
                output_dir: output.path().to_path_buf(),
                recursion_limit: 2,
                encoding_mode: EncodingMode::Auto,
                scanner: ScannerConfig::default(),
                password_candidates: PasswordCandidateRequest::default(),
                layout_policy: Default::default(),
                single_root_name_policy: Default::default(),
                embedded_scan_mode: smartzip_core::EmbeddedScanMode::default(),
                dominant_min_ratio: 0.70,
                confirm_large_scan: false,
                force: false,
                limits: Default::default(),
            },
            None,
            None,
        )
        .await;

    assert!(
        result.is_ok(),
        "matching.tar.gz extraction failed (expected after P0-2 fix): {:?}",
        result
    );
    let workflow = result.unwrap();

    assert!(
        workflow.processed.len() >= 2,
        "expected >=2 processed, got {:?}",
        workflow.processed.len()
    );

    assert!(
        find_file(output.path(), "leaf_m.txt").is_some(),
        "expected leaf_m.txt in {:?}",
        output.path()
    );
    assert!(
        recycled
            .lock()
            .unwrap()
            .iter()
            .any(|path| path.file_name().and_then(|name| name.to_str()) == Some("matching.tar")),
        "processed inner tar should be recycled"
    );
}

#[tokio::test]
async fn test_nested_archive_recycle_failure_warns_and_preserves_success() {
    let archive = fixture_path("real_tar.tar.gz");
    let backend = backend();
    let db = SmartZipDb::in_memory().unwrap();
    let service = PasswordService::new(PasswordRepository::new(db.connection()));
    let recycler: ArchiveRecycleHandler = Arc::new(|_| {
        Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "test recycler rejected path",
        ))
    });
    let engine = SmartZipEngine::default().with_archive_recycler(recycler);
    let output = TempDir::new().unwrap();

    let result = engine
        .extract_recursive(
            &backend,
            &service,
            ExtractWorkflowRequest {
                inputs: vec![archive],
                output_dir: output.path().to_path_buf(),
                recursion_limit: 2,
                encoding_mode: EncodingMode::Auto,
                scanner: ScannerConfig::default(),
                password_candidates: PasswordCandidateRequest::default(),
                layout_policy: Default::default(),
                single_root_name_policy: Default::default(),
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

    assert_eq!(result.processed.len(), 2);
    assert!(find_file(output.path(), "leaf_rt.txt").is_some());
    assert!(
        find_file(output.path(), "real_tar.tar").is_some(),
        "failed recycle must preserve the inner archive"
    );
    assert!(result.events.iter().any(|event| matches!(
        event.kind,
        smartzip_core::TaskEventKind::Warning { ref message }
            if message.contains("failed to move processed nested archive")
    )));
}

// ── Engine: full extract_recursive ────────────────────────────────────────

#[tokio::test]
async fn test_engine_extract_simple_utf8() {
    let archive = fixture_path("enc_utf8.zip");
    assert!(archive.exists(), "fixture missing: enc_utf8.zip");

    let backend = backend();
    let db = SmartZipDb::in_memory().unwrap();
    let service = PasswordService::new(PasswordRepository::new(db.connection()));
    let engine = SmartZipEngine::default();
    let output = TempDir::new().unwrap();

    let result = engine
        .extract_recursive(
            &backend,
            &service,
            ExtractWorkflowRequest {
                inputs: vec![archive.clone()],
                output_dir: output.path().to_path_buf(),
                recursion_limit: 1,
                encoding_mode: EncodingMode::Auto,
                scanner: ScannerConfig::default(),
                password_candidates: PasswordCandidateRequest::default(),
                layout_policy: smartzip_engine::layout::OutputLayoutPolicy::default(),
                single_root_name_policy: smartzip_engine::layout::SingleRootNamePolicy::default(),
                embedded_scan_mode: smartzip_core::EmbeddedScanMode::default(),
                dominant_min_ratio: 0.70,
                confirm_large_scan: false,
                force: false,
                limits: Default::default(),
            },
            None,
            None,
        )
        .await;

    assert!(
        result.is_ok(),
        "engine extract_recursive failed: {result:?}"
    );

    let workflow_result = result.unwrap();
    assert_eq!(workflow_result.processed.len(), 1);
    assert!(workflow_result.processed.iter().any(|c| c.path == archive));
}

#[tokio::test]
async fn test_engine_extract_password_archive() {
    let archive = fixture_path("pass_cn.zip");
    assert!(archive.exists(), "fixture missing: pass_cn.zip");

    let backend = backend();
    let db = SmartZipDb::in_memory().unwrap();
    let service = PasswordService::new(PasswordRepository::new(db.connection()));

    // Pre-register the correct password in the database
    service.add_password("中文密码123", "test", false).unwrap();

    let engine = SmartZipEngine::default();
    let output = TempDir::new().unwrap();

    let result = engine
        .extract_recursive(
            &backend,
            &service,
            ExtractWorkflowRequest {
                inputs: vec![archive.clone()],
                output_dir: output.path().to_path_buf(),
                recursion_limit: 1,
                encoding_mode: EncodingMode::Auto,
                scanner: ScannerConfig::default(),
                password_candidates: PasswordCandidateRequest::default(),
                layout_policy: smartzip_engine::layout::OutputLayoutPolicy::default(),
                single_root_name_policy: smartzip_engine::layout::SingleRootNamePolicy::default(),
                embedded_scan_mode: smartzip_core::EmbeddedScanMode::default(),
                dominant_min_ratio: 0.70,
                confirm_large_scan: false,
                force: false,
                limits: Default::default(),
            },
            None,
            None,
        )
        .await;

    assert!(
        result.is_ok(),
        "engine extract_recursive for password archive failed: {result:?}"
    );

    let workflow_result = result.unwrap();
    assert_eq!(
        workflow_result.processed.len(),
        1,
        "expected 1 processed, got {:?}",
        workflow_result.processed
    );
    assert!(
        workflow_result.skipped.is_empty(),
        "expected 0 skipped, got {:?}",
        workflow_result.skipped
    );
}

#[tokio::test]
async fn test_engine_wrong_manual_password_is_not_saved_to_database() {
    let archive = fixture_path("pass_cn.zip");
    assert!(archive.exists(), "fixture missing: pass_cn.zip");

    let backend = backend();
    let db = SmartZipDb::in_memory().unwrap();
    let service = PasswordService::new(PasswordRepository::new(db.connection()));
    let engine = SmartZipEngine::default();
    let output = TempDir::new().unwrap();

    let result = engine
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
                    manual: vec!["wrong-password".into()],
                    clipboard: None,
                    include_empty: false,
                    limit: 8,
                },
                layout_policy: smartzip_engine::layout::OutputLayoutPolicy::default(),
                single_root_name_policy: smartzip_engine::layout::SingleRootNamePolicy::default(),
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
    assert_eq!(result.skipped.len(), 1);

    let repo = PasswordRepository::new(db.connection());
    assert!(repo.get_by_value("wrong-password").unwrap().is_none());
}

#[tokio::test]
async fn test_engine_successful_manual_password_is_saved_to_database() {
    let archive = fixture_path("pass_cn.zip");
    assert!(archive.exists(), "fixture missing: pass_cn.zip");

    let backend = backend();
    let db = SmartZipDb::in_memory().unwrap();
    let service = PasswordService::new(PasswordRepository::new(db.connection()));
    let engine = SmartZipEngine::default();
    let output = TempDir::new().unwrap();

    let result = engine
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
                    manual: vec!["中文密码123".into()],
                    clipboard: None,
                    include_empty: false,
                    limit: 8,
                },
                layout_policy: smartzip_engine::layout::OutputLayoutPolicy::default(),
                single_root_name_policy: smartzip_engine::layout::SingleRootNamePolicy::default(),
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

    let repo = PasswordRepository::new(db.connection());
    let saved = repo.get_by_value("中文密码123").unwrap();
    assert!(
        saved.is_some(),
        "successful manual password should be persisted"
    );
}

struct StaticPasswordPrompter {
    password: String,
}

#[async_trait]
impl InteractivePasswordPrompter for StaticPasswordPrompter {
    async fn prompt(&self, _archive_path: &Path) -> Option<String> {
        Some(self.password.clone())
    }
}

struct ExtractEmbeddedPrompter;

#[async_trait]
impl InteractiveEmbeddedPrompter for ExtractEmbeddedPrompter {
    async fn prompt(
        &self,
        _archive_path: &Path,
        _decision: &smartzip_core::DetectionDecision,
    ) -> EmbeddedSelectionChoice {
        EmbeddedSelectionChoice::Extract
    }
}

#[tokio::test]
async fn test_engine_interactive_password_reuses_carved_embedded_archive_path() {
    let archive = fixture_path("video_7z_pass.mp4");
    assert!(archive.exists(), "fixture missing: video_7z_pass.mp4");

    let backend = backend();
    let db = SmartZipDb::in_memory().unwrap();
    let service = PasswordService::new(PasswordRepository::new(db.connection()));
    let engine = SmartZipEngine::default().with_min_embedded_size_bytes(0);
    let output = TempDir::new().unwrap();
    let prompter = StaticPasswordPrompter {
        password: "video-pass".into(),
    };
    let embedded_prompter = ExtractEmbeddedPrompter;

    let result = engine
        .extract_recursive_interactive(
            &backend,
            &service,
            ExtractWorkflowRequest {
                inputs: vec![archive.clone()],
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
                layout_policy: smartzip_engine::layout::OutputLayoutPolicy::default(),
                single_root_name_policy: smartzip_engine::layout::SingleRootNamePolicy::default(),
                embedded_scan_mode: smartzip_core::EmbeddedScanMode::default(),
                dominant_min_ratio: 0.70,
                confirm_large_scan: false,
                force: false,
                limits: Default::default(),
            },
            Some(&prompter),
            None,
            Some(&embedded_prompter),
            None,
        )
        .await
        .unwrap();

    assert_eq!(result.processed.len(), 1);
    assert!(
        result.processed[0].embedded_offset.is_some(),
        "fixture should have been handled as an embedded archive"
    );
    assert!(
        find_file(output.path(), "secret.txt").is_some(),
        "interactive fallback should extract the embedded 7z payload"
    );
}

#[tokio::test]
async fn test_router_extracts_split_encrypted_zip_with_manual_password() {
    let root = TempDir::new().unwrap();
    let archive = create_split_encrypted_zip(root.path(), "secret");

    let backend = router();
    let db = SmartZipDb::in_memory().unwrap();
    let service = PasswordService::new(PasswordRepository::new(db.connection()));
    let engine = SmartZipEngine::default();
    let output = TempDir::new().unwrap();

    let result = engine
        .extract_recursive(
            &backend,
            &service,
            ExtractWorkflowRequest {
                inputs: vec![archive],
                output_dir: output.path().to_path_buf(),
                recursion_limit: 0,
                encoding_mode: EncodingMode::Auto,
                scanner: ScannerConfig::default(),
                password_candidates: PasswordCandidateRequest {
                    manual: vec!["secret".into()],
                    clipboard: None,
                    include_empty: false,
                    limit: 8,
                },
                layout_policy: smartzip_engine::layout::OutputLayoutPolicy::default(),
                single_root_name_policy: smartzip_engine::layout::SingleRootNamePolicy::default(),
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

    assert_eq!(
        result.processed.len(),
        1,
        "split zip should extract successfully"
    );
    assert!(result.skipped.is_empty(), "split zip should not be skipped");
    assert!(
        find_file(output.path(), "big.bin").is_some(),
        "split zip payload should be extracted"
    );
}

#[tokio::test]
async fn test_router_prompts_for_split_encrypted_zip_password_when_missing() {
    let root = TempDir::new().unwrap();
    let archive = create_split_encrypted_zip(root.path(), "secret");

    let backend = router();
    let db = SmartZipDb::in_memory().unwrap();
    let service = PasswordService::new(PasswordRepository::new(db.connection()));
    let engine = SmartZipEngine::default();
    let output = TempDir::new().unwrap();
    let prompter = StaticPasswordPrompter {
        password: "secret".into(),
    };

    let result = engine
        .extract_recursive(
            &backend,
            &service,
            ExtractWorkflowRequest {
                inputs: vec![archive],
                output_dir: output.path().to_path_buf(),
                recursion_limit: 0,
                encoding_mode: EncodingMode::Auto,
                scanner: ScannerConfig::default(),
                password_candidates: PasswordCandidateRequest {
                    manual: Vec::new(),
                    clipboard: None,
                    include_empty: true,
                    limit: 8,
                },
                layout_policy: smartzip_engine::layout::OutputLayoutPolicy::default(),
                single_root_name_policy: smartzip_engine::layout::SingleRootNamePolicy::default(),
                embedded_scan_mode: smartzip_core::EmbeddedScanMode::default(),
                dominant_min_ratio: 0.70,
                confirm_large_scan: false,
                force: false,
                limits: Default::default(),
            },
            Some(&prompter),
            None,
        )
        .await
        .unwrap();

    assert_eq!(
        result.processed.len(),
        1,
        "prompted split zip should extract"
    );
    assert!(
        result
            .events
            .iter()
            .all(|event| !matches!(event.kind, smartzip_core::TaskEventKind::Failed { .. })),
        "prompted split zip should not end in failure: {:?}",
        result.events
    );
    assert!(
        find_file(output.path(), "big.bin").is_some(),
        "prompted split zip payload should be extracted"
    );
}

#[tokio::test]
async fn test_engine_respects_recursion_limit() {
    let archive = fixture_path("nested_multi_level.zip");
    assert!(archive.exists(), "fixture missing: nested_multi_level.zip");

    let backend = backend();
    let db = SmartZipDb::in_memory().unwrap();
    let service = PasswordService::new(PasswordRepository::new(db.connection()));
    let (engine, _) = engine_with_test_recycler();
    let output = TempDir::new().unwrap();

    let result = engine
        .extract_recursive(
            &backend,
            &service,
            ExtractWorkflowRequest {
                inputs: vec![archive.clone()],
                output_dir: output.path().to_path_buf(),
                recursion_limit: 1,
                encoding_mode: EncodingMode::Auto,
                scanner: ScannerConfig::default(),
                password_candidates: PasswordCandidateRequest::default(),
                layout_policy: smartzip_engine::layout::OutputLayoutPolicy::default(),
                single_root_name_policy: smartzip_engine::layout::SingleRootNamePolicy::default(),
                embedded_scan_mode: smartzip_core::EmbeddedScanMode::default(),
                dominant_min_ratio: 0.70,
                confirm_large_scan: false,
                force: false,
                limits: Default::default(),
            },
            None,
            None,
        )
        .await;

    assert!(
        result.is_ok(),
        "engine extract_recursive (multi-level) failed: {result:?}"
    );

    let workflow_result = result.unwrap();
    // recursion_limit=1: depth 0 (outer zip) + depth 1 (L2.zip) = 2 processed.
    // L3.zip at depth 2 is enqueued but skipped.
    assert!(
        workflow_result.processed.len() >= 1,
        "should process at least the outer archive, got {:?}",
        workflow_result
            .processed
            .iter()
            .map(|c| c.path.display().to_string())
            .collect::<Vec<_>>()
    );
    assert!(
        workflow_result
            .skipped
            .iter()
            .any(|c| c.path.ends_with("L3.zip")),
        "L3.zip at depth 2 should be skipped due to recursion limit"
    );
}

#[tokio::test]
async fn test_engine_extracts_nested_multi_level_without_path_collision() {
    let archive = fixture_path("nested_multi_level.zip");
    assert!(archive.exists(), "fixture missing: nested_multi_level.zip");

    let backend = backend();
    let db = SmartZipDb::in_memory().unwrap();
    let service = PasswordService::new(PasswordRepository::new(db.connection()));
    let (engine, recycled) = engine_with_test_recycler();
    let output = TempDir::new().unwrap();

    let result = engine
        .extract_recursive(
            &backend,
            &service,
            ExtractWorkflowRequest {
                inputs: vec![archive.clone()],
                output_dir: output.path().to_path_buf(),
                recursion_limit: 3,
                encoding_mode: EncodingMode::Auto,
                scanner: ScannerConfig::default(),
                password_candidates: PasswordCandidateRequest::default(),
                layout_policy: smartzip_engine::layout::OutputLayoutPolicy::default(),
                single_root_name_policy: smartzip_engine::layout::SingleRootNamePolicy::default(),
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

    assert_eq!(
        result.processed.len(),
        3,
        "outer + L2 + L3 should all extract"
    );
    assert!(
        result.skipped.is_empty(),
        "no candidate should be skipped: {:?}",
        result.skipped
    );
    assert!(
        result
            .events
            .iter()
            .all(|event| !matches!(event.kind, smartzip_core::TaskEventKind::Failed { .. })),
        "unexpected failure events: {:?}",
        result.events
    );
    assert!(
        find_file(output.path(), "deep.txt").is_some(),
        "expected deepest file to be extracted"
    );
    assert_eq!(
        recycled.lock().unwrap().len(),
        2,
        "L2.zip and L3.zip should both be recycled after deeper discovery"
    );
}

#[tokio::test]
async fn test_engine_reports_wrong_password_as_failure() {
    let archive = fixture_path("pass_cn.zip");
    assert!(archive.exists(), "fixture missing: pass_cn.zip");

    let backend = backend();
    let db = SmartZipDb::in_memory().unwrap();
    let service = PasswordService::new(PasswordRepository::new(db.connection()));
    let engine = SmartZipEngine::default();
    let output = TempDir::new().unwrap();

    let result = engine
        .extract_recursive(
            &backend,
            &service,
            ExtractWorkflowRequest {
                inputs: vec![archive.clone()],
                output_dir: output.path().to_path_buf(),
                recursion_limit: 1,
                encoding_mode: EncodingMode::Auto,
                scanner: ScannerConfig::default(),
                password_candidates: PasswordCandidateRequest {
                    manual: vec!["wrong-password".into()],
                    clipboard: None,
                    include_empty: false,
                    limit: 8,
                },
                layout_policy: smartzip_engine::layout::OutputLayoutPolicy::default(),
                single_root_name_policy: smartzip_engine::layout::SingleRootNamePolicy::default(),
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
    assert_eq!(result.skipped.len(), 1);
    assert!(
        result.events.iter().any(|event| matches!(
            event.kind,
            smartzip_core::TaskEventKind::Failed { ref error }
            if error.contains("wrong password")
        )),
        "wrong password should surface as a failure event: {:?}",
        result.events
    );
}

#[tokio::test]
async fn test_engine_preserves_nested_archive_paths() {
    let archive = fixture_path("nested_zip_in_zip.zip");
    assert!(archive.exists(), "fixture missing: nested_zip_in_zip.zip");

    let backend = backend();
    let db = SmartZipDb::in_memory().unwrap();
    let service = PasswordService::new(PasswordRepository::new(db.connection()));
    let (engine, _) = engine_with_test_recycler();
    let output = TempDir::new().unwrap();

    let result = engine
        .extract_recursive(
            &backend,
            &service,
            ExtractWorkflowRequest {
                inputs: vec![archive.clone()],
                output_dir: output.path().to_path_buf(),
                recursion_limit: 2,
                encoding_mode: EncodingMode::Auto,
                scanner: ScannerConfig::default(),
                password_candidates: PasswordCandidateRequest::default(),
                layout_policy: smartzip_engine::layout::OutputLayoutPolicy::default(),
                single_root_name_policy: smartzip_engine::layout::SingleRootNamePolicy::default(),
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

    assert_eq!(
        result.processed.len(),
        2,
        "outer and inner archives should be processed"
    );

    let nested_file = output
        .path()
        .join("nested_zip_in_zip")
        .join("inner")
        .join("hello.txt");
    assert!(
        nested_file.exists(),
        "expected nested output at {}",
        nested_file.display()
    );
}

// ── Format detection (rstest parametrized) ────────────────────────────────

#[rstest]
#[case("archive.zip", Some(ArchiveFormat::Zip))]
#[case("archive.7z", Some(ArchiveFormat::SevenZip))]
#[case("archive.rar", Some(ArchiveFormat::Rar))]
#[case("archive.tar", Some(ArchiveFormat::Tar))]
#[case("archive.tar.gz", Some(ArchiveFormat::Gzip))]
#[case("archive.txt", None)]
#[case("archive", None)]
fn test_format_from_extension(#[case] path: &str, #[case] expected: Option<ArchiveFormat>) {
    assert_eq!(format_from_extension(path), expected);
}

// ── Encoding detection via fixture archives ───────────────────────────────

/// Extract raw filenames from a zip file by parsing local file headers.
fn raw_zip_filenames(path: &Path) -> Vec<u8> {
    let data = std::fs::read(path).expect("should read fixture");
    let mut filenames = Vec::new();
    let mut pos = 0usize;

    while pos + 30 <= data.len() {
        // Look for local file header signature
        if &data[pos..pos + 4] != b"PK\x03\x04" {
            pos += 1;
            continue;
        }

        let filename_len = u16::from_le_bytes([data[pos + 26], data[pos + 27]]) as usize;
        let extra_len = u16::from_le_bytes([data[pos + 28], data[pos + 29]]) as usize;

        let fn_start = pos + 30;
        if fn_start + filename_len > data.len() {
            break;
        }

        if filename_len > 0 {
            let raw_name = &data[fn_start..fn_start + filename_len];
            if !filenames.is_empty() {
                filenames.push(b'/');
            }
            filenames.extend_from_slice(raw_name);
        }

        pos = fn_start + filename_len + extra_len;
    }

    filenames
}

#[rstest]
#[case::utf8("enc_utf8.zip", &["UTF-8"])]
#[case::gbk("enc_gbk.zip", &["GB18030", "GBK"])]
#[case::sjis("enc_sjis.zip", &["Shift_JIS"])]
#[case::euckr("enc_euckr.zip", &["EUC-KR"])]
#[case::big5("enc_big5.zip", &["Big5"])]
fn test_encoding_detection_from_fixture(
    #[case] fixture_name: &str,
    #[case] expected_encodings: &[&str],
) {
    let path = fixture_path(fixture_name);
    assert!(path.exists(), "fixture missing: {fixture_name}");

    let raw_names = raw_zip_filenames(&path);
    assert!(
        !raw_names.is_empty(),
        "should extract filenames from {fixture_name}"
    );

    let mut detector = ArchiveEncodingDetector::new();
    let result = detector.detect(&raw_names);

    let found = expected_encodings
        .iter()
        .any(|enc| result.selected == *enc || result.candidates.iter().any(|c| c.name == *enc));

    assert!(
        found,
        "{fixture_name}: expected one of {expected_encodings:?} in selected '{}' or candidates {:?}",
        result.selected,
        result.candidates.iter().map(|c| &c.name).collect::<Vec<_>>()
    );
}

// ── Fixture file existence check ──────────────────────────────────────────

#[rstest]
#[case("enc_utf8.zip")]
#[case("enc_gbk.zip")]
#[case("enc_sjis.zip")]
#[case("enc_euckr.zip")]
#[case("enc_big5.zip")]
#[case("pass_cn.zip")]
#[case("pass_jp.7z")]
#[case("pass_kr.zip")]
#[case("pass_emoji.zip")]
#[case("pass_rtl.zip")]
#[case("nested_zip_in_zip.zip")]
#[case("nested_7z_in_zip.zip")]
#[case("nested_multi_level.zip")]
#[case("nested_mixed_formats.zip")]
#[case("real_tar.tar.gz")]
#[case("matching.tar.gz")]
#[case("zip_containing_real_tar_gz.zip")]
#[case("zip_inner_zip.zip")]
fn test_fixture_exists(#[case] name: &str) {
    let path = fixture_path(name);
    assert!(
        path.exists(),
        "fixture '{name}' not found. Run: cd tests/fixtures && python3 generate.py"
    );
}

// ── Helper: find a file recursively ───────────────────────────────────────

fn find_file(dir: &Path, filename: &str) -> Option<PathBuf> {
    if !dir.is_dir() {
        return None;
    }
    for entry in std::fs::read_dir(dir).ok()? {
        let entry = entry.ok()?;
        let path = entry.path();
        if path.file_name().map_or(false, |n| n == filename) {
            return Some(path);
        }
        if path.is_dir() {
            if let Some(found) = find_file(&path, filename) {
                return Some(found);
            }
        }
    }
    None
}
