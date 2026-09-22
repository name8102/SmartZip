//! Archive workers own their runtime and database; only messages cross the UI boundary.
use async_trait::async_trait;
use smartzip_core::{TaskEvent, TaskEventKind};
use smartzip_engine::{
    CompiledRunPolicy, EmbeddedSelectionChoice, EncodingConfirmationChoice,
    EncodingConfirmationContext, InteractiveEmbeddedPrompter, InteractiveEncodingPrompter,
    InteractiveOutputPrompter, InteractivePasswordPrompter, OutputCollisionStrategy,
    SmartZipEngine,
};
use std::{
    collections::{HashMap, VecDeque},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, Weak},
};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TaskOperation {
    Extract,
    Detect,
    List,
    Test,
}
#[derive(Clone, Default)]
pub struct TaskSettings {
    pub force: bool,
    pub output: Option<PathBuf>,
    pub recursive: Option<bool>,
    pub smart_layout: Option<bool>,
    pub auto_encoding: Option<bool>,
    pub delete_source: bool,
    pub passwords: Vec<String>,
    pub temporary_passwords: bool,
}
#[derive(Clone)]
pub struct JobRequest {
    pub operation: TaskOperation,
    pub paths: Vec<PathBuf>,
    pub settings: TaskSettings,
    pub resolved: Option<smartzip_config::ResolvedConfig>,
}
pub enum InteractionRequest {
    Password {
        path: PathBuf,
        respond: oneshot::Sender<Option<String>>,
    },
    Output {
        path: PathBuf,
        output: PathBuf,
        respond: oneshot::Sender<OutputCollisionStrategy>,
    },
    Embedded {
        path: PathBuf,
        decision: smartzip_core::DetectionDecision,
        respond: oneshot::Sender<EmbeddedSelectionChoice>,
    },
    Encoding {
        path: PathBuf,
        context: EncodingConfirmationContext,
        respond: oneshot::Sender<EncodingConfirmationChoice>,
    },
}
impl InteractionRequest {
    pub fn is_closed(&self) -> bool {
        match self {
            Self::Password { respond, .. } => respond.is_closed(),
            Self::Output { respond, .. } => respond.is_closed(),
            Self::Embedded { respond, .. } => respond.is_closed(),
            Self::Encoding { respond, .. } => respond.is_closed(),
        }
    }
}

pub struct JobOutcome {
    pub status: String,
    pub detail: serde_json::Value,
    pub warnings: Vec<String>,
}
pub enum JobMessage {
    Event(TaskEvent),
    Prompt(InteractionRequest),
    Finished(JobOutcome),
    Failed(String),
}
#[derive(Clone, Default)]
struct Mailbox(Arc<Mutex<VecDeque<JobMessage>>>);
impl Mailbox {
    fn push(&self, message: JobMessage) {
        let mut queue = self.0.lock().unwrap_or_else(|e| e.into_inner());
        // Only adjacent progress events are interchangeable; preserve route and phase ordering.
        if let JobMessage::Event(event) = &message {
            if matches!(event.kind, TaskEventKind::Progress(_)) {
                if let Some(JobMessage::Event(last)) = queue.back_mut() {
                    if last.task_id == event.task_id
                        && matches!(last.kind, TaskEventKind::Progress(_))
                    {
                        *last = event.clone();
                        return;
                    }
                }
            }
        }
        // One worker can have only one outstanding prompt and one terminal
        // message. Bound diagnostic backlog without dropping either control message.
        const MAX_PENDING_EVENTS: usize = 2048;
        if queue.len() >= MAX_PENDING_EVENTS {
            if let Some(index) = queue
                .iter()
                .position(|item| matches!(item, JobMessage::Event(_)))
            {
                queue.remove(index);
            }
        }
        queue.push_back(message);
    }
    fn drain(&self) -> Vec<JobMessage> {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .drain(..)
            .collect()
    }
}
pub struct JobHandle {
    pub roots: Arc<smartzip_engine::root_management::RootManagement>,
    mailbox: Mailbox,
    cancellation: CancellationToken,
}
impl JobHandle {
    pub fn drain(&self) -> Vec<JobMessage> {
        self.mailbox.drain()
    }
    pub fn cancel(&self) {
        self.cancellation.cancel();
    }
}
impl Drop for JobHandle {
    fn drop(&mut self) {
        self.cancel();
    }
}

