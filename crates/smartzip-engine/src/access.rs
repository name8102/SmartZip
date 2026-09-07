//! Root resolve, prepare archive, password access loop.

use smartzip_archive::{ArchiveExecutor, ListRequest};
use smartzip_core::{
    ArchiveFormat, EncodingMode, TaskEvent, TaskEventKind, TaskExecutionContext, TaskId,
};
use smartzip_passwords::{PasswordCandidate, PasswordService};
use smartzip_scanner::{EmbeddedArchiveFinding, EmbeddedScanner, ScannerConfig};
use std::path::{Path, PathBuf};

use crate::backend_util::backend_call;
use crate::encoding_flow::{assess_zip_encoding, resolve_encoding_mode};
use crate::events::EventSink;
use crate::interactive::{InteractiveEncodingPrompter, InteractivePasswordPrompter};
use crate::nested::{archive_output_name, materialize_archive_input};
use crate::password_order::{
    order_password_candidates, password_attempt_index, password_source_label, password_value,
    remember_batch_password,
};
use crate::policy::full_root_scanner_config;
use crate::types::{ArchiveAccessOutcome, CandidateSource, ExtractionCandidate, ResolvedArchive};

pub(crate) fn scan_embedded_findings(
    path: &Path,
    scanner: &ScannerConfig,
) -> Vec<EmbeddedArchiveFinding> {
    EmbeddedScanner::new(full_root_scanner_config(scanner))
        .scan_path(path)
        .unwrap_or_default()
}

pub(crate) fn resolve_root_candidate(
    path: &Path,
    findings: &[EmbeddedArchiveFinding],
    events: &EventSink,
    task_id: &TaskId,
) -> Option<ExtractionCandidate> {
    let mut candidate = ExtractionCandidate {
        detected_format: None,
        path: path.to_path_buf(),
        relative_path: archive_output_name(path),
        depth: 0,
        source: CandidateSource::RootInput,
        embedded_offset: None,
        embedded_size: None,
    };

    // Detect/list resolve one archive; an explicit root selects the largest
    // finding regardless of the nested minimum size or carrier ratio.
    let policy = smartzip_core::EmbeddedScanPolicy {
        mode: smartzip_core::EmbeddedScanMode::Largest,
        ..Default::default()
    };
    let file_size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let decision = crate::embedded::select_embedded_action(
        file_size,
        findings,
        &policy,
        crate::nested::format_from_extension(path).is_some(),
    );
    if let Some(finding) = decision
        .selected_index
        .and_then(|index| findings.get(index))
    {
        candidate.detected_format = Some(finding.format.clone());
        candidate.embedded_offset = Some(finding.offset);
        candidate.embedded_size = finding.size;
        if finding.offset > 0 {
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
    } else if let Some((format, offset)) = crate::detect::probe_file_header(path) {
        candidate.detected_format = Some(format);
        candidate.embedded_offset = (offset > 0).then_some(offset);
    } else {
        candidate.detected_format = crate::nested::format_from_extension(path);
    }

    candidate.detected_format.as_ref()?;
    Some(candidate)
}

pub(crate) async fn prepare_resolved_archive(
    candidate: &ExtractionCandidate,
    archive_path_override: Option<PathBuf>,
    requested_encoding: EncodingMode,
    history: Option<&dyn crate::history::TaskHistoryRecorder>,
    events: &EventSink,
    task_id: &TaskId,
) -> smartzip_core::Result<ResolvedArchive> {
    let (archive_path, archive_temp) = if let Some(path) = archive_path_override {
        (path, None)
    } else {
        let archive_input = materialize_archive_input(candidate)?;
        (archive_input.path, archive_input._temp)
    };
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
    if encoding_mode == EncodingMode::Auto && candidate.detected_format == Some(ArchiveFormat::Zip)
    {
        zip_encoding_assessment = assess_zip_encoding(&archive_path, None).await;
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
        _archive_temp: archive_temp,
        sample_hash,
        sample_size,
        known_hit,
        encoding_mode,
        reused_confirmed_encoding,
        zip_encoding_assessment,
        recorder_name,
    })
}

