//! Application-level orchestration for SmartZip workflows.

pub mod container;
pub mod detect;
pub mod embedded;
pub mod embedded_zip;
pub mod history;
pub mod layout;
mod materialize;
pub mod name_score;

use async_trait::async_trait;
use futures_util::FutureExt;
use materialize::{
    CollisionAction, CollisionResolver, CommitPolicy, MaterializeRequest, OutputMaterializer,
};
use serde::{Deserialize, Serialize};
use smartzip_archive::{
    ArchiveBackend, ArchiveListing, ExtractArchiveRequest, ExtractionProgressCallback, ListRequest,
    NativeZipBackend, TestRequest,
};
use smartzip_core::{ArchiveFormat, EncodingMode, TaskEvent, TaskEventKind, TaskId};
use smartzip_passwords::{PasswordCandidate, PasswordCandidateRequest, PasswordService};
use smartzip_scanner::{
    Confidence, EmbeddedArchiveFinding, EmbeddedScanner, ScanMode, ScannerConfig,
};
use std::any::Any;
use std::collections::{HashSet, VecDeque};
use std::fs::File;
use std::future::Future;
use std::io::{Read, Seek, SeekFrom, Write};
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

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

struct ResolvedArchive<'a> {
    candidate: ExtractionCandidate,
    archive_path: PathBuf,
    _archive_temp: Option<tempfile::NamedTempFile>,
    sample_hash: Option<String>,
    sample_size: Option<i64>,
    known_hit: Option<crate::history::KnownFileHit>,
    encoding_mode: EncodingMode,
    reused_confirmed_encoding: bool,
    zip_encoding_assessment: Option<ZipEncodingAssessment>,
    recorder_name: Option<String>,
    history: Option<&'a dyn crate::history::TaskHistoryRecorder>,
}

#[derive(Debug, Clone)]
struct ArchiveAccessOutcome {
    password_id: Option<i64>,
    has_password: bool,
    // Resolved plaintext password and cancellation flag are populated by the
    // shared password flow but only consumed by the integrity-check backend,
    // which is split into 07-03-test-command-backend-split. Kept here so that
    // task can wire `test` in without reshaping this struct.
    #[allow(dead_code)]
    used_password: Option<String>,
    encoding_mode: EncodingMode,
    listing: Option<ArchiveListing>,
    encrypted: Option<bool>,
    events: Vec<TaskEvent>,
    #[allow(dead_code)]
    password_prompt_cancelled: bool,
}

pub struct SmartZipEngine {
    scanner: EmbeddedScanner,
    archive_recycler: ArchiveRecycleHandler,
    min_embedded_size_bytes: u64,
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

pub type TaskEventListener = Arc<dyn Fn(&TaskEvent) + Send + Sync>;

#[derive(Clone)]
struct EventSink {
    events: Arc<Mutex<Vec<TaskEvent>>>,
    listener: Option<TaskEventListener>,
}

impl EventSink {
    fn new(listener: Option<TaskEventListener>) -> Self {
        Self {
            events: Arc::new(Mutex::new(Vec::new())),
            listener,
        }
    }

    fn push(&self, event: TaskEvent) {
        if let Some(listener) = &self.listener {
            listener(&event);
        }
        self.events
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(event);
    }

