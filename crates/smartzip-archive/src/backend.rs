use crate::types::*;
use async_trait::async_trait;
use smartzip_core::{ArchiveFacts, Result, TaskEventSink, TaskExecutionContext, TaskId};
use std::path::Path;
use std::sync::Arc;

/// Execution seam injected into the engine. Routers implement this trait.
#[async_trait]
pub trait ArchiveExecutor: Send + Sync {
    /// Reset task-local routing observations before a new top-level workflow.
    fn begin_task(
        &self,
        task_id: TaskId,
        events: Arc<dyn TaskEventSink>,
    ) -> Arc<TaskExecutionContext> {
        Arc::new(TaskExecutionContext::new(task_id, events))
    }

    async fn probe(&self, path: &Path) -> Result<ArchiveProbe>;
    async fn probe_with_context(
        &self,
        path: &Path,
        _context: &TaskExecutionContext,
    ) -> Result<ArchiveProbe> {
        self.probe(path).await
    }
    async fn list(&self, request: ListRequest) -> Result<ArchiveListing>;
    async fn list_with_context(
        &self,
        request: ListRequest,
        _context: &TaskExecutionContext,
    ) -> Result<ArchiveListing> {
        self.list(request).await
    }
    async fn test(&self, request: TestRequest) -> Result<TestResult>;
    async fn test_with_context(
        &self,
        request: TestRequest,
        _context: &TaskExecutionContext,
    ) -> Result<TestResult> {
        self.test(request).await
    }
    /// One independent diagnostic pass, outside normal error fallback. None
    /// means no adapter with additional diagnostic value is available.
    async fn diagnose_test_with_context(
        &self,
        _request: TestRequest,
        _previous: &crate::integrity::BackendTestDiagnostics,
        _multivolume: bool,
        _context: &TaskExecutionContext,
    ) -> Result<Option<TestResult>> {
        Ok(None)
    }
    async fn extract(&self, request: ExtractArchiveRequest) -> Result<ExtractArchiveResult>;
    async fn extract_with_context(
        &self,
        request: ExtractArchiveRequest,
        _context: &TaskExecutionContext,
    ) -> Result<ExtractArchiveResult> {
        self.extract(request).await
    }
    async fn extract_with_facts(
        &self,
        request: ExtractArchiveRequest,
        _facts: &ArchiveFacts,
    ) -> Result<ExtractArchiveResult> {
        self.extract(request).await
    }
    async fn extract_with_facts_and_context(
        &self,
        request: ExtractArchiveRequest,
        facts: &ArchiveFacts,
        _context: &TaskExecutionContext,
    ) -> Result<ExtractArchiveResult> {
        self.extract_with_facts(request, facts).await
    }
    async fn compress(&self, request: CompressArchiveRequest) -> Result<CompressArchiveResult>;
    async fn compress_with_context(
        &self,
        request: CompressArchiveRequest,
        _context: &TaskExecutionContext,
    ) -> Result<CompressArchiveResult> {
        self.compress(request).await
    }
}

/// Independent native-library or external-program instance used by a router.
#[async_trait]
pub trait ArchiveAdapter: Send + Sync {
    fn id(&self) -> &str;
    /// Known diagnostic implementation family, used to avoid repeating the
    /// same implementation under another executable path.
    fn diagnostic_family(&self) -> Option<&'static str> {
        None
    }
    async fn probe(&self, path: &Path) -> Result<ArchiveProbe>;
    async fn list(&self, request: ListRequest) -> Result<ArchiveListing>;
    async fn test(&self, request: TestRequest) -> Result<TestResult>;
    async fn extract(&self, request: ExtractArchiveRequest) -> Result<ExtractArchiveResult>;
    async fn compress(&self, request: CompressArchiveRequest) -> Result<CompressArchiveResult>;
    /// Built-in capability profile; config layers are composed by the router.
    fn profile(&self) -> smartzip_core::BackendCapabilityProfile;
}
