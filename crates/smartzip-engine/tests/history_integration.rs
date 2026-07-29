//! Integration tests for task-history persistence (v3, file-grain).
//!
//! Runs a real extraction with a [`DbTaskHistoryRecorder`] attached and
//! asserts that the `tasks`, `task_events`, `file_extractions`, and
//! `known_files` tables receive the expected rows. Requires `7z`/`7zz` in
//! PATH, like the other engine integration suites.

use smartzip_archive::{BackendRouter, NativeZipBackend, SevenZipBackend, SevenZipLocator};
use smartzip_core::EncodingMode;
use smartzip_db::{
    file_extractions::FileExtractionRepository, known_files::KnownFileRepository,
    password::PasswordRepository, task::TaskRepository, task_event::TaskEventRepository,
    SmartZipDb,
};
use smartzip_engine::{history::DbTaskHistoryRecorder, ExtractWorkflowRequest, SmartZipEngine};
use smartzip_passwords::{PasswordCandidateRequest, PasswordService};
use smartzip_scanner::ScannerConfig;
use std::path::PathBuf;
use tempfile::TempDir;

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

fn backend() -> BackendRouter {
    let seven_zip = SevenZipBackend::locate(&SevenZipLocator::default())
        .expect("7z/7zz must be available in PATH to run integration tests");
    BackendRouter::new(NativeZipBackend::new(), None, Some(seven_zip))
}

fn request(inputs: Vec<PathBuf>, output_dir: PathBuf) -> ExtractWorkflowRequest {
    ExtractWorkflowRequest {
        inputs,
        output_dir,
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
    }
}

#[tokio::test]
async fn extract_records_task_events_and_file_row() {
    let archive = fixture_path("enc_utf8.zip");
    assert!(archive.exists(), "fixture missing: enc_utf8.zip");

    let backend = backend();
    let db = SmartZipDb::in_memory().unwrap();
    let service = PasswordService::new(PasswordRepository::new(db.connection()));
    let recorder = DbTaskHistoryRecorder::new(db.connection());
    let engine = SmartZipEngine::default();
    let output = TempDir::new().unwrap();

    let result = engine
        .extract_recursive_with_listener_interactive(
            &backend,
            &service,
            request(vec![archive.clone()], output.path().to_path_buf()),
            None,
            None,
            None,
            None,
            None,
            Some(&recorder),
        )
        .await
        .expect("extraction should succeed");

    // The task row exists and is closed out as completed.
    let task_repo = TaskRepository::new(db.connection());
    let task = task_repo
        .find_by_id(result.task_id.as_str())
        .unwrap()
        .expect("task row should be recorded");
    assert_eq!(task.kind, "extract");
    assert_eq!(task.status, "completed");
    assert!(
        task.finished_at.is_some(),
        "completed task should have a finished_at timestamp"
    );

    // The event timeline was persisted, including terminal states.
    let event_repo = TaskEventRepository::new(db.connection());
    let events = event_repo.list_by_task(result.task_id.as_str()).unwrap();
    assert!(
        events.iter().any(|e| e.event_type == "Started"),
        "timeline should include a Started event"
    );
    assert!(
        events.iter().any(|e| e.event_type == "OutputCreated"),
        "timeline should include an OutputCreated event"
    );
    assert_eq!(
        events
            .iter()
            .filter(|e| e.event_type == "OutputCreated")
            .count(),
        1,
        "events should be persisted once when the final snapshot is replayed",
    );
    assert!(
        events.iter().any(|e| e.event_type == "Completed"),
        "timeline should include a Completed event"
    );

    // A per-file extraction row was logged for the root input.
    let file_repo = FileExtractionRepository::new(db.connection());
    let rows = file_repo.list_by_task(result.task_id.as_str()).unwrap();
    let extracted: Vec<_> = rows.iter().filter(|r| r.status == "extracted").collect();
    assert!(
        !extracted.is_empty(),
        "expected at least one extracted file_extractions row"
    );
    let root = &extracted[0];
    assert!(
        root.sample_hash.is_some() && root.file_size.is_some(),
        "a whole-file extraction should carry a sample hash and size"
    );
    assert!(
        root.output_path.is_some(),
        "an extracted row records where it landed"
    );

    // known_files gained a dedup/reuse entry with last_extract_at set.
    let known_repo = KnownFileRepository::new(db.connection());
    let hash = root.sample_hash.as_deref().unwrap();
    let size = root.file_size.unwrap();
    let known = known_repo
        .find(hash, size)
        .unwrap()
        .expect("known_files should have an entry after a successful extract");
    assert!(
        known.last_extract_at.is_some(),
        "a successful extract stamps last_extract_at"
    );
}

#[tokio::test]
async fn successful_password_is_recorded_on_file_row_and_known_files() {
    let archive = fixture_path("pass_cn.zip");
    assert!(archive.exists(), "fixture missing: pass_cn.zip");

    let backend = backend();
    let db = SmartZipDb::in_memory().unwrap();
    let service = PasswordService::new(PasswordRepository::new(db.connection()));
    let password_id = service.add_password("中文密码123", "test", false).unwrap();
    let recorder = DbTaskHistoryRecorder::new(db.connection());
    let engine = SmartZipEngine::default();
    let output = TempDir::new().unwrap();

    let result = engine
        .extract_recursive_with_listener_interactive(
            &backend,
            &service,
            request(vec![archive.clone()], output.path().to_path_buf()),
            None,
            None,
            None,
            None,
            None,
            Some(&recorder),
        )
        .await
        .expect("extraction should succeed");

    // The file row records the password that unlocked the archive.
    let file_repo = FileExtractionRepository::new(db.connection());
    let rows = file_repo.list_by_task(result.task_id.as_str()).unwrap();
    let unlocked = rows
        .iter()
        .find(|r| r.status == "extracted" && r.has_password)
        .expect("expected an extracted row flagged has_password");
    assert_eq!(
        unlocked.password_id,
        Some(password_id),
        "the unlocking password id is recorded on the file row"
    );

    // known_files remembers the password for reuse on the next run.
    let known_repo = KnownFileRepository::new(db.connection());
    let hash = unlocked.sample_hash.as_deref().unwrap();
    let size = unlocked.file_size.unwrap();
    let known = known_repo.find(hash, size).unwrap().unwrap();
    assert_eq!(
        known.password_id,
        Some(password_id),
        "known_files caches the unlocking password for reuse"
    );
}