    fn snapshot(&self) -> Vec<TaskEvent> {
        self.events
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

impl SmartZipEngine {
    pub fn new(scanner: EmbeddedScanner) -> Self {
        Self {
            scanner,
            archive_recycler: Arc::new(smartzip_platform::move_to_trash),
            min_embedded_size_bytes: smartzip_core::DEFAULT_MIN_EMBEDDED_FINDING_SIZE,
        }
    }

    /// Override how successfully processed nested archives are recycled.
    ///
    /// This is primarily useful for deterministic tests and platform hosts
    /// that provide their own recycle-bin integration.
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
        let task_id = TaskId::new();
        let mut events = vec![TaskEvent::started(task_id.clone())];

        let effective_config = default_root_scanner_config(&request.scanner);
        let scanner = if effective_config == *self.scanner.config() {
            None
        } else {
            Some(EmbeddedScanner::new(effective_config))
        };
        let scanner = scanner.as_ref().unwrap_or(&self.scanner);

        events.push(TaskEvent {
            task_id: task_id.clone(),
            kind: TaskEventKind::Progress(smartzip_core::TaskProgress::indeterminate(format!(
                "Scanning {}",
                request.path.display()
            ))),
        });

        let findings = scanner.scan_path(&request.path)?;
        for finding in &findings {
            events.push(TaskEvent {
                task_id: task_id.clone(),
                kind: TaskEventKind::EmbeddedArchiveFound {
                    offset: finding.offset,
                    size: finding.size,
                    format: finding.format.clone(),
                    confidence: confidence_score(finding.confidence),
                    description: finding.description.clone(),
                },
            });
        }

        events.push(TaskEvent {
            task_id: task_id.clone(),
            kind: TaskEventKind::Completed,
        });

        Ok(DetectResult {
            task_id,
            path: request.path,
            findings,
            events,
        })
    }

    pub async fn inspect_file_with_listener<B: ArchiveBackend>(
        &self,
        backend: &B,
        passwords: &PasswordService<'_>,
        request: InspectRequest,
        listener: Option<TaskEventListener>,
        history: Option<&dyn crate::history::TaskHistoryRecorder>,
    ) -> smartzip_core::Result<FileAwareDetectResult> {
        let task_id = TaskId::new();
        let events = EventSink::new(listener);
        events.push(TaskEvent::started(task_id.clone()));
        if let Some(recorder) = history {
            recorder.start_task(&task_id, "detect", None);
        }

        let candidate = resolve_root_candidate(
            &request.path,
            &request.scanner,
            self.min_embedded_size_bytes,
            &events,
            &task_id,
            None,
            None,
        )
        .await?;

        let mut detected_format = candidate.as_ref().and_then(|c| c.detected_format.clone());
        let mut status = "unreadable".to_string();
        let mut reason = None;
        let mut encrypted = None;
        let mut encoding = None;
        let mut encoding_confidence = None;
        let mut needs_password = false;
        let mut known_password = false;
        let mut known_encoding = None;
        let findings = scan_embedded_findings(
            &request.path,
            &request.scanner,
            self.min_embedded_size_bytes,
        );

        if let Some(kind) = detected_format.as_ref().and_then(|fmt| {
            (*fmt == ArchiveFormat::Zip)
                .then(|| {
                    ext_business_container_kind(&request.path)
                        .or_else(|| crate::container::classify_zip_path(&request.path))
                })
                .flatten()
        }) {
            events.push(TaskEvent {
                task_id: task_id.clone(),
                kind: TaskEventKind::BusinessContainerSkipped {
                    path: request.path.clone(),
                    kind: format!("{kind:?}"),
                },
            });
            reason = Some("business_container".to_string());
            detected_format = Some(ArchiveFormat::Zip);
        } else if let Some(candidate) = candidate {
            let resolved = prepare_resolved_archive(
                &candidate,
                EncodingMode::Auto,
                history,
                &events,
                &task_id,
            )
            .await?;
            known_password = resolved
                .known_hit
                .as_ref()
                .and_then(|hit| hit.password_id)
                .is_some();
            known_encoding = resolved
                .known_hit
                .as_ref()
                .and_then(|hit| hit.confirmed_encoding.clone());
            let probe = backend
                .probe(&resolved.archive_path)
                .await
                .map_err(|error| map_detect_error(error, &request.path))?;
            encrypted = probe.encrypted;
            detected_format = candidate.detected_format.clone().or(probe.format);
            if encrypted == Some(true) {
                needs_password = true;
            }
            if resolved.reused_confirmed_encoding {
                encoding = resolved.known_hit.and_then(|hit| hit.confirmed_encoding);
                encoding_confidence = Some(1.0);
            } else if let Some(assessment) = resolved.zip_encoding_assessment {
                encoding = Some(assessment.detected_raw.selected.clone());
                encoding_confidence = Some(assessment.detected_raw.confidence);
            }
            status = "detected".to_string();
            if let Some(recorder) = history {
                recorder.record_file_extraction(
                    &task_id,
                    crate::history::FileExtractionRow {
                        input_path: &candidate.path,
                        sample_hash: resolved.sample_hash.as_deref(),
                        file_size: resolved.sample_size,
                        offset: candidate.embedded_offset.map(|o| o as i64),
                        output_path: None,
                        has_password: false,
                        password_id: None,
                        status: "detected",
                        reason: None,
                        encoding: encoding.as_deref(),
                        encoding_corrected: resolved.reused_confirmed_encoding,
                        damaged_volumes_json: None,
                    },
                );
            }
        } else {
            reason = Some("not_found".to_string());
            if let Some(recorder) = history {
                recorder.record_file_extraction(
                    &task_id,
                    crate::history::FileExtractionRow {
                        input_path: &request.path,
                        sample_hash: None,
                        file_size: None,
                        offset: None,
                        output_path: None,
                        has_password: false,
                        password_id: None,
                        status: "unreadable",
                        reason: Some("not_found"),
                        encoding: None,
                        encoding_corrected: false,
                        damaged_volumes_json: None,
                    },
                );
            }
        }

        events.push(TaskEvent {
            task_id: task_id.clone(),
            kind: TaskEventKind::Completed,
        });
        let snapshot = events.snapshot();
        if let Some(recorder) = history {
            for event in &snapshot {
                recorder.record_event(&task_id, event);
            }
            recorder.finish(
                &task_id,
                crate::history::TaskOutcome {
                    status: if status == "detected" {
                        crate::history::TaskCompletionStatus::Completed
                    } else {
                        crate::history::TaskCompletionStatus::Failed
                    },
                    output_path: None,
                },
            );
        }

        Ok(FileAwareDetectResult {
            task_id,
            path: request.path,
            detected_format,
            embedded_count: findings.len(),
            embedded_findings: findings,
            encrypted,
            encoding,
            encoding_confidence,
            needs_password,
            known_password,
            known_encoding,
            status,
            reason,
            events: snapshot,
        })
    }

    pub async fn list_archive_with_listener_interactive<B: ArchiveBackend>(
        &self,
        backend: &B,
        passwords: &PasswordService<'_>,
        request: ListArchiveRequest,
        password_prompter: Option<&dyn InteractivePasswordPrompter>,
        encoding_prompter: Option<&dyn InteractiveEncodingPrompter>,
        listener: Option<TaskEventListener>,
        history: Option<&dyn crate::history::TaskHistoryRecorder>,
    ) -> smartzip_core::Result<ListArchiveResult> {
        let task_id = TaskId::new();
        let events = EventSink::new(listener);
        events.push(TaskEvent::started(task_id.clone()));
        if let Some(recorder) = history {
            recorder.start_task(&task_id, "list", None);
        }

        let candidate = resolve_root_candidate(
            &request.path,
            &request.scanner,
            self.min_embedded_size_bytes,
            &events,
            &task_id,
            None,
            None,
        )
        .await?
        .ok_or_else(|| smartzip_core::SmartZipError::UnsupportedFormat {
            path: request.path.clone(),
            format: None,
        })?;

        let resolved = prepare_resolved_archive(
            &candidate,
            request.encoding_mode.clone(),
            history,
            &events,
            &task_id,
        )
        .await?;
        let password_candidates = load_password_candidates(passwords, request.password_candidates.clone())?;
        let mut batch_passwords = Vec::new();
        let outcome = access_archive_with_password(
            backend,
            passwords,
            &resolved,
            &password_candidates,
            &mut batch_passwords,
            password_prompter,
            encoding_prompter,
            &events,
            &task_id,
            true,
        )
        .await?;

        for event in outcome.events {
            events.push(event);
        }

        let listing = outcome.listing.clone().ok_or_else(|| smartzip_core::SmartZipError::UnsupportedFormat {
            path: request.path.clone(),
            format: candidate.detected_format.as_ref().map(|f| f.as_str().to_string()),
        })?;

        if let Some(recorder) = history {
            recorder.record_file_extraction(
                &task_id,
                crate::history::FileExtractionRow {
                    input_path: &candidate.path,
                    sample_hash: resolved.sample_hash.as_deref(),
                    file_size: resolved.sample_size,
                    offset: candidate.embedded_offset.map(|o| o as i64),
                    output_path: None,
                    has_password: outcome.has_password,
                    password_id: outcome.password_id,
                    status: "detected",
                    reason: None,
                    encoding: Some(encoding_mode_label(&outcome.encoding_mode).as_str()),
                    encoding_corrected: resolved.reused_confirmed_encoding
                        || matches!(request.encoding_mode, EncodingMode::Override(_)),
                    damaged_volumes_json: None,
                },
            );
            if let (Some(hash), Some(size), EncodingMode::Override(encoding)) = (
                resolved.sample_hash.as_deref(),
                resolved.sample_size,
                &request.encoding_mode,
            ) {
                recorder.upsert_known_file_confirmed_encoding(crate::history::KnownFileEncodingUpsert {
                    sample_hash: hash,
                    size,
                    name: resolved.recorder_name.as_deref(),
                    offset: candidate.embedded_offset.map(|o| o as i64),
                    encoding,
                });
            }
            if let (Some(hash), Some(size), Some(password_id)) = (
                resolved.sample_hash.as_deref(),
                resolved.sample_size,
                outcome.password_id,
            ) {
                recorder.upsert_known_file_extract(crate::history::KnownFileUpsert {
                    sample_hash: hash,
                    size,
                    name: resolved.recorder_name.as_deref(),
                    offset: candidate.embedded_offset.map(|o| o as i64),
                    password_id: Some(password_id),
                });
            }
        }

        events.push(TaskEvent {
            task_id: task_id.clone(),
            kind: TaskEventKind::Completed,
        });
        let snapshot = events.snapshot();
        if let Some(recorder) = history {
            for event in &snapshot {
                recorder.record_event(&task_id, event);
            }
            recorder.finish(
                &task_id,
                crate::history::TaskOutcome {
                    status: crate::history::TaskCompletionStatus::Completed,
                    output_path: None,
                },
            );
        }

        Ok(ListArchiveResult {
            task_id,
            path: request.path,
            detected_format: candidate.detected_format,
            entries: listing.entries,
            encrypted: outcome.encrypted,
            encoding: encoding_mode_label(&outcome.encoding_mode),
            password_id: outcome.password_id,
            used_password: outcome.has_password,
            embedded_offset: candidate.embedded_offset,
            events: snapshot,
        })
    }

    pub async fn extract_recursive<B: ArchiveBackend>(
        &self,
        backend: &B,
        passwords: &PasswordService<'_>,
        request: ExtractWorkflowRequest,
        password_prompter: Option<&dyn InteractivePasswordPrompter>,
        output_prompter: Option<&dyn InteractiveOutputPrompter>,
    ) -> smartzip_core::Result<ExtractWorkflowResult> {
        self.extract_recursive_interactive(
            backend,
            passwords,
            request,
            password_prompter,
            output_prompter,
            None,
            None,
        )
        .await
    }

    pub async fn extract_recursive_interactive<B: ArchiveBackend>(
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

    pub async fn extract_recursive_with_listener<B: ArchiveBackend>(
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

    pub async fn extract_recursive_with_listener_interactive<B: ArchiveBackend>(
        &self,
        backend: &B,
        passwords: &PasswordService<'_>,
        request: ExtractWorkflowRequest,
        password_prompter: Option<&dyn InteractivePasswordPrompter>,
        output_prompter: Option<&dyn InteractiveOutputPrompter>,
        embedded_prompter: Option<&dyn InteractiveEmbeddedPrompter>,
        encoding_prompter: Option<&dyn InteractiveEncodingPrompter>,
        listener: Option<TaskEventListener>,
        history: Option<&dyn crate::history::TaskHistoryRecorder>,
    ) -> smartzip_core::Result<ExtractWorkflowResult> {
        let task_id = TaskId::new();
        let nested_scanner = if request.scanner == *self.scanner.config() {
            None
        } else {
            Some(EmbeddedScanner::new(request.scanner.clone()))
        };
        let nested_scanner = nested_scanner.as_ref().unwrap_or(&self.scanner);
        let root_scanner = EmbeddedScanner::new(full_root_scanner_config(&request.scanner));

        let events = EventSink::new(listener);
        events.push(TaskEvent::started(task_id.clone()));
        let mut queue = VecDeque::new();
        let mut seen = HashSet::new();
        let mut processed = Vec::new();
        let mut skipped = Vec::new();
        let mut enqueued = Vec::new();
        let output_materializer = OutputMaterializer::default();
        let root_input_total = request.inputs.len();
        let mut root_input_started = 0usize;
        let mut embedded_policy = embedded_policy_from_request(&request);
        embedded_policy.min_finding_size_bytes = self.min_embedded_size_bytes;
        let nested_embedded_enabled = !matches!(
            embedded_policy.mode,
            smartzip_core::EmbeddedScanMode::Ignore
        );
        let mut embedded_extract_all = false;

        for input in &request.inputs {
            let relative_path = archive_output_name(input);
            // Header-first: leave detected_format empty here. The main loop
            // resolves format from file header, embedded findings, and finally
            // the extension as a hint.
            queue.push_back(ExtractionCandidate {
                detected_format: None,
                path: input.clone(),
                relative_path,
                depth: 0,
                source: CandidateSource::RootInput,
                embedded_offset: None,
                embedded_size: None,
            });
        }

        // C6: Cache password candidates once before the extraction loop.
        let password_candidates = passwords
            .ranked_candidates(request.password_candidates.clone())
            .map_err(|error| smartzip_core::SmartZipError::BackendFailed {
                backend: "password-db".into(),
                exit_code: None,
                stderr: error.to_string(),
            })?;
        // Passwords entered interactively and accepted during this invocation.
        // Keep them in-memory as well as in SQLite so later files in the same
        // batch can use them without rebuilding the task-wide DB snapshot.
        let mut batch_passwords: Vec<PasswordCandidate> = Vec::new();

        let collision_resolver = output_prompter.map(|p| make_collision_resolver(p));

        // History: register the task up-front and accumulate metrics as the
        // loop runs. All history writes are best-effort — a repo error becomes
        // a Warning event through the recorder and never aborts extraction.
        if let Some(recorder) = history {
            recorder.start_extract(&task_id, Some(&request.output_dir));
        }
        // Dedup window lower bound: skip a re-extract only when the prior
        // success is at or after this instant. Hardcoded to 30 days for now;
        // TODO: read the window from config once the config layer lands.
        let dedup_window_start = {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            smartzip_db::timestamp::format_utc_seconds(now - 30 * 24 * 3600)
        };
        let mut hist_saw_failure = false;

        loop {
            let Some(mut candidate) = queue.pop_front() else {
                break;
            };
            let key = candidate_key(&candidate);
            let is_new = seen.insert(key);
            // Split the merged skip so each reason lands its own file_extractions
            // row (duplicate within this run / over recursion limit / a non-first
            // volume of a split set).
            if !is_new {
                record_skip(history, &task_id, &candidate, "duplicate");
                skipped.push(candidate);
                continue;
            }
            if candidate.depth > request.recursion_limit {
                record_skip(history, &task_id, &candidate, "recursion_limit");
                skipped.push(candidate);
                continue;
            }
            if !is_first_volume(&candidate.path) {
                record_skip(history, &task_id, &candidate, "not_first_volume");
                skipped.push(candidate);
                continue;
            }

            // Header-based detection first, then scanner confirmation
            let header_result = crate::detect::probe_file_header(&candidate.path);
            let _has_non_archive_header = {
                let mut file = match std::fs::File::open(&candidate.path) {
                    Ok(f) => f,
                    Err(_) => {
                        hist_saw_failure = true;
                        record_skip(history, &task_id, &candidate, "not_found");
                        skipped.push(candidate);
                        continue;
                    }
                };
                let mut buf = [0u8; 8192];
                let n = file.read(&mut buf).unwrap_or(0);
                crate::detect::detect_non_archive_header(&buf[..n])
            };

            // Root scans enqueue every embedded payload as its own candidate.
            // Confirm those candidates one at a time without rescanning the
            // carrier file.
            if candidate.source == CandidateSource::EmbeddedFinding
                && matches!(
                    embedded_policy.mode,
                    smartzip_core::EmbeddedScanMode::Auto | smartzip_core::EmbeddedScanMode::Ask
                )
                && !embedded_extract_all
            {
                let finding = EmbeddedArchiveFinding {
                    offset: candidate.embedded_offset.unwrap_or(0),
                    size: candidate.embedded_size,
                    format: candidate
                        .detected_format
                        .clone()
                        .unwrap_or_else(|| ArchiveFormat::Unknown("embedded".into())),
                    confidence: Confidence::High,
                    description: "queued embedded archive finding".into(),
                };
                let file_size = std::fs::metadata(&candidate.path)
                    .map(|metadata| metadata.len())
                    .unwrap_or(0);
                let decision = crate::embedded::select_embedded_action(
                    file_size,
                    std::slice::from_ref(&finding),
                    &embedded_policy,
                    false,
                );
                events.push(TaskEvent {
                    task_id: task_id.clone(),
                    kind: TaskEventKind::EmbeddedArchiveSelectionRequired {
                        path: candidate.path.clone(),
                        findings_count: 1,
                    },
                });
                let selection = if let Some(prompter) = embedded_prompter {
                    Some(prompter.prompt(&candidate.path, &decision).await)
                } else {
                    None
                };
                match selection {
                    Some(EmbeddedSelectionChoice::Extract) => {}
                    Some(EmbeddedSelectionChoice::ExtractAll) => embedded_extract_all = true,
                    Some(EmbeddedSelectionChoice::Skip) | None => {
                        record_skip(history, &task_id, &candidate, "not_found");
                        skipped.push(candidate);
                        continue;
                    }
                }
                events.push(TaskEvent {
                    task_id: task_id.clone(),
                    kind: TaskEventKind::EmbeddedArchiveSelected {
                        offset: finding.offset,
                        size: finding.size,
                        format: finding.format,
                        reason: "user confirmed queued embedded finding".into(),
                    },
                });
            }

            let scan_with = if candidate.source == CandidateSource::RootInput {
                &root_scanner
            } else {
                nested_scanner
            };
            let findings: Vec<_> = if should_scan_candidate_for_embedded(
                &candidate,
                &embedded_policy,
                nested_embedded_enabled,
                request.confirm_large_scan,
                &events,
                &task_id,
            ) {
                scan_with
                    .scan_path(&candidate.path)
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|finding| finding_meets_min_size(finding, &embedded_policy))
                    .collect()
            } else {
                Vec::new()
            };

            let root_findings = if matches!(
                embedded_policy.mode,
                smartzip_core::EmbeddedScanMode::Auto
                    | smartzip_core::EmbeddedScanMode::Ask
                    | smartzip_core::EmbeddedScanMode::Aggressive
                    | smartzip_core::EmbeddedScanMode::All
            ) {
                root_embedded_candidates(&candidate, &findings)
            } else {
                Vec::new()
            };
            if !root_findings.is_empty() {
                for embedded_candidate in root_findings {
                    if let (Some(offset), Some(format)) = (
                        embedded_candidate.embedded_offset,
                        embedded_candidate.detected_format.clone(),
                    ) {
                        events.push(TaskEvent {
                            task_id: task_id.clone(),
                            kind: TaskEventKind::EmbeddedArchiveFound {
                                offset,
                                size: embedded_candidate.embedded_size,
                                format,
                                confidence: confidence_score(Confidence::High),
                                description: "embedded archive queued from root scan".into(),
                            },
                        });
                    }
                    enqueued.push(embedded_candidate.clone());
                    queue.push_back(embedded_candidate);
                }
                continue;
            }

            // Use dominant selector for embedded findings
            if !findings.is_empty() {
                let ext_is_archive = crate::format_from_extension(&candidate.path).is_some();
                let file_size = std::fs::metadata(&candidate.path)
                    .map(|m| m.len())
                    .unwrap_or(0);
                let decision = crate::embedded::select_embedded_action(
                    file_size,
                    &findings,
                    &embedded_policy,
                    ext_is_archive,
                );

                match decision.action {
                    smartzip_core::DetectionAction::ExtractDirect => {
                        if let Some(idx) = decision.selected_index {
                            let f = &findings[idx];
                            candidate.detected_format = Some(f.format.clone());
                            candidate.embedded_offset = Some(f.offset);
                            candidate.embedded_size = f.size;
                        }
                    }
                    smartzip_core::DetectionAction::CarveAndExtract => {
                        if let Some(idx) = decision.selected_index {
                            let f = &findings[idx];
                            candidate.detected_format = Some(f.format.clone());
                            candidate.embedded_offset = Some(f.offset);
                            candidate.embedded_size = f.size;
                            events.push(TaskEvent {
                                task_id: task_id.clone(),
                                kind: TaskEventKind::EmbeddedArchiveSelected {
                                    offset: f.offset,
                                    size: f.size,
                                    format: f.format.clone(),
                                    reason: decision.reason.clone(),
                                },
                            });
                        }
                    }
                    smartzip_core::DetectionAction::AskUser => {
                        events.push(TaskEvent {
                            task_id: task_id.clone(),
                            kind: TaskEventKind::EmbeddedArchiveSelectionRequired {
                                path: candidate.path.clone(),
                                findings_count: findings.len(),
                            },
                        });
                        let selection = if embedded_extract_all {
                            Some(EmbeddedSelectionChoice::Extract)
                        } else if let Some(prompter) = embedded_prompter {
                            Some(prompter.prompt(&candidate.path, &decision).await)
                        } else {
                            None
                        };

                        match selection {
                            Some(choice) => match choice {
                                EmbeddedSelectionChoice::Extract => {
                                    if let Some(idx) = decision.selected_index {
                                        let f = &findings[idx];
                                        candidate.detected_format = Some(f.format.clone());
                                        candidate.embedded_offset = Some(f.offset);
                                        candidate.embedded_size = f.size;
                                        events.push(TaskEvent {
                                            task_id: task_id.clone(),
                                            kind: TaskEventKind::EmbeddedArchiveSelected {
                                                offset: f.offset,
                                                size: f.size,
                                                format: f.format.clone(),
                                                reason: decision.reason.clone(),
                                            },
                                        });
                                    }
                                }
                                EmbeddedSelectionChoice::ExtractAll => {
                                    embedded_extract_all = true;
                                    if let Some(idx) = decision.selected_index {
                                        let f = &findings[idx];
                                        candidate.detected_format = Some(f.format.clone());
                                        candidate.embedded_offset = Some(f.offset);
                                        candidate.embedded_size = f.size;
                                        events.push(TaskEvent {
                                            task_id: task_id.clone(),
                                            kind: TaskEventKind::EmbeddedArchiveSelected {
                                                offset: f.offset,
                                                size: f.size,
                                                format: f.format.clone(),
                                                reason: decision.reason.clone(),
                                            },
                                        });
                                    }
                                }
                                EmbeddedSelectionChoice::Skip => {
                                    record_skip(history, &task_id, &candidate, "not_found");
                                    skipped.push(candidate);
                                    continue;
                                }
                            },
                            None => {
                                record_skip(history, &task_id, &candidate, "not_found");
                                skipped.push(candidate);
                                continue;
                            }
                        }
                    }
                    _ => {
                        record_skip(history, &task_id, &candidate, "not_found");
                        skipped.push(candidate);
                        continue;
                    }
                }
            } else if candidate.detected_format.is_none() {
                // Header-first, extension as hint/fallback
                if let Some((fmt, offset)) = header_result {
                    candidate.detected_format = Some(fmt);
                    if offset > 0 {
                        candidate.embedded_offset = Some(offset);
                    }
                } else {
                    candidate.detected_format = crate::format_from_extension(&candidate.path);
                }
            }

            if candidate.detected_format.is_none() {
                record_skip(history, &task_id, &candidate, "not_found");
                skipped.push(candidate);
                continue;
            }

            // Business container filter for root inputs: nested candidates are
            // filtered in discover_nested_candidates, but root inputs (a .docx
            // dropped straight in, or a plain .zip whose contents match docx
            // structure) reach the main loop directly.
            if candidate.detected_format == Some(ArchiveFormat::Zip) {
                if let Some(kind) = ext_business_container_kind(&candidate.path)
                    .or_else(|| crate::container::classify_zip_path(&candidate.path))
                {
                    events.push(TaskEvent {
                        task_id: task_id.clone(),
                        kind: TaskEventKind::BusinessContainerSkipped {
                            path: candidate.path.clone(),
                            kind: format!("{kind:?}"),
                        },
                    });
                    record_skip(history, &task_id, &candidate, "business_container");
                    skipped.push(candidate);
                    continue;
                }
            }

            let archive_input = materialize_archive_input(&candidate)?;
            let archive_path = archive_input.path.clone();

            // File-grain history: identify this physical file by content
            // sampling so we can dedup, reuse a confirmed encoding, and reuse a
            // known password. Carve candidates hash their [offset, offset+size)
            // segment; a size-unknown carve yields no hash and skips dedup.
            let (sample_hash, sample_size) = match candidate.embedded_offset {
                Some(offset) if offset > 0 => smartzip_db::sample_hash::sample_hash_segment(
                    &candidate.path,
                    offset,
                    candidate.embedded_size,
                )
                .map(|(h, s)| (Some(h), Some(s as i64)))
                .unwrap_or((None, None)),
                _ => smartzip_db::sample_hash::sample_hash(&archive_path)
                    .map(|(h, s)| (Some(h), Some(s as i64)))
                    .unwrap_or((None, None)),
            };
            let known_hit = match (history, sample_hash.as_deref(), sample_size) {
                (Some(recorder), Some(hash), Some(size)) => recorder.lookup_known_file(hash, size),
                _ => None,
            };

            // Dedup: a prior successful extract inside the window means skip,
            // unless --force. Emit a hint event and log a skipped row.
            if !request.force {
                if let (Some(hash), Some(size), Some(hit)) =
                    (sample_hash.as_deref(), sample_size, known_hit.as_ref())
                {
                    if hit
                        .last_extract_at
                        .as_deref()
                        .is_some_and(|at| at >= dedup_window_start.as_str())
                    {
                        events.push(TaskEvent {
                            task_id: task_id.clone(),
                            kind: TaskEventKind::Warning {
                                message: format!(
                                    "skipping {} — already extracted within the dedup window (use --force to re-extract)",
                                    candidate.path.display()
                                ),
                            },
                        });
                        if let Some(recorder) = history {
                            recorder.record_file_extraction(
                                &task_id,
                                crate::history::FileExtractionRow {
                                    input_path: &candidate.path,
                                    sample_hash: Some(hash),
                                    file_size: Some(size),
                                    offset: candidate.embedded_offset.map(|o| o as i64),
                                    output_path: None,
                                    has_password: false,
                                    password_id: None,
                                    status: "skipped",
                                    reason: Some("duplicate"),
                                    encoding: None,
                                    encoding_corrected: false,
                                    damaged_volumes_json: None,
                                },
                            );
                        }
                        skipped.push(candidate);
                        continue;
                    }
                }
            }

            // Confirmed-encoding reuse: a user-confirmed encoding for this exact
            // file beats auto-detection, but never a command-line override.
            // Detect-time guesses are recomputed each run, so only a confirmed
            // encoding (written by the future `list` command) is reused here.
            let candidate_encoding_mode = match (
                &request.encoding_mode,
                known_hit
                    .as_ref()
                    .and_then(|h| h.confirmed_encoding.clone()),
            ) {
                (EncodingMode::Auto, Some(enc)) => EncodingMode::Override(enc),
                _ => request.encoding_mode.clone(),
            };
            // True when the encoding above came from a reused user-confirmed
            // choice; recorded on the file_extractions row as encoding_corrected.
            let reused_confirmed_encoding = request.encoding_mode == EncodingMode::Auto
                && known_hit
                    .as_ref()
                    .map(|h| h.confirmed_encoding.is_some())
                    .unwrap_or(false);

            // Password try order: command-line/manual > exact known-file hit >
            // passwords accepted earlier in this batch > empty/database
            // fallback. Values are deduplicated while preserving that order.
            let known_password = known_hit
                .as_ref()
                .and_then(|h| h.password_id)
                .and_then(|id| passwords.candidate_by_id(id).ok().flatten());
            let candidate_passwords = order_password_candidates(
                &password_candidates,
                known_password.as_ref(),
                &batch_passwords,
            );

            if candidate.source == CandidateSource::RootInput {
                root_input_started += 1;
                events.push(TaskEvent {
                    task_id: task_id.clone(),
                    kind: TaskEventKind::Progress(smartzip_core::TaskProgress::indeterminate(
                        format!(
                            "Processing input [{}/{}]: {}",
                            root_input_started,
                            root_input_total,
                            candidate.path.display()
                        ),
                    )),
                });
            } else {
                events.push(TaskEvent {
                    task_id: task_id.clone(),
                    kind: TaskEventKind::Progress(smartzip_core::TaskProgress::indeterminate(
                        format!(
                            "Processing nested archive at depth {}: {}",
                            candidate.depth,
                            candidate.path.display()
                        ),
                    )),
                });
            }

            events.push(TaskEvent {
                task_id: task_id.clone(),
                kind: TaskEventKind::Progress(smartzip_core::TaskProgress::indeterminate(format!(
                    "Extracting {} at depth {}",
                    candidate.path.display(),
                    candidate.depth
                ))),
            });

            // Skip auto-detection entirely when a user-confirmed encoding was
            // reused from known_files — that choice already beat auto once.
            let mut zip_encoding_assessment = None;
            if candidate_encoding_mode == EncodingMode::Auto
                && candidate.detected_format == Some(ArchiveFormat::Zip)
            {
                let native_zip = NativeZipBackend::new();
                if let Ok(probe) = native_zip.probe(&archive_path).await {
                    if probe.encrypted == Some(false) {
                        zip_encoding_assessment =
                            assess_zip_encoding(&native_zip, &archive_path, None).await;
                    }
                }
            }
            if let Some(assessment) = &zip_encoding_assessment {
                events.push(TaskEvent {
                    task_id: task_id.clone(),
                    kind: TaskEventKind::EncodingDetected(assessment.context.detected.clone()),
                });
            }

            let _key = candidate_key(&candidate);
            let output_dir = output_dir_for_candidate(&request.output_dir, &candidate);

            let mut extracted = false;
            let mut terminal_skip = false;
            let mut last_error = None;
            let mut saw_wrong_password = false;
            let mut password_prompt_cancelled = false;
            let mut actual_output_dir = output_dir.clone();
            // File-grain success state, recorded once after the try loop.
            let mut candidate_password_id: Option<i64> = None;
            let mut candidate_has_password = false;
            let mut candidate_encoding_used: Option<String> = None;
            let test_before_extract = backend
                .should_test_before_extract(&archive_path, candidate.detected_format.as_ref());
            let total_password_attempts = candidate_passwords.len();
            for password in &candidate_passwords {
                let pw_value = password_value(password);
                let attempt_index = password_attempt_index(password, &candidate_passwords);
                events.push(TaskEvent {
                    task_id: task_id.clone(),
                    kind: TaskEventKind::Progress(smartzip_core::TaskProgress::indeterminate(
                        format!(
                            "Trying password [{}/{}] ({}) for {}",
                            attempt_index,
                            total_password_attempts,
                            password_source_label(password),
                            candidate.path.display()
                        ),
                    )),
                });
                if test_before_extract {
                    match backend_call(
                        "archive-backend",
                        "test",
                        &archive_path,
                        backend.test(TestRequest {
                            archive: archive_path.clone(),
                            format: candidate.detected_format.clone(),
                            password: pw_value.clone(),
                            encoding: candidate_encoding_mode.clone(),
                        }),
                    )
                    .await
                    {
                        Ok(result) if result.ok => {
                            let matched_password_id =
                                passwords.record_success(password).ok().flatten();
                            events.push(TaskEvent {
                                task_id: task_id.clone(),
                                kind: TaskEventKind::Progress(
                                    smartzip_core::TaskProgress::indeterminate(format!(
                                        "Password accepted ({}) for {}",
                                        password_source_label(password),
                                        candidate.path.display()
                                    )),
                                ),
                            });
                            candidate_password_id = matched_password_id;
                            candidate_has_password =
                                pw_value.as_deref().map(|v| !v.is_empty()).unwrap_or(false);

                            if zip_encoding_assessment.is_none()
                                && candidate_encoding_mode == EncodingMode::Auto
                                && candidate.detected_format == Some(ArchiveFormat::Zip)
                            {
                                let native_zip = NativeZipBackend::new();
                                zip_encoding_assessment = assess_zip_encoding(
                                    &native_zip,
                                    &archive_path,
                                    pw_value.clone(),
                                )
                                .await;
                                if let Some(assessment) = &zip_encoding_assessment {
                                    events.push(TaskEvent {
                                        task_id: task_id.clone(),
                                        kind: TaskEventKind::EncodingDetected(
                                            assessment.context.detected.clone(),
                                        ),
                                    });
                                }
                            }

                            let encoding_to_use = resolve_encoding_mode(
                                &archive_path,
                                candidate_encoding_mode.clone(),
                                zip_encoding_assessment.as_ref(),
                                encoding_prompter,
                            )
                            .await?;
                            candidate_encoding_used = Some(encoding_mode_label(&encoding_to_use));
                            let extract_archive_path = archive_path.clone();
                            let extract_format = candidate.detected_format.clone();
                            let extract_password = pw_value.clone();
                            let extract_encoding = encoding_to_use.clone();
                            let extraction_progress = extraction_progress_callback(
                                events.clone(),
                                task_id.clone(),
                                candidate.path.clone(),
                            );
                            events.push(TaskEvent {
                                task_id: task_id.clone(),
                                kind: TaskEventKind::Progress(
                                    smartzip_core::TaskProgress::indeterminate(format!(
                                        "Extracting {} to {}",
                                        candidate.path.display(),
                                        output_dir.display()
                                    )),
                                ),
                            });

                            let extract_result = output_materializer
                                .materialize(
                                    MaterializeRequest {
                                        output_dir: output_dir.clone(),
                                        archive_path: candidate.path.clone(),
                                        commit_policy: CommitPolicy::FailIfExists,
                                        archive_stem: Some(
                                            archive_stem(&candidate.path)
                                                .to_string_lossy()
                                                .into_owned(),
                                        ),
                                        layout_policy: request.layout_policy,
                                        single_root_name_policy: request.single_root_name_policy,
                                    },
                                    |temp_output_dir| async move {
                                        backend_call(
                                            "archive-backend",
                                            "extract",
                                            &extract_archive_path,
                                            backend.extract_with_progress(
                                                ExtractArchiveRequest {
                                                    archive: extract_archive_path.clone(),
                                                    format: extract_format,
                                                    output_dir: temp_output_dir,
                                                    password: extract_password,
                                                    encoding: extract_encoding,
                                                },
                                                Some(extraction_progress),
                                            ),
                                        )
                                        .await
                                        .map(|_| ())
                                    },
                                    collision_resolver.as_ref(),
                                )
                                .await;

                            match extract_result {
                                Ok(result) => {
                                    if result.output_dir != output_dir {
                                        candidate.relative_path = output_relative_path_for(
                                            &request.output_dir,
                                            &result.output_dir,
                                        );
                                    }
                                    actual_output_dir = result.output_dir;
                                    extracted = true;
                                    break;
                                }
                                Err(failure) => {
                                    if failure.kind
                                        == materialize::MaterializeFailureKind::CollisionSkipped
                                    {
                                        terminal_skip = true;
                                        break;
                                    }
                                    if let Some(temp_dir) = &failure.preserved_temp_dir {
                                        eprintln!(
                                            "preserved failed extraction temp dir: {}",
                                            temp_dir.display()
                                        );
                                    }
                                    last_error = Some(failure.error);
                                }
                            }
                        }
                        Ok(_) => {}
                        Err(error) => {
                            if matches!(&error, smartzip_core::SmartZipError::WrongPassword { .. })
                            {
                                saw_wrong_password = true;
                                let _ = passwords.record_failure(password);
                            } else {
                                last_error = Some(error);
                            }
                        }
                    }
                } else {
                    events.push(TaskEvent {
                        task_id: task_id.clone(),
                        kind: TaskEventKind::Progress(smartzip_core::TaskProgress::indeterminate(
                            format!(
                                "Attempting direct extract with password [{}/{}] ({}) for {}",
                                attempt_index,
                                total_password_attempts,
                                password_source_label(password),
                                candidate.path.display()
                            ),
                        )),
                    });
                    let extract_encoding_mode = resolve_encoding_mode(
                        &archive_path,
                        candidate_encoding_mode.clone(),
                        zip_encoding_assessment.as_ref(),
                        encoding_prompter,
                    )
                    .await?;
                    candidate_encoding_used = Some(encoding_mode_label(&extract_encoding_mode));
                    let extract_archive_path = archive_path.clone();
                    let extract_format = candidate.detected_format.clone();
                    let extract_password = pw_value.clone();
                    let extract_encoding = extract_encoding_mode.clone();
                    let extraction_progress = extraction_progress_callback(
                        events.clone(),
                        task_id.clone(),
                        candidate.path.clone(),
                    );
                    events.push(TaskEvent {
                        task_id: task_id.clone(),
                        kind: TaskEventKind::Progress(smartzip_core::TaskProgress::indeterminate(
                            format!(
                                "Extracting {} to {}",
                                candidate.path.display(),
                                output_dir.display()
                            ),
                        )),
                    });
                    let extract_result = output_materializer
                        .materialize(
                            MaterializeRequest {
                                output_dir: output_dir.clone(),
                                archive_path: candidate.path.clone(),
                                commit_policy: CommitPolicy::FailIfExists,
                                archive_stem: Some(
                                    archive_stem(&candidate.path).to_string_lossy().into_owned(),
                                ),
                                layout_policy: request.layout_policy,
                                single_root_name_policy: request.single_root_name_policy,
                            },
                            |temp_output_dir| async move {
                                backend_call(
                                    "archive-backend",
                                    "extract",
                                    &extract_archive_path,
                                    backend.extract_with_progress(
                                        ExtractArchiveRequest {
                                            archive: extract_archive_path.clone(),
                                            format: extract_format,
                                            output_dir: temp_output_dir,
                                            password: extract_password,
                                            encoding: extract_encoding,
                                        },
                                        Some(extraction_progress),
                                    ),
                                )
                                .await
                                .map(|_| ())
                            },
                            collision_resolver.as_ref(),
                        )
                        .await;

                    match extract_result {
                        Ok(result) => {
                            let matched_password_id =
                                passwords.record_success(password).ok().flatten();
                            events.push(TaskEvent {
                                task_id: task_id.clone(),
                                kind: TaskEventKind::Progress(
                                    smartzip_core::TaskProgress::indeterminate(format!(
                                        "Password accepted ({}) for {}",
                                        password_source_label(password),
                                        candidate.path.display()
                                    )),
                                ),
                            });
                            candidate_password_id = matched_password_id;
                            candidate_has_password =
                                pw_value.as_deref().map(|v| !v.is_empty()).unwrap_or(false);
                            if result.output_dir != output_dir {
                                candidate.relative_path = output_relative_path_for(
                                    &request.output_dir,
                                    &result.output_dir,
                                );
                            }
                            actual_output_dir = result.output_dir;
                            extracted = true;
                            break;
                        }
                        Err(failure) => {
                            if failure.kind == materialize::MaterializeFailureKind::CollisionSkipped
                            {
                                terminal_skip = true;
                                break;
                            }
                            if let Some(temp_dir) = &failure.preserved_temp_dir {
                                eprintln!(
                                    "preserved failed extraction temp dir: {}",
                                    temp_dir.display()
                                );
                            }
                            if matches!(
                                &failure.error,
                                smartzip_core::SmartZipError::WrongPassword { .. }
                            ) {
                                saw_wrong_password = true;
                                let _ = passwords.record_failure(password);
                            } else {
                                last_error = Some(failure.error);
                            }
                        }
                    }
                }
            }

            if !extracted && !terminal_skip {
                // Interactive fallback: prompt the user for a password. Use test->extract
                // and reuse the materialized archive path (carved temp when embedded).
                if let Some(prompter) = password_prompter {
                    events.push(TaskEvent {
                        task_id: task_id.clone(),
                        kind: TaskEventKind::Progress(smartzip_core::TaskProgress::indeterminate(
                            format!("Prompting for password: {}", candidate.path.display()),
                        )),
                    });
                    let interactive_password = prompter.prompt(&candidate.path).await;
                    password_prompt_cancelled = interactive_password
                        .as_deref()
                        .map(str::trim)
                        .is_none_or(str::is_empty);
                    if let Some(interactive_pw) = interactive_password {
                        let pw = interactive_pw.trim().to_string();
                        if !pw.is_empty() {
                            if test_before_extract {
                                match backend_call(
                                    "archive-backend",
                                    "test",
                                    &archive_path,
                                    backend.test(TestRequest {
                                        archive: archive_path.clone(),
                                        format: candidate.detected_format.clone(),
                                        password: Some(pw.clone()),
                                        encoding: candidate_encoding_mode.clone(),
                                    }),
                                )
                                .await
                                {
                                    Ok(result) if result.ok => {
                                        events.push(TaskEvent {
                                            task_id: task_id.clone(),
                                            kind: TaskEventKind::Progress(
                                                smartzip_core::TaskProgress::indeterminate(
                                                    format!(
                                                        "Interactive password accepted for {}",
                                                        candidate.path.display()
                                                    ),
                                                ),
                                            ),
                                        });
                                        if zip_encoding_assessment.is_none()
                                            && candidate_encoding_mode == EncodingMode::Auto
                                            && candidate.detected_format == Some(ArchiveFormat::Zip)
                                        {
                                            let native_zip = NativeZipBackend::new();
                                            zip_encoding_assessment = assess_zip_encoding(
                                                &native_zip,
                                                &archive_path,
                                                Some(pw.clone()),
                                            )
                                            .await;
                                            if let Some(assessment) = &zip_encoding_assessment {
                                                events.push(TaskEvent {
                                                    task_id: task_id.clone(),
                                                    kind: TaskEventKind::EncodingDetected(
                                                        assessment.context.detected.clone(),
                                                    ),
                                                });
                                            }
                                        }
                                        let encoding_to_use = resolve_encoding_mode(
                                            &archive_path,
                                            candidate_encoding_mode.clone(),
                                            zip_encoding_assessment.as_ref(),
                                            encoding_prompter,
                                        )
                                        .await?;
                                        candidate_encoding_used =
                                            Some(encoding_mode_label(&encoding_to_use));
                                        let extract_archive_path = archive_path.clone();
                                        let extract_format = candidate.detected_format.clone();
                                        let extract_password = pw.clone();
                                        let extract_encoding = encoding_to_use.clone();
                                        let extraction_progress = extraction_progress_callback(
                                            events.clone(),
                                            task_id.clone(),
                                            candidate.path.clone(),
                                        );
                                        events.push(TaskEvent {
                                            task_id: task_id.clone(),
                                            kind: TaskEventKind::Progress(
                                                smartzip_core::TaskProgress::indeterminate(
                                                    format!(
                                                        "Extracting {} to {}",
                                                        candidate.path.display(),
                                                        output_dir.display()
                                                    ),
                                                ),
                                            ),
                                        });
                                        let extract_result = output_materializer
                                            .materialize(
                                                MaterializeRequest {
                                                    output_dir: output_dir.clone(),
                                                    archive_path: candidate.path.clone(),
                                                    commit_policy: CommitPolicy::FailIfExists,
                                                    archive_stem: Some(
                                                        archive_stem(&candidate.path)
                                                            .to_string_lossy()
                                                            .into_owned(),
                                                    ),
                                                    layout_policy: request.layout_policy,
                                                    single_root_name_policy: request
                                                        .single_root_name_policy,
                                                },
                                                |temp_output_dir| async move {
                                                    backend_call(
                                                        "archive-backend",
                                                        "extract",
                                                        &extract_archive_path,
                                                        backend.extract_with_progress(
                                                            ExtractArchiveRequest {
                                                                archive: extract_archive_path
                                                                    .clone(),
                                                                format: extract_format,
                                                                output_dir: temp_output_dir,
                                                                password: Some(extract_password),
                                                                encoding: extract_encoding,
                                                            },
                                                            Some(extraction_progress),
                                                        ),
                                                    )
                                                    .await
                                                    .map(|_| ())
                                                },
                                                output_prompter
                                                    .map(|p| make_collision_resolver(p))
                                                    .as_ref(),
                                            )
                                            .await;

                                        match extract_result {
                                            Ok(result) => {
                                                if result.output_dir != output_dir {
                                                    candidate.relative_path =
                                                        output_relative_path_for(
                                                            &request.output_dir,
                                                            &result.output_dir,
                                                        );
                                                }
                                                actual_output_dir = result.output_dir;
                                                let accepted = PasswordCandidate {
                                                    id: None,
                                                    value: pw.clone(),
                                                    source:
                                                        smartzip_passwords::PasswordSource::Manual,
                                                };
                                                candidate_password_id = passwords
                                                    .record_success(&accepted)
                                                    .ok()
                                                    .flatten();
                                                candidate_has_password = true;
                                                remember_batch_password(
                                                    &mut batch_passwords,
                                                    &accepted.value,
                                                    candidate_password_id,
                                                );
                                                extracted = true;
                                            }
                                            Err(failure) => {
                                                if let Some(temp_dir) = &failure.preserved_temp_dir
                                                {
                                                    eprintln!(
                                                        "preserved failed extraction temp dir: {}",
                                                        temp_dir.display()
                                                    );
                                                }
                                                eprintln!(
                                                    "Interactive extract failed for {}: {}",
                                                    archive_path.display(),
                                                    failure.error
                                                );
                                            }
                                        }
                                    }
                                    Ok(_) => {
                                        saw_wrong_password = true;
                                        eprintln!(
                                            "Interactive password did not validate for {}",
                                            archive_path.display()
                                        );
                                    }
                                    Err(error) => {
                                        if matches!(
                                            &error,
                                            smartzip_core::SmartZipError::WrongPassword { .. }
                                        ) {
                                            saw_wrong_password = true;
                                        }
                                        eprintln!(
                                            "Interactive password test failed for {}: {}",
                                            archive_path.display(),
                                            error
                                        );
                                    }
                                }
                            } else {
                                events.push(TaskEvent {
                                    task_id: task_id.clone(),
                                    kind: TaskEventKind::Progress(
                                        smartzip_core::TaskProgress::indeterminate(format!(
                                            "Attempting direct extract with interactive password for {}",
                                            candidate.path.display()
                                        )),
                                    ),
                                });
                                let extract_archive_path = archive_path.clone();
                                let extract_format = candidate.detected_format.clone();
                                let extract_password = pw.clone();
                                let extract_encoding = resolve_encoding_mode(
                                    &archive_path,
                                    candidate_encoding_mode.clone(),
                                    zip_encoding_assessment.as_ref(),
                                    encoding_prompter,
                                )
                                .await?;
                                candidate_encoding_used =
                                    Some(encoding_mode_label(&extract_encoding));
                                let extraction_progress = extraction_progress_callback(
                                    events.clone(),
                                    task_id.clone(),
                                    candidate.path.clone(),
                                );
                                events.push(TaskEvent {
                                    task_id: task_id.clone(),
                                    kind: TaskEventKind::Progress(
                                        smartzip_core::TaskProgress::indeterminate(format!(
                                            "Extracting {} to {}",
                                            candidate.path.display(),
                                            output_dir.display()
                                        )),
                                    ),
                                });
                                let extract_result = output_materializer
                                    .materialize(
                                        MaterializeRequest {
                                            output_dir: output_dir.clone(),
                                            archive_path: candidate.path.clone(),
                                            commit_policy: CommitPolicy::FailIfExists,
                                            archive_stem: Some(
                                                archive_stem(&candidate.path)
                                                    .to_string_lossy()
                                                    .into_owned(),
                                            ),
                                            layout_policy: request.layout_policy,
                                            single_root_name_policy: request
                                                .single_root_name_policy,
                                        },
                                        |temp_output_dir| async move {
                                            backend_call(
                                                "archive-backend",
                                                "extract",
                                                &extract_archive_path,
                                                backend.extract_with_progress(
                                                    ExtractArchiveRequest {
                                                        archive: extract_archive_path.clone(),
                                                        format: extract_format,
                                                        output_dir: temp_output_dir,
                                                        password: Some(extract_password),
                                                        encoding: extract_encoding,
                                                    },
                                                    Some(extraction_progress),
                                                ),
                                            )
                                            .await
                                            .map(|_| ())
                                        },
                                        output_prompter
                                            .map(|p| make_collision_resolver(p))
                                            .as_ref(),
                                    )
                                    .await;

                                match extract_result {
                                    Ok(result) => {
                                        if result.output_dir != output_dir {
                                            candidate.relative_path = output_relative_path_for(
                                                &request.output_dir,
                                                &result.output_dir,
                                            );
                                        }
                                        actual_output_dir = result.output_dir;
                                        events.push(TaskEvent {
                                            task_id: task_id.clone(),
                                            kind: TaskEventKind::Progress(
                                                smartzip_core::TaskProgress::indeterminate(
                                                    format!(
                                                        "Interactive password accepted for {}",
                                                        candidate.path.display()
                                                    ),
                                                ),
                                            ),
                                        });
                                        let accepted = PasswordCandidate {
                                            id: None,
                                            value: pw.clone(),
                                            source: smartzip_passwords::PasswordSource::Manual,
                                        };
                                        candidate_password_id =
                                            passwords.record_success(&accepted).ok().flatten();
                                        candidate_has_password = true;
                                        remember_batch_password(
                                            &mut batch_passwords,
                                            &accepted.value,
                                            candidate_password_id,
                                        );
                                        extracted = true;
                                    }
                                    Err(failure) => {
                                        if matches!(
                                            &failure.error,
                                            smartzip_core::SmartZipError::WrongPassword { .. }
                                        ) {
                                            saw_wrong_password = true;
                                            eprintln!(
                                                "Interactive password did not validate for {}",
                                                archive_path.display()
                                            );
                                        } else {
                                            if let Some(temp_dir) = &failure.preserved_temp_dir {
                                                eprintln!(
                                                    "preserved failed extraction temp dir: {}",
                                                    temp_dir.display()
                                                );
                                            }
                                            eprintln!(
                                                "Interactive extract failed for {}: {}",
                                                archive_path.display(),
                                                failure.error
                                            );
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            if !extracted && !terminal_skip {
                if password_prompt_cancelled {
                    if let Some(recorder) = history {
                        recorder.record_file_extraction(
                            &task_id,
                            crate::history::FileExtractionRow {
                                input_path: &candidate.path,
                                sample_hash: sample_hash.as_deref(),
                                file_size: sample_size,
                                offset: candidate.embedded_offset.map(|o| o as i64),
                                output_path: None,
                                has_password: false,
                                password_id: None,
                                status: "skipped",
                                reason: Some("password_required"),
                                encoding: candidate_encoding_used.as_deref(),
                                encoding_corrected: reused_confirmed_encoding
                                    || matches!(request.encoding_mode, EncodingMode::Override(_)),
                                damaged_volumes_json: None,
                            },
                        );
                    }
                } else if let Some(error) = last_error.or_else(|| {
                    saw_wrong_password.then(|| smartzip_core::SmartZipError::WrongPassword {
                        path: candidate.path.clone(),
                    })
                }) {
                    hist_saw_failure = true;
                    // File-grain failure: classify the reason from the error so
                    // `history files --reason` can filter later.
                    let reason = match &error {
                        smartzip_core::SmartZipError::WrongPassword { .. }
                        | smartzip_core::SmartZipError::PasswordRequired { .. } => "wrong_password",
                        smartzip_core::SmartZipError::Io { .. } => "not_found",
                        _ => "corrupt",
                    };
                    if let Some(recorder) = history {
                        recorder.record_file_extraction(
                            &task_id,
                            crate::history::FileExtractionRow {
                                input_path: &candidate.path,
                                sample_hash: sample_hash.as_deref(),
                                file_size: sample_size,
                                offset: candidate.embedded_offset.map(|o| o as i64),
                                output_path: None,
                                has_password: candidate_has_password,
                                password_id: candidate_password_id,
                                status: "failed",
                                reason: Some(reason),
                                encoding: candidate_encoding_used.as_deref(),
                                encoding_corrected: reused_confirmed_encoding
                                    || matches!(request.encoding_mode, EncodingMode::Override(_)),
                                damaged_volumes_json: None,
                            },
                        );
                    }
                    let event = TaskEvent::failed(task_id.clone(), &error);
                    events.push(event);
                } else if let Some(recorder) = history {
                    // No error and not extracted: candidates were tried but none
                    // opened it (e.g. needed a password we never got). Record a
                    // skip with `password_required` rather than a failure.
                    recorder.record_file_extraction(
                        &task_id,
                        crate::history::FileExtractionRow {
                            input_path: &candidate.path,
                            sample_hash: sample_hash.as_deref(),
                            file_size: sample_size,
                            offset: candidate.embedded_offset.map(|o| o as i64),
                            output_path: None,
                            has_password: candidate_has_password,
                            password_id: candidate_password_id,
                            status: "skipped",
                            reason: Some("password_required"),
                            encoding: candidate_encoding_used.as_deref(),
                            encoding_corrected: reused_confirmed_encoding
                                || matches!(request.encoding_mode, EncodingMode::Override(_)),
                            damaged_volumes_json: None,
                        },
                    );
                }
            }
            if terminal_skip {
                record_skip(history, &task_id, &candidate, "target_exists");
                skipped.push(candidate);
                continue;
            }
            if !extracted {
                skipped.push(candidate);
                continue;
            }

            let output_event = TaskEvent {
                task_id: task_id.clone(),
                kind: TaskEventKind::OutputCreated {
                    path: actual_output_dir.clone(),
                },
            };
            events.push(output_event);
            // File-grain success record: one file_extractions row for this
            // extracted candidate, and a known_files upsert so future runs can
            // dedup and reuse its password.
            if let Some(recorder) = history {
                recorder.record_file_extraction(
                    &task_id,
                    crate::history::FileExtractionRow {
                        input_path: &candidate.path,
                        sample_hash: sample_hash.as_deref(),
                        file_size: sample_size,
                        offset: candidate.embedded_offset.map(|o| o as i64),
                        output_path: Some(&actual_output_dir),
                        has_password: candidate_has_password,
                        password_id: candidate_password_id,
                        status: "extracted",
                        reason: None,
                        encoding: candidate_encoding_used.as_deref(),
                        encoding_corrected: reused_confirmed_encoding
                            || matches!(request.encoding_mode, EncodingMode::Override(_)),
                        damaged_volumes_json: None,
                    },
                );
                if let (Some(hash), Some(size)) = (sample_hash.as_deref(), sample_size) {
                    let name = candidate
                        .path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned());
                    recorder.upsert_known_file_extract(crate::history::KnownFileUpsert {
                        sample_hash: hash,
                        size,
                        name: name.as_deref(),
                        offset: candidate.embedded_offset.map(|o| o as i64),
                        password_id: candidate_password_id,
                    });
                    if let EncodingMode::Override(encoding) = &request.encoding_mode {
                        recorder.upsert_known_file_confirmed_encoding(
                            crate::history::KnownFileEncodingUpsert {
                                sample_hash: hash,
                                size,
                                name: name.as_deref(),
                                offset: candidate.embedded_offset.map(|o| o as i64),
                                encoding,
                            },
                        );
                    }
                }
            }

            processed.push(candidate.clone());
            let output_relative_path = candidate_output_relative_path(&candidate);
            let nested_candidates = discover_nested_candidates(
                nested_scanner,
                &actual_output_dir,
                candidate.depth + 1,
                &output_relative_path,
                &embedded_policy,
                nested_embedded_enabled,
            );
            for nested in nested_candidates {
                enqueued.push(nested.clone());
                queue.push_back(nested);
            }

            if let Some(path) = recyclable_nested_archive_path(&candidate, &request.output_dir) {
                if let Err(error) =
                    recycle_archive(self.archive_recycler.clone(), path.clone()).await
                {
                    events.push(TaskEvent {
                        task_id: task_id.clone(),
                        kind: TaskEventKind::Warning {
                            message: format!(
                                "failed to move processed nested archive {} to trash: {}",
                                path.display(),
                                error
                            ),
                        },
                    });
                }
            }
        }

        events.push(TaskEvent {
            task_id: task_id.clone(),
            kind: TaskEventKind::Completed,
        });

        let snapshot = events.snapshot();

        // History: replay the full event timeline into task_events, then close
        // out the task row. Per-file detail (encoding, password, embedded
        // findings) now lives in file_extractions rows written inline above;
        // this final pass only handles task_events + the slim task finish.
        if let Some(recorder) = history {
            for event in &snapshot {
                recorder.record_event(&task_id, event);
            }
            let status = if processed.is_empty() {
                crate::history::TaskCompletionStatus::Failed
            } else if hist_saw_failure || !skipped.is_empty() {
                crate::history::TaskCompletionStatus::Partial
            } else {
                crate::history::TaskCompletionStatus::Completed
            };
            recorder.finish(
                &task_id,
                crate::history::TaskOutcome {
                    status,
                    output_path: Some(&request.output_dir),
                },
            );
        }

        Ok(ExtractWorkflowResult {
            task_id,
            processed,
            skipped,
            enqueued,
            events: snapshot,
        })
    }
}

impl Default for SmartZipEngine {
    fn default() -> Self {
        Self::new(EmbeddedScanner::default())
    }
}

fn scan_embedded_findings(
    path: &Path,
    scanner: &ScannerConfig,
    min_embedded_size_bytes: u64,
) -> Vec<EmbeddedArchiveFinding> {
    let scanner = EmbeddedScanner::new(full_root_scanner_config(scanner));
    let mut policy = smartzip_core::EmbeddedScanPolicy::default();
    policy.min_finding_size_bytes = min_embedded_size_bytes;
    scanner
        .scan_path(path)
        .unwrap_or_default()
        .into_iter()
        .filter(|finding| finding_meets_min_size(finding, &policy))
        .collect()
}

async fn resolve_root_candidate(
    path: &Path,
    scanner: &ScannerConfig,
    min_embedded_size_bytes: u64,
    events: &EventSink,
    task_id: &TaskId,
    embedded_prompter: Option<&dyn InteractiveEmbeddedPrompter>,
    embedded_extract_all: Option<&mut bool>,
) -> smartzip_core::Result<Option<ExtractionCandidate>> {
    let mut candidate = ExtractionCandidate {
        detected_format: None,
        path: path.to_path_buf(),
        relative_path: archive_output_name(path),
        depth: 0,
        source: CandidateSource::RootInput,
        embedded_offset: None,
        embedded_size: None,
    };

    let header_result = crate::detect::probe_file_header(&candidate.path);
    let findings = scan_embedded_findings(&candidate.path, scanner, min_embedded_size_bytes);
    if !findings.is_empty() {
        let mut policy = smartzip_core::EmbeddedScanPolicy::default();
        policy.min_finding_size_bytes = min_embedded_size_bytes;
        let ext_is_archive = crate::format_from_extension(&candidate.path).is_some();
        let file_size = std::fs::metadata(&candidate.path)
            .map(|m| m.len())
            .unwrap_or(0);
        let decision = crate::embedded::select_embedded_action(
            file_size,
            &findings,
            &policy,
            ext_is_archive,
        );
        match decision.action {
            smartzip_core::DetectionAction::ExtractDirect
            | smartzip_core::DetectionAction::CarveAndExtract => {
                if let Some(idx) = decision.selected_index {
                    let finding = &findings[idx];
                    candidate.detected_format = Some(finding.format.clone());
                    candidate.embedded_offset = Some(finding.offset);
                    candidate.embedded_size = finding.size;
                    if matches!(decision.action, smartzip_core::DetectionAction::CarveAndExtract) {
                        events.push(TaskEvent {
                            task_id: task_id.clone(),
                            kind: TaskEventKind::EmbeddedArchiveSelected {
                                offset: finding.offset,
                                size: finding.size,
                                format: finding.format.clone(),
                                reason: decision.reason,
                            },
                        });
                    }
                }
            }
            smartzip_core::DetectionAction::AskUser => {
                events.push(TaskEvent {
                    task_id: task_id.clone(),
                    kind: TaskEventKind::EmbeddedArchiveSelectionRequired {
                        path: candidate.path.clone(),
                        findings_count: findings.len(),
                    },
                });
                let mut extract_all = false;
                let selection = if let Some(prompter) = embedded_prompter {
                    Some(prompter.prompt(&candidate.path, &decision).await)
                } else {
                    None
                };
                match selection {
                    Some(EmbeddedSelectionChoice::Extract) => {}
                    Some(EmbeddedSelectionChoice::ExtractAll) => extract_all = true,
                    Some(EmbeddedSelectionChoice::Skip) | None => return Ok(None),
                }
                if let Some(flag) = embedded_extract_all {
                    *flag = extract_all;
                }
                if let Some(idx) = decision.selected_index {
                    let finding = &findings[idx];
                    candidate.detected_format = Some(finding.format.clone());
                    candidate.embedded_offset = Some(finding.offset);
                    candidate.embedded_size = finding.size;
                }
            }
            _ => return Ok(None),
        }
    } else if candidate.detected_format.is_none() {
        if let Some((fmt, offset)) = header_result {
            candidate.detected_format = Some(fmt);
            if offset > 0 {
                candidate.embedded_offset = Some(offset);
            }
        } else {
            candidate.detected_format = crate::format_from_extension(&candidate.path);
        }
    }

    if candidate.detected_format.is_none() {
        return Ok(None);
    }
    if candidate.detected_format == Some(ArchiveFormat::Zip)
        && ext_business_container_kind(&candidate.path)
            .or_else(|| crate::container::classify_zip_path(&candidate.path))
            .is_some()
    {
        return Ok(None);
    }
    Ok(Some(candidate))
}

async fn prepare_resolved_archive<'a>(
    candidate: &ExtractionCandidate,
    requested_encoding: EncodingMode,
    history: Option<&'a dyn crate::history::TaskHistoryRecorder>,
    events: &EventSink,
    task_id: &TaskId,
) -> smartzip_core::Result<ResolvedArchive<'a>> {
    let archive_input = materialize_archive_input(candidate)?;
    let archive_path = archive_input.path.clone();
    let (sample_hash, sample_size) = match candidate.embedded_offset {
        Some(offset) if offset > 0 => smartzip_db::sample_hash::sample_hash_segment(
            &candidate.path,
            offset,
            candidate.embedded_size,
        )
        .map(|(h, s)| (Some(h), Some(s as i64)))
        .unwrap_or((None, None)),
        _ => smartzip_db::sample_hash::sample_hash(&archive_path)
            .map(|(h, s)| (Some(h), Some(s as i64)))
            .unwrap_or((None, None)),
    };
    let known_hit = match (history, sample_hash.as_deref(), sample_size) {
        (Some(recorder), Some(hash), Some(size)) => recorder.lookup_known_file(hash, size),
        _ => None,
    };
    let encoding_mode = match (
        &requested_encoding,
        known_hit
            .as_ref()
            .and_then(|hit| hit.confirmed_encoding.clone()),
    ) {
        (EncodingMode::Auto, Some(enc)) => EncodingMode::Override(enc),
        _ => requested_encoding.clone(),
    };
    let reused_confirmed_encoding = requested_encoding == EncodingMode::Auto
        && known_hit
            .as_ref()
            .map(|hit| hit.confirmed_encoding.is_some())
            .unwrap_or(false);
    let recorder_name = candidate
        .path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned());
    let mut zip_encoding_assessment = None;
    if encoding_mode == EncodingMode::Auto && candidate.detected_format == Some(ArchiveFormat::Zip) {
        let native_zip = NativeZipBackend::new();
        if let Ok(probe) = native_zip.probe(&archive_path).await {
            if probe.encrypted == Some(false) {
                zip_encoding_assessment = assess_zip_encoding(&native_zip, &archive_path, None).await;
            }
        }
    }
    if let Some(assessment) = &zip_encoding_assessment {
        events.push(TaskEvent {
            task_id: task_id.clone(),
            kind: TaskEventKind::EncodingDetected(assessment.context.detected.clone()),
        });
    }
    Ok(ResolvedArchive {
        candidate: candidate.clone(),
        archive_path,
        _archive_temp: archive_input._temp,
        sample_hash,
        sample_size,
        known_hit,
        encoding_mode,
        reused_confirmed_encoding,
        zip_encoding_assessment,
        recorder_name,
        history,
    })
}

fn load_password_candidates(
    passwords: &PasswordService<'_>,
    request: PasswordCandidateRequest,
) -> smartzip_core::Result<Vec<PasswordCandidate>> {
    passwords
        .ranked_candidates(request)
        .map_err(|error| smartzip_core::SmartZipError::BackendFailed {
            backend: "password-db".into(),
            exit_code: None,
            stderr: error.to_string(),
        })
}

async fn access_archive_with_password<B: ArchiveBackend>(
    backend: &B,
    passwords: &PasswordService<'_>,
    resolved: &ResolvedArchive<'_>,
    password_candidates: &[PasswordCandidate],
    batch_passwords: &mut Vec<PasswordCandidate>,
    password_prompter: Option<&dyn InteractivePasswordPrompter>,
    encoding_prompter: Option<&dyn InteractiveEncodingPrompter>,
    events: &EventSink,
    task_id: &TaskId,
    load_listing: bool,
) -> smartzip_core::Result<ArchiveAccessOutcome> {
    let known_password = resolved
        .known_hit
        .as_ref()
        .and_then(|hit| hit.password_id)
        .and_then(|id| passwords.candidate_by_id(id).ok().flatten());
    let ordered_candidates = order_password_candidates(
        password_candidates,
        known_password.as_ref(),
        batch_passwords,
    );
    let test_before_access = backend.should_test_before_extract(
        &resolved.archive_path,
        resolved.candidate.detected_format.as_ref(),
    );
    let total_password_attempts = ordered_candidates.len();
    let mut accepted_password_id = None;
    let mut used_password = None;
    let mut has_password = false;
    let mut listing = None;
    let mut encrypted = None;
    let mut emitted = Vec::new();
    let mut last_error = None;
    let mut saw_wrong_password = false;
    let mut password_prompt_cancelled = false;
    let mut assessment = resolved.zip_encoding_assessment.clone();

    for password in &ordered_candidates {
        let pw_value = password_value(password);
        let attempt_index = password_attempt_index(password, &ordered_candidates);
        emitted.push(TaskEvent {
            task_id: task_id.clone(),
            kind: TaskEventKind::Progress(smartzip_core::TaskProgress::indeterminate(format!(
                "Trying password [{}/{}] ({}) for {}",
                attempt_index,
                total_password_attempts,
                password_source_label(password),
                resolved.candidate.path.display()
            ))),
        });
        if test_before_access {
            match backend_call(
                "archive-backend",
                "test",
                &resolved.archive_path,
                backend.test(TestRequest {
                    archive: resolved.archive_path.clone(),
                    format: resolved.candidate.detected_format.clone(),
                    password: pw_value.clone(),
                    encoding: resolved.encoding_mode.clone(),
                }),
            )
            .await
            {
                Ok(result) if result.ok => {
                    accepted_password_id = passwords.record_success(password).ok().flatten();
                    used_password = pw_value.clone();
                    has_password = pw_value.as_deref().map(|v| !v.is_empty()).unwrap_or(false);
                    encrypted = result.encrypted;
                    if assessment.is_none()
                        && resolved.encoding_mode == EncodingMode::Auto
                        && resolved.candidate.detected_format == Some(ArchiveFormat::Zip)
                    {
                        let native_zip = NativeZipBackend::new();
                        assessment = assess_zip_encoding(
                            &native_zip,
                            &resolved.archive_path,
                            pw_value.clone(),
                        )
                        .await;
                    }
                    break;
                }
                Ok(result) => {
                    encrypted = result.encrypted;
                }
                Err(error) => {
                    if matches!(&error, smartzip_core::SmartZipError::WrongPassword { .. }) {
                        saw_wrong_password = true;
                        let _ = passwords.record_failure(password);
                    } else {
                        last_error = Some(error);
                    }
                }
            }
        } else {
            used_password = pw_value.clone();
            has_password = pw_value.as_deref().map(|v| !v.is_empty()).unwrap_or(false);
            break;
        }
    }

    if used_password.is_none() {
        if let Some(prompter) = password_prompter {
            emitted.push(TaskEvent {
                task_id: task_id.clone(),
                kind: TaskEventKind::Progress(smartzip_core::TaskProgress::indeterminate(format!(
                    "Prompting for password: {}",
                    resolved.candidate.path.display()
                ))),
            });
            let interactive_password = prompter.prompt(&resolved.candidate.path).await;
            password_prompt_cancelled = interactive_password
                .as_deref()
                .map(str::trim)
                .is_none_or(str::is_empty);
            if let Some(interactive_pw) = interactive_password {
                let pw = interactive_pw.trim().to_string();
                if !pw.is_empty() {
                    let accepted = PasswordCandidate {
                        id: None,
                        value: pw.clone(),
                        source: smartzip_passwords::PasswordSource::Manual,
                    };
                    if test_before_access {
                        let result = backend_call(
                            "archive-backend",
                            "test",
                            &resolved.archive_path,
                            backend.test(TestRequest {
                                archive: resolved.archive_path.clone(),
                                format: resolved.candidate.detected_format.clone(),
                                password: Some(pw.clone()),
                                encoding: resolved.encoding_mode.clone(),
                            }),
                        )
                        .await?;
                        if !result.ok {
                            return Err(smartzip_core::SmartZipError::WrongPassword {
                                path: resolved.candidate.path.clone(),
                            });
                        }
                        encrypted = result.encrypted;
                    }
                    accepted_password_id = passwords.record_success(&accepted).ok().flatten();
                    remember_batch_password(batch_passwords, &accepted.value, accepted_password_id);
                    used_password = Some(pw.clone());
                    has_password = true;
                    if assessment.is_none()
                        && resolved.encoding_mode == EncodingMode::Auto
                        && resolved.candidate.detected_format == Some(ArchiveFormat::Zip)
                    {
                        let native_zip = NativeZipBackend::new();
                        assessment =
                            assess_zip_encoding(&native_zip, &resolved.archive_path, Some(pw)).await;
                    }
                }
            }
        }
    }

    if used_password.is_none() && password_prompt_cancelled {
        return Err(smartzip_core::SmartZipError::PasswordRequired {
            path: resolved.candidate.path.clone(),
        });
    }
    if used_password.is_none() {
        if let Some(error) = last_error {
            return Err(error);
        }
        if saw_wrong_password {
            return Err(smartzip_core::SmartZipError::WrongPassword {
                path: resolved.candidate.path.clone(),
            });
        }
    }

    let encoding_mode = resolve_encoding_mode(
        &resolved.archive_path,
        resolved.encoding_mode.clone(),
        assessment.as_ref(),
        encoding_prompter,
    )
    .await?;

    if load_listing {
        listing = Some(
            backend_call(
                "archive-backend",
                "list",
                &resolved.archive_path,
                backend.list(ListRequest {
                    archive: resolved.archive_path.clone(),
                    format: resolved.candidate.detected_format.clone(),
                    password: used_password.clone(),
                    encoding: encoding_mode.clone(),
                }),
            )
            .await?,
        );
    }

    Ok(ArchiveAccessOutcome {
        password_id: accepted_password_id,
        has_password,
        used_password,
        encoding_mode,
        listing,
        encrypted,
        events: emitted,
        password_prompt_cancelled,
    })
}

fn map_detect_error(error: smartzip_core::SmartZipError, path: &Path) -> smartzip_core::SmartZipError {
    match error {
        smartzip_core::SmartZipError::UnsupportedFormat { .. } => {
            smartzip_core::SmartZipError::UnsupportedFormat {
                path: path.to_path_buf(),
                format: None,
            }
        }
        other => other,
    }
}

fn extraction_progress_callback(
    events: EventSink,
    task_id: TaskId,
    archive: PathBuf,
) -> ExtractionProgressCallback {
    Arc::new(move |percent| {
        events.push(TaskEvent {
            task_id: task_id.clone(),
            kind: TaskEventKind::Progress(smartzip_core::TaskProgress::percent(
                percent,
                format!("Extracting {}", archive.display()),
            )),
        });
    })
}

fn confidence_score(confidence: smartzip_scanner::Confidence) -> f32 {
    match confidence {
        smartzip_scanner::Confidence::Low => 0.33,
        smartzip_scanner::Confidence::Medium => 0.66,
        smartzip_scanner::Confidence::High => 1.0,
    }
}

fn password_value(candidate: &PasswordCandidate) -> Option<String> {
    Some(candidate.value.clone())
}

fn order_password_candidates(
    base: &[PasswordCandidate],
    known: Option<&PasswordCandidate>,
    batch: &[PasswordCandidate],
) -> Vec<PasswordCandidate> {
    let mut ordered = Vec::with_capacity(base.len() + batch.len() + usize::from(known.is_some()));

    for candidate in base.iter().filter(|candidate| {
        matches!(
            candidate.source,
            smartzip_passwords::PasswordSource::Manual
                | smartzip_passwords::PasswordSource::Clipboard
        )
    }) {
        push_password_unique(&mut ordered, candidate.clone());
    }
    if let Some(candidate) = known {
        push_password_unique(&mut ordered, candidate.clone());
    }
    for candidate in batch {
        push_password_unique(&mut ordered, candidate.clone());
    }
    for candidate in base.iter().filter(|candidate| {
        !matches!(
            candidate.source,
            smartzip_passwords::PasswordSource::Manual
                | smartzip_passwords::PasswordSource::Clipboard
        )
    }) {
        push_password_unique(&mut ordered, candidate.clone());
    }

    ordered
}

fn remember_batch_password(batch: &mut Vec<PasswordCandidate>, value: &str, id: Option<i64>) {
    if batch.iter().any(|candidate| candidate.value == value) {
        return;
    }
    batch.push(PasswordCandidate {
        id,
        value: value.to_string(),
        source: smartzip_passwords::PasswordSource::Recent,
    });
}

fn push_password_unique(ordered: &mut Vec<PasswordCandidate>, candidate: PasswordCandidate) {
    if !ordered
        .iter()
        .any(|existing| existing.value == candidate.value)
    {
        ordered.push(candidate);
    }
}

/// Human-readable label for an [`EncodingMode`], used for the
/// `file_extractions.encoding` column.
fn encoding_mode_label(mode: &EncodingMode) -> String {
    match mode {
        EncodingMode::Auto => "auto".to_string(),
        EncodingMode::Override(name) => name.clone(),
    }
}

/// Append a `skipped` row to `file_extractions` for a candidate that never
/// reached extraction. `reason` is one of the skip reason strings from the v3
/// schema (`duplicate` / `recursion_limit` / `not_first_volume` /
/// `business_container` / `password_required` / …). No-op without a recorder.
fn record_skip(
    history: Option<&dyn crate::history::TaskHistoryRecorder>,
    task_id: &TaskId,
    candidate: &ExtractionCandidate,
    reason: &str,
) {
    if let Some(recorder) = history {
        recorder.record_file_extraction(
            task_id,
            crate::history::FileExtractionRow {
                input_path: &candidate.path,
                sample_hash: None,
                file_size: None,
                offset: candidate.embedded_offset.map(|o| o as i64),
                output_path: None,
                has_password: false,
                password_id: None,
                status: "skipped",
                reason: Some(reason),
                encoding: None,
                encoding_corrected: false,
                damaged_volumes_json: None,
            },
        );
    }
}

fn password_source_label(candidate: &PasswordCandidate) -> &'static str {
    match candidate.source {
        smartzip_passwords::PasswordSource::Empty => "empty",
        smartzip_passwords::PasswordSource::Manual => "manual",
        smartzip_passwords::PasswordSource::Clipboard => "clipboard",
        smartzip_passwords::PasswordSource::Recent => "recent",
        smartzip_passwords::PasswordSource::Database => "database",
    }
}

fn password_attempt_index(
    candidate: &PasswordCandidate,
    candidates: &[PasswordCandidate],
) -> usize {
    candidates
        .iter()
        .position(|existing| existing == candidate)
        .map(|idx| idx + 1)
        .unwrap_or(0)
}

fn archive_output_name(path: &Path) -> PathBuf {
    PathBuf::from(archive_stem(path))
}

fn archive_stem(path: &Path) -> std::ffi::OsString {
    std::ffi::OsString::from(name_score::archive_display_stem(path))
}

fn candidate_key(candidate: &ExtractionCandidate) -> String {
    format!(
        "{}:{}:{:?}",
        candidate.path.display(),
        candidate.embedded_offset.unwrap_or(0),
        candidate.source
    )
}

fn root_embedded_candidates(
    root: &ExtractionCandidate,
    findings: &[EmbeddedArchiveFinding],
) -> Vec<ExtractionCandidate> {
    if root.source != CandidateSource::RootInput
        || root.embedded_offset.is_some()
        || findings.iter().any(|finding| finding.offset == 0)
    {
        return Vec::new();
    }

    findings
        .iter()
        .enumerate()
        .map(|(index, finding)| {
            let mut relative_path = root.relative_path.clone();
            let base_name = relative_path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy();
            relative_path.set_file_name(format!(
                "{base_name}-embedded-{}-{:X}",
                index + 1,
                finding.offset
            ));
            ExtractionCandidate {
                path: root.path.clone(),
                relative_path,
                depth: root.depth,
                source: CandidateSource::EmbeddedFinding,
                detected_format: Some(finding.format.clone()),
                embedded_offset: Some(finding.offset),
                embedded_size: finding.size,
            }
        })
        .collect()
}

fn output_dir_for_candidate(base: &Path, candidate: &ExtractionCandidate) -> PathBuf {
    match candidate.source {
        CandidateSource::RootInput => base.join(candidate_output_relative_path(candidate)),
        CandidateSource::EmbeddedFinding if candidate.depth == 0 => {
            base.join(candidate_output_relative_path(candidate))
        }
        CandidateSource::ExtractedFile | CandidateSource::EmbeddedFinding => candidate
            .path
            .parent()
            .unwrap_or(base)
            .join(archive_output_name(&candidate.path)),
    }
}

fn candidate_output_relative_path(candidate: &ExtractionCandidate) -> PathBuf {
    candidate.relative_path.clone()
}

fn recyclable_nested_archive_path(
    candidate: &ExtractionCandidate,
    managed_output_root: &Path,
) -> Option<PathBuf> {
    if candidate.source != CandidateSource::ExtractedFile
        || candidate.embedded_offset.is_some_and(|offset| offset > 0)
    {
        return None;
    }

    let metadata = std::fs::symlink_metadata(&candidate.path).ok()?;
    if !metadata.file_type().is_file() {
        return None;
    }

    let canonical_output_root = managed_output_root.canonicalize().ok()?;
    let canonical_path = candidate.path.canonicalize().ok()?;
    canonical_path
        .starts_with(&canonical_output_root)
        .then_some(candidate.path.clone())
}

async fn recycle_archive(
    archive_recycler: ArchiveRecycleHandler,
    path: PathBuf,
) -> std::io::Result<()> {
    tokio::task::spawn_blocking(move || archive_recycler(path))
        .await
        .map_err(std::io::Error::other)?
}

struct ArchiveInput {
    path: PathBuf,
    _temp: Option<tempfile::NamedTempFile>,
}

fn materialize_archive_input(
    candidate: &ExtractionCandidate,
) -> smartzip_core::Result<ArchiveInput> {
    if let Some(offset) = candidate.embedded_offset.filter(|offset| *offset > 0) {
        let temp = carve_embedded_archive(
            &candidate.path,
            offset,
            candidate.embedded_size,
            candidate.detected_format.as_ref(),
        )
        .map_err(
            |source| smartzip_core::SmartZipError::EmbeddedArchiveCarveFailed {
                path: candidate.path.clone(),
                offset,
                detail: source.to_string(),
            },
        )?;
        let path = temp.path().to_path_buf();
        Ok(ArchiveInput {
            path,
            _temp: Some(temp),
        })
    } else {
        Ok(ArchiveInput {
            path: candidate.path.clone(),
            _temp: None,
        })
    }
}

fn output_relative_path_for(base: &Path, output_dir: &Path) -> PathBuf {
    output_dir
        .strip_prefix(base)
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            output_dir
                .file_name()
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("archive"))
        })
}

fn make_collision_resolver<'a>(
    prompter: &'a dyn InteractiveOutputPrompter,
) -> CollisionResolver<'a> {
    Box::new(move |archive_path, target_path, _plan| {
        let prompter = prompter;
        Box::pin(async move {
            let strategy = prompter.prompt(archive_path, target_path).await;
            match strategy {
                OutputCollisionStrategy::Skip => CollisionAction::Skip,
                OutputCollisionStrategy::Overwrite => CollisionAction::Overwrite,
                OutputCollisionStrategy::Rename => CollisionAction::Rename,
            }
        })
    })
}

fn carve_embedded_archive(
    source: &Path,
    offset: u64,
    size: Option<u64>,
    format: Option<&ArchiveFormat>,
) -> std::io::Result<tempfile::NamedTempFile> {
    let file_len = std::fs::metadata(source)?.len();

    if offset >= file_len {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("carve offset {} exceeds file size {}", offset, file_len),
        ));
    }

    let effective_end = match size {
        Some(s) => {
            if s == 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "carve size cannot be zero",
                ));
            }
            offset.saturating_add(s).min(file_len)
        }
        None => {
            if format == Some(&ArchiveFormat::Zip) {
                if let Ok(Some(zip_end)) = crate::embedded_zip::detect_zip_end(source, offset) {
                    zip_end
                } else {
                    file_len
                }
            } else {
                file_len
            }
        }
    };

    if effective_end <= offset {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "carve range is empty",
        ));
    }

    let mut input = File::open(source)?;
    input.seek(SeekFrom::Start(offset))?;

    let mut output = tempfile::NamedTempFile::new()?;
    let bytes_to_copy = effective_end - offset;
    std::io::copy(&mut input.take(bytes_to_copy), &mut output)?;
    output.flush()?;

    Ok(output)
}

