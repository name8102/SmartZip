//! Request/result and candidate data types.

use serde::{Deserialize, Serialize};
use smartzip_archive::ArchiveListing;
use smartzip_core::{ArchiveFormat, EncodingMode, TaskEvent, TaskId};
use smartzip_passwords::PasswordCandidateRequest;
use smartzip_scanner::{EmbeddedArchiveFinding, ScannerConfig};
use std::path::PathBuf;
use std::sync::Arc;

use crate::encoding_flow::ZipEncodingAssessment;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DetectRequest {
    pub path: PathBuf,
    pub scanner: ScannerConfig,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DetectResult {
    pub task_id: TaskId,
    pub path: PathBuf,
    pub findings: Vec<EmbeddedArchiveFinding>,
    pub events: Vec<TaskEvent>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InspectRequest {
    pub path: PathBuf,
    pub scanner: ScannerConfig,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListArchiveRequest {
    pub path: PathBuf,
    pub scanner: ScannerConfig,
    pub encoding_mode: EncodingMode,
    pub password_candidates: PasswordCandidateRequest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileAwareDetectResult {
    pub task_id: TaskId,
    pub path: PathBuf,
    pub detected_format: Option<ArchiveFormat>,
    pub embedded_findings: Vec<EmbeddedArchiveFinding>,
    pub embedded_count: usize,
    pub encrypted: Option<bool>,
    pub encoding: Option<String>,
    pub encoding_confidence: Option<f32>,
    pub needs_password: bool,
    pub known_password: bool,
    pub known_encoding: Option<String>,
    pub status: String,
    pub reason: Option<String>,
    pub events: Vec<TaskEvent>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ListArchiveResult {
    pub task_id: TaskId,
    pub path: PathBuf,
    pub detected_format: Option<ArchiveFormat>,
    pub entries: Vec<smartzip_archive::ArchiveEntry>,
    pub encrypted: Option<bool>,
    pub encoding: String,
    pub password_id: Option<i64>,
    pub used_password: bool,
    pub embedded_offset: Option<u64>,
    pub events: Vec<TaskEvent>,
}

pub(crate) struct ResolvedArchive {
    pub(crate) candidate: ExtractionCandidate,
    pub(crate) archive_path: PathBuf,
    pub(crate) _archive_temp: Option<tempfile::NamedTempFile>,
    pub(crate) sample_hash: Option<String>,
    pub(crate) sample_size: Option<i64>,
    pub(crate) known_hit: Option<crate::history::KnownFileHit>,
    pub(crate) encoding_mode: EncodingMode,
    pub(crate) reused_confirmed_encoding: bool,
    pub(crate) zip_encoding_assessment: Option<ZipEncodingAssessment>,
    pub(crate) recorder_name: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct ArchiveAccessOutcome {
    pub(crate) password_id: Option<i64>,
    pub(crate) has_password: bool,
    pub(crate) encoding_mode: EncodingMode,
    pub(crate) listing: Option<ArchiveListing>,
    pub(crate) encrypted: Option<bool>,
    pub(crate) events: Vec<TaskEvent>,
}

pub struct SmartZipEngine {
    pub(crate) scanner: smartzip_scanner::EmbeddedScanner,
    pub(crate) archive_recycler: ArchiveRecycleHandler,
    pub(crate) min_embedded_size_bytes: u64,
}

pub type ArchiveRecycleHandler = Arc<dyn Fn(PathBuf) -> std::io::Result<()> + Send + Sync>;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExtractWorkflowRequest {
    pub inputs: Vec<PathBuf>,
    pub output_dir: PathBuf,
    pub recursion_limit: u8,
    pub encoding_mode: EncodingMode,
    pub scanner: ScannerConfig,
    pub password_candidates: PasswordCandidateRequest,
    pub layout_policy: crate::layout::OutputLayoutPolicy,
    pub single_root_name_policy: crate::layout::SingleRootNamePolicy,
    pub embedded_scan_mode: smartzip_core::EmbeddedScanMode,
    pub dominant_min_ratio: f32,
    pub confirm_large_scan: bool,
    /// Bypass the `known_files` dedup skip and re-extract even when this file
    /// was already extracted inside the dedup window.
    pub force: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtractionCandidate {
    pub path: PathBuf,
    pub relative_path: PathBuf,
    pub depth: u8,
    pub source: CandidateSource,
    pub detected_format: Option<ArchiveFormat>,
    pub embedded_offset: Option<u64>,
    pub embedded_size: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CandidateSource {
    RootInput,
    ExtractedFile,
    EmbeddedFinding,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExtractWorkflowResult {
    pub task_id: TaskId,
    pub processed: Vec<ExtractionCandidate>,
    pub skipped: Vec<ExtractionCandidate>,
    pub enqueued: Vec<ExtractionCandidate>,
    pub events: Vec<TaskEvent>,
}
