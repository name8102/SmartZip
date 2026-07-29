use crate::types::*;
use async_trait::async_trait;
use smartzip_core::Result;
use std::path::Path;

/// Execution seam injected into the engine. Routers implement this trait.
#[async_trait]
pub trait ArchiveExecutor: Send + Sync {
    /// Reset task-local routing observations before a new top-level workflow.
    fn begin_task(&self) {}

    async fn probe(&self, path: &Path) -> Result<ArchiveProbe>;
    async fn list(&self, request: ListRequest) -> Result<ArchiveListing>;
    async fn test(&self, request: TestRequest) -> Result<TestResult>;
    async fn extract(&self, request: ExtractArchiveRequest) -> Result<ExtractArchiveResult>;
    async fn compress(&self, request: CompressArchiveRequest) -> Result<CompressArchiveResult>;
}

/// Independent native-library or external-program instance used by a router.
#[async_trait]
pub trait ArchiveAdapter: Send + Sync {
    fn id(&self) -> &str;
    async fn probe(&self, path: &Path) -> Result<ArchiveProbe>;
    async fn list(&self, request: ListRequest) -> Result<ArchiveListing>;
    async fn test(&self, request: TestRequest) -> Result<TestResult>;
    async fn extract(&self, request: ExtractArchiveRequest) -> Result<ExtractArchiveResult>;
    async fn compress(&self, request: CompressArchiveRequest) -> Result<CompressArchiveResult>;
    fn capabilities(&self) -> BackendCapabilities;
}