async fn backend_call<T, F>(
    backend: &str,
    action: &str,
    path: &Path,
    future: F,
) -> smartzip_core::Result<T>
where
    F: Future<Output = smartzip_core::Result<T>>,
{
    match AssertUnwindSafe(future).catch_unwind().await {
        Ok(result) => result,
        Err(panic) => Err(smartzip_core::SmartZipError::BackendFailed {
            backend: backend.to_string(),
            exit_code: None,
            stderr: format!(
                "panic while {action} {}: {}",
                path.display(),
                panic_message(panic)
            ),
        }),
    }
}

fn panic_message(panic: Box<dyn Any + Send>) -> String {
    if let Some(message) = panic.downcast_ref::<&str>() {
        (*message).to_string()
    } else if let Some(message) = panic.downcast_ref::<String>() {
        message.clone()
    } else {
        "unknown panic payload".to_string()
    }
}

fn is_business_container(path: &Path) -> bool {
    ext_business_container_kind(path).is_some()
}

fn ext_business_container_kind(path: &Path) -> Option<smartzip_core::BusinessContainerKind> {
    let ext = path.extension().and_then(|e| e.to_str())?;
    match ext.to_ascii_lowercase().as_str() {
        "docx" => Some(smartzip_core::BusinessContainerKind::OfficeDocx),
        "xlsx" => Some(smartzip_core::BusinessContainerKind::OfficeXlsx),
        "pptx" => Some(smartzip_core::BusinessContainerKind::OfficePptx),
        "epub" => Some(smartzip_core::BusinessContainerKind::Epub),
        "apk" => Some(smartzip_core::BusinessContainerKind::Apk),
        "jar" => Some(smartzip_core::BusinessContainerKind::Jar),
        "cbr" => Some(smartzip_core::BusinessContainerKind::Cbr),
        _ => None,
    }
}

