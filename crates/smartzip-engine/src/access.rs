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
use crate::password_order::password_source_label;
use crate::policy::full_root_scanner_config;
use crate::types::{ArchiveAccessOutcome, CandidateSource, ExtractionCandidate, PreparedArchive};

pub(crate) async fn scan_embedded_findings(
    path: &Path,
    scanner: &ScannerConfig,
    cancellation: tokio_util::sync::CancellationToken,
) -> smartzip_core::Result<Vec<EmbeddedArchiveFinding>> {
    scan_file(path, full_root_scanner_config(scanner), cancellation).await
}

pub(crate) async fn scan_file(
    path: &Path,
    config: ScannerConfig,
    cancellation: tokio_util::sync::CancellationToken,
) -> smartzip_core::Result<Vec<EmbeddedArchiveFinding>> {
    let path = path.to_owned();
    let token = cancellation.child_token();
    let _cancel_on_drop = token.clone().drop_guard();
    let result = tokio::task::spawn_blocking(move || {
        EmbeddedScanner::new(config).scan_path_cancellable(path, &|| token.is_cancelled())
    })
    .await
    .map_err(|e| smartzip_core::SmartZipError::io(None, std::io::Error::other(e)))?;
    if cancellation.is_cancelled()
        || result
            .as_ref()
            .err()
            .is_some_and(|e| e.kind() == std::io::ErrorKind::Interrupted)
    {
        return Err(smartzip_core::SmartZipError::Cancelled);
    }
    Ok(result.unwrap_or_default())
}

