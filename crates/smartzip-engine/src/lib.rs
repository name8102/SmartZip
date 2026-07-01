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
use smartzip_scanner::{EmbeddedArchiveFinding, EmbeddedScanner, ScannerConfig};
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

pub struct SmartZipEngine {
    scanner: EmbeddedScanner,
    archive_recycler: ArchiveRecycleHandler,
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

    pub fn detect(&self, request: DetectRequest) -> std::io::Result<DetectResult> {
        let task_id = TaskId::new();
        let mut events = vec![TaskEvent::started(task_id.clone())];

        let scanner = if request.scanner == *self.scanner.config() {
            None
        } else {
            Some(EmbeddedScanner::new(request.scanner.clone()))
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
        let scanner = if request.scanner == *self.scanner.config() {
            None
        } else {
            Some(EmbeddedScanner::new(request.scanner.clone()))
        };
        let scanner = scanner.as_ref().unwrap_or(&self.scanner);

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
        let embedded_policy = embedded_policy_from_request(&request);
        let nested_embedded_enabled = matches!(
            embedded_policy.mode,
            smartzip_core::EmbeddedScanMode::Aggressive | smartzip_core::EmbeddedScanMode::All
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

        let collision_resolver = output_prompter.map(|p| make_collision_resolver(p));

        // History: register the task up-front and accumulate metrics as the
        // loop runs. All history writes are best-effort — a repo error becomes
        // a Warning event through the recorder and never aborts extraction.
        if let Some(recorder) = history {
            let summary = summarize_inputs(&request.inputs);
            recorder.start_extract(&task_id, &summary, Some(&request.output_dir));
        }
        let mut hist_password_attempts: i64 = 0;
        let mut hist_encoding_selected: Option<String> = None;
        let mut hist_embedded_found: i64 = 0;
        let mut hist_last_output: Option<PathBuf> = None;
        let mut hist_saw_failure = false;

        loop {
            let Some(mut candidate) = queue.pop_front() else {
                break;
            };
            let key = candidate_key(&candidate);
            let is_new = seen.insert(key);
            if !is_new
                || candidate.depth > request.recursion_limit
                || !is_first_volume(&candidate.path)
            {
                skipped.push(candidate);
                continue;
            }

            // Header-based detection first, then scanner confirmation
            let header_result = crate::detect::probe_file_header(&candidate.path);
            let _has_non_archive_header = {
                let mut file = match std::fs::File::open(&candidate.path) {
                    Ok(f) => f,
                    Err(_) => {
                        return Err(smartzip_core::SmartZipError::BackendFailed {
                            backend: "detect".into(),
                            exit_code: None,
                            stderr: format!("cannot open {}", candidate.path.display()),
                        })
                    }
                };
                let mut buf = [0u8; 8192];
                let n = file.read(&mut buf).unwrap_or(0);
                crate::detect::detect_non_archive_header(&buf[..n])
            };

            let findings = if should_scan_candidate_for_embedded(
                &candidate,
                &embedded_policy,
                nested_embedded_enabled,
                request.confirm_large_scan,
                &events,
                &task_id,
            ) {
                scanner.scan_path(&candidate.path).unwrap_or_default()
            } else {
                Vec::new()
            };

            if !findings.is_empty() {
                if let Some(recorder) = history {
                    recorder.record_embedded_findings(&task_id, &candidate.path, &findings);
                }
                hist_embedded_found += findings.len() as i64;
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
                                    skipped.push(candidate);
                                    continue;
                                }
                            },
                            None => {
                                skipped.push(candidate);
                                continue;
                            }
                        }
                    }
                    _ => {
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
                skipped.push(candidate);
                continue;
            }

            // Business container filter for root inputs: nested candidates are
            // filtered in discover_nested_candidates, but root inputs (a .docx
            // dropped straight in, or a plain .zip whose contents match docx
            // structure) reach the main loop directly.
            if candidate.detected_format == Some(ArchiveFormat::Zip) {
                if let Some(kind) =
                    ext_business_container_kind(&candidate.path).or_else(|| {
                        crate::container::classify_zip_path(&candidate.path)
                    })
                {
                    events.push(TaskEvent {
                        task_id: task_id.clone(),
                        kind: TaskEventKind::BusinessContainerSkipped {
                            path: candidate.path.clone(),
                            kind: format!("{kind:?}"),
                        },
                    });
                    skipped.push(candidate);
                    continue;
                }
            }

            let archive_input = materialize_archive_input(&candidate)?;
            let archive_path = archive_input.path.clone();

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

            let mut zip_encoding_assessment = None;
            if request.encoding_mode == EncodingMode::Auto
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
                if let Some(recorder) = history {
                    recorder.record_encoding_detection(
                        &task_id,
                        &candidate.path,
                        candidate.detected_format.as_ref().map(|f| f.as_str()),
                        &assessment.context.detected,
                        Some(&assessment.context),
                        false,
                    );
                    hist_encoding_selected =
                        Some(encoding_mode_label(&assessment.context.detected.selected));
                }
            }