fn embedded_policy_from_request(
    request: &ExtractWorkflowRequest,
) -> smartzip_core::EmbeddedScanPolicy {
    smartzip_core::EmbeddedScanPolicy {
        mode: request.embedded_scan_mode,
        dominant_min_ratio: request.dominant_min_ratio,
        ..smartzip_core::EmbeddedScanPolicy::default()
    }
}

fn full_root_scanner_config(requested: &ScannerConfig) -> ScannerConfig {
    ScannerConfig {
        mode: ScanMode::Deep,
        max_scan_bytes: None,
        ..requested.clone()
    }
}

fn default_root_scanner_config(requested: &ScannerConfig) -> ScannerConfig {
    if requested == &ScannerConfig::default() || requested.max_scan_bytes.is_none() {
        full_root_scanner_config(requested)
    } else {
        requested.clone()
    }
}

fn finding_meets_min_size(
    finding: &EmbeddedArchiveFinding,
    policy: &smartzip_core::EmbeddedScanPolicy,
) -> bool {
    finding.offset == 0
        || finding
            .size
            .is_none_or(|size| size >= policy.min_finding_size_bytes)
}

fn should_scan_candidate_for_embedded(
    candidate: &ExtractionCandidate,
    policy: &smartzip_core::EmbeddedScanPolicy,
    nested_embedded_enabled: bool,
    confirm_large_scan: bool,
    events: &EventSink,
    task_id: &TaskId,
) -> bool {
    if matches!(policy.mode, smartzip_core::EmbeddedScanMode::Ignore) {
        return false;
    }

    if candidate.source == CandidateSource::EmbeddedFinding && candidate.embedded_offset.is_some() {
        return false;
    }

    if candidate.source != CandidateSource::RootInput && !nested_embedded_enabled {
        return false;
    }

    let file_size = std::fs::metadata(&candidate.path)
        .map(|metadata| metadata.len())
        .unwrap_or(0);
    if candidate.source == CandidateSource::RootInput
        && !confirm_large_scan
        && crate::format_from_extension(&candidate.path).is_none()
        && file_size > policy.root_full_scan_confirm_threshold
    {
        events.push(TaskEvent {
            task_id: task_id.clone(),
            kind: TaskEventKind::LargeEmbeddedScanConfirmationRequired {
                path: candidate.path.clone(),
                file_size,
                threshold: policy.root_full_scan_confirm_threshold,
            },
        });
        return false;
    }

    if candidate.source != CandidateSource::RootInput
        && policy
            .inner_scan_max_bytes
            .is_some_and(|max_bytes| file_size > max_bytes)
    {
        return false;
    }

    true
}

