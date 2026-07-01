//! Integration tests for task-history persistence.
//!
//! Runs a real extraction with a [`DbTaskHistoryRecorder`] attached and
//! asserts that the `tasks`, `task_events`, and (where applicable)
//! `encoding_detections` / `password_matches` tables receive rows. Requires
//! `7z`/`7zz` in PATH, like the other engine integration suites.

use smartzip_core::EncodingMode;
use smartzip_db::{
    encoding_detection::EncodingDetectionRepository,
    password::PasswordRepository,
    task::TaskRepository,
    task_event::TaskEventRepository,
    SmartZipDb,
};
use smartzip_engine::{
    history::DbTaskHistoryRecorder, ExtractWorkflowRequest, SmartZipEngine,
};
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

fn backend() -> smartzip_archive::SevenZipBackend {
    smartzip_archive::SevenZipBackend::locate(&smartzip_archive::SevenZipLocator::default())
        .expect("7z/7zz must be available in PATH to run integration tests")
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
    }
}

#[tokio::test]
async fn extract_records_task_and_events() {
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
        task.password_attempts >= 1,
        "expected at least one password attempt recorded, got {}",
        task.password_attempts
    );
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
    assert!(
        events.iter().any(|e| e.event_type == "Completed"),
        "timeline should include a Completed event"
    );

    // A UTF-8 ZIP goes through encoding assessment, so a detection row lands.
    let enc_repo = EncodingDetectionRepository::new(db.connection());
    let hash = smartzip_db::path_hash::path_hash(&archive);
    let detections = enc_repo.recent_by_hash(&hash, 5).unwrap();
    assert!(
        !detections.is_empty(),
        "expected an encoding_detections row for a UTF-8 ZIP"
    );
}

#[tokio::test]
async fn successful_password_backfills_password_match() {
    let archive = fixture_path("pass_cn.zip");
    assert!(archive.exists(), "fixture missing: pass_cn.zip");

    let backend = backend();
    let db = SmartZipDb::in_memory().unwrap();
    let service = PasswordService::new(PasswordRepository::new(db.connection()));
    let password_id = service.add_password("中文密码123", "test", false).unwrap();
    let recorder = DbTaskHistoryRecorder::new(db.connection());
    let engine = SmartZipEngine::default();
    let output = TempDir::new().unwrap();

    engine
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

    // The password that unlocked the archive should have a match row.
    let repo = PasswordRepository::new(db.connection());
    let matches = repo.matches_for(password_id).unwrap();
    assert!(
        matches.iter().any(|m| m.success_count >= 1),
        "expected a password_matches success row for the unlocking password"
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
