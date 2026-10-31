use crate::types::*;
use async_trait::async_trait;
use smartzip_core::{
    AdapterCapabilities, ArchiveFacts, Result, TaskEventSink, TaskExecutionContext, TaskId,
};
use std::path::Path;
use std::sync::Arc;

/// Execution seam injected into the engine. Routers implement this trait.
#[async_trait]
pub trait ArchiveExecutor: Send + Sync {
    fn begin_task_with_cancellation(
        &self,
        task_id: TaskId,
        events: Arc<dyn TaskEventSink>,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> Arc<TaskExecutionContext> {
        Arc::new(TaskExecutionContext::new(task_id, events).with_cancellation(cancellation))
    }

    async fn probe_with_context(
        &self,
        path: &Path,
        context: Arc<TaskExecutionContext>,
    ) -> Result<ArchiveProbe>;
    async fn list_with_context(
        &self,
        request: ListRequest,
        context: Arc<TaskExecutionContext>,
    ) -> Result<ArchiveListing>;
    async fn test_with_context(
        &self,
        request: TestRequest,
        context: Arc<TaskExecutionContext>,
    ) -> Result<TestResult>;
    /// One independent diagnostic pass, outside normal error fallback. None
    /// means no adapter with additional diagnostic value is available.
    async fn diagnose_test_with_context(
        &self,
        _request: TestRequest,
        _previous: &crate::integrity::BackendTestDiagnostics,
        _multivolume: bool,
        _context: Arc<TaskExecutionContext>,
    ) -> Result<Option<TestResult>> {
        Ok(None)
    }
    async fn extract_with_facts_and_context(
        &self,
        request: ExtractArchiveRequest,
        facts: &ArchiveFacts,
        context: Arc<TaskExecutionContext>,
    ) -> Result<ExtractArchiveResult>;
}

/// Independent native-library or external-program instance used by a router.
#[async_trait]
pub trait ArchiveAdapter: Send + Sync {
    fn id(&self) -> &str;
    fn executable_path(&self) -> Option<&Path> {
        None
    }
    /// Known diagnostic implementation family, used to avoid repeating the
    /// same implementation under another executable path.
    fn diagnostic_family(&self) -> Option<&'static str> {
        None
    }
    async fn probe_with_context(
        &self,
        path: &Path,
        context: Arc<TaskExecutionContext>,
    ) -> Result<ArchiveProbe>;
    async fn list_with_context(
        &self,
        request: ListRequest,
        context: Arc<TaskExecutionContext>,
    ) -> Result<ArchiveListing>;
    async fn test_with_context(
        &self,
        request: TestRequest,
        context: Arc<TaskExecutionContext>,
    ) -> Result<TestResult>;
    /// Observe cancellation and stop all output writes before returning.
    async fn extract_with_context(
        &self,
        request: ExtractArchiveRequest,
        context: Arc<TaskExecutionContext>,
    ) -> Result<ExtractArchiveResult>;
    /// Concrete capabilities consumed by the router.
    fn capabilities(&self) -> AdapterCapabilities;
}
