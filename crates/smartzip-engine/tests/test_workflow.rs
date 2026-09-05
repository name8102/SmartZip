use async_trait::async_trait;
use smartzip_archive::diagnostic::DiagnosticControl;
use smartzip_archive::integrity::{
    BackendTestDiagnostics, Coverage, Integrity, PasswordStatus, TestFailure,
};
use smartzip_archive::*;
use smartzip_core::{AdapterCapabilities, EncodingMode, Result, TaskEventKind};
use smartzip_db::{password::PasswordRepository, SmartZipDb};
use smartzip_engine::{DiagnoseMode, SmartZipEngine, TestWorkflowRequest};
use smartzip_passwords::{PasswordCandidateRequest, PasswordService};
use smartzip_scanner::ScannerConfig;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Clone, Copy)]
enum Behavior {
    Good,
    Bad,
    Password,
    Indeterminate,
    Change,
    Slow,
}
struct Adapter {
    id: &'static str,
    family: &'static str,
    behavior: Behavior,
    calls: Arc<Mutex<Vec<PathBuf>>>,
}
#[async_trait]
impl ArchiveAdapter for Adapter {
    fn id(&self) -> &str {
        self.id
    }
    fn diagnostic_family(&self) -> Option<&'static str> {
        Some(self.family)
    }
    fn capabilities(&self) -> AdapterCapabilities {
        AdapterCapabilities {
            operations: vec![smartzip_core::ArchiveOperation::Test],
            read_containers: vec![
                smartzip_core::ArchiveFormat::Zip,
                smartzip_core::ArchiveFormat::Rar,
                smartzip_core::ArchiveFormat::SevenZip,
            ],
            compress_containers: vec![],
            supports_passwords: true,
            supports_charset_override: true,
        }
    }
    async fn probe(&self, _: &Path) -> Result<ArchiveProbe> {
        unreachable!()
    }
    async fn list(&self, _: ListRequest) -> Result<ArchiveListing> {
        unreachable!()
    }
    async fn extract(&self, _: ExtractArchiveRequest) -> Result<ExtractArchiveResult> {
        unreachable!()
    }
    async fn compress(&self, _: CompressArchiveRequest) -> Result<CompressArchiveResult> {
        unreachable!()
    }
    async fn test(&self, request: TestRequest) -> Result<TestResult> {
        self.calls.lock().unwrap().push(request.archive.clone());
        if matches!(self.behavior, Behavior::Slow) {
            tokio::time::sleep(Duration::from_secs(30)).await;
        }
        if matches!(self.behavior, Behavior::Change) {
            std::fs::write(&request.archive, b"changed length").unwrap();
        }
        let failure = match self.behavior {
            Behavior::Indeterminate => Some(TestFailure::PasswordIndeterminate),
            Behavior::Bad => Some(TestFailure::Corruption),
            Behavior::Password => match request.password.as_deref() {
                None => Some(TestFailure::PasswordRequired),
                Some("correct") => None,
                _ => Some(TestFailure::PasswordRejected),
            },
            _ => None,
        };
        Ok(TestResult {
            ok: failure.is_none(),
            encrypted: Some(matches!(
                self.behavior,
                Behavior::Password | Behavior::Change
            )),
            diagnostics: BackendTestDiagnostics {
                adapter_id: self.id.into(),
                family: self.family.into(),
                failure,
                coverage: if failure.is_none() {
                    Coverage::Complete
                } else {
                    Coverage::Partial
                },
                ..BackendTestDiagnostics::default()
            },
        })
    }
}
fn adapter(
    id: &'static str,
    family: &'static str,
    behavior: Behavior,
    priority: i32,
) -> (AdapterRegistration, Arc<Mutex<Vec<PathBuf>>>) {
    let calls = Arc::new(Mutex::new(Vec::new()));
    (
        AdapterRegistration::from_adapter(
            Adapter {
                id,
                family,
                behavior,
                calls: calls.clone(),
            },
            priority,
        ),
        calls,
    )
}
fn request(paths: Vec<PathBuf>) -> TestWorkflowRequest {
    TestWorkflowRequest {
        paths,
        encoding: EncodingMode::Auto,
        scanner: ScannerConfig::default(),
        password_candidates: PasswordCandidateRequest::default(),
        diagnose: DiagnoseMode::Auto,
        diagnostic_timeout: None,
        control: DiagnosticControl::default(),
    }
}
fn engine() -> SmartZipEngine {
    SmartZipEngine::with_scanner_config(ScannerConfig::default())
}

