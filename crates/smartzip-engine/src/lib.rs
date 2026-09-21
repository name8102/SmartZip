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
pub mod run_policy;
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

    pub(crate) fn user_token(&self) -> tokio_util::sync::CancellationToken {
        self.user.clone()
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
pub use execution::{
    ArtifactIdentity, CommitIntent, CommitRecord, CommitSuccessFacts, ExecutionStateRecorder,
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

/// Observers for one extraction. History remains best-effort and caller-owned.
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

    /// Recycle explicit input archives and all successfully used volumes after
    /// the entire task succeeds. Disabled by default; failures retain sources.
    /// Attach live per-root controls and node snapshots for this extraction.
    pub fn with_root_management(
        mut self,
        management: Arc<root_management::RootManagement>,
    ) -> Self {
        self.root_management = Some(management);
        self
    }

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
        identity: ExtractTaskIdentity,
        backend: &B,
        passwords: &PasswordService<'_>,
        request: ExtractWorkflowRequest,
        interaction: ExtractInteraction<'_>,
        observer: ExtractObserver<'_>,
    ) -> smartzip_core::Result<ExtractWorkflowResult> {
        let cleanup = self.recycle_sources.then(|| {
            std::rc::Rc::new(std::cell::RefCell::new(source_cleanup::SourceCleanup::new(
                &request.inputs,
            )))
        });
        let listener = observer.listener.clone();
        let mut result = self
            .extract_task_inner(
                identity,
                backend,
                passwords,
                request,
                interaction,
                observer,
                cleanup.clone(),
            )
            .await?;
        source_cleanup::SourceCleanup::finish(
            cleanup,
            &mut result,
            &self.cancellation,
            &self.archive_recycler,
            listener.as_ref(),
        )
        .await;
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
    ) -> smartzip_core::Result<ExtractWorkflowResult> {
        if let Some(policy) = self.run_policy.as_deref() {
            request = policy
                .resolve_request(request)
                .map_err(|error| smartzip_core::SmartZipError::io(None, error))?;
        }
        let task_budget =
            std::sync::Arc::new(crate::budget::TaskBudget::from_snapshot(identity.budget));
        let task_cancellation = TaskCancellation::new(self.cancellation.child_token());
        let dedup = std::rc::Rc::new(std::cell::RefCell::new(
            extract_workflow::TaskDedup::default(),
        ));
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
            let batch_passwords = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
            let futures = request
                .inputs
                .iter()
                .cloned()
                .zip(identity.roots.iter().cloned())
                .enumerate()
                .map(|(index, (input, root))| {
                    let batch_passwords = batch_passwords.clone();
                    let task_budget = task_budget.clone();
                    let dedup = dedup.clone();
                    let source_cleanup = source_cleanup.clone();
                    let management = self.root_management.clone();
                    let cancellation = management
                        .as_ref()
                        .map(|m| m.cancellation(&root.root_id))
                        .unwrap_or_else(|| task_cancellation.clone());
                    let task_id = task_id.clone();
                    let listener = observer.listener.clone();
                    let history = observer.history;
                    let mut root_request = request.clone();
                    root_request.inputs = vec![input];
                    async move {
                        let root_id = root.root_id.clone();
                        let managed =
                            management
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
                            listener,
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
                            batch_passwords,
                            task_budget,
                            dedup,
                            source_cleanup,
                        )
                        .await;
                        if let Some(m) = &management {
                            m.finish(&root_id, &result);
                        }
                        (index, result)
                    }
                });
            // Keep one additional root in flight for preparation/interaction.
            // Polling the entire batch lets full scans queue ahead of extraction
            // on the same disk, delaying all output until every input is read.
            use futures_util::StreamExt;
            let mut results: Vec<_> = if let Some(management) = &self.root_management {
                // Parked roots release the preparation slot; unrelated roots keep
                // progressing without creating a future for every queued input.
                let mut revision = management.subscribe();
                let mut pending: std::collections::VecDeque<_> = futures.enumerate().collect();
                let mut active = futures_util::stream::FuturesUnordered::new();
                let mut active_indices = std::collections::HashSet::new();
                let mut results = Vec::new();
                while !pending.is_empty() || !active.is_empty() {
                    let occupied = active_indices
                        .iter()
                        .filter(|&&i: &&usize| management.occupies_slot(&identity.roots[i].node_id))
                        .count();
                    for _ in occupied..2 {
                        let Some(position) = pending.iter().position(|(i, _)| {
                            (task_cancellation.is_cancelled()
                                || !management.paused(&identity.roots[*i].root_id))
                                && !active_indices.iter().any(|&active| {
                                    identity.roots[active].root_id == identity.roots[*i].root_id
                                })
                        }) else {
                            break;
                        };
                        let (i, future) = pending.remove(position).unwrap();
                        active_indices.insert(i);
                        active.push(future);
                    }
                    tokio::select! {
                        Some((index, result)) = active.next(), if !active.is_empty() => { active_indices.remove(&index); results.push((index, result)); }
                        _ = revision.changed() => {}
                        _ = task_cancellation.cancelled(), if !pending.is_empty() && active.is_empty() => {
                            // Re-enter bounded admission, including paused queued roots.
                            // Their cancellation is persisted by the ordinary workflow.
                        }
                    }
                }
                results
            } else {
                futures_util::stream::iter(futures)
                    .buffer_unordered(2)
                    .collect()
                    .await
            };
            // Completion is unordered so a waiting root cannot block admission,
            // but the public result keeps the original input ordering.
            results.sort_by_key(|(index, _)| *index);
            let mut processed = Vec::new();
            let mut skipped = Vec::new();
            let mut enqueued = Vec::new();
            let mut events = Vec::new();
            let mut failed_count = 0;
            let mut cancelled = false;
            for (_, result) in results {
                let result = result?;
                failed_count += result.failed_count;
                cancelled |= result.status == history::TaskCompletionStatus::Cancelled;
                processed.extend(result.processed);
                skipped.extend(result.skipped);
                enqueued.extend(result.enqueued);
                events.extend(result.events);
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
                events,
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
            observer.listener,
            observer.history,
            observer.execution,
            identity,
            std::rc::Rc::new(std::cell::RefCell::new(Vec::new())),
            task_budget,
            dedup,
            source_cleanup,
        )
        .await
    }
}

impl Default for SmartZipEngine {
    fn default() -> Self {
        Self::new(EmbeddedScanner::default())
    }
}
