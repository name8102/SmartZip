//! Thin facade for SmartZip archive workflows.
//!
//! The facade owns caller-injected dependencies and delegates capability work
//! to the private workflow modules. Public request/result types are re-exported
//! for API compatibility.

pub mod container;
pub mod coordinator;
pub mod detect;
pub mod embedded;
pub mod embedded_zip;
pub mod history;
pub mod layout;
mod materialize;
pub mod name_score;
pub mod root_inputs;
pub mod root_management;
mod source_cleanup;

mod access;
mod backend_util;
pub mod budget;
mod encoding_flow;
mod events;
mod execution;
pub mod execution_runtime;
mod extract_workflow;
pub mod interactive;
mod nested;
mod password_order;
mod policy;
mod prepared_extract;
pub mod run_policy;
mod run_services;
pub use prepared_extract::PreparedExtractTask;
pub use run_services::RunServices;
pub mod state_store;
pub use run_policy::CompiledRunPolicy;
mod test_reduce;
mod test_workflow;
mod types;
pub mod volumes;
mod workflow;

#[cfg(test)]
mod engine_tests;

use smartzip_archive::ArchiveExecutor;
use smartzip_passwords::PasswordService;
use smartzip_scanner::{EmbeddedScanner, ScannerConfig};
use std::sync::Arc;

#[derive(Clone)]
pub(crate) struct TaskCancellation {
    token: tokio_util::sync::CancellationToken,
    user: tokio_util::sync::CancellationToken,
    failure: tokio_util::sync::CancellationToken,
    stopped_on_error: Arc<std::sync::atomic::AtomicBool>,
}

impl TaskCancellation {
    fn new(user: tokio_util::sync::CancellationToken) -> Self {
        let token = user.child_token();
        Self {
            failure: token.clone(),
            token,
            user,
            stopped_on_error: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    fn fork(&self) -> Self {
        Self {
            token: self.token.child_token(),
            user: self.user.child_token(),
            failure: self.failure.clone(),
            stopped_on_error: self.stopped_on_error.clone(),
        }
    }

    pub(crate) fn token(&self) -> &tokio_util::sync::CancellationToken {
        &self.token
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.token.is_cancelled()
    }

    pub(crate) fn is_user_cancelled(&self) -> bool {
        self.user.is_cancelled()
    }

    pub(crate) fn stopped_on_error(&self) -> bool {
        self.stopped_on_error
            .load(std::sync::atomic::Ordering::Acquire)
    }

    pub(crate) fn stop_on_error(&self) {
        self.stopped_on_error
            .store(true, std::sync::atomic::Ordering::Release);
        self.failure.cancel();
    }

    pub(crate) async fn cancelled(&self) {
        self.token.cancelled().await;
    }
}

pub use events::TaskEventListener;
/// Compatibility name for the active execution controller.
pub use execution::ExecutionControl as ExecutionStateRecorder;
pub use execution::{
    ArtifactIdentity, CommitIntent, CommitRecord, CommitSuccessFacts, ExecutionControl,
    ExtractRootIdentity, ExtractTaskIdentity, NodeOutcome, StagingArtifact, TaskBudgetSnapshot,
};
pub use interactive::{
    EmbeddedSelectionChoice, EncodingConfirmationChoice, EncodingConfirmationContext,
    InteractiveEmbeddedPrompter, InteractiveEncodingPrompter, InteractiveOutputPrompter,
    InteractivePasswordPrompter, OutputCollisionStrategy,
};
pub use nested::format_from_extension;
pub use test_workflow::{DiagnoseMode, TestWorkflowRequest, TestWorkflowResult};
pub use types::{
    ArchiveRecycleHandler, CandidateSource, DetectRequest, DetectResult, ExtractWorkflowRequest,
    ExtractWorkflowResult, ExtractionCandidate, FileAwareDetectResult, InspectRequest,
    ListArchiveRequest, ListArchiveResult, SmartZipEngine,
};

/// Interactive choices for one extraction; absent callbacks retain existing defaults.
#[derive(Default)]
pub struct ExtractInteraction<'a> {
    pub password: Option<&'a dyn InteractivePasswordPrompter>,
    pub output: Option<&'a dyn InteractiveOutputPrompter>,
    pub embedded: Option<&'a dyn InteractiveEmbeddedPrompter>,
    pub encoding: Option<&'a dyn InteractiveEncodingPrompter>,
}

/// Extraction hooks. History is best-effort; `execution` actively admits stages
/// and owns durable transitions/commits when supplied.
#[derive(Default)]
pub struct ExtractObserver<'a> {
    pub listener: Option<TaskEventListener>,
    pub history: Option<&'a dyn history::TaskHistoryRecorder>,
    pub execution: Option<&'a dyn ExecutionStateRecorder>,
}