#[tokio::test]
async fn extraction_without_recorder_writes_no_history() {
    let archive = fixture_path("enc_utf8.zip");
    assert!(archive.exists(), "fixture missing: enc_utf8.zip");

    let backend = backend();
    let db = SmartZipDb::in_memory().unwrap();
    let service = PasswordService::new(PasswordRepository::new(db.connection()));
    let engine = SmartZipEngine::default();
    let output = TempDir::new().unwrap();

    engine
        .extract_recursive(
            &backend,
            &service,
            request(vec![archive], output.path().to_path_buf()),
            None,
            None,
        )
        .await
        .expect("extraction should succeed");

    let task_repo = TaskRepository::new(db.connection());
    assert!(
        task_repo.recent(10).unwrap().is_empty(),
        "no recorder attached → no task rows should be written"
    );
}

#[tokio::test]
async fn duplicate_is_skipped_and_force_reextracts() {
    let archive = fixture_path("enc_utf8.zip");
    let backend = backend();
    let db = SmartZipDb::in_memory().unwrap();
    let service = PasswordService::new(PasswordRepository::new(db.connection()));
    let recorder = DbTaskHistoryRecorder::new(db.connection());
    let engine = SmartZipEngine::default();

    let first_output = TempDir::new().unwrap();
    let first = engine
        .extract_recursive_with_listener_interactive(
            &backend,
            &service,
            request(vec![archive.clone()], first_output.path().to_path_buf()),
            None,
            None,
            None,
            None,
            None,
            Some(&recorder),
        )
        .await
        .unwrap();
    assert_eq!(first.processed.len(), 1);

    let second_output = TempDir::new().unwrap();
    let second = engine
        .extract_recursive_with_listener_interactive(
            &backend,
            &service,
            request(vec![archive.clone()], second_output.path().to_path_buf()),
            None,
            None,
            None,
            None,
            None,
            Some(&recorder),
        )
        .await
        .unwrap();
    assert!(second.processed.is_empty());
    let duplicate_rows = FileExtractionRepository::new(db.connection())
        .list_by_task(second.task_id.as_str())
        .unwrap();
    assert!(duplicate_rows
        .iter()
        .any(|row| row.status == "skipped" && row.reason.as_deref() == Some("duplicate")));

    let forced_output = TempDir::new().unwrap();
    let mut forced_request = request(vec![archive], forced_output.path().to_path_buf());
    forced_request.force = true;
    let forced = engine
        .extract_recursive_with_listener_interactive(
            &backend,
            &service,
            forced_request,
            None,
            None,
            None,
            None,
            None,
            Some(&recorder),
        )
        .await
        .unwrap();
    assert_eq!(forced.processed.len(), 1);
}

#[tokio::test]
async fn explicit_encoding_is_saved_for_future_known_file_reuse() {
    let archive = fixture_path("enc_utf8.zip");
    let backend = backend();
    let db = SmartZipDb::in_memory().unwrap();
    let service = PasswordService::new(PasswordRepository::new(db.connection()));
    let recorder = DbTaskHistoryRecorder::new(db.connection());
    let engine = SmartZipEngine::default();
    let output = TempDir::new().unwrap();
    let mut extract_request = request(vec![archive.clone()], output.path().to_path_buf());
    extract_request.encoding_mode = EncodingMode::Override("utf-8".to_string());

    let result = engine
        .extract_recursive_with_listener_interactive(
            &backend,
            &service,
            extract_request,
            None,
            None,
            None,
            None,
            None,
            Some(&recorder),
        )
        .await
        .unwrap();

    let row = FileExtractionRepository::new(db.connection())
        .list_by_task(result.task_id.as_str())
        .unwrap()
        .into_iter()
        .find(|row| row.status == "extracted")
        .unwrap();
    assert!(row.encoding_corrected);
    let known = KnownFileRepository::new(db.connection())
        .find(row.sample_hash.as_deref().unwrap(), row.file_size.unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(known.confirmed_encoding.as_deref(), Some("utf-8"));

    let reused_output = TempDir::new().unwrap();
    let mut reused_request = request(vec![archive], reused_output.path().to_path_buf());
    reused_request.force = true;
    let reused = engine
        .extract_recursive_with_listener_interactive(
            &backend,
            &service,
            reused_request,
            None,
            None,
            None,
            None,
            None,
            Some(&recorder),
        )
        .await
        .unwrap();
    let reused_row = FileExtractionRepository::new(db.connection())
        .list_by_task(reused.task_id.as_str())
        .unwrap()
        .into_iter()
        .find(|row| row.status == "extracted")
        .unwrap();
    assert_eq!(reused_row.encoding.as_deref(), Some("utf-8"));
    assert!(reused_row.encoding_corrected);
}
