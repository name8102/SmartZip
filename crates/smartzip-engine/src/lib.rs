//! Application-level orchestration for SmartZip workflows.

pub mod container;
pub mod detect;
pub mod embedded;
pub mod embedded_zip;
mod materialize;
pub mod layout;
pub mod name_score;

use async_trait::async_trait;
use futures_util::FutureExt;
use materialize::{CollisionAction, CollisionResolver, CommitPolicy, MaterializeRequest, OutputMaterializer};
use serde::{Deserialize, Serialize};
use smartzip_archive::{ArchiveBackend, ExtractArchiveRequest, ListRequest, TestRequest};
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
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtractWorkflowRequest {
    pub inputs: Vec<PathBuf>,
    pub output_dir: PathBuf,
    pub recursion_limit: u8,
    pub encoding_mode: EncodingMode,
    pub scanner: ScannerConfig,
    pub password_candidates: PasswordCandidateRequest,
    pub layout_policy: crate::layout::OutputLayoutPolicy,
    pub single_root_name_policy: crate::layout::SingleRootNamePolicy,
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

impl SmartZipEngine {
    pub fn new(scanner: EmbeddedScanner) -> Self {
        Self { scanner }
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
        let task_id = TaskId::new();
        let scanner = if request.scanner == *self.scanner.config() {
            None
        } else {
            Some(EmbeddedScanner::new(request.scanner.clone()))
        };
        let scanner = scanner.as_ref().unwrap_or(&self.scanner);

        let mut events = vec![TaskEvent::started(task_id.clone())];
        let mut queue = VecDeque::new();
        let mut seen = HashSet::new();
        let mut processed = Vec::new();
        let mut skipped = Vec::new();
        let mut enqueued = Vec::new();
        let output_materializer = OutputMaterializer::default();

        for input in &request.inputs {
            let relative_path = archive_output_name(input);
            queue.push_back(ExtractionCandidate {
                detected_format: format_from_extension(input),
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

        let collision_resolver = output_prompter
            .map(|p| make_collision_resolver(p));

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
                    Err(_) => return Err(smartzip_core::SmartZipError::BackendFailed {
                        backend: "detect".into(),
                        exit_code: None,
                        stderr: format!("cannot open {}", candidate.path.display()),
                    }),
                };
                let mut buf = [0u8; 8192];
                let n = file.read(&mut buf).unwrap_or(0);
                crate::detect::detect_non_archive_header(&buf[..n])
            };

            let findings = scanner.scan_path(&candidate.path).unwrap_or_default();

            // Use dominant selector for embedded findings
            if !findings.is_empty() {
                let ext_is_archive =
                    crate::format_from_extension(&candidate.path).is_some();
                let file_size =
                    std::fs::metadata(&candidate.path).map(|m| m.len()).unwrap_or(0);
                let decision = crate::embedded::select_embedded_action(
                    file_size,
                    &findings,
                    &smartzip_core::EmbeddedScanPolicy::default(),
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
                        if let Some(idx) = decision.selected_index {
                            let f = &findings[idx];
                            candidate.detected_format = Some(f.format.clone());
                            candidate.embedded_offset = Some(f.offset);
                            candidate.embedded_size = f.size;
                        }
                    }
                    _ => {
                        // Skip or report — don't extract
                        if candidate.detected_format.is_none() {
                            candidate.detected_format =
                                crate::format_from_extension(&candidate.path);
                        }
                    }
                }
            } else if candidate.detected_format.is_none() {
                // Fallback to extension
                candidate.detected_format = crate::format_from_extension(&candidate.path);
                // Also try header detection
                if candidate.detected_format.is_none() {
                    if let Some((fmt, offset)) = header_result {
                        candidate.detected_format = Some(fmt);
                        if offset > 0 {
                            candidate.embedded_offset = Some(offset);
                        }
                    }
                }
            }

            if candidate.detected_format.is_none() {
                skipped.push(candidate);
                continue;
            }

            let archive_input = materialize_archive_input(&candidate)?;
            let archive_path = archive_input.path.clone();

            events.push(TaskEvent {
                task_id: task_id.clone(),
                kind: TaskEventKind::Progress(smartzip_core::TaskProgress::indeterminate(format!(
                    "Extracting {} at depth {}",
                    candidate.path.display(),
                    candidate.depth
                ))),
            });

            let mut encoding_result = None;
            if request.encoding_mode == EncodingMode::Auto {
                if let Ok(listing) = backend
                    .list(ListRequest {
                        archive: archive_path.clone(),
                        format: candidate.detected_format.clone(),
                        password: Some(String::new()),
                        encoding: EncodingMode::Auto,
                    })
                    .await
                {
                    let raw_names: Vec<u8> = listing
                        .entries
                        .iter()
                        .flat_map(|entry| entry.raw_name.iter().copied())
                        .collect();
                    if !raw_names.is_empty() {
                        let mut detector = smartzip_encoding::ArchiveEncodingDetector::new();
                        let result = detector.detect(&raw_names);
                        events.push(TaskEvent {
                            task_id: task_id.clone(),
                            kind: TaskEventKind::EncodingDetected(
                                smartzip_core::EncodingDetectionResult {
                                    selected: EncodingMode::Override(result.selected.clone()),
                                    confidence: result.confidence,
                                    candidates: result
                                        .candidates
                                        .iter()
                                        .map(|c| smartzip_core::EncodingCandidate {
                                            name: c.name.clone(),
                                            confidence: c.confidence,
                                        })
                                        .collect(),
                                },
                            ),
                        });
                        encoding_result = Some(result);
                    }
                }
            }

            let _key = candidate_key(&candidate);
            let output_dir = output_dir_for_candidate(&request.output_dir, &candidate);

            let mut extracted = false;
            let mut terminal_skip = false;
            let mut last_error = None;
            let mut actual_output_dir = output_dir.clone();
            for password in &password_candidates {
                // B3: Test-first — use test() to check password, then extract once
                let pw_value = password_value(password);
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
                        let _ = passwords.record_success(password);

                        // B2: If pre-list failed (encrypted archive), detect encoding now
                        if encoding_result.is_none() && request.encoding_mode == EncodingMode::Auto
                        {
                            if let Ok(listing) = backend_call(
                                "archive-backend",
                                "list",
                                &archive_path,
                                backend.list(ListRequest {
                                    archive: archive_path.clone(),
                                    format: candidate.detected_format.clone(),
                                    password: pw_value.clone(),
                                    encoding: EncodingMode::Auto,
                                }),
                            )
                            .await
                            {
                                let raw_names: Vec<u8> = listing
                                    .entries
                                    .iter()
                                    .flat_map(|entry| entry.raw_name.iter().copied())
                                    .collect();
                                if !raw_names.is_empty() {
                                    let mut detector =
                                        smartzip_encoding::ArchiveEncodingDetector::new();
                                    let result = detector.detect(&raw_names);
                                    events.push(TaskEvent {
                                        task_id: task_id.clone(),
                                        kind: TaskEventKind::EncodingDetected(
                                            smartzip_core::EncodingDetectionResult {
                                                selected: EncodingMode::Override(
                                                    result.selected.clone(),
                                                ),
                                                confidence: result.confidence,
                                                candidates: result
                                                    .candidates
                                                    .iter()
                                                    .map(|c| smartzip_core::EncodingCandidate {
                                                        name: c.name.clone(),
                                                        confidence: c.confidence,
                                                    })
                                                    .collect(),
                                            },
                                        ),
                                    });
                                    encoding_result = Some(result);
                                }
                            }
                        }

                        let encoding_to_use = encoding_result
                            .as_ref()
                            .map(|r| EncodingMode::Override(r.selected.clone()))
                            .unwrap_or_else(|| request.encoding_mode.clone());
                        let extract_archive_path = archive_path.clone();
                        let extract_format = candidate.detected_format.clone();
                        let extract_password = pw_value.clone();
                        let extract_encoding = encoding_to_use.clone();

                        // Single extract with the correct password + encoding
                        let extract_result = output_materializer
                            .materialize(
                                MaterializeRequest {
                                    output_dir: output_dir.clone(),
                                    archive_path: candidate.path.clone(),
                                    commit_policy: CommitPolicy::FailIfExists,
                                    archive_stem: Some(archive_stem(&candidate.path).to_string_lossy().into_owned()),
                                    layout_policy: request.layout_policy,
                                    single_root_name_policy: request.single_root_name_policy,
                                },
                                |temp_output_dir| async move {
                                    backend_call(
                                        "archive-backend",
                                        "extract",
                                        &extract_archive_path,
                                        backend.extract(ExtractArchiveRequest {
                                            archive: extract_archive_path.clone(),
                                            format: extract_format,
                                            output_dir: temp_output_dir,
                                            password: extract_password,
                                            encoding: extract_encoding,
                                        }),
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
                                if failure.kind == materialize::MaterializeFailureKind::CollisionSkipped {
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
                    Ok(_) => {
                        // test returned non-ok result. Do not mark password as failed here
                        // because non-ok can indicate corruption/IO rather than wrong pw.
                    }
                    Err(error) => {
                        // Only record password failure when error explicitly indicates wrong password.
                        if matches!(&error, smartzip_core::SmartZipError::WrongPassword { .. }) {
                            let _ = passwords.record_failure(password);
                        } else {
                            // treat other errors as backend/IO failures, surface for reporting
                            last_error = Some(error);
                        }
                    }
                }
            }

            if !extracted && !terminal_skip {
                // Interactive fallback: prompt the user for a password. Use test->extract
                // and reuse the materialized archive path (carved temp when embedded).
                if let Some(prompter) = password_prompter {
                    if let Some(interactive_pw) = prompter.prompt(&candidate.path).await {
                        let pw = interactive_pw.trim().to_string();
                        if !pw.is_empty() {
                            // Test first using the same archive_path used above
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
                                    let encoding_to_use = encoding_result
                                        .as_ref()
                                        .map(|r| EncodingMode::Override(r.selected.clone()))
                                        .unwrap_or_else(|| request.encoding_mode.clone());
                                    let extract_archive_path = archive_path.clone();
                                    let extract_format = candidate.detected_format.clone();
                                    let extract_password = pw.clone();
                                    let extract_encoding = encoding_to_use.clone();
                                    let extract_result = output_materializer
                                        .materialize(
                                            MaterializeRequest {
                                                output_dir: output_dir.clone(),
                                                archive_path: candidate.path.clone(),
                                                commit_policy: CommitPolicy::FailIfExists,
                                                archive_stem: Some(archive_stem(&candidate.path).to_string_lossy().into_owned()),
                                                layout_policy: request.layout_policy,
                                                single_root_name_policy: request.single_root_name_policy,
                                            },
                                            |temp_output_dir| async move {
                                                backend_call(
                                                    "archive-backend",
                                                    "extract",
                                                    &extract_archive_path,
                                                    backend.extract(ExtractArchiveRequest {
                                                        archive: extract_archive_path.clone(),
                                                        format: extract_format,
                                                        output_dir: temp_output_dir,
                                                        password: Some(extract_password),
                                                        encoding: extract_encoding,
                                                    }),
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
                                            // Save successful interactive password to DB
                                            let _ = passwords.record_success(&PasswordCandidate {
                                                id: None,
                                                value: pw.clone(),
                                                source: smartzip_passwords::PasswordSource::Manual,
                                            });
                                            extracted = true;
                                        }
                                        Err(failure) => {
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
                                Ok(_) => {
                                    eprintln!(
                                        "Interactive password did not validate for {}",
                                        archive_path.display()
                                    );
                                }
                                Err(error) => {
                                    eprintln!(
                                        "Interactive password test failed for {}: {}",
                                        archive_path.display(),
                                        error
                                    );
                                }
                            }
                        }
                    }
                }
            }

            if !extracted && !terminal_skip {
                if let Some(error) = last_error {
                    events.push(TaskEvent::failed(task_id.clone(), &error));
                }
            }
            if terminal_skip || !extracted {
                skipped.push(candidate);
                continue;
            }

            events.push(TaskEvent {
                task_id: task_id.clone(),
                kind: TaskEventKind::OutputCreated {
                    path: actual_output_dir.clone(),
                },
            });

            processed.push(candidate.clone());
            let output_relative_path = candidate_output_relative_path(&candidate);
            for nested in discover_nested_candidates(
                scanner,
                &actual_output_dir,
                candidate.depth + 1,
                &output_relative_path,
            ) {
                enqueued.push(nested.clone());
                queue.push_back(nested);
            }
        }

        events.push(TaskEvent {
            task_id: task_id.clone(),
            kind: TaskEventKind::Completed,
        });

        Ok(ExtractWorkflowResult {
            task_id,
            processed,
            skipped,
            enqueued,
            events,
        })
    }
}

impl Default for SmartZipEngine {
    fn default() -> Self {
        Self::new(EmbeddedScanner::default())
    }
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
    base.join(candidate_output_relative_path(candidate))
}

fn candidate_output_relative_path(candidate: &ExtractionCandidate) -> PathBuf {
    candidate.relative_path.clone()
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
        .map_err(|source| {
            smartzip_core::SmartZipError::EmbeddedArchiveCarveFailed {
                path: candidate.path.clone(),
                offset,
                detail: source.to_string(),
            }
        })?;
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
            let strategy = prompter
                .prompt(archive_path, target_path)
                .await;
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
                if let Ok(Some(zip_end)) =
                    crate::embedded_zip::detect_zip_end(source, offset)
                {
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
    let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
        return false;
    };
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "docx" | "xlsx" | "pptx" | "epub" | "apk" | "jar" | "cbz" | "cbr"
    )
}

fn discover_nested_candidates(
    scanner: &EmbeddedScanner,
    root: &Path,
    depth: u8,
    prefix: &Path,
) -> Vec<ExtractionCandidate> {
    let mut candidates = Vec::new();

    // Handle single-file roots directly when a candidate resolves to one file.
    if root.is_file() {
        let header_result = crate::detect::probe_file_header(root);
        if let Some((fmt, offset)) = header_result {
            if is_business_container(root) {
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
            if is_business_container(root) {
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
            ));
            continue;
        }

        let detected_format = format_from_extension(&path);
        let mut relative_path = prefix.to_path_buf();
        relative_path.push(path.strip_prefix(root).unwrap_or(path.as_path()));
        relative_path.set_file_name(archive_stem(&path));

        let header_result = crate::detect::probe_file_header(&path);
        if let Some((fmt, offset)) = header_result {
            if is_business_container(&path) {
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
            if is_business_container(&path) {
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

        for finding in scanner.scan_path(&path).unwrap_or_default() {
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
        ArchiveBackend, ArchiveListing, ArchiveProbe, BackendCapabilities, CompressArchiveRequest,
        CompressArchiveResult, ExtractArchiveRequest, ExtractArchiveResult, ListRequest,
        SevenZipBackend, TestRequest, TestResult,
    };
    use smartzip_db::{password::PasswordRepository, SmartZipDb};
    use smartzip_passwords::{PasswordCandidateRequest, PasswordService};
    use smartzip_scanner::ScanMode;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    };
    use std::time::Duration;

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
    fn recognizes_first_volume_rules() {
        assert!(is_first_volume("archive.part1.rar"));
        assert!(is_first_volume("archive.part01.rar"));
        assert!(!is_first_volume("archive.part2.rar"));
        assert!(is_first_volume("archive.001"));
        assert!(!is_first_volume("archive.002"));
        assert!(is_first_volume("archive.zip"));
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

        let engine = SmartZipEngine::default();
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
                |source| {
                    smartzip_core::SmartZipError::io(Some(request.output_dir.clone()), source)
                },
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

        let engine = SmartZipEngine::default();
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
