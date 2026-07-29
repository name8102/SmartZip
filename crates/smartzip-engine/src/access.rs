//! Root resolve, prepare archive, password access loop.

use smartzip_archive::{ArchiveAdapter, ArchiveExecutor, ListRequest, NativeZipBackend};
use smartzip_core::{
    ArchiveFormat, EncodingMode, TaskEvent, TaskEventKind, TaskExecutionContext, TaskId,
};
use smartzip_passwords::{PasswordCandidate, PasswordService};
use smartzip_scanner::{EmbeddedArchiveFinding, EmbeddedScanner, ScannerConfig};
use std::path::Path;

use crate::backend_util::backend_call;
use crate::encoding_flow::{assess_zip_encoding, resolve_encoding_mode};
use crate::events::EventSink;
use crate::interactive::{
    EmbeddedSelectionChoice, InteractiveEmbeddedPrompter, InteractiveEncodingPrompter,
    InteractivePasswordPrompter,
};
use crate::nested::{archive_output_name, materialize_archive_input};
use crate::password_order::{
    order_password_candidates, password_attempt_index, password_source_label, password_value,
    remember_batch_password,
};
use crate::policy::{
    ext_business_container_kind, finding_meets_min_size, full_root_scanner_config,
};
use crate::types::{ArchiveAccessOutcome, CandidateSource, ExtractionCandidate, ResolvedArchive};

pub(crate) fn scan_embedded_findings(
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

pub(crate) async fn resolve_root_candidate(
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
        let ext_is_archive = crate::nested::format_from_extension(&candidate.path).is_some();
        let file_size = std::fs::metadata(&candidate.path)
            .map(|m| m.len())
            .unwrap_or(0);
        let decision =
            crate::embedded::select_embedded_action(file_size, &findings, &policy, ext_is_archive);
        match decision.action {
            smartzip_core::DetectionAction::ExtractDirect
            | smartzip_core::DetectionAction::CarveAndExtract => {
                if let Some(idx) = decision.selected_index {
                    let finding = &findings[idx];
                    candidate.detected_format = Some(finding.format.clone());
                    candidate.embedded_offset = Some(finding.offset);
                    candidate.embedded_size = finding.size;
                    if matches!(
                        decision.action,
                        smartzip_core::DetectionAction::CarveAndExtract
                    ) {
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
            candidate.detected_format = crate::nested::format_from_extension(&candidate.path);
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

pub(crate) async fn prepare_resolved_archive(
    candidate: &ExtractionCandidate,
    requested_encoding: EncodingMode,
    history: Option<&dyn crate::history::TaskHistoryRecorder>,
    events: &EventSink,
    task_id: &TaskId,
) -> smartzip_core::Result<ResolvedArchive> {
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
    if encoding_mode == EncodingMode::Auto && candidate.detected_format == Some(ArchiveFormat::Zip)
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
    })
}

pub(crate) async fn access_archive_with_password<B: ArchiveExecutor>(
    backend: &B,
    task_context: &TaskExecutionContext,
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
    let mut last_error = None;
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
                task_context,
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
                    let native_zip = NativeZipBackend::new();
                    assessment =
                        assess_zip_encoding(&native_zip, &resolved.archive_path, pw_value.clone())
                            .await;
                }
                break;
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
    }

    if used_password.is_none() {
        if let Some(prompter) = password_prompter {
            events.push(TaskEvent {
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
                                    task_context,
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
                        let native_zip = NativeZipBackend::new();
                        assessment =
                            assess_zip_encoding(&native_zip, &resolved.archive_path, Some(pw))
                                .await;
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
                backend.list_with_context(
                    ListRequest {
                        archive: resolved.archive_path.clone(),
                        format: resolved.candidate.detected_format.clone(),
                        password: used_password.clone(),
                        encoding: encoding_mode.clone(),
                    },
                    task_context,
                ),
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
        events: Vec::new(),
        password_prompt_cancelled,
    })
}
