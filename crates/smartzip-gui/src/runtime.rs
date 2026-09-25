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
    collections::VecDeque,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
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

pub fn spawn_job(request: JobRequest) -> Result<JobHandle, String> {
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
                    worker_mailbox.clone(),
                    worker_token.clone(),
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
        mailbox,
        cancellation,
    })
}

struct Prompter {
    mailbox: Mailbox,
    cancellation: CancellationToken,
}
impl Prompter {
    async fn receive<T>(&self, receiver: oneshot::Receiver<T>, fallback: T) -> T {
        tokio::select! { biased; _ = self.cancellation.cancelled() => fallback, result = receiver => result.unwrap_or(fallback) }
    }
}
#[async_trait]
impl InteractivePasswordPrompter for Prompter {
    async fn prompt(&self, path: &Path) -> Option<String> {
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
async fn run_job(
    request: JobRequest,
    mailbox: Mailbox,
    cancellation: CancellationToken,
) -> Result<JobOutcome, String> {
    run_job_inner(request, mailbox, cancellation)
        .await
        .map_err(|e| e.to_string())
}
async fn run_job_inner(
    request: JobRequest,
    mailbox: Mailbox,
    cancellation: CancellationToken,
) -> Result<JobOutcome, Box<dyn std::error::Error>> {
    let policy = load_policy(&request)?;
    let c = policy.values();
    let db = open_database(&policy)?;
    let services = policy.services(db.as_ref().map(|db| db.connection()));
    let stores = services.stores();
    let recorder = stores.recorder();
    let passwords = &services.passwords;
    let backend = smartzip_archive::BackendRouter::from_config(&c.backends)?;
    let mut warnings = policy.resolved().diagnostics.clone();
    warnings.extend(backend.warnings().iter().cloned());
    let engine = SmartZipEngine::default()
        .with_cancellation_token(cancellation.clone())
        .with_run_policy(policy.clone());
    let prompts = Prompter {
        mailbox: mailbox.clone(),
        cancellation: cancellation.clone(),
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
            // Capture only explicit roots, before any backend reads. Never infer roots from nested candidates.
            let sources = if request.settings.delete_source {
                request
                    .paths
                    .iter()
                    .map(|p| SourceSnapshot::capture(p))
                    .collect::<Result<Vec<_>, _>>()?
            } else {
                vec![]
            };
            let mut extraction = policy.extract_request(
                request.paths.clone(),
                request
                    .settings
                    .output
                    .clone()
                    .unwrap_or_else(|| path.parent().unwrap_or(Path::new(".")).into()),
            );
            extraction.scanner = scanner;
            extraction.password_candidates = candidates;
            let result = engine
                .extract(
                    &backend,
                    passwords,
                    extraction,
                    smartzip_engine::ExtractPrompts {
                        password: password_prompt,
                        output: Some(&prompts),
                        embedded: Some(&prompts),
                        encoding: Some(&prompts),
                    },
                    smartzip_engine::ExtractObserver {
                        listener,
                        history: recorder,
                    },
                )
                .await?;
            if request.settings.delete_source {
                recycle_roots(&sources, &result, &cancellation, &mut warnings);
            }
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

/// A root must retain its identity, size and timestamps before it can be recycled.
struct SourceSnapshot {
    path: PathBuf,
    metadata: std::fs::Metadata,
}
impl SourceSnapshot {
    fn capture(path: &Path) -> std::io::Result<Self> {
        let metadata = std::fs::symlink_metadata(path)?;
        if !metadata.is_file() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "解压后删除仅支持普通文件，不能用于目录或符号链接",
            ));
        }
        Ok(Self {
            path: path.into(),
            metadata,
        })
    }
    fn unchanged(&self) -> bool {
        std::fs::symlink_metadata(&self.path).is_ok_and(|now| {
            let basic = now.is_file()
                && now.len() == self.metadata.len()
                && now.modified().ok() == self.metadata.modified().ok();
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                basic
                    && now.dev() == self.metadata.dev()
                    && now.ino() == self.metadata.ino()
                    && now.ctime() == self.metadata.ctime()
                    && now.ctime_nsec() == self.metadata.ctime_nsec()
            }
            #[cfg(not(unix))]
            {
                basic && now.created().ok() == self.metadata.created().ok()
            }
        })
    }
}
// Empty archives and vanished outputs cannot justify removing the last source copy.
fn contains_output_file(path: &Path) -> bool {
    let mut pending = vec![path.to_path_buf()];
    let mut visited = 0;
    while let Some(path) = pending.pop() {
        visited += 1;
        if visited > 100_000 {
            return false;
        }
        let Ok(metadata) = std::fs::symlink_metadata(&path) else {
            return false;
        };
        if metadata.is_file() {
            return true;
        }
        if metadata.is_dir() {
            let Ok(entries) = std::fs::read_dir(path) else {
                return false;
            };
            for entry in entries {
                let Ok(entry) = entry else {
                    return false;
                };
                pending.push(entry.path());
            }
        }
    }
    false
}
fn recycle_roots(
    sources: &[SourceSnapshot],
    result: &smartzip_engine::ExtractWorkflowResult,
    cancellation: &CancellationToken,
    warnings: &mut Vec<String>,
) {
    let safe = result.status == smartzip_engine::history::TaskCompletionStatus::Completed
        && result.failed_count == 0
        && result.skipped.is_empty()
        && !result.processed.is_empty()
        && !cancellation.is_cancelled()
        && result
            .events
            .iter()
            .any(|event| matches!(event.kind, TaskEventKind::OutputCreated { .. }))
        && result
            .events
            .iter()
            .filter_map(|event| match &event.kind {
                TaskEventKind::OutputCreated { path } => Some(path),
                _ => None,
            })
            .all(|path| contains_output_file(path))
        && sources.iter().all(|source| {
            source.unchanged()
                && result.processed.iter().any(|candidate| {
                    candidate.path == source.path
                        && candidate.depth == 0
                        && candidate.source == smartzip_engine::CandidateSource::RootInput
                })
        });
    if !safe {
        warnings
            .push("原包已保留：任务未全部成功、源文件变化、输出为空或缺少根归档提交记录".into());
        return;
    }
    for source in sources {
        if cancellation.is_cancelled() || !source.unchanged() {
            warnings.push(format!("原包已保留：{}", source.path.display()));
            continue;
        }
        if let Err(error) = smartzip_platform::move_to_trash(&source.path) {
            warnings.push(format!("原包回收失败 {}：{error}", source.path.display()));
        }
    }
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
    #[tokio::test]
    async fn cancellation_releases_unanswered_password_prompt() {
        let prompts = Prompter {
            mailbox: Mailbox::default(),
            cancellation: CancellationToken::new(),
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
    #[test]
    fn empty_success_never_recycles_input() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("archive.zip");
        std::fs::write(&path, b"data").unwrap();
        let sources = vec![SourceSnapshot::capture(&path).unwrap()];
        let mut result = smartzip_engine::ExtractWorkflowResult {
            status: smartzip_engine::history::TaskCompletionStatus::Completed,
            failed_count: 0,
            task_id: smartzip_core::TaskId::new(),
            processed: vec![],
            skipped: vec![],
            enqueued: vec![],
            events: vec![],
        };
        let mut warnings = vec![];
        recycle_roots(&sources, &result, &CancellationToken::new(), &mut warnings);
        assert!(path.exists());
        assert_eq!(warnings.len(), 1);
        let empty_output = temp.path().join("empty");
        std::fs::create_dir(&empty_output).unwrap();
        result.processed.push(smartzip_engine::ExtractionCandidate {
            path: path.clone(),
            relative_path: "archive".into(),
            depth: 0,
            source: smartzip_engine::CandidateSource::RootInput,
            detected_format: Some(smartzip_core::ArchiveFormat::Zip),
            embedded_offset: None,
            embedded_size: None,
        });
        result.events.push(TaskEvent {
            task_id: result.task_id.clone(),
            kind: TaskEventKind::OutputCreated { path: empty_output },
        });
        recycle_roots(&sources, &result, &CancellationToken::new(), &mut warnings);
        assert!(path.exists());
        assert_eq!(warnings.len(), 2);
    }
    #[test]
    fn source_replacement_is_detected_even_with_same_size() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("archive.zip");
        std::fs::write(&path, b"same").unwrap();
        let source = SourceSnapshot::capture(&path).unwrap();
        let replacement = temp.path().join("other.zip");
        std::fs::write(&replacement, b"same").unwrap();
        std::fs::rename(replacement, &path).unwrap();
        assert!(!source.unchanged());
    }
}