impl SmartZipEngine {
    pub async fn test_archives<B: ArchiveExecutor>(
        &self,
        backend: &B,
        passwords: &PasswordService<'_>,
        request: TestWorkflowRequest,
        password_prompter: Option<&dyn InteractivePasswordPrompter>,
        listener: Option<TaskEventListener>,
        history: Option<&dyn history::TaskHistoryRecorder>,
    ) -> smartzip_core::Result<TestWorkflowResult> {
        test_workflow::run(
            backend,
            passwords,
            request,
            password_prompter,
            listener,
            history,
        )
        .await
    }
    pub fn with_cancellation_token(mut self, token: tokio_util::sync::CancellationToken) -> Self {
        self.cancellation = token;
        self
    }

    pub fn new(scanner: EmbeddedScanner) -> Self {
        Self {
            scanner,
            run_policy: None,
            cancellation: tokio_util::sync::CancellationToken::new(),
            archive_recycler: Arc::new(smartzip_platform::move_to_trash),
            recycle_sources: false,
            root_management: None,
            min_embedded_size_bytes: smartzip_core::DEFAULT_MIN_EMBEDDED_FINDING_SIZE,
        }
    }

    pub fn with_run_policy(mut self, policy: CompiledRunPolicy) -> Self {
        self.run_policy = Some(Arc::new(policy));
        self
    }

    /// Attach live per-root controls and node snapshots for this extraction.
    pub fn with_root_management(
        mut self,
        management: Arc<root_management::RootManagement>,
    ) -> Self {
        self.root_management = Some(management);
        self
    }

    /// Recycle explicit inputs and winning volumes only after the task succeeds.
    pub fn with_source_recycling(mut self, enabled: bool) -> Self {
        self.recycle_sources = enabled;
        self
    }

    /// Override recycling for sources and successfully processed nested archives.
    pub fn with_archive_recycler(mut self, archive_recycler: ArchiveRecycleHandler) -> Self {
        self.archive_recycler = archive_recycler;
        self
    }

    pub fn with_scanner_config(config: ScannerConfig) -> Self {
        Self::new(EmbeddedScanner::new(config))
    }

    pub fn with_min_embedded_size_bytes(mut self, min_embedded_size_bytes: u64) -> Self {
        self.min_embedded_size_bytes = min_embedded_size_bytes;
        self
    }

    pub fn detect(&self, request: DetectRequest) -> std::io::Result<DetectResult> {
        workflow::detect(&self.scanner, request)
    }

    pub async fn inspect_file_with_listener<B: ArchiveExecutor>(
        &self,
        backend: &B,
        _passwords: &PasswordService<'_>,
        request: InspectRequest,
        listener: Option<TaskEventListener>,
        history: Option<&dyn history::TaskHistoryRecorder>,
    ) -> smartzip_core::Result<FileAwareDetectResult> {
        self.inspect(backend, request, listener, history).await
    }

    /// Inspect an archive without constructing a password service.
    pub async fn inspect<B: ArchiveExecutor>(
        &self,
        backend: &B,
        request: InspectRequest,
        listener: Option<TaskEventListener>,
        history: Option<&dyn history::TaskHistoryRecorder>,
    ) -> smartzip_core::Result<FileAwareDetectResult> {
        workflow::inspect_file_with_listener(
            self.cancellation.clone(),
            self.run_policy.as_deref(),
            backend,
            request,
            listener,
            history,
        )
        .await
    }

    pub async fn list_archive_with_listener_interactive<B: ArchiveExecutor>(
        &self,
        backend: &B,
        passwords: &PasswordService<'_>,
        request: ListArchiveRequest,
        password_prompter: Option<&dyn InteractivePasswordPrompter>,
        encoding_prompter: Option<&dyn InteractiveEncodingPrompter>,
        listener: Option<TaskEventListener>,
        history: Option<&dyn history::TaskHistoryRecorder>,
    ) -> smartzip_core::Result<ListArchiveResult> {
        workflow::list_archive_with_listener_interactive(
            self.cancellation.clone(),
            self.run_policy.as_deref(),
            backend,
            passwords,
            request,
            password_prompter,
            encoding_prompter,
            listener,
            history,
        )
        .await
    }