pub fn spawn_job_at(request: JobRequest, queue_position: i64) -> Result<JobHandle, String> {
    if request.paths.is_empty() {
        return Err("请先添加文件".into());
    }
    if matches!(
        request.operation,
        TaskOperation::Detect | TaskOperation::List
    ) && request.paths.len() != 1
    {
        return Err("检测和预览任务每次需要一个文件".into());
    }
    let mailbox = Mailbox::default();
    let cancellation = CancellationToken::new();
    let roots = Arc::new(smartzip_engine::root_management::RootManagement::default());
    let worker_roots = roots.clone();
    let worker_mailbox = mailbox.clone();
    let worker_token = cancellation.clone();
    std::thread::Builder::new()
        .name("smartzip-archive".into())
        .spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|e| e.to_string())?;
                runtime.block_on(run_job(
                    request,
                    queue_position,
                    worker_mailbox.clone(),
                    worker_token.clone(),
                    worker_roots.clone(),
                ))
            }));
            let message = match result {
                Ok(Ok(outcome)) => JobMessage::Finished(outcome),
                Ok(Err(_)) if worker_token.is_cancelled() => JobMessage::Finished(JobOutcome {
                    status: "cancelled".into(),
                    detail: serde_json::Value::Null,
                    warnings: vec![],
                }),
                Ok(Err(error)) => JobMessage::Failed(error),
                Err(_) => JobMessage::Failed("归档工作线程异常终止".into()),
            };
            worker_mailbox.push(message);
        })
        .map_err(|e| e.to_string())?;
    Ok(JobHandle {
        roots,
        mailbox,
        cancellation,
    })
}

#[derive(Clone)]
struct Prompter {
    mailbox: Mailbox,
    cancellation: CancellationToken,
    gate: Arc<tokio::sync::Mutex<()>>,
}
impl Prompter {
    async fn receive<T>(&self, receiver: oneshot::Receiver<T>, fallback: T) -> T {
        tokio::select! { biased; _ = self.cancellation.cancelled() => fallback, result = receiver => result.unwrap_or(fallback) }
    }
}
#[async_trait]
impl InteractivePasswordPrompter for Prompter {
    async fn prompt(&self, path: &Path) -> Option<String> {
        let _guard = self.gate.lock().await;
        let (respond, receiver) = oneshot::channel();
        self.mailbox
            .push(JobMessage::Prompt(InteractionRequest::Password {
                path: path.into(),
                respond,
            }));
        self.receive(receiver, None).await
    }
}
#[async_trait]
impl InteractiveOutputPrompter for Prompter {
    async fn prompt(&self, path: PathBuf, output: PathBuf) -> OutputCollisionStrategy {
        let _guard = self.gate.lock().await;
        let (respond, receiver) = oneshot::channel();
        self.mailbox
            .push(JobMessage::Prompt(InteractionRequest::Output {
                path,
                output,
                respond,
            }));
        self.receive(receiver, OutputCollisionStrategy::Skip).await
    }
}
#[async_trait]
impl InteractiveEmbeddedPrompter for Prompter {
    async fn prompt(
        &self,
        path: &Path,
        decision: &smartzip_core::DetectionDecision,
    ) -> EmbeddedSelectionChoice {
        let _guard = self.gate.lock().await;
        let (respond, receiver) = oneshot::channel();
        self.mailbox
            .push(JobMessage::Prompt(InteractionRequest::Embedded {
                path: path.into(),
                decision: decision.clone(),
                respond,
            }));
        self.receive(receiver, EmbeddedSelectionChoice::Skip).await
    }
}
#[async_trait]
impl InteractiveEncodingPrompter for Prompter {
    async fn prompt(
        &self,
        path: &Path,
        context: &EncodingConfirmationContext,
    ) -> EncodingConfirmationChoice {
        let _guard = self.gate.lock().await;
        let (respond, receiver) = oneshot::channel();
        self.mailbox
            .push(JobMessage::Prompt(InteractionRequest::Encoding {
                path: path.into(),
                context: context.clone(),
                respond,
            }));
        self.receive(receiver, EncodingConfirmationChoice::SkipArchive)
            .await
    }
}