async fn resolve_encoding_mode(
    archive_path: &Path,
    requested: EncodingMode,
    assessment: Option<&ZipEncodingAssessment>,
    prompter: Option<&dyn InteractiveEncodingPrompter>,
) -> smartzip_core::Result<EncodingMode> {
    if requested != EncodingMode::Auto {
        return Ok(requested);
    }

    let Some(assessment) = assessment else {
        return Ok(EncodingMode::Auto);
    };

    if assessment.should_confirm {
        if let Some(prompter) = prompter {
            match prompter.prompt(archive_path, &assessment.context).await {
                EncodingConfirmationChoice::AcceptDetected => {}
                EncodingConfirmationChoice::Override(encoding) => {
                    return Ok(EncodingMode::Override(encoding));
                }
                EncodingConfirmationChoice::SkipArchive => {
                    return Err(smartzip_core::SmartZipError::BackendFailed {
                        backend: "encoding-confirmation".into(),
                        exit_code: None,
                        stderr: format!("encoding confirmation skipped {}", archive_path.display()),
                    });
                }
            }
        }
    }

    Ok(EncodingMode::Override(
        assessment.detected_raw.selected.clone(),
    ))
}

#[derive(Debug, Clone)]
struct ZipEncodingAssessment {
    detected_raw: smartzip_encoding::EncodingDetectionResult,
    context: EncodingConfirmationContext,
    should_confirm: bool,
}