    pub async fn extract_recursive<B: ArchiveExecutor>(
        &self,
        backend: &B,
        passwords: &PasswordService<'_>,
        request: ExtractWorkflowRequest,
        password_prompter: Option<&dyn InteractivePasswordPrompter>,
        output_prompter: Option<&dyn InteractiveOutputPrompter>,
    ) -> smartzip_core::Result<ExtractWorkflowResult> {
        self.extract_recursive_with_listener_interactive(
            backend,
            passwords,
            request,
            password_prompter,
            output_prompter,
            None,
            None,
            None,
            None,
        )
        .await
    }

    pub async fn extract_recursive_interactive<B: ArchiveExecutor>(
        &self,
        backend: &B,
        passwords: &PasswordService<'_>,
        request: ExtractWorkflowRequest,
        password_prompter: Option<&dyn InteractivePasswordPrompter>,
        output_prompter: Option<&dyn InteractiveOutputPrompter>,
        embedded_prompter: Option<&dyn InteractiveEmbeddedPrompter>,
        encoding_prompter: Option<&dyn InteractiveEncodingPrompter>,
    ) -> smartzip_core::Result<ExtractWorkflowResult> {
        self.extract_recursive_with_listener_interactive(
            backend,
            passwords,
            request,
            password_prompter,
            output_prompter,
            embedded_prompter,
            encoding_prompter,
            None,
            None,
        )
        .await
    }

    pub async fn extract_recursive_with_listener<B: ArchiveExecutor>(
        &self,
        backend: &B,
        passwords: &PasswordService<'_>,
        request: ExtractWorkflowRequest,
        password_prompter: Option<&dyn InteractivePasswordPrompter>,
        output_prompter: Option<&dyn InteractiveOutputPrompter>,
        listener: Option<TaskEventListener>,
    ) -> smartzip_core::Result<ExtractWorkflowResult> {
        self.extract_recursive_with_listener_interactive(
            backend,
            passwords,
            request,
            password_prompter,
            output_prompter,
            None,
            None,
            listener,
            None,
        )
        .await
    }

    pub async fn extract_recursive_with_listener_interactive<B: ArchiveExecutor>(
        &self,
        backend: &B,
        passwords: &PasswordService<'_>,
        request: ExtractWorkflowRequest,
        password_prompter: Option<&dyn InteractivePasswordPrompter>,
        output_prompter: Option<&dyn InteractiveOutputPrompter>,
        embedded_prompter: Option<&dyn InteractiveEmbeddedPrompter>,
        encoding_prompter: Option<&dyn InteractiveEncodingPrompter>,
        listener: Option<TaskEventListener>,
        history: Option<&dyn history::TaskHistoryRecorder>,
    ) -> smartzip_core::Result<ExtractWorkflowResult> {
        self.extract(
            backend,
            passwords,
            request,
            ExtractInteraction {
                password: password_prompter,
                output: output_prompter,
                embedded: embedded_prompter,
                encoding: encoding_prompter,
            },
            ExtractObserver {
                listener,
                history,
                execution: None,
            },
        )
        .await
    }

    /// Canonical extraction entrypoint. Legacy overloads remain source-compatible.
    pub async fn extract<B: ArchiveExecutor>(
        &self,
        backend: &B,
        passwords: &PasswordService<'_>,
        request: ExtractWorkflowRequest,
        interaction: ExtractInteraction<'_>,
        observer: ExtractObserver<'_>,
    ) -> smartzip_core::Result<ExtractWorkflowResult> {
        self.extract_task(
            ExtractTaskIdentity::new(&request.inputs),
            backend,
            passwords,
            request,
            interaction,
            observer,
        )
        .await
    }

    pub async fn extract_task<B: ArchiveExecutor>(
        &self,
        mut identity: ExtractTaskIdentity,
        backend: &B,
        passwords: &PasswordService<'_>,
        request: ExtractWorkflowRequest,
        interaction: ExtractInteraction<'_>,
        observer: ExtractObserver<'_>,
    ) -> smartzip_core::Result<ExtractWorkflowResult> {
        let request = if let Some(policy) = self.run_policy.as_deref() {
            for root in &mut identity.roots {
                root.candidate.path = std::path::absolute(&root.candidate.path)
                    .map_err(|error| smartzip_core::SmartZipError::io(None, error))?;
            }
            policy
                .resolve_request(request)
                .map_err(|error| smartzip_core::SmartZipError::io(None, error))?
        } else {
            request
        };
        self.extract_resolved(identity, backend, passwords, request, interaction, observer)
            .await
    }

