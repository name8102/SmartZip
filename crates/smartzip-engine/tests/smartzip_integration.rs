//! SmartZip integration tests using archive fixtures.
//!
//! Requires `7z` or `7zz` in PATH. Generate fixtures first:
//!   cd tests/fixtures && python3 generate.py && cd ../..
//! Run:
//!   cargo test -p smartzip-engine --test smartzip_integration -- --test-threads=1

use async_trait::async_trait;
use rstest::*;
use smartzip_archive::{
    ArchiveBackend, ExtractArchiveRequest, ListRequest, SevenZipBackend, SevenZipLocator,
    TestRequest,
};
use smartzip_core::{ArchiveFormat, EncodingMode};
use smartzip_db::{password::PasswordRepository, SmartZipDb};
use smartzip_encoding::ArchiveEncodingDetector;
use smartzip_engine::{
    format_from_extension, is_first_volume, ExtractWorkflowRequest, InteractivePasswordPrompter,
    SmartZipEngine,
};
use smartzip_passwords::{PasswordCandidateRequest, PasswordService};
use smartzip_scanner::{EmbeddedScanner, ScannerConfig};
use std::path::{Path, PathBuf};
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

fn backend() -> SevenZipBackend {
    SevenZipBackend::locate(&SevenZipLocator::default())
        .expect("7z/7zz must be available in PATH to run integration tests")
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

#[tokio::test]
async fn test_engine_interactive_password_reuses_carved_embedded_archive_path() {
    let archive = fixture_path("video_7z_pass.mp4");
    assert!(archive.exists(), "fixture missing: video_7z_pass.mp4");

    let backend = backend();
    let db = SmartZipDb::in_memory().unwrap();
    let service = PasswordService::new(PasswordRepository::new(db.connection()));
    let engine = SmartZipEngine::default();
    let output = TempDir::new().unwrap();
    let prompter = StaticPasswordPrompter {
        password: "video-pass".into(),
    };

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
                    manual: Vec::new(),
                    clipboard: None,
                    include_empty: false,
                    limit: 8,
                },
            },
            Some(&prompter),
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
async fn test_engine_respects_recursion_limit() {
    let archive = fixture_path("nested_multi_level.zip");
    assert!(archive.exists(), "fixture missing: nested_multi_level.zip");

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
async fn test_engine_preserves_nested_archive_paths() {
    let archive = fixture_path("nested_zip_in_zip.zip");
    assert!(archive.exists(), "fixture missing: nested_zip_in_zip.zip");

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
                recursion_limit: 2,
                encoding_mode: EncodingMode::Auto,
                scanner: ScannerConfig::default(),
                password_candidates: PasswordCandidateRequest::default(),
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

// ── Volume detection (rstest parametrized) ────────────────────────────────

#[rstest]
#[case("archive.part1.rar", true)]
#[case("archive.part2.rar", false)]
#[case("archive.part5.rar", false)]
#[case("archive.001", true)]
#[case("archive.002", false)]
#[case("archive.zip", true)]
#[case("archive.7z", true)]
fn test_is_first_volume(#[case] path: &str, #[case] expected: bool) {
    assert_eq!(is_first_volume(path), expected);
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