pub(crate) async fn access_archive_with_password<B: ArchiveExecutor>(
    backend: &B,
    task_context: std::sync::Arc<TaskExecutionContext>,
    passwords: &PasswordService<'_>,
    resolved: &ResolvedArchive,
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
    let total_password_attempts = ordered_candidates.len();
    let mut accepted_password_id = None;
    let mut used_password = None;
    let mut has_password = false;
    let mut listing = None;
    let encrypted = None;
    let mut saw_wrong_password = false;
    let mut password_prompt_cancelled = false;
    let mut assessment = resolved.zip_encoding_assessment.clone();

    for password in &ordered_candidates {
        let pw_value = password_value(password);
        let attempt_index = password_attempt_index(password, &ordered_candidates);
        events.push(TaskEvent {
            task_id: task_id.clone(),
            kind: TaskEventKind::Progress(smartzip_core::TaskProgress::indeterminate(format!(
                "Trying password [{}/{}] ({}) for {}",
                attempt_index,
                total_password_attempts,
                password_source_label(password),
                resolved.candidate.path.display()
            ))),
        });
        if !load_listing {
            used_password = pw_value.clone();
            has_password = pw_value.as_deref().map(|v| !v.is_empty()).unwrap_or(false);
            break;
        }
        match backend_call(
            "archive-backend",
            "list",
            &resolved.archive_path,
            backend.list_with_context(
                ListRequest {
                    archive: resolved.archive_path.clone(),
                    format: resolved.candidate.detected_format.clone(),
                    password: pw_value.clone(),
                    encoding: resolved.encoding_mode.clone(),
                },
                std::sync::Arc::clone(&task_context),
            ),
        )
        .await
        {
            Ok(result) => {
                accepted_password_id = passwords.record_success(password).ok().flatten();
                used_password = pw_value.clone();
                has_password = pw_value.as_deref().map(|v| !v.is_empty()).unwrap_or(false);
                listing = Some(result);
                if assessment.is_none()
                    && resolved.encoding_mode == EncodingMode::Auto
                    && resolved.candidate.detected_format == Some(ArchiveFormat::Zip)
                {
                    assessment =
                        assess_zip_encoding(&resolved.archive_path, pw_value.clone()).await;
                }
                break;
            }
            Err(error) => {
                if matches!(&error, smartzip_core::SmartZipError::WrongPassword { .. }) {
                    saw_wrong_password = true;
                    let _ = passwords.record_failure(password);
                } else if matches!(
                    &error,
                    smartzip_core::SmartZipError::PasswordRequired { .. }
                ) && pw_value.as_deref().is_none_or(str::is_empty)
                {
                    saw_wrong_password = true;
                } else {
                    return Err(error);
                }
            }
        }
    }

    if listing.is_none() && used_password.is_none() {
        if let Some(prompter) = password_prompter {
            events.push(TaskEvent {
                task_id: task_id.clone(),
                kind: TaskEventKind::Progress(smartzip_core::TaskProgress::indeterminate(format!(
                    "Prompting for password: {}",
                    resolved.candidate.path.display()
                ))),
            });
            let interactive_password = prompter.prompt(&resolved.candidate.path).await;
            password_prompt_cancelled = interactive_password.as_deref().is_none_or(str::is_empty);
            if let Some(interactive_pw) = interactive_password {
                let pw = interactive_pw;
                if !pw.is_empty() {
                    let accepted = PasswordCandidate {
                        id: None,
                        value: pw.clone(),
                        source: smartzip_passwords::PasswordSource::Manual,
                    };
                    if load_listing {
                        listing = Some(
                            backend_call(
                                "archive-backend",
                                "list",
                                &resolved.archive_path,
                                backend.list_with_context(
                                    ListRequest {
                                        archive: resolved.archive_path.clone(),
                                        format: resolved.candidate.detected_format.clone(),
                                        password: Some(pw.clone()),
                                        encoding: resolved.encoding_mode.clone(),
                                    },
                                    std::sync::Arc::clone(&task_context),
                                ),
                            )
                            .await
                            .map_err(|error| {
                                if matches!(
                                    error,
                                    smartzip_core::SmartZipError::WrongPassword { .. }
                                ) {
                                    smartzip_core::SmartZipError::WrongPassword {
                                        path: resolved.candidate.path.clone(),
                                    }
                                } else {
                                    error
                                }
                            })?,
                        );
                    }
                    accepted_password_id = passwords.record_success(&accepted).ok().flatten();
                    remember_batch_password(batch_passwords, &accepted.value, accepted_password_id);
                    used_password = Some(pw.clone());
                    has_password = true;
                    if assessment.is_none()
                        && resolved.encoding_mode == EncodingMode::Auto
                        && resolved.candidate.detected_format == Some(ArchiveFormat::Zip)
                    {
                        assessment = assess_zip_encoding(&resolved.archive_path, Some(pw)).await;
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
    .await?
    .ok_or_else(|| smartzip_core::SmartZipError::BackendProtocolError {
        backend: "user-input".into(),
        detail: "archive skipped during encoding confirmation".into(),
    })?;

    if load_listing && (listing.is_none() || encoding_mode != resolved.encoding_mode) {
        listing = Some(
            backend_call(
                "archive-backend",
                "list",
                &resolved.archive_path,
                backend.list_with_context(
                    ListRequest {
                        archive: resolved.archive_path.clone(),
                        format: resolved.candidate.detected_format.clone(),
                        password: used_password.clone(),
                        encoding: encoding_mode.clone(),
                    },
                    std::sync::Arc::clone(&task_context),
                ),
            )
            .await?,
        );
    }

    Ok(ArchiveAccessOutcome {
        password_id: accepted_password_id,
        has_password,
        encoding_mode,
        listing,
        encrypted,
        events: Vec::new(),
    })
}