    pub(crate) async fn extract_resolved<B: ArchiveExecutor>(
        &self,
        identity: ExtractTaskIdentity,
        backend: &B,
        passwords: &PasswordService<'_>,
        request: ExtractWorkflowRequest,
        interaction: ExtractInteraction<'_>,
        observer: ExtractObserver<'_>,
    ) -> smartzip_core::Result<ExtractWorkflowResult> {
        let task_id = identity.task_id.clone();
        let output_dir = request.output_dir.clone();
        let events = events::EventSink::new(observer.listener.clone());
        let legacy_history = observer
            .history
            .filter(|_| observer.execution.is_none_or(|e| !e.durable()));
        if let Some(history) = legacy_history {
            history.start_extract(&task_id, Some(&output_dir));
        }
        let mut completion = history::CompletionGuard::new(
            legacy_history,
            task_id.clone(),
            events.clone(),
            self.cancellation.clone(),
        );
        events.push(smartzip_core::TaskEvent::started(task_id.clone()));
        if let Some(policy) = &self.run_policy {
            policy.emit_plan(&events, &task_id);
        }
        let cleanup = self.recycle_sources.then(|| {
            std::rc::Rc::new(std::cell::RefCell::new(source_cleanup::SourceCleanup::new(
                &request.inputs,
            )))
        });
        let mut result = self
            .extract_task_inner(
                identity,
                backend,
                passwords,
                request,
                interaction,
                observer,
                cleanup.clone(),
                events.clone(),
            )
            .await?;
        source_cleanup::SourceCleanup::finish(
            cleanup,
            &mut result,
            &self.cancellation,
            &self.archive_recycler,
            &events,
        )
        .await;
        events.push(smartzip_core::TaskEvent {
            task_id: task_id.clone(),
            kind: smartzip_core::TaskEventKind::Finished {
                status: format!("{:?}", result.status).to_ascii_lowercase(),
            },
        });
        result.events = events.snapshot();
        if let Some(history) = legacy_history {
            for event in &result.events {
                history.record_event(&task_id, event);
            }
            history.finish(
                &task_id,
                history::TaskOutcome {
                    status: result.status,
                    output_path: Some(&output_dir),
                },
            );
        }
        completion.complete();
        Ok(result)
    }

