//! Interactive prompter traits and choice types.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Allows interactive password prompting during extraction.
///
/// When all stored/candidate passwords fail for an archive, the engine
/// calls this trait to give the user a chance to enter a password manually.
/// If the user provides one and it succeeds, the password is automatically
/// saved to the password database via [`PasswordService::record_success`].
#[async_trait]
pub trait InteractivePasswordPrompter: Send + Sync {
    /// Prompt the user for a password for the given archive.
    ///
    /// Return `Some(password)` if the user entered one, or `None` to skip
    /// this archive. Implementations should use `spawn_blocking` for any
    /// blocking I/O (e.g. stdin reads) to avoid stalling the async runtime.
    async fn prompt(&self, archive_path: &Path) -> Option<String>;
}

/// Strategy used when the requested output path already exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OutputCollisionStrategy {
    Skip,
    Overwrite,
    Rename,
}

/// Allows interactive resolution of output path collisions.
#[async_trait]
pub trait InteractiveOutputPrompter: Send + Sync {
    /// Prompt the user for how to handle an existing output path.
    ///
    /// Implementations should use `spawn_blocking` for terminal I/O so the
    /// async runtime can continue extracting unrelated archives while the
    /// user decides.
    async fn prompt(&self, archive_path: PathBuf, output_path: PathBuf) -> OutputCollisionStrategy;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EmbeddedSelectionChoice {
    Extract,
    Skip,
    ExtractAll,
}

#[async_trait]
pub trait InteractiveEmbeddedPrompter: Send + Sync {
    async fn prompt(
        &self,
        archive_path: &Path,
        decision: &smartzip_core::DetectionDecision,
    ) -> EmbeddedSelectionChoice;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EncodingConfirmationChoice {
    AcceptDetected,
    Override(String),
    SkipArchive,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EncodingConfirmationContext {
    pub detected: smartzip_core::EncodingDetectionResult,
    pub preview_names: Vec<String>,
    pub suspicious_reasons: Vec<String>,
}

#[async_trait]
pub trait InteractiveEncodingPrompter: Send + Sync {
    async fn prompt(
        &self,
        archive_path: &Path,
        context: &EncodingConfirmationContext,
    ) -> EncodingConfirmationChoice;
}