async fn assess_zip_encoding(
    native_zip: &NativeZipBackend,
    archive_path: &Path,
    password: Option<String>,
) -> Option<ZipEncodingAssessment> {
    let listing = native_zip
        .list(ListRequest {
            archive: archive_path.to_path_buf(),
            format: Some(ArchiveFormat::Zip),
            password,
            encoding: EncodingMode::Auto,
        })
        .await
        .ok()?;

    build_zip_encoding_assessment(listing)
}

fn build_zip_encoding_assessment(listing: ArchiveListing) -> Option<ZipEncodingAssessment> {
    let raw_entries: Vec<&[u8]> = listing
        .entries
        .iter()
        .map(|entry| entry.raw_name.as_slice())
        .filter(|raw| !raw.is_empty())
        .collect();
    if raw_entries.is_empty() {
        return None;
    }

    let ascii_only = raw_entries.iter().all(|raw| raw.is_ascii());
    let raw_names: Vec<u8> = raw_entries
        .iter()
        .enumerate()
        .flat_map(|(idx, raw)| {
            let mut merged = Vec::new();
            if idx > 0 {
                merged.push(b'/');
            }
            merged.extend_from_slice(raw);
            merged
        })
        .collect();

    let mut detector = smartzip_encoding::ArchiveEncodingDetector::new();
    let detected_raw = detector.detect(&raw_names);
    let detected = to_core_encoding_detection(&detected_raw);
    let preview_names = raw_entries
        .iter()
        .take(6)
        .map(|raw| decode_preview_name(raw, &detected_raw.selected))
        .collect::<Vec<_>>();
    let suspicious_reasons =
        suspicious_encoding_reasons(&detected_raw, &preview_names, ascii_only, &raw_entries);
    Some(ZipEncodingAssessment {
        detected_raw,
        context: EncodingConfirmationContext {
            detected,
            preview_names,
            suspicious_reasons: suspicious_reasons.clone(),
        },
        should_confirm: !suspicious_reasons.is_empty(),
    })
}

fn to_core_encoding_detection(
    result: &smartzip_encoding::EncodingDetectionResult,
) -> smartzip_core::EncodingDetectionResult {
    smartzip_core::EncodingDetectionResult {
        selected: EncodingMode::Override(result.selected.clone()),
        confidence: result.confidence,
        candidates: result
            .candidates
            .iter()
            .map(|candidate| smartzip_core::EncodingCandidate {
                name: candidate.name.clone(),
                confidence: candidate.confidence,
            })
            .collect(),
    }
}

fn decode_preview_name(raw_name: &[u8], encoding: &str) -> String {
    smartzip_encoding::decode_name(raw_name, encoding)
        .unwrap_or_else(|| String::from_utf8_lossy(raw_name).into_owned())
}

fn suspicious_encoding_reasons(
    detected: &smartzip_encoding::EncodingDetectionResult,
    preview_names: &[String],
    ascii_only: bool,
    raw_entries: &[&[u8]],
) -> Vec<String> {
    let mut reasons = Vec::new();
    if ascii_only {
        return reasons;
    }

    if raw_entries
        .iter()
        .all(|raw| std::str::from_utf8(raw).is_ok())
        && detected.selected.eq_ignore_ascii_case("utf-8")
    {
        return reasons;
    }

    let second_confidence = detected
        .candidates
        .get(1)
        .map(|candidate| candidate.confidence)
        .unwrap_or(0.0);
    if detected.confidence < 0.90 {
        reasons.push(format!(
            "low confidence {:.0}%",
            detected.confidence * 100.0
        ));
    }
    if (detected.confidence - second_confidence).abs() < 0.15 {
        reasons.push("top encoding candidates are close".into());
    }
    if preview_names.iter().any(|name| looks_like_mojibake(name)) {
        reasons.push("previewed names look garbled".into());
    }

    reasons
}

fn looks_like_mojibake(value: &str) -> bool {
    if value.contains('\u{FFFD}') {
        return true;
    }
    let suspicious_markers = ['Ã', 'Â', 'Ð', 'Ñ', 'æ', 'ç', 'ø', '¢', '¤', '¥'];
    let suspicious_count = value
        .chars()
        .filter(|ch| suspicious_markers.contains(ch))
        .count();
    suspicious_count >= 2
        || value
            .chars()
            .any(|ch| ch.is_control() && ch != '\n' && ch != '\t')
}

fn discover_nested_candidates(
    scanner: &EmbeddedScanner,
    root: &Path,
    depth: u8,
    prefix: &Path,
    policy: &smartzip_core::EmbeddedScanPolicy,
    nested_embedded_enabled: bool,
) -> Vec<ExtractionCandidate> {
    let mut candidates = Vec::new();

    // Handle single-file roots directly when a candidate resolves to one file.
    if root.is_file() {
        let header_result = crate::detect::probe_file_header(root);
        if let Some((fmt, offset)) = header_result {
            if is_business_container(root) || crate::container::classify_zip_path(root).is_some() {
                return candidates;
            }
            candidates.push(ExtractionCandidate {
                path: root.to_path_buf(),
                relative_path: prefix.join(archive_stem(root)),
                depth,
                source: CandidateSource::ExtractedFile,
                detected_format: Some(fmt),
                embedded_offset: if offset > 0 { Some(offset) } else { None },
                embedded_size: None,
            });
            return candidates;
        }

        if let Some(format) = format_from_extension(root) {
            if is_business_container(root) || crate::container::classify_zip_path(root).is_some() {
                return candidates;
            }
            candidates.push(ExtractionCandidate {
                path: root.to_path_buf(),
                relative_path: prefix.join(archive_stem(root)),
                depth,
                source: CandidateSource::ExtractedFile,
                detected_format: Some(format),
                embedded_offset: None,
                embedded_size: None,
            });
            return candidates;
        }
        return candidates;
    }

    let Ok(entries) = std::fs::read_dir(root) else {
        return candidates;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let mut next_prefix = prefix.to_path_buf();
            next_prefix.push(entry.file_name());
            candidates.extend(discover_nested_candidates(
                scanner,
                &path,
                depth,
                &next_prefix,
                policy,
                nested_embedded_enabled,
            ));
            continue;
        }

        let detected_format = format_from_extension(&path);
        let mut relative_path = prefix.to_path_buf();
        relative_path.push(path.strip_prefix(root).unwrap_or(path.as_path()));
        relative_path.set_file_name(archive_stem(&path));

        let header_result = crate::detect::probe_file_header(&path);
        if let Some((fmt, offset)) = header_result {
            if is_business_container(&path) || crate::container::classify_zip_path(&path).is_some()
            {
                continue;
            }
            candidates.push(ExtractionCandidate {
                path: path.clone(),
                relative_path,
                depth,
                source: CandidateSource::ExtractedFile,
                detected_format: Some(fmt),
                embedded_offset: if offset > 0 { Some(offset) } else { None },
                embedded_size: None,
            });
            continue;
        }

        if detected_format.is_some() {
            if is_business_container(&path) || crate::container::classify_zip_path(&path).is_some()
            {
                continue;
            }
            candidates.push(ExtractionCandidate {
                path: path.clone(),
                relative_path,
                depth,
                source: CandidateSource::ExtractedFile,
                detected_format,
                embedded_offset: None,
                embedded_size: None,
            });
            continue;
        }

        if !nested_embedded_enabled {
            continue;
        }
        let file_size = std::fs::metadata(&path)
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        if policy
            .inner_scan_max_bytes
            .is_some_and(|max_bytes| file_size > max_bytes)
        {
            continue;
        }
        let findings: Vec<_> = scanner
            .scan_path(&path)
            .unwrap_or_default()
            .into_iter()
            .filter(|finding| finding_meets_min_size(finding, policy))
            .collect();
        if findings.is_empty() {
            continue;
        }
        if matches!(
            policy.mode,
            smartzip_core::EmbeddedScanMode::Auto
                | smartzip_core::EmbeddedScanMode::Ask
                | smartzip_core::EmbeddedScanMode::Aggressive
                | smartzip_core::EmbeddedScanMode::All
        ) {
            for finding in findings {
                candidates.push(ExtractionCandidate {
                    path: path.clone(),
                    relative_path: relative_path.clone(),
                    depth,
                    source: CandidateSource::EmbeddedFinding,
                    detected_format: Some(finding.format),
                    embedded_offset: Some(finding.offset),
                    embedded_size: finding.size,
                });
            }
            continue;
        }

        let decision = crate::embedded::select_embedded_action(file_size, &findings, policy, false);
        if let Some(idx) = decision.selected_index {
            let finding = &findings[idx];
            if matches!(
                decision.action,
                smartzip_core::DetectionAction::ExtractDirect
                    | smartzip_core::DetectionAction::CarveAndExtract
            ) {
                candidates.push(ExtractionCandidate {
                    path: path.clone(),
                    relative_path: relative_path.clone(),
                    depth,
                    source: CandidateSource::EmbeddedFinding,
                    detected_format: Some(finding.format.clone()),
                    embedded_offset: Some(finding.offset),
                    embedded_size: finding.size,
                });
            }
        }
    }

    candidates
}

pub fn is_first_volume(path: impl AsRef<std::path::Path>) -> bool {
    let path = path.as_ref();
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();

    if let Some(volume_index) = rar_part_volume_index(&file_name) {
        return volume_index == 1;
    }

    if let Some(volume_index) = numeric_volume_index(path) {
        return volume_index == 1;
    }

    true
}

fn rar_part_volume_index(file_name: &str) -> Option<u64> {
    let stem = file_name.strip_suffix(".rar")?;
    let part_index = stem.rfind(".part")?;
    let suffix = &stem[part_index + ".part".len()..];
    if suffix.is_empty() || !suffix.chars().all(|ch| ch.is_ascii_digit()) {
        return None;
    }
    suffix.parse().ok()
}

fn numeric_volume_index(path: &Path) -> Option<u64> {
    let extension = path.extension()?.to_str()?;
    if extension.is_empty() || !extension.chars().all(|ch| ch.is_ascii_digit()) {
        return None;
    }
    extension.parse().ok()
}