            let _key = candidate_key(&candidate);
            let output_dir = output_dir_for_candidate(&request.output_dir, &candidate);

            let mut extracted = false;
            let mut terminal_skip = false;
            let mut last_error = None;
            let mut saw_wrong_password = false;
            let mut actual_output_dir = output_dir.clone();
            let test_before_extract = backend
                .should_test_before_extract(&archive_path, candidate.detected_format.as_ref());
            let total_password_attempts = password_candidates.len();
            for password in &password_candidates {
                let pw_value = password_value(password);
                let attempt_index = password_attempt_index(password, &password_candidates);
                hist_password_attempts += 1;
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
                            encoding: request.encoding_mode.clone(),
                        }),
                    )
                    .await
                    {
                        Ok(result) if result.ok => {
                            let matched_password_id = passwords.record_success(password).ok().flatten();
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
                            if let Some(recorder) = history {
                                record_password_match_success(
                                    recorder,
                                    matched_password_id,
                                    &candidate,
                                );
                            }

                            if zip_encoding_assessment.is_none()
                                && request.encoding_mode == EncodingMode::Auto
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
                                    if let Some(recorder) = history {
                                        recorder.record_encoding_detection(
                                            &task_id,
                                            &candidate.path,
                                            candidate
                                                .detected_format
                                                .as_ref()
                                                .map(|f| f.as_str()),
                                            &assessment.context.detected,
                                            Some(&assessment.context),
                                            false,
                                        );
                                    }
                                    hist_encoding_selected =
                                        Some(encoding_mode_label(&assessment.context.detected.selected));
                                }
                            }

                            let encoding_to_use = resolve_encoding_mode(
                                &archive_path,
                                request.encoding_mode.clone(),
                                zip_encoding_assessment.as_ref(),
                                encoding_prompter,
                            )
                            .await?;
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
                    let extract_archive_path = archive_path.clone();
                    let extract_format = candidate.detected_format.clone();
                    let extract_password = pw_value.clone();
                    let extract_encoding = request.encoding_mode.clone();
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
                            if let Some(recorder) = history {
                                record_password_match_success(
                                    recorder,
                                    matched_password_id,
                                    &candidate,
                                );
                            }
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
                    if let Some(interactive_pw) = prompter.prompt(&candidate.path).await {
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
                                        encoding: request.encoding_mode.clone(),
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
                                            && request.encoding_mode == EncodingMode::Auto
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
                                            request.encoding_mode.clone(),
                                            zip_encoding_assessment.as_ref(),
                                            encoding_prompter,
                                        )
                                        .await?;
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
                                                let _ = passwords
                                                    .record_success(&PasswordCandidate {
                                                    id: None,
                                                    value: pw.clone(),
                                                    source:
                                                        smartzip_passwords::PasswordSource::Manual,
                                                });
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
                                    request.encoding_mode.clone(),
                                    zip_encoding_assessment.as_ref(),
                                    encoding_prompter,
                                )
                                .await?;
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
                                        let _ = passwords.record_success(&PasswordCandidate {
                                            id: None,
                                            value: pw.clone(),
                                            source: smartzip_passwords::PasswordSource::Manual,
                                        });
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
                if let Some(error) = last_error.or_else(|| {
                    saw_wrong_password.then(|| smartzip_core::SmartZipError::WrongPassword {
                        path: candidate.path.clone(),
                    })
                }) {
                    hist_saw_failure = true;
                    let event = TaskEvent::failed(task_id.clone(), &error);
                    if let Some(recorder) = history {
                        recorder.record_event(&task_id, &event);
                    }
                    events.push(event);
                }
            }
            if terminal_skip || !extracted {
                skipped.push(candidate);
                continue;
            }

            let output_event = TaskEvent {
                task_id: task_id.clone(),
                kind: TaskEventKind::OutputCreated {
                    path: actual_output_dir.clone(),
                },
            };
            if let Some(recorder) = history {
                recorder.record_event(&task_id, &output_event);
            }
            events.push(output_event);
            hist_last_output = Some(actual_output_dir.clone());

            processed.push(candidate.clone());
            let output_relative_path = candidate_output_relative_path(&candidate);
            let nested_candidates = discover_nested_candidates(
                scanner,
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
        // out the task row with aggregated metrics. Detection-table rows and
        // password_matches were written inline above where path context was
        // available; this final pass only handles task_events + finish.
        if let Some(recorder) = history {
            for event in &snapshot {
                recorder.record_event(&task_id, event);
            }
            let status = if processed.is_empty() {
                crate::history::TaskCompletionStatus::Failed
            } else if hist_saw_failure {
                crate::history::TaskCompletionStatus::Partial
            } else {
                crate::history::TaskCompletionStatus::Completed
            };
            recorder.finish(
                &task_id,
                crate::history::TaskOutcome {
                    status,
                    error_code: None,
                    error_message: None,
                    password_attempts: hist_password_attempts,
                    encoding_selected: hist_encoding_selected.as_deref(),
                    embedded_found: hist_embedded_found,
                    output_path: hist_last_output.as_deref(),
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

/// Summarize root inputs for the `tasks.input_summary` column. Joins the file
/// names with commas and appends "…and N more" once the list grows long so the
/// column stays compact for many-input batch runs.
fn summarize_inputs(inputs: &[PathBuf]) -> String {
    const MAX_SHOWN: usize = 5;
    let names: Vec<String> = inputs
        .iter()
        .map(|p| {
            p.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| p.to_string_lossy().into_owned())
        })
        .collect();
    if names.len() <= MAX_SHOWN {
        return names.join(", ");
    }
    let shown = names[..MAX_SHOWN].join(", ");
    format!("{shown}, …and {} more", names.len() - MAX_SHOWN)
}

/// Human-readable label for an [`EncodingMode`], used for the
/// `tasks.encoding_selected` column.
fn encoding_mode_label(mode: &EncodingMode) -> String {
    match mode {
        EncodingMode::Auto => "auto".to_string(),
        EncodingMode::Override(name) => name.clone(),
    }
}

/// Backfill a `password_matches` success row for the archive that a password
/// just unlocked. No-op when the password had no database id (empty password
/// or a not-yet-persisted manual candidate).
fn record_password_match_success(
    recorder: &dyn crate::history::TaskHistoryRecorder,
    password_id: Option<i64>,
    candidate: &ExtractionCandidate,
) {
    let Some(id) = password_id else {
        return;
    };
    let format = candidate.detected_format.as_ref().map(|f| f.as_str());
    let pattern = crate::history::normalize_filename_pattern(&candidate.path);
    recorder.record_password_match(Some(id), format, pattern.as_deref(), true);
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

fn output_dir_for_candidate(base: &Path, candidate: &ExtractionCandidate) -> PathBuf {
    match candidate.source {
        CandidateSource::RootInput => base.join(candidate_output_relative_path(candidate)),
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
        "cbz" => Some(smartzip_core::BusinessContainerKind::Cbz),
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
        let findings = scanner.scan_path(&path).unwrap_or_default();
        if findings.is_empty() {
            continue;
        }
        if matches!(policy.mode, smartzip_core::EmbeddedScanMode::All) {
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
        atomic::{AtomicBool, Ordering},
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

        let engine = engine_with_test_recycler();
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
                    embedded_scan_mode: smartzip_core::EmbeddedScanMode::default(),
                    dominant_min_ratio: 0.70,
                    confirm_large_scan: false,
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