#[tokio::test]
async fn arbitrary_member_deduplicates_one_group_and_history_stores_the_full_report() {
    let dir = canonical_tempdir();
    let first = dir.path().join("a.part01.rar");
    let second = dir.path().join("a.part02.rar");
    for p in [&first, &second] {
        std::fs::write(p, b"Rar!\x1a\x07\x01\x00").unwrap();
    }
    let (registration, calls) = adapter("reader", "unrar", Behavior::Good, 10);
    let backend = BackendRouter::from_adapters(vec![registration]);
    let db = SmartZipDb::in_memory().unwrap();
    let service = PasswordService::new(PasswordRepository::new(db.connection()));
    let history = smartzip_engine::history::DbTaskHistoryRecorder::new(db.connection());
    let result = engine()
        .test_archives(
            &backend,
            &service,
            request(vec![second.clone(), first.clone()]),
            None,
            None,
            Some(&history),
        )
        .await
        .unwrap();
    assert_eq!(result.exit_code, 0);
    assert_eq!(result.files.len(), 1);
    assert_eq!(result.files[0].input_paths, [second, first.clone()]);
    assert_eq!(*calls.lock().unwrap(), [first]);
    let rows = smartzip_db::file_extractions::FileExtractionRepository::new(db.connection())
        .list_by_task(result.task_id.as_str())
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert!(rows[0].sample_hash.is_none());
    let stored: smartzip_archive::integrity::TestArchiveReport =
        serde_json::from_str(rows[0].test_report_json.as_ref().unwrap()).unwrap();
    assert_eq!(stored, result.files[0]);
    assert_eq!(
        db.connection()
            .query_row("SELECT COUNT(*) FROM known_files", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn diagnostic_pass_is_separate_and_cannot_erase_primary_corruption() {
    let dir = canonical_tempdir();
    let path = dir.path().join("a.rar");
    std::fs::write(&path, b"unknown archive").unwrap();
    let (primary, a) = adapter("primary", "unrar", Behavior::Bad, 20);
    let (secondary, b) = adapter("secondary", "7z", Behavior::Good, 10);
    let backend = BackendRouter::from_adapters(vec![primary, secondary]);
    let db = SmartZipDb::in_memory().unwrap();
    let service = PasswordService::new(PasswordRepository::new(db.connection()));
    let result = engine()
        .test_archives(&backend, &service, request(vec![path]), None, None, None)
        .await
        .unwrap();
    assert_eq!(result.files[0].integrity, Integrity::Corrupt);
    assert_eq!(result.exit_code, 1);
    assert_eq!(a.lock().unwrap().len(), 1);
    assert_eq!(b.lock().unwrap().len(), 1);
    assert_eq!(
        result.files[0]
            .passes
            .iter()
            .map(|p| p.purpose.as_str())
            .collect::<Vec<_>>(),
        ["integrity", "diagnostic"]
    );
    assert!(result.events.iter().any(
        |e| matches!(&e.kind,TaskEventKind::TestPhase{phase,..}if phase=="diagnostic_backend")
    ));
}

#[tokio::test]
async fn off_forced_backend_and_expired_budget_prevent_additional_full_passes() {
    let dir = canonical_tempdir();
    let path = dir.path().join("a.rar");
    std::fs::write(&path, b"unknown archive").unwrap();
    for mode in 0..3 {
        let (primary, a) = adapter("primary", "unrar", Behavior::Bad, 20);
        let (secondary, b) = adapter("secondary", "7z", Behavior::Good, 10);
        let mut backend = BackendRouter::from_adapters(vec![primary, secondary]);
        let mut req = request(vec![path.clone()]);
        match mode {
            0 => req.diagnose = DiagnoseMode::Off,
            1 => backend = backend.with_forced_adapter("primary"),
            _ => req.diagnostic_timeout = Some(Duration::ZERO),
        }
        let db = SmartZipDb::in_memory().unwrap();
        let service = PasswordService::new(PasswordRepository::new(db.connection()));
        let result = engine()
            .test_archives(&backend, &service, req, None, None, None)
            .await
            .unwrap();
        assert_eq!(result.exit_code, 1);
        assert_eq!(a.lock().unwrap().len(), 1);
        assert_eq!(b.lock().unwrap().len(), 0);
    }
}

#[tokio::test]
async fn password_is_saved_only_after_verified_success_and_no_failed_guess_is_penalized() {
    let dir = canonical_tempdir();
    let path = dir.path().join("a.7z");
    std::fs::write(&path, b"unknown archive").unwrap();
    let (registration, _) = adapter("reader", "7z", Behavior::Password, 10);
    let backend = BackendRouter::from_adapters(vec![registration]);
    let db = SmartZipDb::in_memory().unwrap();
    let service = PasswordService::new(PasswordRepository::new(db.connection()));
    service.add_password("wrong", "manual", false).unwrap();
    let mut req = request(vec![path]);
    req.password_candidates.manual = vec!["wrong".into(), "correct".into()];
    let result = engine()
        .test_archives(&backend, &service, req, None, None, None)
        .await
        .unwrap();
    assert_eq!(result.exit_code, 0);
    assert_eq!(result.files[0].password_status, PasswordStatus::Verified);
    assert_eq!(result.files[0].passes.len(), 3);
    let records = PasswordRepository::new(db.connection())
        .ranked_candidates(10)
        .unwrap();
    assert_eq!(
        records
            .iter()
            .find(|r| r.value == "correct")
            .unwrap()
            .success_count,
        1
    );
    assert_eq!(
        records
            .iter()
            .find(|r| r.value == "wrong")
            .unwrap()
            .failure_count,
        0
    );
    let json = serde_json::to_string(&result).unwrap();
    assert!(!json.contains("correct"));
    assert!(!json.contains("wrong"));
}

#[tokio::test]
async fn changed_input_invalidates_success_and_password_recording() {
    let dir = canonical_tempdir();
    let path = dir.path().join("a.7z");
    std::fs::write(&path, b"old bytes").unwrap();
    let (registration, _) = adapter("reader", "7z", Behavior::Change, 10);
    let backend = BackendRouter::from_adapters(vec![registration]);
    let db = SmartZipDb::in_memory().unwrap();
    let service = PasswordService::new(PasswordRepository::new(db.connection()));
    let mut req = request(vec![path]);
    req.password_candidates.include_empty = false;
    req.password_candidates.manual = vec!["correct".into()];
    let result = engine()
        .test_archives(&backend, &service, req, None, None, None)
        .await
        .unwrap();
    assert_eq!(result.exit_code, 1);
    assert_eq!(result.files[0].integrity, Integrity::Unknown);
    assert!(result.files[0]
        .stop_reasons
        .contains(&"input_changed".into()));
    assert!(PasswordRepository::new(db.connection())
        .ranked_candidates(10)
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn cancellation_returns_partial_reports_and_records_unstarted_groups() {
    let dir = canonical_tempdir();
    let first = dir.path().join("a.rar");
    let second = dir.path().join("b.rar");
    for path in [&first, &second] {
        std::fs::write(path, b"unknown archive").unwrap();
    }
    let (registration, calls) = adapter("reader", "unrar", Behavior::Slow, 10);
    let backend = BackendRouter::from_adapters(vec![registration]);
    let db = SmartZipDb::in_memory().unwrap();
    let service = PasswordService::new(PasswordRepository::new(db.connection()));
    let history = smartzip_engine::history::DbTaskHistoryRecorder::new(db.connection());
    let req = request(vec![first, second]);
    let control = req.control.clone();
    let cancel = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        control.cancel();
    });
    let result = engine()
        .test_archives(&backend, &service, req, None, None, Some(&history))
        .await
        .unwrap();
    cancel.await.unwrap();
    assert_eq!(result.exit_code, 130);
    assert_eq!(result.files.len(), 2);
    assert_eq!(calls.lock().unwrap().len(), 1);
    let rows = smartzip_db::file_extractions::FileExtractionRepository::new(db.connection())
        .list_by_task(result.task_id.as_str())
        .unwrap();
    assert_eq!(rows.len(), 2);
}

#[tokio::test]
async fn history_write_failure_does_not_change_integrity_or_exit_code() {
    let dir = canonical_tempdir();
    let path = dir.path().join("a.rar");
    std::fs::write(&path, b"Rar!\x1a\x07\x01\x00").unwrap();
    let (registration, _) = adapter("reader", "unrar", Behavior::Good, 10);
    let backend = BackendRouter::from_adapters(vec![registration]);
    let db = SmartZipDb::in_memory().unwrap();
    db.connection()
        .execute_batch("PRAGMA query_only = ON")
        .unwrap();
    let service = PasswordService::new(PasswordRepository::new(db.connection()));
    let history = smartzip_engine::history::DbTaskHistoryRecorder::new(db.connection());
    let result = engine()
        .test_archives(
            &backend,
            &service,
            request(vec![path]),
            None,
            None,
            Some(&history),
        )
        .await
        .unwrap();
    assert_eq!(result.exit_code, 0);
    assert_eq!(result.files[0].integrity, Integrity::Intact);
    assert_eq!(
        db.connection()
            .query_row("SELECT COUNT(*) FROM tasks", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn indeterminate_failure_does_not_search_more_passwords() {
    let dir = canonical_tempdir();
    let path = dir.path().join("a.7z");
    std::fs::write(&path, b"unknown archive").unwrap();
    let (registration, calls) = adapter("reader", "7z", Behavior::Indeterminate, 10);
    let backend = BackendRouter::from_adapters(vec![registration]);
    let db = SmartZipDb::in_memory().unwrap();
    let service = PasswordService::new(PasswordRepository::new(db.connection()));
    let mut req = request(vec![path]);
    req.diagnose = DiagnoseMode::Off;
    req.password_candidates.manual = vec!["wrong".into(), "correct".into()];
    let result = engine()
        .test_archives(&backend, &service, req, None, None, None)
        .await
        .unwrap();
    assert_eq!(result.exit_code, 1);
    assert_eq!(calls.lock().unwrap().len(), 1);
}

// VolumeSet deliberately canonicalizes parent directories. macOS TMPDIR
// commonly passes through /var -> /private/var; expected fixture paths must
// use the same physical parent, including names of deliberately missing files.
fn canonical_tempdir() -> tempfile::TempDir {
    tempfile::Builder::new()
        .tempdir_in(std::env::temp_dir().canonicalize().unwrap())
        .unwrap()
}
