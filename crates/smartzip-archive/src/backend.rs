use crate::types::*;
use async_trait::async_trait;
use smartzip_core::Result;
use std::path::Path;

#[async_trait]
pub trait ArchiveBackend: Send + Sync {
    async fn probe(&self, path: &Path) -> Result<ArchiveProbe>;
    async fn list(&self, request: ListRequest) -> Result<ArchiveListing>;
    async fn test(&self, request: TestRequest) -> Result<TestResult>;
    async fn extract(&self, request: ExtractArchiveRequest) -> Result<ExtractArchiveResult>;
    async fn compress(&self, request: CompressArchiveRequest) -> Result<CompressArchiveResult>;
    fn capabilities(&self) -> BackendCapabilities;
}