    async fn extract_task_inner<B: ArchiveExecutor>(
        &self,
        identity: ExtractTaskIdentity,
        backend: &B,
        passwords: &PasswordService<'_>,
        mut request: ExtractWorkflowRequest,
        interaction: ExtractInteraction<'_>,
        observer: ExtractObserver<'_>,
        source_cleanup: source_cleanup::SharedCleanup,
        events: events::EventSink,
    ) -> smartzip_core::Result<ExtractWorkflowResult> {
        let task_cancellation = TaskCancellation::new(self.cancellation.child_token());
        let state = extract_workflow::ExtractTaskState {
            budget: Arc::new(crate::budget::TaskBudget::from_snapshot(identity.budget)),
            batch_passwords: std::rc::Rc::new(std::cell::RefCell::new(Vec::new())),
            dedup: Default::default(),
            source_cleanup,
            backend_context: backend.begin_task_with_cancellation(
                identity.task_id.clone(),
                Arc::new(events.clone()),
                task_cancellation.token().child_token(),
            ),
        };
        if self.root_management.is_some()
            || (observer.execution.is_some() && request.inputs.len() > 1)
        {
            let execution = observer.execution;
            if let Some(management) = &self.root_management {
                management.register(&identity, &task_cancellation);
            }
            if identity.roots.len() != request.inputs.len() {
                return Err(smartzip_core::SmartZipError::ResourceLimit {
                    detail: "task root identity count does not match input count".into(),
                });
            }
            let task_id = identity.task_id.clone();
            let inputs = std::mem::take(&mut request.inputs);
            let make_root = |index: usize| {
                let input = inputs[index].clone();
                let root = identity.roots[index].clone();
                let state = state.clone();
                let management = self.root_management.clone();
                let cancellation = management
                    .as_ref()
                    .map(|m| m.cancellation(&root.root_id))
                    .unwrap_or_else(|| task_cancellation.clone());
                let task_id = task_id.clone();
                let listener = observer.listener.clone();
                let history = observer.history;
                let mut root_request = request.clone();
                root_request.inputs.push(input);
                let events = events.clone();
                async move {
                    let root_id = root.root_id.clone();
                    let managed = management
                        .as_ref()
                        .map(|m| root_management::ManagedRecorder {
                            inner: execution,
                            management: m.clone(),
                            root: root_id.clone(),
                            lane: root.node_id.clone(),
                        });
                    let listener = if let Some(m) = &management {
                        let m = m.clone();
                        let root_id = root.node_id.clone();
                        Some(Arc::new(move |event: &smartzip_core::TaskEvent| {
                            m.event(&root_id, event);
                            if let Some(listener) = &listener {
                                listener(event);
                            }
                        }) as TaskEventListener)
                    } else {
                        listener
                    };
                    let result = workflow::extract_recursive_with_listener_interactive(
                        &self.scanner,
                        self.run_policy.as_deref(),
                        self.min_embedded_size_bytes,
                        &self.archive_recycler,
                        cancellation.clone(),
                        backend,
                        passwords,
                        root_request,
                        interaction.password,
                        interaction.output,
                        interaction.embedded,
                        interaction.encoding,
                        events.with_listener(listener),
                        history,
                        managed
                            .as_ref()
                            .map(|m| m as &dyn ExecutionStateRecorder)
                            .or(execution),
                        ExtractTaskIdentity {
                            task_id: task_id.clone(),
                            roots: vec![root],
                            budget: TaskBudgetSnapshot::default(),
                        },
                        state,
                    )
                    .await;
                    if let Some(m) = &management {
                        m.finish(&root_id, &result);
                    }
                    (index, result)
                }
            };
            // Keep one additional root in flight for preparation/interaction.
            // Polling the entire batch lets full scans queue ahead of extraction
            // on the same disk, delaying all output until every input is read.
            use futures_util::StreamExt;
            let management = self.root_management.as_ref();
            let mut revision = management.map(|m| m.subscribe());
            // Queued roots contain only indices. Construct requests/futures after
            // admission so a large batch does not clone all inputs per root.
            let mut pending: std::collections::VecDeque<_> = (0..inputs.len()).collect();
            let mut active = futures_util::stream::FuturesUnordered::new();
            let mut active_indices = std::collections::HashSet::new();
            let mut results = Vec::new();
            while !pending.is_empty() || !active.is_empty() {
                let occupied = active_indices
                    .iter()
                    .filter(|&&i: &&usize| {
                        management.is_none_or(|m| m.occupies_slot(&identity.roots[i].node_id))
                    })
                    .count();
                for _ in occupied..2 {
                    let Some(position) = pending.iter().position(|&i| {
                        management.is_none_or(|m| {
                            (task_cancellation.is_cancelled()
                                || !m.paused(&identity.roots[i].root_id))
                                && !active_indices.iter().any(|&active| {
                                    identity.roots[active].root_id == identity.roots[i].root_id
                                })
                        })
                    }) else {
                        break;
                    };
                    let index = pending.remove(position).unwrap();
                    active_indices.insert(index);
                    active.push(make_root(index));
                }
                tokio::select! {
                    Some((index, result)) = active.next(), if !active.is_empty() => {
                        active_indices.remove(&index);
                        results.push((index, result));
                    }
                    _ = async { match &mut revision {
                        Some(revision) => { let _ = revision.changed().await; }
                        None => std::future::pending::<()>().await,
                    } } => {}
                    _ = task_cancellation.cancelled(), if !pending.is_empty() && active.is_empty() => {}
                }
            }
            // Completion is unordered so a waiting root cannot block admission,
            // but the public result keeps the original input ordering.
            results.sort_by_key(|(index, _)| *index);
            let mut processed = Vec::new();
            let mut skipped = Vec::new();
            let mut enqueued = Vec::new();
            let mut failed_count = 0;
            let mut cancelled = false;
            for (_, result) in results {
                let result = result?;
                failed_count += result.failed_count;
                cancelled |= result.status == history::TaskCompletionStatus::Cancelled;
                processed.extend(result.processed);
                skipped.extend(result.skipped);
                enqueued.extend(result.enqueued);
            }
            return Ok(ExtractWorkflowResult {
                status: history::TaskCompletionStatus::from_counts(
                    processed.len(),
                    failed_count,
                    cancelled,
                ),
                failed_count,
                task_id: identity.task_id,
                processed,
                skipped,
                enqueued,
                events: Vec::new(),
            });
        }
        workflow::extract_recursive_with_listener_interactive(
            &self.scanner,
            self.run_policy.as_deref(),
            self.min_embedded_size_bytes,
            &self.archive_recycler,
            task_cancellation,
            backend,
            passwords,
            request,
            interaction.password,
            interaction.output,
            interaction.embedded,
            interaction.encoding,
            events,
            observer.history,
            observer.execution,
            identity,
            state,
        )
        .await
    }
}

impl Default for SmartZipEngine {
    fn default() -> Self {
        Self::new(EmbeddedScanner::default())
    }
}