pub(crate) fn resolve_root_candidate(
    path: &Path,
    findings: &[EmbeddedArchiveFinding],
    events: &EventSink,
    task_id: &TaskId,
    scan_root: bool,
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
    } else if let Some((format, offset)) = scan_root
        .then(|| crate::detect::probe_file_header(path))
        .flatten()
    {
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
    volume_input: Option<(PathBuf, crate::volumes::materialize::MaterializedVolumeSet)>,
    staging_root: Option<&Path>,
    requested_encoding: EncodingMode,
    history: Option<&dyn crate::history::TaskHistoryRecorder>,
    run_policy: Option<&crate::CompiledRunPolicy>,
) -> smartzip_core::Result<PreparedArchive> {
    let (archive_path, archive_temp, volume_keep) = if let Some((path, guard)) = volume_input {
        (path, None, Some(guard))
    } else {
        let archive_input = materialize_archive_input(candidate, staging_root)?;
        (archive_input.path, archive_input._temp, None)
    };
    let (sample_hash, sample_size) = if history.is_none() {
        (None, None)
    } else {
        match candidate.embedded_offset {
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
        }
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
    if run_policy.is_none_or(|p| p.values().extraction.encoding.mode == "auto")
        && encoding_mode == EncodingMode::Auto
        && candidate.detected_format == Some(ArchiveFormat::Zip)
    {
        zip_encoding_assessment = assess_zip_encoding(&archive_path).await;
    }
    Ok(PreparedArchive {
        candidate: candidate.clone(),
        archive_path,
        _archive_temp: archive_temp,
        _volume_keep: volume_keep,
        sample_hash,
        sample_size,
        known_hit,
        encoding_mode,
        reused_confirmed_encoding,
        zip_encoding_assessment,
        detect_encoding: run_policy.is_none_or(|p| p.values().extraction.encoding.mode == "auto"),
        recorder_name,
    })
}

pub(crate) async fn access_archive_with_password<B: ArchiveExecutor>(
    backend: &B,
    task_context: std::sync::Arc<TaskExecutionContext>,
    passwords: &PasswordService<'_>,
    resolved: &PreparedArchive,
    password_candidates: &[PasswordCandidate],
    password_prompter: Option<&dyn InteractivePasswordPrompter>,
    encoding_prompter: Option<&dyn InteractiveEncodingPrompter>,
    events: &EventSink,
    task_id: &TaskId,
) -> smartzip_core::Result<ArchiveAccessOutcome> {
    let password_prompter = password_prompter.filter(|_| passwords.allows_prompt());
    let known_password = resolved
        .known_hit
        .as_ref()
        .and_then(|hit| hit.password_id)
        .and_then(|id| passwords.candidate_by_id(id).ok().flatten());
    let ordered_candidates =
        passwords.order_candidates(password_candidates, known_password.as_ref(), &[]);
    let total_password_attempts = ordered_candidates.len();
    let mut accepted_password_id = None;
    let mut used_password = None;
    let mut listing = None;
    let mut saw_wrong_password = false;
    let mut password_prompt_cancelled = false;
    let mut assessment = resolved.zip_encoding_assessment.clone();

    for (index, password) in ordered_candidates.iter().enumerate() {
        let pw_value = Some(password.value.clone());
        events.push(TaskEvent {
            task_id: task_id.clone(),
            kind: TaskEventKind::Progress(smartzip_core::TaskProgress::indeterminate(format!(
                "Trying password [{}/{}] ({}) for {}",
                index + 1,
                total_password_attempts,
                password_source_label(password),
                resolved.candidate.path.display()
            ))),
        });
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
                accepted_password_id = passwords.record_listing_access(password).ok().flatten();
                used_password = pw_value.clone();
                listing = Some(result);
                if resolved.detect_encoding
                    && assessment.is_none()
                    && resolved.encoding_mode == EncodingMode::Auto
                    && resolved.candidate.detected_format == Some(ArchiveFormat::Zip)
                {
                    assessment = assess_zip_encoding(&resolved.archive_path).await;
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

    while listing.is_none() {
        let Some(prompter) = password_prompter else {
            break;
        };
        events.push(TaskEvent {
            task_id: task_id.clone(),
            kind: TaskEventKind::Progress(smartzip_core::TaskProgress::indeterminate(format!(
                "Prompting for password: {}",
                resolved.candidate.path.display()
            ))),
        });
        let token = task_context.cancellation_token();
        let input = tokio::select! { biased;
            _ = token.cancelled() => return Err(smartzip_core::SmartZipError::Cancelled),
            input = prompter.prompt(&resolved.candidate.path) => input,
        };
        if task_context.is_cancelled() {
            return Err(smartzip_core::SmartZipError::Cancelled);
        }
        let Some(password) = input.filter(|p| !p.is_empty()) else {
            password_prompt_cancelled = true;
            break;
        };
        match backend_call(
            "archive-backend",
            "list",
            &resolved.archive_path,
            backend.list_with_context(
                ListRequest {
                    archive: resolved.archive_path.clone(),
                    format: resolved.candidate.detected_format.clone(),
                    password: Some(password.clone()),
                    encoding: resolved.encoding_mode.clone(),
                },
                std::sync::Arc::clone(&task_context),
            ),
        )
        .await
        {
            Ok(result) => {
                used_password = Some(password);
                listing = Some(result);
                if resolved.detect_encoding
                    && assessment.is_none()
                    && resolved.encoding_mode == EncodingMode::Auto
                    && resolved.candidate.detected_format == Some(ArchiveFormat::Zip)
                {
                    assessment = assess_zip_encoding(&resolved.archive_path).await;
                }
            }
            Err(
                smartzip_core::SmartZipError::WrongPassword { .. }
                | smartzip_core::SmartZipError::PasswordRequired { .. },
            ) => {
                saw_wrong_password = true;
                events.push(TaskEvent {
                    task_id: task_id.clone(),
                    kind: TaskEventKind::Warning {
                        message: "Password rejected; try again or skip".into(),
                    },
                });
            }
            Err(error) => return Err(error),
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

    let listing = match listing {
        Some(listing) if encoding_mode == resolved.encoding_mode => listing,
        _ => {
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
            .await?
        }
    };

    Ok(ArchiveAccessOutcome {
        password_id: accepted_password_id,
        // Listing alone never proves a content password.
        has_password: false,
        encoding_mode,
        listing,
    })
}
