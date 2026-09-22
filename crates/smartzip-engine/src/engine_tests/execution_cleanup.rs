use super::*;
use crate::history::{FileExtractionRow, TaskHistoryRecorder, TaskOutcome};
use crate::{CompiledRunPolicy, ExtractInteraction, ExtractObserver, PreparedExtractTask};
use std::cell::{Cell, RefCell};

fn policy() -> CompiledRunPolicy {
    let mut resolved = smartzip_config::ResolvedConfig::load(None).unwrap();
    resolved.values.state.mode = smartzip_config::StateMode::Off;
    resolved.values.extraction.recursion.enabled = false;
    resolved.values.extraction.embedded.root = smartzip_config::RootScan::Off;
    resolved.values.extraction.embedded.nested = smartzip_config::NestedScan::Off;
    resolved.values.extraction.volumes.auto_discover = false;
    CompiledRunPolicy::compile(resolved).unwrap()
}

fn request(inputs: Vec<PathBuf>, output_dir: PathBuf) -> ExtractWorkflowRequest {
    ExtractWorkflowRequest {
        inputs,
        output_dir,
        recursion_limit: 0,
        encoding_mode: EncodingMode::Auto,
        scanner: ScannerConfig::default(),
        password_candidates: PasswordCandidateRequest::default(),
        layout_policy: Default::default(),
        single_root_name_policy: Default::default(),
        embedded_scan_mode: EmbeddedScanMode::Ignore,
        dominant_min_ratio: 0.7,
        confirm_large_scan: false,
        force: false,
        limits: Default::default(),
    }
}

#[derive(Default)]
struct History {
    starts: Cell<usize>,
    finishes: RefCell<Vec<crate::history::TaskCompletionStatus>>,
    events: RefCell<Vec<TaskEvent>>,
    files: Cell<usize>,
}
impl TaskHistoryRecorder for History {
    fn start_task(&self, _: &TaskId, _: &str, _: Option<&Path>) {
        self.starts.set(self.starts.get() + 1);
    }
    fn record_event(&self, _: &TaskId, event: &TaskEvent) {
        self.events.borrow_mut().push(event.clone());
    }
    fn record_file_extraction(&self, _: &TaskId, _: FileExtractionRow<'_>) {
        self.files.set(self.files.get() + 1);
    }
    fn finish(&self, _: &TaskId, outcome: TaskOutcome<'_>) {
        self.finishes.borrow_mut().push(outcome.status);
    }
}

