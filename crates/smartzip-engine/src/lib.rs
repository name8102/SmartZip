//! Thin facade for SmartZip archive workflows.
//!
//! The facade owns caller-injected dependencies and delegates capability work
//! to the private workflow modules. Public request/result types are re-exported
//! for API compatibility.

pub mod container;
pub mod detect;
pub mod embedded;
pub mod embedded_zip;
pub mod history;
pub mod layout;
mod materialize;
pub mod name_score;

mod access;
mod backend_util;
mod encoding_flow;
mod events;
mod extract_workflow;
pub mod interactive;
mod nested;
mod password_order;
mod policy;
mod types;
mod workflow;

#[cfg(test)]
mod engine_tests;

use smartzip_archive::ArchiveExecutor;
use smartzip_passwords::PasswordService;
use smartzip_scanner::{EmbeddedScanner, ScannerConfig};
use std::sync::Arc;

pub use events::TaskEventListener;
pub use interactive::{
    EmbeddedSelectionChoice, EncodingConfirmationChoice, EncodingConfirmationContext,
    InteractiveEmbeddedPrompter, InteractiveEncodingPrompter, InteractiveOutputPrompter,
    InteractivePasswordPrompter, OutputCollisionStrategy,
};
pub use nested::{format_from_extension, is_first_volume};
pub use types::{
    ArchiveRecycleHandler, CandidateSource, DetectRequest, DetectResult, ExtractWorkflowRequest,
    ExtractWorkflowResult, ExtractionCandidate, FileAwareDetectResult, InspectRequest,
    ListArchiveRequest, ListArchiveResult, SmartZipEngine,
};

impl SmartZipEngine {
    pub fn new(scanner: EmbeddedScanner) -> Self {
        Self {
            scanner,
            archive_recycler: Arc::new(smartzip_platform::move_to_trash),
            min_embedded_size_bytes: smartzip_core::DEFAULT_MIN_EMBEDDED_FINDING_SIZE,
        }
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
        passwords: &PasswordService<'_>,
        request: InspectRequest,
        listener: Option<TaskEventListener>,
        history: Option<&dyn history::TaskHistoryRecorder>,
    ) -> smartzip_core::Result<FileAwareDetectResult> {
        workflow::inspect_file_with_listener(
            self.min_embedded_size_bytes,
            backend,
            passwords,
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
            self.min_embedded_size_bytes,
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
        workflow::extract_recursive_with_listener_interactive(
            &self.scanner,
            self.min_embedded_size_bytes,
            &self.archive_recycler,
            backend,
            passwords,
            request,
            password_prompter,
            output_prompter,
            embedded_prompter,
            encoding_prompter,
            listener,
            history,
        )
        .await
    }
}

impl Default for SmartZipEngine {
    fn default() -> Self {
        Self::new(EmbeddedScanner::default())
    }
}