pub fn format_from_extension(path: impl AsRef<std::path::Path>) -> Option<ArchiveFormat> {
    let extension = path
        .as_ref()
        .extension()
        .and_then(|extension| extension.to_str())?
        .to_ascii_lowercase();

    match extension.as_str() {
        "zip" => Some(ArchiveFormat::Zip),
        "7z" => Some(ArchiveFormat::SevenZip),
        "rar" => Some(ArchiveFormat::Rar),
        "tar" => Some(ArchiveFormat::Tar),
        "gz" | "gzip" | "tgz" => Some(ArchiveFormat::Gzip),
        "bz2" => Some(ArchiveFormat::Bzip2),
        "xz" => Some(ArchiveFormat::Xz),
        "cab" => Some(ArchiveFormat::Cab),
        "iso" => Some(ArchiveFormat::Iso),
        "dmg" => Some(ArchiveFormat::Dmg),
        "zst" | "zstd" => Some(ArchiveFormat::Zstd),
        "lz4" => Some(ArchiveFormat::Lz4),
        "lzma" => Some(ArchiveFormat::Lzma),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use rstest::*;
    use smartzip_archive::{
        ArchiveBackend, ArchiveListing, ArchiveProbe, BackendCapabilities, BackendRouter,
        CompressArchiveRequest, CompressArchiveResult, ExtractArchiveRequest, ExtractArchiveResult,
        ListRequest, SevenZipBackend, TestRequest, TestResult,
    };
    use smartzip_db::{password::PasswordRepository, SmartZipDb};
    use smartzip_passwords::{PasswordCandidateRequest, PasswordService};
    use smartzip_scanner::ScanMode;
    use std::sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    };
    use std::time::Duration;

    fn engine_with_test_recycler() -> SmartZipEngine {
        let recycler: ArchiveRecycleHandler = Arc::new(std::fs::remove_file);
        SmartZipEngine::default().with_archive_recycler(recycler)
    }

    fn fixture_path(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("tests")
            .join("fixtures")
            .join(name)
    }

    fn password_candidate(
        value: &str,
        source: smartzip_passwords::PasswordSource,
    ) -> PasswordCandidate {
        PasswordCandidate {
            id: None,
            value: value.to_string(),
            source,
        }
    }

    #[test]
    fn password_order_is_explicit_then_known_then_batch_then_database() {
        use smartzip_passwords::PasswordSource;

        let base = vec![
            password_candidate("cli", PasswordSource::Manual),
            password_candidate("", PasswordSource::Empty),
            password_candidate("db", PasswordSource::Database),
        ];
        let known = password_candidate("known", PasswordSource::Database);
        let batch = vec![password_candidate("batch", PasswordSource::Recent)];

        let ordered = order_password_candidates(&base, Some(&known), &batch);
        assert_eq!(
            ordered
                .iter()
                .map(|candidate| candidate.value.as_str())
                .collect::<Vec<_>>(),
            vec!["cli", "known", "batch", "", "db"],
        );
    }

    #[test]
    fn detects_empty_file_without_findings() {
        let path =
            std::env::temp_dir().join(format!("smartzip-engine-empty-{}", std::process::id()));
        std::fs::write(&path, []).unwrap();

        let engine = SmartZipEngine::default();
        let result = engine
            .detect(DetectRequest {
                path: path.clone(),
                scanner: ScannerConfig::default(),
            })
            .unwrap();
        let _ = std::fs::remove_file(path);

        assert!(result.findings.is_empty());
        assert!(matches!(
            result.events.first().unwrap().kind,
            TaskEventKind::Started
        ));
        assert!(matches!(
            result.events.last().unwrap().kind,
            TaskEventKind::Completed
        ));
    }

    #[test]
    fn root_scan_is_full_while_default_nested_scan_stays_fast() {
        let nested = ScannerConfig::default();
        let root = full_root_scanner_config(&nested);

        assert_eq!(root.mode, ScanMode::Deep);
        assert_eq!(root.max_scan_bytes, None);
        assert_eq!(nested.mode, ScanMode::Fast);
        assert_eq!(nested.max_scan_bytes, Some(64 * 1024 * 1024));
    }

    #[test]
    fn root_scan_enqueues_every_eligible_finding_with_unique_output() {
        let root = ExtractionCandidate {
            path: PathBuf::from("/inputs/carrier.mp4"),
            relative_path: PathBuf::from("carrier"),
            depth: 0,
            source: CandidateSource::RootInput,
            detected_format: None,
            embedded_offset: None,
            embedded_size: None,
        };
        let policy = smartzip_core::EmbeddedScanPolicy::default();
        let findings = vec![
            EmbeddedArchiveFinding {
                offset: 100,
                size: Some(policy.min_finding_size_bytes - 1),
                format: ArchiveFormat::Zip,
                confidence: Confidence::High,
                description: String::new(),
            },
            EmbeddedArchiveFinding {
                offset: 200,
                size: Some(policy.min_finding_size_bytes),
                format: ArchiveFormat::Zip,
                confidence: Confidence::High,
                description: String::new(),
            },
            EmbeddedArchiveFinding {
                offset: 300,
                size: None,
                format: ArchiveFormat::SevenZip,
                confidence: Confidence::High,
                description: String::new(),
            },
        ];
        let eligible: Vec<_> = findings
            .into_iter()
            .filter(|finding| finding_meets_min_size(finding, &policy))
            .collect();
        let candidates = root_embedded_candidates(&root, &eligible);

        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].embedded_offset, Some(200));
        assert_eq!(candidates[1].embedded_offset, Some(300));
        assert_ne!(candidates[0].relative_path, candidates[1].relative_path);
        assert!(candidates
            .iter()
            .all(|candidate| candidate.source == CandidateSource::EmbeddedFinding));
    }

    #[test]
    fn zip_encoding_assessment_skips_confirmation_for_ascii_names() {
        let assessment = build_zip_encoding_assessment(ArchiveListing {
            format: Some(ArchiveFormat::Zip),
            entries: vec![smartzip_archive::ArchiveEntry {
                path: PathBuf::from("docs/readme.txt"),
                raw_name: b"docs/readme.txt".to_vec(),
                compressed_size: None,
                uncompressed_size: None,
                is_dir: false,
            }],
        })
        .unwrap();

        assert!(!assessment.should_confirm);
        assert!(assessment.context.suspicious_reasons.is_empty());
    }

    #[tokio::test]
    async fn embedded_ask_without_prompter_skips_archive() {
        let archive = fixture_path("video_7z_pass.mp4");
        let backend = BackendRouter::locate().unwrap();
        let db = SmartZipDb::in_memory().unwrap();
        let service = PasswordService::new(PasswordRepository::new(db.connection()));
        let output = tempfile::tempdir().unwrap();

        let result = SmartZipEngine::default()
            .with_min_embedded_size_bytes(0)
            .extract_recursive(
                &backend,
                &service,
                ExtractWorkflowRequest {
                    inputs: vec![archive],
                    output_dir: output.path().to_path_buf(),
                    recursion_limit: 1,
                    encoding_mode: EncodingMode::Auto,
                    scanner: ScannerConfig::default(),
                    password_candidates: PasswordCandidateRequest {
                        manual: Vec::new(),
                        clipboard: None,
                        include_empty: false,
                        limit: 8,
                    },
                    layout_policy: Default::default(),
                    single_root_name_policy: Default::default(),
                    embedded_scan_mode: smartzip_core::EmbeddedScanMode::Ask,
                    dominant_min_ratio: 0.70,
                    confirm_large_scan: false,
                    force: false,
                },
                None,
                None,
            )
            .await
            .unwrap();

        assert!(result.processed.is_empty());
        assert_eq!(result.skipped.len(), 1);
        assert!(result.events.iter().any(|event| matches!(
            event.kind,
            TaskEventKind::EmbeddedArchiveSelectionRequired { .. }
        )));
    }

    #[test]
    fn cbz_extension_is_not_a_business_container() {
        assert_eq!(ext_business_container_kind(Path::new("comic.cbz")), None);
    }

    #[test]
    fn recognizes_first_volume_rules() {
        assert!(is_first_volume("archive.part1.rar"));
        assert!(is_first_volume("archive.part01.rar"));
        assert!(!is_first_volume("archive.part2.rar"));
        assert!(is_first_volume("archive.001"));
        assert!(!is_first_volume("archive.002"));
        assert!(is_first_volume("archive.zip"));
    }

    #[test]
    fn nested_candidate_output_uses_archive_parent_as_global_output_root() {
        let managed_output = PathBuf::from("/managed-output");
        let root = ExtractionCandidate {
            path: PathBuf::from("/inputs/outer.zip"),
            relative_path: PathBuf::from("outer"),
            depth: 0,
            source: CandidateSource::RootInput,
            detected_format: Some(ArchiveFormat::Zip),
            embedded_offset: None,
            embedded_size: None,
        };
        let nested = ExtractionCandidate {
            path: PathBuf::from("/managed-output/outer/inner.zip"),
            relative_path: PathBuf::from("outer/inner"),
            depth: 1,
            source: CandidateSource::ExtractedFile,
            detected_format: Some(ArchiveFormat::Zip),
            embedded_offset: None,
            embedded_size: None,
        };

        assert_eq!(
            output_dir_for_candidate(&managed_output, &root),
            PathBuf::from("/managed-output/outer")
        );
        assert_eq!(
            output_dir_for_candidate(&managed_output, &nested),
            PathBuf::from("/managed-output/outer/inner")
        );
    }

    #[test]
    fn only_regular_extracted_archives_inside_output_are_recyclable() {
        let root = tempfile::tempdir().unwrap();
        let output = root.path().join("output");
        std::fs::create_dir_all(&output).unwrap();
        let archive = output.join("inner.zip");
        std::fs::write(&archive, b"archive").unwrap();

        let mut candidate = ExtractionCandidate {
            path: archive.clone(),
            relative_path: PathBuf::from("inner"),
            depth: 1,
            source: CandidateSource::ExtractedFile,
            detected_format: Some(ArchiveFormat::Zip),
            embedded_offset: None,
            embedded_size: None,
        };
        assert_eq!(
            recyclable_nested_archive_path(&candidate, &output),
            Some(archive.clone())
        );

        candidate.source = CandidateSource::RootInput;
        assert!(recyclable_nested_archive_path(&candidate, &output).is_none());

        candidate.source = CandidateSource::EmbeddedFinding;
        assert!(recyclable_nested_archive_path(&candidate, &output).is_none());

        candidate.source = CandidateSource::ExtractedFile;
        candidate.embedded_offset = Some(0);
        assert_eq!(
            recyclable_nested_archive_path(&candidate, &output),
            Some(archive.clone())
        );

        candidate.embedded_offset = Some(16);
        assert!(recyclable_nested_archive_path(&candidate, &output).is_none());

        candidate.embedded_offset = None;
        candidate.path = root.path().join("outside.zip");
        std::fs::write(&candidate.path, b"archive").unwrap();
        assert!(recyclable_nested_archive_path(&candidate, &output).is_none());
    }

    #[test]
    fn maps_common_extensions() {
        assert_eq!(format_from_extension("a.7z"), Some(ArchiveFormat::SevenZip));
        assert_eq!(format_from_extension("a.tgz"), Some(ArchiveFormat::Gzip));
        assert_eq!(format_from_extension("a.bin"), None);
    }

    #[rstest]
    #[case("a.zip", Some(ArchiveFormat::Zip))]
    #[case("a.7z", Some(ArchiveFormat::SevenZip))]
    #[case("a.rar", Some(ArchiveFormat::Rar))]
    #[case("a.tar", Some(ArchiveFormat::Tar))]
    #[case("a.gz", Some(ArchiveFormat::Gzip))]
    #[case("a.gzip", Some(ArchiveFormat::Gzip))]
    #[case("a.tgz", Some(ArchiveFormat::Gzip))]
    #[case("a.bz2", Some(ArchiveFormat::Bzip2))]
    #[case("a.xz", Some(ArchiveFormat::Xz))]
    #[case("a.cab", Some(ArchiveFormat::Cab))]
    #[case("a.iso", Some(ArchiveFormat::Iso))]
    #[case("a.dmg", Some(ArchiveFormat::Dmg))]
    #[case("a.zst", Some(ArchiveFormat::Zstd))]
    #[case("a.zstd", Some(ArchiveFormat::Zstd))]
    #[case("a.lz4", Some(ArchiveFormat::Lz4))]
    #[case("a.lzma", Some(ArchiveFormat::Lzma))]
    #[case("a.txt", None)]
    #[case("a.bin", None)]
    #[case("no-extension", None)]
    #[case("a.ZIP", Some(ArchiveFormat::Zip))]
    #[case("A.7Z", Some(ArchiveFormat::SevenZip))]
    fn format_from_extension_parametrized(
        #[case] path: &str,
        #[case] expected: Option<ArchiveFormat>,
    ) {
        assert_eq!(format_from_extension(path), expected);
    }

    #[rstest]
    #[case("archive.part1.rar", true)]
    #[case("archive.part01.rar", true)]
    #[case("archive.part2.rar", false)]
    #[case("archive.part5.rar", false)]
    #[case("archive.rar", true)]
    #[case("archive.001", true)]
    #[case("archive.002", false)]
    #[case("archive.010", false)]
    #[case("archive.zip", true)]
    #[case("archive.7z", true)]
    #[case("data.tar.gz", true)]
    fn is_first_volume_parametrized(#[case] path: &str, #[case] expected: bool) {
        assert_eq!(is_first_volume(path), expected);
    }

    #[test]
    fn engine_accepts_custom_scanner_config() {
        let engine = SmartZipEngine::with_scanner_config(ScannerConfig {
            mode: ScanMode::Deep,
            ..ScannerConfig::default()
        });
        assert_eq!(engine.scanner.config().mode, ScanMode::Deep);
    }

    #[tokio::test]
    async fn recursive_extract_enqueues_nested_archives_and_skips_non_first_volume() {
        let root =
            std::env::temp_dir().join(format!("smartzip-engine-recursive-{}", std::process::id()));
        let input = root.join("root.zip");
        let output = root.join("out");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(&input, b"not really a zip").unwrap();

        let backend = FakeBackend::default();
        let db = SmartZipDb::in_memory().unwrap();
        let service = PasswordService::new(PasswordRepository::new(db.connection()));
        service.add_password("secret", "manual", false).unwrap();

        let engine = engine_with_test_recycler();
        let result = engine
            .extract_recursive(
                &backend,
                &service,
                ExtractWorkflowRequest {
                    inputs: vec![input.clone(), root.join("skip.part2.rar")],
                    output_dir: output.clone(),
                    recursion_limit: 2,
                    encoding_mode: EncodingMode::Auto,
                    scanner: ScannerConfig::default(),
                    password_candidates: PasswordCandidateRequest {
                        include_empty: false,
                        limit: 10,
                        ..PasswordCandidateRequest::default()
                    },
                    layout_policy: crate::layout::OutputLayoutPolicy::default(),
                    single_root_name_policy: crate::layout::SingleRootNamePolicy::default(),
                    embedded_scan_mode: smartzip_core::EmbeddedScanMode::default(),
                    dominant_min_ratio: 0.70,
                    confirm_large_scan: false,
                    force: false,
                },
                None,
                None,
            )
            .await
            .unwrap();

        let calls = backend.calls.lock().unwrap().clone();
        assert!(calls.iter().any(|path| path.ends_with("root.zip")));
        assert!(calls.iter().any(|path| path.ends_with("nested.zip")));
        assert!(!calls.iter().any(|path| path.ends_with("skip.part2.rar")));
        assert!(result
            .processed
            .iter()
            .any(|candidate| candidate.path == input));
        assert!(
            output.join("root").exists(),
            "root archive should materialize without a depth suffix"
        );
        assert!(
            !output.join("root-d0").exists(),
            "depth is candidate state, not part of the output directory name"
        );
        assert!(result
            .enqueued
            .iter()
            .any(|candidate| candidate.path.ends_with("nested.zip")));
        assert!(result
            .skipped
            .iter()
            .any(|candidate| candidate.path.ends_with("skip.part2.rar")));

        let ranked = PasswordRepository::new(db.connection())
            .ranked_candidates(10)
            .unwrap();
        assert_eq!(ranked[0].success_count, 2);

        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn extract_fails_when_output_target_already_exists() {
        let root =
            std::env::temp_dir().join(format!("smartzip-engine-collision-{}", std::process::id()));
        let input = root.join("root.zip");
        let output = root.join("out");
        // The layout planner targets output_root/archive_stem = out/root.
        // Pre-create it to trigger a collision after layout planning.
        let target = output.join("root");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(&input, b"not really a zip").unwrap();

        let backend = FakeBackend::default();
        let db = SmartZipDb::in_memory().unwrap();
        let service = PasswordService::new(PasswordRepository::new(db.connection()));

        let engine = SmartZipEngine::default();
        let result = engine
            .extract_recursive(
                &backend,
                &service,
                ExtractWorkflowRequest {
                    inputs: vec![input.clone()],
                    output_dir: output,
                    recursion_limit: 1,
                    encoding_mode: EncodingMode::Auto,
                    scanner: ScannerConfig::default(),
                    password_candidates: PasswordCandidateRequest::default(),
                    layout_policy: crate::layout::OutputLayoutPolicy::default(),
                    single_root_name_policy: crate::layout::SingleRootNamePolicy::default(),
                    embedded_scan_mode: smartzip_core::EmbeddedScanMode::default(),
                    dominant_min_ratio: 0.70,
                    confirm_large_scan: false,
                    force: false,
                },
                None,
                None,
            )
            .await
            .unwrap();

        // With collision-after-layout, backend IS called (extraction happens first),
        // but the collision is detected after layout planning and the archive is skipped.
        assert!(result
            .skipped
            .iter()
            .any(|candidate| candidate.path == input));
        assert!(result.events.iter().any(|event| matches!(
            event.kind,
            TaskEventKind::Failed { ref error } if error.contains("output path already exists")
        )));

        let _ = std::fs::remove_dir_all(root);
    }

    #[derive(Clone)]
    struct DelayedOutputPrompter {
        started: Arc<AtomicBool>,
    }

    #[async_trait]
    impl InteractiveOutputPrompter for DelayedOutputPrompter {
        async fn prompt(
            &self,
            _archive_path: PathBuf,
            _output_path: PathBuf,
        ) -> OutputCollisionStrategy {
            let started = self.started.clone();
            tokio::task::spawn_blocking(move || {
                started.store(true, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(200));
                OutputCollisionStrategy::Skip
            })
            .await
            .unwrap()
        }
    }

    #[tokio::test]
    async fn extract_keeps_other_archives_moving_while_prompt_waits() {
        let root =
            std::env::temp_dir().join(format!("smartzip-engine-prompt-{}", std::process::id()));
        let conflict = root.join("conflict.zip");
        let other = root.join("other.zip");
        let output = root.join("out");
        std::fs::create_dir_all(&output).unwrap();
        // The layout planner targets output_root/archive_stem = out/conflict.
        // Pre-create it to trigger a collision after layout planning.
        std::fs::create_dir_all(output.join("conflict")).unwrap();
        std::fs::write(&conflict, b"not really a zip").unwrap();
        std::fs::write(&other, b"not really a zip either").unwrap();

        let backend = FakeBackend::default();
        let db = SmartZipDb::in_memory().unwrap();
        let service = PasswordService::new(PasswordRepository::new(db.connection()));
        let prompt_started = Arc::new(AtomicBool::new(false));
        let output_prompter = DelayedOutputPrompter {
            started: prompt_started.clone(),
        };

        let engine = SmartZipEngine::default();
        let result = engine
            .extract_recursive(
                &backend,
                &service,
                ExtractWorkflowRequest {
                    inputs: vec![conflict.clone(), other.clone()],
                    output_dir: output,
                    recursion_limit: 1,
                    encoding_mode: EncodingMode::Auto,
                    scanner: ScannerConfig::default(),
                    password_candidates: PasswordCandidateRequest::default(),
                    layout_policy: crate::layout::OutputLayoutPolicy::default(),
                    single_root_name_policy: crate::layout::SingleRootNamePolicy::default(),
                    embedded_scan_mode: smartzip_core::EmbeddedScanMode::default(),
                    dominant_min_ratio: 0.70,
                    confirm_large_scan: false,
                    force: false,
                },
                None,
                Some(&output_prompter),
            )
            .await
            .unwrap();

        // conflict.zip → layout target = out/conflict (exists) → collision → Skip
        let calls = backend.calls.lock().unwrap().clone();
        assert!(calls.iter().any(|path| path.ends_with("conflict.zip")));
        assert!(result
            .skipped
            .iter()
            .any(|candidate| candidate.path == conflict));

        let _ = std::fs::remove_dir_all(root);
    }

    #[derive(Clone, Default)]
    struct EncodingAwareBackend {
        seen_test_encodings: Arc<Mutex<Vec<EncodingMode>>>,
        seen_extract_encodings: Arc<Mutex<Vec<EncodingMode>>>,
    }

    #[async_trait]
    impl ArchiveBackend for EncodingAwareBackend {
        async fn probe(&self, path: &std::path::Path) -> smartzip_core::Result<ArchiveProbe> {
            Ok(ArchiveProbe {
                path: path.to_path_buf(),
                format: Some(ArchiveFormat::Zip),
                encrypted: Some(false),
                supported: true,
            })
        }

        async fn list(&self, _request: ListRequest) -> smartzip_core::Result<ArchiveListing> {
            Ok(ArchiveListing {
                format: Some(ArchiveFormat::Zip),
                entries: Vec::new(),
            })
        }

        async fn test(&self, request: TestRequest) -> smartzip_core::Result<TestResult> {
            self.seen_test_encodings
                .lock()
                .unwrap()
                .push(request.encoding);
            Ok(TestResult {
                ok: true,
                encrypted: Some(false),
            })
        }

        async fn extract(
            &self,
            request: ExtractArchiveRequest,
        ) -> smartzip_core::Result<ExtractArchiveResult> {
            self.seen_extract_encodings
                .lock()
                .unwrap()
                .push(request.encoding);
            std::fs::create_dir_all(&request.output_dir).map_err(|source| {
                smartzip_core::SmartZipError::io(Some(request.output_dir.clone()), source)
            })?;
            Ok(ExtractArchiveResult {
                output_dir: request.output_dir,
            })
        }

        async fn compress(
            &self,
            request: CompressArchiveRequest,
        ) -> smartzip_core::Result<CompressArchiveResult> {
            Ok(CompressArchiveResult {
                output: request.output,
            })
        }

        fn capabilities(&self) -> BackendCapabilities {
            BackendCapabilities {
                can_extract: vec![ArchiveFormat::Zip],
                can_compress: vec![ArchiveFormat::Zip],
                supports_passwords: true,
                supports_listing: true,
                supports_test: true,
            }
        }
    }

    #[tokio::test]
    async fn explicit_encoding_override_is_preserved_for_test_and_extract() {
        let root =
            std::env::temp_dir().join(format!("smartzip-engine-encoding-{}", std::process::id()));
        let input = root.join("root.zip");
        let output = root.join("out");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(&input, b"not really a zip").unwrap();

        let backend = EncodingAwareBackend::default();
        let db = SmartZipDb::in_memory().unwrap();
        let service = PasswordService::new(PasswordRepository::new(db.connection()));

        let engine = SmartZipEngine::default();
        let result = engine
            .extract_recursive(
                &backend,
                &service,
                ExtractWorkflowRequest {
                    inputs: vec![input.clone()],
                    output_dir: output,
                    recursion_limit: 0,
                    encoding_mode: EncodingMode::Override("gbk".into()),
                    scanner: ScannerConfig::default(),
                    password_candidates: PasswordCandidateRequest::default(),
                    layout_policy: crate::layout::OutputLayoutPolicy::default(),
                    single_root_name_policy: crate::layout::SingleRootNamePolicy::default(),
                    embedded_scan_mode: smartzip_core::EmbeddedScanMode::default(),
                    dominant_min_ratio: 0.70,
                    confirm_large_scan: false,
                    force: false,
                },
                None,
                None,
            )
            .await
            .unwrap();

        assert_eq!(result.processed.len(), 1);
        assert_eq!(
            backend.seen_test_encodings.lock().unwrap().as_slice(),
            &[EncodingMode::Override("gbk".into())]
        );
        assert_eq!(
            backend.seen_extract_encodings.lock().unwrap().as_slice(),
            &[EncodingMode::Override("gbk".into())]
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[derive(Clone, Default)]
    struct FailingTestBackend;

    #[async_trait]
    impl ArchiveBackend for FailingTestBackend {
        async fn probe(&self, path: &std::path::Path) -> smartzip_core::Result<ArchiveProbe> {
            Ok(ArchiveProbe {
                path: path.to_path_buf(),
                format: Some(ArchiveFormat::Zip),
                encrypted: Some(true),
                supported: true,
            })
        }

        async fn list(&self, _request: ListRequest) -> smartzip_core::Result<ArchiveListing> {
            Ok(ArchiveListing {
                format: Some(ArchiveFormat::Zip),
                entries: Vec::new(),
            })
        }

        async fn test(&self, request: TestRequest) -> smartzip_core::Result<TestResult> {
            Err(smartzip_core::SmartZipError::BackendFailed {
                backend: "test-backend".into(),
                exit_code: Some(2),
                stderr: format!("i/o failure while testing {}", request.archive.display()),
            })
        }

        async fn extract(
            &self,
            request: ExtractArchiveRequest,
        ) -> smartzip_core::Result<ExtractArchiveResult> {
            Ok(ExtractArchiveResult {
                output_dir: request.output_dir,
            })
        }

        async fn compress(
            &self,
            request: CompressArchiveRequest,
        ) -> smartzip_core::Result<CompressArchiveResult> {
            Ok(CompressArchiveResult {
                output: request.output,
            })
        }

        fn capabilities(&self) -> BackendCapabilities {
            BackendCapabilities {
                can_extract: vec![ArchiveFormat::Zip],
                can_compress: vec![ArchiveFormat::Zip],
                supports_passwords: true,
                supports_listing: true,
                supports_test: true,
            }
        }
    }

    #[tokio::test]
    async fn backend_failures_do_not_record_password_failures() {
        let root = std::env::temp_dir().join(format!(
            "smartzip-engine-backend-fail-{}",
            std::process::id()
        ));
        let input = root.join("root.zip");
        let output = root.join("out");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(&input, b"not really a zip").unwrap();

        let backend = FailingTestBackend;
        let db = SmartZipDb::in_memory().unwrap();
        let repo = PasswordRepository::new(db.connection());
        let password_id = repo
            .upsert(smartzip_db::password::NewPassword {
                value: "candidate-password",
                source: "test",
                pinned: false,
            })
            .unwrap();
        let service = PasswordService::new(PasswordRepository::new(db.connection()));

        let engine = SmartZipEngine::default();
        let result = engine
            .extract_recursive(
                &backend,
                &service,
                ExtractWorkflowRequest {
                    inputs: vec![input],
                    output_dir: output,
                    recursion_limit: 0,
                    encoding_mode: EncodingMode::Auto,
                    scanner: ScannerConfig::default(),
                    password_candidates: PasswordCandidateRequest {
                        manual: Vec::new(),
                        clipboard: None,
                        include_empty: false,
                        limit: 8,
                    },
                    layout_policy: crate::layout::OutputLayoutPolicy::default(),
                    single_root_name_policy: crate::layout::SingleRootNamePolicy::default(),
                    embedded_scan_mode: smartzip_core::EmbeddedScanMode::default(),
                    dominant_min_ratio: 0.70,
                    confirm_large_scan: false,
                    force: false,
                },
                None,
                None,
            )
            .await
            .unwrap();

        assert!(result.processed.is_empty());
        let stored = PasswordRepository::new(db.connection())
            .ranked_candidates(10)
            .unwrap()
            .into_iter()
            .find(|record| record.id == password_id)
            .unwrap();
        assert_eq!(stored.failure_count, 0);

        let _ = std::fs::remove_dir_all(root);
    }

    #[derive(Default, Clone)]
    struct FakeBackend {
        calls: Arc<Mutex<Vec<String>>>,
    }

    #[derive(Default, Clone)]
    struct BatchPasswordBackend {
        attempted_passwords: Arc<Mutex<Vec<Option<String>>>>,
    }

    #[async_trait]
    impl ArchiveBackend for BatchPasswordBackend {
        async fn probe(&self, path: &Path) -> smartzip_core::Result<ArchiveProbe> {
            Ok(ArchiveProbe {
                path: path.to_path_buf(),
                format: Some(ArchiveFormat::Zip),
                encrypted: Some(true),
                supported: true,
            })
        }

        async fn list(&self, _request: ListRequest) -> smartzip_core::Result<ArchiveListing> {
            Ok(ArchiveListing {
                format: Some(ArchiveFormat::Zip),
                entries: Vec::new(),
            })
        }

        async fn test(&self, request: TestRequest) -> smartzip_core::Result<TestResult> {
            self.attempted_passwords
                .lock()
                .unwrap()
                .push(request.password.clone());
            if request.password.as_deref() == Some("batch-secret") {
                Ok(TestResult {
                    ok: true,
                    encrypted: Some(true),
                })
            } else {
                Err(smartzip_core::SmartZipError::WrongPassword {
                    path: request.archive,
                })
            }
        }

        async fn extract(
            &self,
            request: ExtractArchiveRequest,
        ) -> smartzip_core::Result<ExtractArchiveResult> {
            std::fs::create_dir_all(&request.output_dir).map_err(|source| {
                smartzip_core::SmartZipError::io(Some(request.output_dir.clone()), source)
            })?;
            std::fs::write(request.output_dir.join("content.txt"), b"content").map_err(
                |source| smartzip_core::SmartZipError::io(Some(request.output_dir.clone()), source),
            )?;
            Ok(ExtractArchiveResult {
                output_dir: request.output_dir,
            })
        }

        async fn compress(
            &self,
            request: CompressArchiveRequest,
        ) -> smartzip_core::Result<CompressArchiveResult> {
            Ok(CompressArchiveResult {
                output: request.output,
            })
        }

        fn capabilities(&self) -> BackendCapabilities {
            BackendCapabilities {
                can_extract: vec![ArchiveFormat::Zip],
                can_compress: vec![ArchiveFormat::Zip],
                supports_passwords: true,
                supports_listing: true,
                supports_test: true,
            }
        }
    }

    struct CountingPasswordPrompter {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl InteractivePasswordPrompter for CountingPasswordPrompter {
        async fn prompt(&self, _archive_path: &Path) -> Option<String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Some("batch-secret".to_string())
        }
    }

    #[tokio::test]
    async fn interactive_password_is_reused_for_later_files_in_same_batch() {
        let root = tempfile::tempdir().unwrap();
        let first = root.path().join("first.zip");
        let second = root.path().join("second.zip");
        std::fs::write(&first, b"first archive").unwrap();
        std::fs::write(&second, b"second archive").unwrap();

        let backend = BatchPasswordBackend::default();
        let db = SmartZipDb::in_memory().unwrap();
        let service = PasswordService::new(PasswordRepository::new(db.connection()));
        let prompter = CountingPasswordPrompter {
            calls: AtomicUsize::new(0),
        };

        let result = SmartZipEngine::default()
            .extract_recursive_interactive(
                &backend,
                &service,
                ExtractWorkflowRequest {
                    inputs: vec![first, second],
                    output_dir: root.path().join("out"),
                    recursion_limit: 0,
                    encoding_mode: EncodingMode::Auto,
                    scanner: ScannerConfig::default(),
                    password_candidates: PasswordCandidateRequest {
                        manual: Vec::new(),
                        clipboard: None,
                        include_empty: false,
                        limit: 8,
                    },
                    layout_policy: Default::default(),
                    single_root_name_policy: Default::default(),
                    embedded_scan_mode: smartzip_core::EmbeddedScanMode::default(),
                    dominant_min_ratio: 0.70,
                    confirm_large_scan: false,
                    force: false,
                },
                Some(&prompter),
                None,
                None,
                None,
            )
            .await
            .unwrap();

        assert_eq!(result.processed.len(), 2);
        assert_eq!(
            prompter.calls.load(Ordering::SeqCst),
            1,
            "the password accepted for the first file should be reused in-memory for the second",
        );
        assert_eq!(
            backend.attempted_passwords.lock().unwrap().as_slice(),
            &[
                Some("batch-secret".to_string()),
                Some("batch-secret".to_string())
            ],
        );
        let stored = PasswordRepository::new(db.connection())
            .get_by_value("batch-secret")
            .unwrap()
            .expect("the first interactive success should be persisted immediately");
        assert_eq!(stored.success_count, 2);
    }

    #[async_trait]
    impl ArchiveBackend for FakeBackend {
        async fn probe(&self, path: &std::path::Path) -> smartzip_core::Result<ArchiveProbe> {
            Ok(ArchiveProbe {
                path: path.to_path_buf(),
                format: format_from_extension(path),
                encrypted: Some(true),
                supported: true,
            })
        }

        async fn list(&self, _request: ListRequest) -> smartzip_core::Result<ArchiveListing> {
            Ok(ArchiveListing {
                format: Some(ArchiveFormat::Zip),
                entries: Vec::new(),
            })
        }

        async fn test(&self, _request: TestRequest) -> smartzip_core::Result<TestResult> {
            Ok(TestResult {
                ok: true,
                encrypted: Some(true),
            })
        }

        async fn extract(
            &self,
            request: ExtractArchiveRequest,
        ) -> smartzip_core::Result<ExtractArchiveResult> {
            self.calls
                .lock()
                .unwrap()
                .push(request.archive.display().to_string());
            std::fs::create_dir_all(&request.output_dir).map_err(|source| {
                smartzip_core::SmartZipError::io(Some(request.output_dir.clone()), source)
            })?;
            // Always create a file so the layout planner sees a non-Empty shape
            std::fs::write(request.output_dir.join("extracted.txt"), b"content").map_err(
                |source| smartzip_core::SmartZipError::io(Some(request.output_dir.clone()), source),
            )?;
            if request.archive.file_name().and_then(|name| name.to_str()) == Some("root.zip") {
                std::fs::write(request.output_dir.join("nested.zip"), b"nested").map_err(
                    |source| {
                        smartzip_core::SmartZipError::io(Some(request.output_dir.clone()), source)
                    },
                )?;
            }
            Ok(ExtractArchiveResult {
                output_dir: request.output_dir,
            })
        }

        async fn compress(
            &self,
            request: CompressArchiveRequest,
        ) -> smartzip_core::Result<CompressArchiveResult> {
            Ok(CompressArchiveResult {
                output: request.output,
            })
        }

        fn capabilities(&self) -> BackendCapabilities {
            BackendCapabilities {
                can_extract: vec![ArchiveFormat::Zip],
                can_compress: vec![ArchiveFormat::Zip],
                supports_passwords: true,
                supports_listing: true,
                supports_test: true,
            }
        }
    }

    #[tokio::test]
    async fn extract_via_real_seven_zip_with_smart_output() {
        let root = std::env::temp_dir().join(format!("smartzip-int-{}", std::process::id()));
        let archive = root.join("test.zip");
        let extracted_file = root.join("hello.txt");
        let output = root.join("out");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(&extracted_file, b"hello world").unwrap();

        let status = std::process::Command::new("7z")
            .arg("a")
            .arg(&archive)
            .arg(&extracted_file)
            .current_dir(&root)
            .status()
            .unwrap();
        assert!(status.success(), "7z must be available in PATH");
        std::fs::remove_file(&extracted_file).unwrap();

        let backend = SevenZipBackend::locate(&smartzip_archive::SevenZipLocator::default())
            .expect("7z/7zz must be available");
        let db = SmartZipDb::in_memory().unwrap();
        let service = PasswordService::new(PasswordRepository::new(db.connection()));

        let engine = SmartZipEngine::default();
        let result = engine
            .extract_recursive(
                &backend,
                &service,
                ExtractWorkflowRequest {
                    inputs: vec![archive.clone()],
                    output_dir: output.clone(),
                    recursion_limit: 1,
                    encoding_mode: EncodingMode::Auto,
                    scanner: ScannerConfig::default(),
                    password_candidates: PasswordCandidateRequest::default(),
                    layout_policy: crate::layout::OutputLayoutPolicy::default(),
                    single_root_name_policy: crate::layout::SingleRootNamePolicy::default(),
                    embedded_scan_mode: smartzip_core::EmbeddedScanMode::default(),
                    dominant_min_ratio: 0.70,
                    confirm_large_scan: false,
                    force: false,
                },
                None,
                None,
            )
            .await
            .unwrap();

        assert_eq!(result.processed.len(), 1);

        // Verify the extracted content exists somewhere under output.
        let candidates = [
            output.join("hello.txt"),
            output.join("test").join("hello.txt"),
            output.join("test.txt"),
        ];
        assert!(
            candidates.iter().any(|p| p.exists()),
            "expected hello.txt in one of {:?}",
            candidates
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn embedded_archive_is_carved_before_extraction_and_recurses() {
        let root = std::env::temp_dir().join(format!("smartzip-embedded-{}", std::process::id()));
        let archive = root.join("payload.zip");
        let disguised = root.join("photo.jpg");
        let output = root.join("out");
        std::fs::create_dir_all(&root).unwrap();

        let payload = root.join("payload.txt");
        std::fs::write(&payload, b"payload").unwrap();
        let status = std::process::Command::new("7z")
            .arg("a")
            .arg(&archive)
            .arg(&payload)
            .current_dir(&root)
            .status()
            .unwrap();
        assert!(status.success(), "7z must be available in PATH");

        let mut composite = Vec::from(&b"JPEG-HEADER"[..]);
        composite.extend_from_slice(&std::fs::read(&archive).unwrap());
        std::fs::write(&disguised, composite).unwrap();

        let backend = EmbeddedAwareFakeBackend::default();
        let db = SmartZipDb::in_memory().unwrap();
        let service = PasswordService::new(PasswordRepository::new(db.connection()));

        let engine = engine_with_test_recycler().with_min_embedded_size_bytes(0);
        let result = engine
            .extract_recursive(
                &backend,
                &service,
                ExtractWorkflowRequest {
                    inputs: vec![disguised.clone()],
                    output_dir: output.clone(),
                    recursion_limit: 2,
                    encoding_mode: EncodingMode::Auto,
                    scanner: ScannerConfig::default(),
                    password_candidates: PasswordCandidateRequest::default(),
                    layout_policy: crate::layout::OutputLayoutPolicy::default(),
                    single_root_name_policy: crate::layout::SingleRootNamePolicy::default(),
                    embedded_scan_mode: smartzip_core::EmbeddedScanMode::Aggressive,
                    dominant_min_ratio: 0.70,
                    confirm_large_scan: false,
                    force: false,
                },
                None,
                None,
            )
            .await
            .unwrap();

        let calls = backend.calls.lock().unwrap().clone();
        assert_eq!(
            calls.len(),
            2,
            "expected root archive and nested archive calls"
        );
        assert_ne!(calls[0].0, disguised.display().to_string());
        assert!(calls[0].1, "carved archive should start with zip magic");
        assert!(calls[1].0.ends_with("nested.zip"));
        assert!(result
            .enqueued
            .iter()
            .any(|candidate| candidate.path.ends_with("nested.zip")));

        let _ = std::fs::remove_dir_all(root);
    }

    #[derive(Default, Clone)]
    struct EmbeddedAwareFakeBackend {
        calls: Arc<Mutex<Vec<(String, bool)>>>,
    }

    #[async_trait]
    impl ArchiveBackend for EmbeddedAwareFakeBackend {
        async fn probe(&self, path: &std::path::Path) -> smartzip_core::Result<ArchiveProbe> {
            Ok(ArchiveProbe {
                path: path.to_path_buf(),
                format: format_from_extension(path),
                encrypted: Some(true),
                supported: true,
            })
        }

        async fn list(&self, _request: ListRequest) -> smartzip_core::Result<ArchiveListing> {
            Ok(ArchiveListing {
                format: Some(ArchiveFormat::Zip),
                entries: Vec::new(),
            })
        }

        async fn test(&self, _request: TestRequest) -> smartzip_core::Result<TestResult> {
            Ok(TestResult {
                ok: true,
                encrypted: Some(true),
            })
        }

        async fn extract(
            &self,
            request: ExtractArchiveRequest,
        ) -> smartzip_core::Result<ExtractArchiveResult> {
            let starts_with_zip = std::fs::read(&request.archive)
                .map(|bytes| bytes.starts_with(b"PK"))
                .unwrap_or(false);
            self.calls
                .lock()
                .unwrap()
                .push((request.archive.display().to_string(), starts_with_zip));
            std::fs::create_dir_all(&request.output_dir).map_err(|source| {
                smartzip_core::SmartZipError::io(Some(request.output_dir.clone()), source)
            })?;
            if self.calls.lock().unwrap().len() == 1 {
                std::fs::write(request.output_dir.join("nested.zip"), b"nested").map_err(
                    |source| {
                        smartzip_core::SmartZipError::io(Some(request.output_dir.clone()), source)
                    },
                )?;
                std::fs::write(request.output_dir.join("readme.txt"), b"readme").map_err(
                    |source| {
                        smartzip_core::SmartZipError::io(Some(request.output_dir.clone()), source)
                    },
                )?;
            }
            Ok(ExtractArchiveResult {
                output_dir: request.output_dir,
            })
        }

        async fn compress(
            &self,
            request: CompressArchiveRequest,
        ) -> smartzip_core::Result<CompressArchiveResult> {
            Ok(CompressArchiveResult {
                output: request.output,
            })
        }

        fn capabilities(&self) -> BackendCapabilities {
            BackendCapabilities {
                can_extract: vec![ArchiveFormat::Zip],
                can_compress: vec![ArchiveFormat::Zip],
                supports_passwords: true,
                supports_listing: true,
                supports_test: true,
            }
        }
    }
}