#[derive(Default)]
struct InterleavingBackend {
    inner: FakeBackend,
    first_started: tokio::sync::Notify,
    second_started: tokio::sync::Notify,
    starts: AtomicUsize,
    flood: usize,
}
impl InterleavingBackend {
    fn key() -> NegativeCapabilityKey {
        NegativeCapabilityKey {
            adapter_id: "synthetic".into(),
            operation: ArchiveOperation::Extract,
            container: Some(ArchiveFormat::Zip),
            codec: None,
        }
    }
}
#[async_trait]
impl ArchiveExecutor for InterleavingBackend {
    fn begin_task_with_cancellation(
        &self,
        task_id: TaskId,
        events: Arc<dyn TaskEventSink>,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> Arc<TaskExecutionContext> {
        self.starts.fetch_add(1, Ordering::SeqCst);
        Arc::new(TaskExecutionContext::new(task_id, events).with_cancellation(cancellation))
    }
    async fn probe(&self, path: &Path) -> smartzip_core::Result<ArchiveProbe> {
        self.inner.probe(path).await
    }
    async fn extract_with_facts_and_context(
        &self,
        request: ExtractArchiveRequest,
        _: &ArchiveFacts,
        context: Arc<TaskExecutionContext>,
    ) -> smartzip_core::Result<ExtractArchiveResult> {
        let path = &request.archive;
        if path.file_name().unwrap() == "first.zip" {
            context.record_rejection(Self::key(), "shared within this task");
            context.emit_progress(TaskProgress::indeterminate("first-before"));
            self.first_started.notify_one();
            self.second_started.notified().await;
            context.emit_progress(TaskProgress::indeterminate("first-after"));
        } else {
            self.first_started.notified().await;
            assert_eq!(
                context.rejection(&Self::key()).as_deref(),
                Some("shared within this task")
            );
            context.emit_progress(TaskProgress::indeterminate("second"));
            self.second_started.notify_one();
        }
        for i in 0..self.flood {
            context.emit_progress(TaskProgress::indeterminate(format!("diagnostic-{i}")));
        }
        self.inner.extract(request).await
    }
    async fn list(&self, request: ListRequest) -> smartzip_core::Result<ArchiveListing> {
        self.inner.list(request).await
    }
    async fn test(&self, request: TestRequest) -> smartzip_core::Result<TestResult> {
        self.inner.test(request).await
    }
    async fn extract(
        &self,
        request: ExtractArchiveRequest,
    ) -> smartzip_core::Result<ExtractArchiveResult> {
        self.inner.extract(request).await
    }
    async fn compress(
        &self,
        request: CompressArchiveRequest,
    ) -> smartzip_core::Result<CompressArchiveResult> {
        self.inner.compress(request).await
    }
}

#[rstest]
#[case(0)]
#[case(5000)]
#[tokio::test]
async fn concurrent_roots_share_lifecycle_event_order_retention_and_route_cache(
    #[case] flood: usize,
) {
    let temp = tempfile::tempdir().unwrap();
    let inputs: Vec<_> = ["first.zip", "second.zip"]
        .map(|n| temp.path().join(n))
        .into();
    for (i, path) in inputs.iter().enumerate() {
        std::fs::write(path, format!("archive-{i}")).unwrap();
    }
    let backend = InterleavingBackend {
        flood,
        ..Default::default()
    };
    let history = History::default();
    let live = Arc::new(Mutex::new(Vec::new()));
    let captured = live.clone();
    let prepared =
        PreparedExtractTask::new(policy(), request(inputs.clone(), temp.path().join("out")))
            .unwrap();
    let result = tokio::time::timeout(
        Duration::from_secs(10),
        prepared.run(
            SmartZipEngine::default().with_root_management(Default::default()),
            &backend,
            None,
            ExtractInteraction::default(),
            ExtractObserver {
                history: Some(&history),
                execution: None,
                listener: Some(Arc::new(move |e| captured.lock().unwrap().push(e.clone()))),
            },
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        result
            .processed
            .iter()
            .map(|c| c.path.clone())
            .collect::<Vec<_>>(),
        inputs
    );
    assert_eq!(backend.starts.load(Ordering::SeqCst), 1);
    assert_eq!(history.starts.get(), 1);
    assert_eq!(
        history.finishes.borrow().as_slice(),
        &[crate::history::TaskCompletionStatus::Completed]
    );
    assert_eq!(history.files.get(), 2);
    assert_eq!(*history.events.borrow(), result.events);
    let live = live.lock().unwrap();
    assert_eq!(
        live.iter()
            .filter(|e| matches!(e.kind, TaskEventKind::Started))
            .count(),
        1
    );
    assert_eq!(
        live.iter()
            .filter(|e| matches!(e.kind, TaskEventKind::Finished { .. }))
            .count(),
        1
    );
    assert_eq!(live.iter().filter(|e| matches!(&e.kind, TaskEventKind::Decision { stage, .. } if stage == "task_policy")).count(), 1);
    assert!(matches!(
        result.events.first().unwrap().kind,
        TaskEventKind::Started
    ));
    assert!(matches!(
        result.events.last().unwrap().kind,
        TaskEventKind::Finished { .. }
    ));
    if flood == 0 {
        assert_eq!(result.events, *live);
    } else {
        assert_eq!(result.events.len(), 4096);
        assert!(live.len() > 10_000);
        assert_eq!(result.events.iter().filter(|e| matches!(&e.kind, TaskEventKind::Warning { message } if message.contains("earlier diagnostic events omitted"))).count(), 1);
    }
}

#[tokio::test]
async fn invalid_root_identity_finishes_task_history_once() {
    let temp = tempfile::tempdir().unwrap();
    let history = History::default();
    let backend = FakeBackend::default();
    let services = policy().services(None);
    let result = SmartZipEngine::default()
        .with_root_management(Default::default())
        .extract_task(
            crate::ExtractTaskIdentity::new(&[]),
            &backend,
            &services.passwords,
            request(
                vec![temp.path().join("invalid.zip")],
                temp.path().join("out"),
            ),
            ExtractInteraction::default(),
            ExtractObserver {
                history: Some(&history),
                ..Default::default()
            },
        )
        .await;
    assert!(result.is_err());
    assert_eq!(history.starts.get(), 1);
    assert_eq!(
        history.finishes.borrow().as_slice(),
        &[crate::history::TaskCompletionStatus::Failed]
    );
    assert_eq!(
        history
            .events
            .borrow()
            .iter()
            .filter(|e| matches!(e.kind, TaskEventKind::Finished { .. }))
            .count(),
        1
    );
}

#[tokio::test]
async fn prepared_submission_and_recovery_preserve_identity_budget_and_redact_passwords() {
    let temp = tempfile::tempdir().unwrap();
    let mut raw = request(vec!["relative.zip".into()], temp.path().join("out"));
    raw.password_candidates
        .manual
        .push("synthetic-manual-secret".into());
    raw.password_candidates.clipboard = Some("synthetic-clipboard-secret".into());
    let prepared = PreparedExtractTask::new(policy(), raw).unwrap();
    let submission = prepared.submission(3).unwrap();
    assert!(submission.roots[0].input_path.is_absolute());
    assert!(!submission.config_snapshot_json.contains("synthetic-"));
    let saved: crate::state_store::PersistedExtractPlan =
        serde_json::from_str(&submission.config_snapshot_json).unwrap();
    assert!(saved.request.password_candidates.manual.is_empty());
    assert!(saved.request.password_candidates.clipboard.is_none());
    let db_path = temp.path().join("execution.db");
    {
        let store = crate::state_store::StateStore::start(&db_path).unwrap();
        store.submit(submission).await.unwrap();
    }
    let store = crate::state_store::StateStore::start(&db_path).unwrap();
    let mut record = store.recovery_snapshot()[0].clone();
    record.committed_output_files = 5;
    record.committed_output_bytes = 99;
    record.nested_candidate_count = 2;
    // Recovery can dispatch nested nodes with the original root identity.
    let mut candidate: ExtractionCandidate =
        serde_json::from_str(&record.nodes[0].input_ref_json).unwrap();
    candidate.depth = 2;
    candidate.source = CandidateSource::EmbeddedFinding;
    candidate.embedded_offset = Some(17);
    candidate.embedded_size = Some(128);
    record.nodes[0].input_ref_json = serde_json::to_string(&candidate).unwrap();
    let recovered = PreparedExtractTask::recover(&record).unwrap();
    assert_eq!(recovered.identity().task_id, prepared.identity().task_id);
    assert_eq!(
        recovered.identity().roots[0].node_id,
        prepared.identity().roots[0].node_id
    );
    assert_eq!(
        recovered.identity().roots[0].root_id,
        prepared.identity().roots[0].root_id
    );
    assert_eq!(recovered.identity().roots[0].generation, 1);
    assert_eq!(recovered.identity().roots[0].candidate, candidate);
    assert_eq!(
        recovered.identity().budget,
        crate::TaskBudgetSnapshot {
            output_files: 5,
            output_bytes: 99,
            nested_candidates: 2
        }
    );
    assert_eq!(recovered.policy().values(), prepared.policy().values());
    record.nodes[0].execution_state = "committing".into();
    assert!(PreparedExtractTask::recover(&record).is_err());
    record.nodes[0].execution_state = "ready".into();
    record.nodes[0].generation = -1;
    assert!(PreparedExtractTask::recover(&record).is_err());
}

#[test]
fn shared_services_keep_history_reuse_when_writes_are_disabled() {
    use smartzip_config::StateMode;
    let db = SmartZipDb::in_memory().unwrap();
    let seed = crate::history::DbTaskHistoryRecorder::new(db.connection());
    let id = TaskId::new();
    seed.start_extract(&id, None);
    let mut row = FileExtractionRow::skipped(Path::new("seed.zip"), None, "seed");
    row.status = "extracted";
    row.sample_hash = Some("sample");
    row.file_size = Some(10);
    seed.record_file_extraction(&id, row);
    for mode in [StateMode::Off, StateMode::ReadOnly, StateMode::ReadWrite] {
        for history_enabled in [false, true] {
            let mut resolved = policy().resolved().clone();
            resolved.values.state.mode = mode;
            resolved.values.state.history = history_enabled;
            let policy = CompiledRunPolicy::compile(resolved).unwrap();
            let services = policy.services(Some(db.connection()));
            let stores = services.stores();
            assert_eq!(stores.was_extracted("sample", 10), mode != StateMode::Off);
            let attempt = TaskId::new();
            stores.start_extract(&attempt, None);
            let count: i64 = db
                .connection()
                .query_row(
                    "SELECT count(*) FROM tasks WHERE id=?1",
                    [attempt.to_string()],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(
                count,
                i64::from(mode == StateMode::ReadWrite && history_enabled)
            );
            assert_eq!(
                services.known.as_ref().is_some_and(|known| known.writable),
                mode == StateMode::ReadWrite
            );
        }
    }
}

#[tokio::test]
async fn legacy_and_prepared_entrypoints_resolve_relative_inputs_consistently() {
    let temp = tempfile::tempdir().unwrap();
    let raw = request(
        vec!["missing-relative-input.zip".into()],
        temp.path().join("out"),
    );
    let backend = FakeBackend::default();
    let services = policy().services(None);
    let legacy = SmartZipEngine::default()
        .with_run_policy(policy())
        .extract(
            &backend,
            &services.passwords,
            raw.clone(),
            ExtractInteraction::default(),
            ExtractObserver::default(),
        )
        .await
        .unwrap();
    let prepared = PreparedExtractTask::new(policy(), raw)
        .unwrap()
        .run(
            SmartZipEngine::default(),
            &backend,
            None,
            ExtractInteraction::default(),
            ExtractObserver::default(),
        )
        .await
        .unwrap();
    assert_eq!(legacy.status, prepared.status);
    assert_eq!(legacy.skipped, prepared.skipped);
    assert_eq!(legacy.skipped.len(), 1);
    assert!(legacy.skipped[0].path.is_absolute());
}
