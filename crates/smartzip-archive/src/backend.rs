use crate::types::*;
use async_trait::async_trait;
use smartzip_core::ArchiveFormat;
use smartzip_core::Result;
use std::path::Path;
use std::sync::Arc;

pub type ExtractionProgressCallback = Arc<dyn Fn(f32) + Send + Sync>;

#[async_trait]
pub trait ArchiveBackend: Send + Sync {
    async fn probe(&self, path: &Path) -> Result<ArchiveProbe>;
    async fn list(&self, request: ListRequest) -> Result<ArchiveListing>;
    async fn test(&self, request: TestRequest) -> Result<TestResult>;
    async fn extract(&self, request: ExtractArchiveRequest) -> Result<ExtractArchiveResult>;

    async fn extract_with_progress(
        &self,
        request: ExtractArchiveRequest,
        _progress: Option<ExtractionProgressCallback>,
    ) -> Result<ExtractArchiveResult> {
        self.extract(request).await
    }
    async fn compress(&self, request: CompressArchiveRequest) -> Result<CompressArchiveResult>;
    fn capabilities(&self) -> BackendCapabilities;

    fn should_test_before_extract(&self, _archive: &Path, _format: Option<&ArchiveFormat>) -> bool {
        true
    }
}