fn load_policy(request: &JobRequest) -> Result<CompiledRunPolicy, Box<dyn std::error::Error>> {
    let mut resolved = if let Some(resolved) = &request.resolved {
        resolved.clone()
    } else {
        let paths = smartzip_platform::PlatformPaths::try_new()?;
        let legacy = smartzip_platform::PlatformPaths::legacy()?;
        let environment = std::env::var_os("SMARTZIP_CONFIG").map(PathBuf::from);
        let (selected, diagnostics) = smartzip_config::selected_config(
            None,
            false,
            environment.as_deref(),
            &paths.config_path(),
            &legacy.config_path(),
        )?;
        let mut resolved = smartzip_config::ResolvedConfig::load(selected.as_deref())?;
        resolved.diagnostics.extend(diagnostics);
        resolved
    };
    let mut patches = Vec::new();
    if let Some(value) = request.settings.recursive {
        patches.push((
            "extraction.recursion.enabled".into(),
            toml::Value::Boolean(value),
        ));
    }
    if let Some(value) = request.settings.smart_layout {
        patches.push((
            "extraction.output.layout".into(),
            toml::Value::String(if value { "smart" } else { "conservative" }.into()),
        ));
    }
    if let Some(value) = request.settings.auto_encoding {
        patches.push((
            "extraction.encoding.mode".into(),
            toml::Value::String(if value { "auto" } else { "backend" }.into()),
        ));
    }
    if let Some(output) = &request.settings.output {
        patches.push((
            "extraction.output.destination".into(),
            toml::Value::String("directory".into()),
        ));
        patches.push((
            "extraction.output.directory".into(),
            toml::Value::String(std::path::absolute(output)?.to_string_lossy().into_owned()),
        ));
    }
    resolved.apply(&patches)?;
    if request.settings.temporary_passwords {
        resolved.apply(&[
            ("passwords.save_success".into(), toml::Value::Boolean(false)),
            (
                "passwords.record_statistics".into(),
                toml::Value::Boolean(false),
            ),
        ])?;
        for key in ["passwords.save_success", "passwords.record_statistics"] {
            resolved
                .origins
                .insert(key.into(), "task:temporary-password".into());
        }
        if let Some(position) = resolved
            .values
            .passwords
            .sources
            .iter()
            .position(|source| *source == smartzip_config::PasswordSource::Manual)
        {
            resolved.values.passwords.sources.remove(position);
        }
        resolved
            .values
            .passwords
            .sources
            .insert(0, smartzip_config::PasswordSource::Manual);
        resolved
            .origins
            .insert("passwords.sources".into(), "task:temporary-password".into());
    }
    if !request.settings.passwords.is_empty()
        && resolved.values.passwords.mode == smartzip_config::PasswordMode::Off
    {
        return Err("密码功能已在配置中关闭".into());
    }
    Ok(CompiledRunPolicy::compile(resolved)?)
}
fn open_database(
    policy: &CompiledRunPolicy,
) -> Result<Option<smartzip_db::SmartZipDb>, Box<dyn std::error::Error>> {
    if !policy.needs_database() {
        return Ok(None);
    }
    let path = match &policy.values().state.database {
        Some(path) => path.clone(),
        None => {
            smartzip_platform::PlatformPaths::try_new()?
                .select_database(&smartzip_platform::PlatformPaths::legacy()?)?
                .0
        }
    };
    let db = if policy.values().state.mode == smartzip_config::StateMode::ReadOnly {
        smartzip_db::SmartZipDb::open_read_only(&path)?
    } else {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        smartzip_db::SmartZipDb::open(&path)?
    };
    Ok(Some(db))
}

