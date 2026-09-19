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
    stopped_on_error: Arc<std::sync::atomic::AtomicBool>,
}

impl TaskCancellation {
    fn new(user: tokio_util::sync::CancellationToken) -> Self {
        Self {
            token: user.child_token(),
            user,
            stopped_on_error: Arc::new(std::sync::atomic::AtomicBool::new(false)),
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
        self.token.cancel();
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
            min_embedded_size_bytes: smartzip_core::DEFAULT_MIN_EMBEDDED_FINDING_SIZE,
        }
    }

    pub fn with_run_policy(mut self, policy: CompiledRunPolicy) -> Self {
        self.run_policy = Some(Arc::new(policy));
        self
    }

    /// Override how successfully processed nested archives are recycled.
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
        mut request: ExtractWorkflowRequest,
        interaction: ExtractInteraction<'_>,
        observer: ExtractObserver<'_>,
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
        if let Some(execution) = observer.execution.filter(|_| request.inputs.len() > 1) {
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
                .map(|(input, root)| {
                    let batch_passwords = batch_passwords.clone();
                    let task_budget = task_budget.clone();
                    let dedup = dedup.clone();
                    let cancellation = task_cancellation.clone();
                    let task_id = task_id.clone();
                    let listener = observer.listener.clone();
                    let history = observer.history;
                    let mut root_request = request.clone();
                    root_request.inputs = vec![input];
                    async move {
                        workflow::extract_recursive_with_listener_interactive(
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
                            Some(execution),
                            ExtractTaskIdentity {
                                task_id: task_id.clone(),
                                roots: vec![root],
                                budget: TaskBudgetSnapshot::default(),
                            },
                            batch_passwords,
                            task_budget,
                            dedup,
                        )
                        .await
                    }
                });
            let results = futures_util::future::join_all(futures).await;
            let mut processed = Vec::new();
            let mut skipped = Vec::new();
            let mut enqueued = Vec::new();
            let mut events = Vec::new();
            let mut failed_count = 0;
            let mut cancelled = false;
            for result in results {
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
        )
        .await
    }
}

impl Default for SmartZipEngine {
    fn default() -> Self {
        Self::new(EmbeddedScanner::default())
    }
}