fn shared_state_store(
    path: &Path,
) -> Result<Arc<smartzip_engine::execution_runtime::ExecutionCoordinator>, Box<dyn std::error::Error>>
{
    static STORES: OnceLock<
        Mutex<HashMap<PathBuf, Weak<smartzip_engine::execution_runtime::ExecutionCoordinator>>>,
    > = OnceLock::new();
    let stores = STORES.get_or_init(|| Mutex::new(HashMap::new()));
    let mut stores = stores.lock().unwrap();
    if let Some(store) = stores.get(path).and_then(Weak::upgrade) {
        return Ok(store);
    }
    let store = Arc::new(smartzip_engine::state_store::StateStore::start(path)?);
    let execution =
        Arc::new(smartzip_engine::execution_runtime::ExecutionCoordinator::for_host(store)?);
    stores.insert(path.to_path_buf(), Arc::downgrade(&execution));
    Ok(execution)
}
async fn run_job(
    request: JobRequest,
    queue_position: i64,
    mailbox: Mailbox,
    cancellation: CancellationToken,
    roots: Arc<smartzip_engine::root_management::RootManagement>,
) -> Result<JobOutcome, String> {
    run_job_inner(request, queue_position, mailbox, cancellation, roots)
        .await
        .map_err(|e| e.to_string())
}
async fn run_job_inner(
    request: JobRequest,
    queue_position: i64,
    mailbox: Mailbox,
    cancellation: CancellationToken,
    roots: Arc<smartzip_engine::root_management::RootManagement>,
) -> Result<JobOutcome, Box<dyn std::error::Error>> {
    use smartzip_config::StateMode;
    use smartzip_engine::history::TaskHistoryRecorder;
    let policy = load_policy(&request)?;
    let c = policy.values();
    let db = open_database(&policy)?;
    let execution_store = if c.state.mode == StateMode::ReadWrite && c.state.history {
        db.as_ref()
            .and_then(smartzip_db::SmartZipDb::db_path)
            .map(shared_state_store)
            .transpose()?
    } else {
        None
    };
    let services = policy.services(db.as_ref().map(smartzip_db::SmartZipDb::connection));
    let passwords = &services.passwords;
    let stores = services.stores();
    let recorder = services
        .has_stores()
        .then_some(&stores as &dyn TaskHistoryRecorder);
    let backend = smartzip_archive::BackendRouter::from_config(&c.backends)?;
    let mut warnings = policy.resolved().diagnostics.clone();
    warnings.extend(backend.warnings().iter().cloned());
    let engine = SmartZipEngine::default()
        .with_cancellation_token(cancellation.clone())
        .with_root_management(roots.clone())
        .with_source_recycling(request.settings.delete_source)
        .with_run_policy(policy.clone());
    let prompts = Prompter {
        mailbox: mailbox.clone(),
        cancellation: cancellation.clone(),
        gate: Arc::new(tokio::sync::Mutex::new(())),
    };
    let event_mailbox = mailbox.clone();
    let listener: Option<smartzip_engine::TaskEventListener> =
        Some(Arc::new(move |event: &TaskEvent| {
            event_mailbox.push(JobMessage::Event(event.clone()))
        }));
    let scanner = smartzip_scanner::ScannerConfig::default();
    let encoding = if ["auto", "backend"].contains(&c.extraction.encoding.mode.as_str()) {
        smartzip_core::EncodingMode::Auto
    } else {
        smartzip_core::EncodingMode::Override(c.extraction.encoding.mode.clone())
    };
    let candidates = smartzip_passwords::PasswordCandidateRequest {
        manual: request.settings.passwords.clone(),
        clipboard: None,
        include_empty: true,
        limit: c.passwords.database_limit,
    };
    let interactive = c.interaction.mode != smartzip_config::InteractionMode::Never;
    let password_prompt = interactive.then_some(&prompts as &dyn InteractivePasswordPrompter);
    let path = request.paths[0].clone();
    let (status, detail) = match request.operation {
        TaskOperation::Extract => {
            let inputs = request
                .paths
                .iter()
                .map(std::path::absolute)
                .collect::<Result<Vec<_>, _>>()?;
            let output_dir = request
                .settings
                .output
                .clone()
                .map(std::path::absolute)
                .transpose()?
                .unwrap_or_else(|| {
                    inputs[0]
                        .parent()
                        .expect("absolute input path has a parent")
                        .to_path_buf()
                });
            let groups = smartzip_engine::root_inputs::group_root_inputs(
                &inputs,
                c.extraction.volumes.auto_discover,
            );
            let inputs: Vec<_> = groups
                .iter()
                .flat_map(|group| group.inputs.iter().cloned())
                .collect();
            let workflow_request = smartzip_engine::ExtractWorkflowRequest {
                inputs: inputs.clone(),
                output_dir: output_dir.clone(),
                recursion_limit: c.extraction.recursion.max_depth,
                encoding_mode: encoding,
                scanner,
                password_candidates: candidates,
                layout_policy: smartzip_engine::layout::OutputLayoutPolicy::Conservative,
                single_root_name_policy: smartzip_engine::layout::SingleRootNamePolicy::Auto,
                embedded_scan_mode: smartzip_core::EmbeddedScanMode::Auto,
                dominant_min_ratio: c.extraction.embedded.dominant_min_ratio,
                confirm_large_scan: false,
                force: request.settings.force,
                limits: c.limits.clone(),
            };
            let mut prepared =
                smartzip_engine::PreparedExtractTask::new(policy.clone(), workflow_request)?;
            prepared.configure_groups(&roots, groups)?;
            let task_id = prepared.identity().task_id.clone();
            if let Some(execution) = &execution_store {
                execution
                    .submit(prepared.submission(queue_position)?)
                    .await?;
            }
            let recovery_db = db.as_ref();
            let recovery_work = async {
                let Some(execution) = &execution_store else {
                    return;
                };
                let futures = execution
                    .take_runnable_recovery()
                    .into_iter()
                    .map(|recovered| {
                        let execution = execution.as_ref();
                        let listener = listener.clone();
                        let recovery_prompts = prompts.clone();
                        async move {
                            let recovered_task_id =
                                smartzip_core::TaskId::from_stored(recovered.task_id.clone());
                            let recovered_result: Result<(), Box<dyn std::error::Error>> = async {
                                let prepared =
                                    smartzip_engine::PreparedExtractTask::recover(&recovered)?;
                                let recovered_backend =
                                    smartzip_archive::BackendRouter::from_config(
                                        &prepared.policy().values().backends,
                                    )?;
                                prepared
                                    .run(
                                        SmartZipEngine::default().with_cancellation_token(
                                            recovery_prompts.cancellation.clone(),
                                        ),
                                        &recovered_backend,
                                        recovery_db.map(smartzip_db::SmartZipDb::connection),
                                        smartzip_engine::ExtractInteraction {
                                            password: Some(&recovery_prompts),
                                            output: Some(&recovery_prompts),
                                            embedded: Some(&recovery_prompts),
                                            encoding: Some(&recovery_prompts),
                                        },
                                        smartzip_engine::ExtractObserver {
                                            listener: listener.clone(),
                                            history: None,
                                            execution: Some(execution),
                                        },
                                    )
                                    .await?;
                                Ok(())
                            }
                            .await;
                            match recovered_result {
                                Ok(()) => execution.release_task(&recovered_task_id),
                                Err(error) => {
                                    let cancelled = recovery_prompts.cancellation.is_cancelled();
                                    let stop_result = execution
                                        .stop_task(
                                            &recovered_task_id,
                                            if cancelled { "cancelled" } else { "failed" },
                                            if cancelled {
                                                "cancelled"
                                            } else {
                                                "recovery_failed"
                                            },
                                        )
                                        .await;
                                    if let Some(listener) = &listener {
                                        let detail = match stop_result {
                                            Ok(()) => error.to_string(),
                                            Err(stop_error) => format!("{error}; {stop_error}"),
                                        };
                                        listener(&TaskEvent {
                                            task_id: recovered_task_id,
                                            kind: TaskEventKind::Warning {
                                                message: format!("task recovery failed: {detail}"),
                                            },
                                        });
                                    }
                                }
                            }
                        }
                    });
                futures_util::future::join_all(futures).await;
            };
            let current_work = prepared.run(
                engine,
                &backend,
                db.as_ref().map(smartzip_db::SmartZipDb::connection),
                smartzip_engine::ExtractInteraction {
                    password: Some(&prompts),
                    output: Some(&prompts),
                    embedded: Some(&prompts),
                    encoding: Some(&prompts),
                },
                smartzip_engine::ExtractObserver {
                    listener: listener.clone(),
                    history: None,
                    execution: execution_store
                        .as_deref()
                        .map(|execution| execution as &dyn smartzip_engine::ExecutionStateRecorder),
                },
            );
            let (_, result) = tokio::join!(recovery_work, current_work);
            let result = match result {
                Ok(result) => {
                    if let Some(execution) = &execution_store {
                        execution.release_task(&task_id);
                    }
                    result
                }
                Err(error) => {
                    if let Some(execution) = &execution_store {
                        let cancelled = matches!(error, smartzip_core::SmartZipError::Cancelled);
                        execution
                            .stop_task(
                                &task_id,
                                if cancelled { "cancelled" } else { "failed" },
                                if cancelled {
                                    "cancelled"
                                } else {
                                    "workflow_failed"
                                },
                            )
                            .await?;
                    }
                    return Err(error.into());
                }
            };
            warnings.extend(result.events.iter().filter_map(|event| match &event.kind {
                TaskEventKind::Warning { message } => Some(message.clone()),
                _ => None,
            }));
            (
                serde_json::to_value(result.status)?
                    .as_str()
                    .unwrap_or("failed")
                    .to_string(),
                serde_json::to_value(result)?,
            )
        }
        TaskOperation::Detect => {
            let result = engine
                .inspect_file_with_listener(
                    &backend,
                    passwords,
                    smartzip_engine::InspectRequest { path, scanner },
                    listener,
                    recorder,
                )
                .await?;
            let status = if result.status == "unreadable" {
                "failed"
            } else {
                "completed"
            };
            (status.into(), serde_json::to_value(result)?)
        }
        TaskOperation::List => {
            let result = engine
                .list_archive_with_listener_interactive(
                    &backend,
                    passwords,
                    smartzip_engine::ListArchiveRequest {
                        path,
                        scanner,
                        encoding_mode: encoding,
                        password_candidates: candidates,
                    },
                    password_prompt,
                    Some(&prompts),
                    listener,
                    recorder,
                )
                .await?;
            ("completed".into(), serde_json::to_value(result)?)
        }
        TaskOperation::Test => {
            let result = engine
                .test_archives(
                    &backend,
                    passwords,
                    smartzip_engine::TestWorkflowRequest {
                        paths: request.paths,
                        encoding,
                        scanner,
                        password_candidates: candidates,
                        diagnose: smartzip_engine::DiagnoseMode::Auto,
                        diagnostic_timeout: None,
                        control: smartzip_archive::diagnostic::DiagnosticControl::with_cancellation(
                            cancellation.clone(),
                        ),
                    },
                    password_prompt,
                    listener,
                    recorder,
                )
                .await?;
            (
                match result.exit_code {
                    0 => "completed",
                    130 => "cancelled",
                    2 => "partial",
                    _ => "failed",
                }
                .into(),
                serde_json::to_value(result)?,
            )
        }
    };
    Ok(JobOutcome {
        status,
        detail,
        warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn progress_coalescing_preserves_route_and_terminal_order() {
        let mailbox = Mailbox::default();
        let task_id = smartzip_core::TaskId::new();
        for value in 0..1000 {
            mailbox.push(JobMessage::Event(TaskEvent {
                task_id: task_id.clone(),
                kind: TaskEventKind::Progress(smartzip_core::TaskProgress::percent(
                    value as f32 / 10.,
                    "extract",
                )),
            }));
        }
        mailbox.push(JobMessage::Event(TaskEvent {
            task_id,
            kind: TaskEventKind::Finished {
                status: "completed".into(),
            },
        }));
        let messages = mailbox.drain();
        assert_eq!(messages.len(), 2);
        assert!(
            matches!(&messages[0], JobMessage::Event(TaskEvent { kind: TaskEventKind::Progress(progress), .. }) if progress.percent == Some(99.9))
        );
        assert!(matches!(
            &messages[1],
            JobMessage::Event(TaskEvent {
                kind: TaskEventKind::Finished { .. },
                ..
            })
        ));
    }
    #[test]
    fn non_progress_backlog_preserves_prompt_and_terminal_delivery() {
        let mailbox = Mailbox::default();
        let (respond, _reply) = oneshot::channel();
        mailbox.push(JobMessage::Prompt(InteractionRequest::Password {
            path: "input.zip".into(),
            respond,
        }));
        for _ in 0..10_000 {
            mailbox.push(JobMessage::Event(TaskEvent {
                task_id: smartzip_core::TaskId::new(),
                kind: TaskEventKind::PasswordTried { candidate_id: None },
            }));
        }
        mailbox.push(JobMessage::Failed("terminal failure".into()));
        let messages = mailbox.drain();
        assert!(messages.len() <= 2050);
        assert!(matches!(messages.first(), Some(JobMessage::Prompt(_))));
        assert!(matches!(messages.last(), Some(JobMessage::Failed(_))));
    }

    #[tokio::test]
    async fn cancellation_releases_unanswered_password_prompt() {
        let prompts = Prompter {
            mailbox: Mailbox::default(),
            cancellation: CancellationToken::new(),
            gate: Arc::new(tokio::sync::Mutex::new(())),
        };
        let token = prompts.cancellation.clone();
        let pending = InteractivePasswordPrompter::prompt(&prompts, Path::new("secret.zip"));
        tokio::pin!(pending);
        tokio::select! {
            _ = &mut pending => panic!("prompt unexpectedly resolved"),
            _ = tokio::task::yield_now() => {}
        }
        let messages = prompts.mailbox.drain();
        assert!(matches!(
            messages.as_slice(),
            [JobMessage::Prompt(InteractionRequest::Password { .. })]
        ));
        token.cancel();
        assert_eq!(pending.await, None);
        // Keep the sender alive through cancellation, so channel closure cannot satisfy this test.
        drop(messages);
    }

    #[tokio::test]
    async fn cloned_prompters_serialize_prompts_and_share_cancellation() {
        let mailbox = Mailbox::default();
        let prompts = Prompter {
            mailbox: mailbox.clone(),
            cancellation: CancellationToken::new(),
            gate: Arc::new(tokio::sync::Mutex::new(())),
        };
        let other = prompts.clone();
        let first = InteractivePasswordPrompter::prompt(&prompts, Path::new("first.zip"));
        tokio::pin!(first);
        tokio::select! {
            _ = &mut first => panic!("first prompt unexpectedly resolved"),
            _ = tokio::task::yield_now() => {}
        }
        let mut first_messages = mailbox.drain();
        let JobMessage::Prompt(InteractionRequest::Password { respond, .. }) =
            first_messages.remove(0)
        else {
            panic!("missing first password prompt");
        };

        let second = InteractivePasswordPrompter::prompt(&other, Path::new("second.zip"));
        tokio::pin!(second);
        tokio::select! {
            _ = &mut second => panic!("second prompt unexpectedly resolved"),
            _ = tokio::task::yield_now() => {}
        }
        assert!(mailbox.drain().is_empty());

        respond.send(Some("secret".into())).unwrap();
        assert_eq!(first.await.as_deref(), Some("secret"));
        tokio::select! {
            _ = &mut second => panic!("second prompt unexpectedly resolved"),
            _ = tokio::task::yield_now() => {}
        }
        let second_messages = mailbox.drain();
        assert!(matches!(
            second_messages.as_slice(),
            [JobMessage::Prompt(InteractionRequest::Password { .. })]
        ));
        prompts.cancellation.cancel();
        assert_eq!(second.await, None);
        drop(second_messages);
    }
    #[test]
    fn quick_settings_override_frozen_configuration() {
        let request = JobRequest {
            operation: TaskOperation::Extract,
            paths: vec!["archive.zip".into()],
            settings: TaskSettings {
                recursive: Some(false),
                auto_encoding: Some(false),
                smart_layout: Some(true),
                ..Default::default()
            },
            resolved: Some(smartzip_config::ResolvedConfig::load(None).unwrap()),
        };
        let policy = load_policy(&request).unwrap();
        assert!(!policy.values().extraction.recursion.enabled);
        assert_eq!(policy.values().extraction.encoding.mode, "backend");
        assert_eq!(
            policy.values().extraction.output.layout,
            smartzip_config::Layout::Smart
        );
    }
    #[test]
    fn temporary_passwords_disable_persistence_without_mutating_snapshot() {
        let mut resolved = smartzip_config::ResolvedConfig::load(None).unwrap();
        resolved.values.passwords.sources = vec![smartzip_config::PasswordSource::Empty];
        let original = resolved.clone();
        let request = JobRequest {
            operation: TaskOperation::Extract,
            paths: vec!["archive.zip".into()],
            settings: TaskSettings {
                passwords: vec!["temporary".into()],
                temporary_passwords: true,
                ..Default::default()
            },
            resolved: Some(resolved),
        };
        let policy = load_policy(&request).unwrap();
        assert!(!policy.values().passwords.save_success);
        assert!(!policy.values().passwords.record_statistics);
        assert_eq!(
            policy.resolved().origins["passwords.save_success"],
            "task:temporary-password"
        );
        assert_eq!(
            policy.resolved().origins["passwords.record_statistics"],
            "task:temporary-password"
        );
        assert_eq!(
            policy.values().passwords.sources[0],
            smartzip_config::PasswordSource::Manual
        );
        assert_eq!(request.resolved.as_ref().unwrap(), &original);
        let mut disabled = request;
        disabled.resolved.as_mut().unwrap().values.passwords.mode =
            smartzip_config::PasswordMode::Off;
        assert!(load_policy(&disabled).is_err());
    }
    #[tokio::test]
    async fn recovery_prompts_from_saved_policy_when_new_task_disables_interaction() {
        let temp = tempfile::tempdir().unwrap();
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures");
        let encrypted = temp.path().join("recover.zip");
        let current = temp.path().join("current.zip");
        std::fs::copy(fixtures.join("pass_cn.zip"), &encrypted).unwrap();
        std::fs::copy(fixtures.join("enc_utf8.zip"), &current).unwrap();
        let db_path = temp.path().join("state.db");
        let mut resolved = smartzip_config::ResolvedConfig::load(None).unwrap();
        resolved.values.state.database = Some(db_path.clone());
        resolved.values.extraction.recursion.enabled = false;
        resolved.values.extraction.embedded.root = smartzip_config::RootScan::Off;
        resolved.values.extraction.embedded.nested = smartzip_config::NestedScan::Off;
        resolved.values.passwords.sources = vec![
            smartzip_config::PasswordSource::Manual,
            smartzip_config::PasswordSource::Empty,
        ];
        resolved.values.passwords.save_success = false;
        resolved.values.passwords.record_statistics = false;
        let prepared = smartzip_engine::PreparedExtractTask::new(
            CompiledRunPolicy::compile(resolved.clone()).unwrap(),
            smartzip_engine::ExtractWorkflowRequest {
                inputs: vec![encrypted.clone()],
                output_dir: temp.path().join("recovered-output"),
                recursion_limit: 0,
                encoding_mode: smartzip_core::EncodingMode::Auto,
                scanner: Default::default(),
                password_candidates: Default::default(),
                layout_policy: Default::default(),
                single_root_name_policy: Default::default(),
                embedded_scan_mode: smartzip_core::EmbeddedScanMode::Ignore,
                dominant_min_ratio: 0.7,
                confirm_large_scan: false,
                force: false,
                limits: Default::default(),
            },
        )
        .unwrap();
        let recovered_id = prepared.identity().task_id.to_string();
        {
            let store = smartzip_engine::state_store::StateStore::start(&db_path).unwrap();
            store.submit(prepared.submission(0).unwrap()).await.unwrap();
        }
        resolved.values.interaction.mode = smartzip_config::InteractionMode::Never;
        let mailbox = Mailbox::default();
        let worker = run_job_inner(
            JobRequest {
                operation: TaskOperation::Extract,
                paths: vec![current],
                resolved: Some(resolved),
                settings: TaskSettings {
                    output: Some(temp.path().join("current-output")),
                    ..Default::default()
                },
            },
            1,
            mailbox.clone(),
            CancellationToken::new(),
            Default::default(),
        );
        tokio::pin!(worker);
        let mut password_prompts = 0;
        tokio::time::timeout(std::time::Duration::from_secs(30), async {
            loop {
                tokio::select! {
                    result = &mut worker => { result.unwrap(); break; }
                    _ = tokio::time::sleep(std::time::Duration::from_millis(10)) => {
                        for message in mailbox.drain() {
                            if let JobMessage::Prompt(prompt) = message {
                                match prompt {
                                    InteractionRequest::Password { path, respond } => {
                                        assert_eq!(path, encrypted);
                                        password_prompts += 1;
                                        respond.send(Some("中文密码123".into())).unwrap();
                                    }
                                    _ => panic!("unexpected non-password interaction"),
                                }
                            }
                        }
                    }
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(password_prompts, 1);
        let db = smartzip_db::SmartZipDb::open(&db_path).unwrap();
        let status: String = db
            .connection()
            .query_row(
                "SELECT status FROM tasks WHERE id=?1",
                [&recovered_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(status, "completed");
        assert!(temp
            .path()
            .join("recovered-output/recover/文档.txt")
            .exists());
        let mut statement = db.connection().prepare("SELECT status FROM tasks").unwrap();
        let statuses = statement
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(statuses.len(), 2);
        assert!(statuses.iter().all(|status| status == "completed"));
    }
}
