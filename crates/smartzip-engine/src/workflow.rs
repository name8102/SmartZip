//! Extract/detect/list orchestration (scheduler over capability modules).

use smartzip_archive::ArchiveExecutor;
use smartzip_core::{ArchiveFormat, EncodingMode, TaskEvent, TaskEventKind, TaskId};
use smartzip_passwords::PasswordService;
use smartzip_scanner::EmbeddedScanner;

use crate::access::{
    access_archive_with_password, prepare_resolved_archive, resolve_root_candidate,
    scan_embedded_findings,
};
use crate::backend_util::{confidence_score, map_detect_error};
use crate::encoding_flow::encoding_mode_label;
use crate::events::{EventSink, TaskEventListener};
use crate::interactive::{InteractiveEncodingPrompter, InteractivePasswordPrompter};
use crate::password_order::load_password_candidates;
use crate::policy::{default_root_scanner_config, ext_business_container_kind};
use crate::types::{
    DetectRequest, DetectResult, FileAwareDetectResult, InspectRequest, ListArchiveRequest,
    ListArchiveResult,
};

/// Override how successfully processed nested archives are recycled.
///
/// This is primarily useful for deterministic tests and platform hosts
/// that provide their own recycle-bin integration.

pub(crate) fn detect(
    engine_scanner: &EmbeddedScanner,
    request: DetectRequest,
) -> std::io::Result<DetectResult> {
    let task_id = TaskId::new();
    let mut events = vec![TaskEvent::started(task_id.clone())];

    let effective_config = default_root_scanner_config(&request.scanner);
    let scanner = if effective_config == *engine_scanner.config() {
        None
    } else {
        Some(EmbeddedScanner::new(effective_config))
    };
    let scanner = scanner.as_ref().unwrap_or(engine_scanner);

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

pub(crate) async fn inspect_file_with_listener<B: ArchiveExecutor>(
    cancellation: tokio_util::sync::CancellationToken,
    backend: &B,
    _passwords: &PasswordService<'_>,
    request: InspectRequest,
    listener: Option<TaskEventListener>,
    history: Option<&dyn crate::history::TaskHistoryRecorder>,
) -> smartzip_core::Result<FileAwareDetectResult> {
    let task_id = TaskId::new();
    let events = EventSink::new(listener);
    let task_context = backend.begin_task_with_cancellation(
        task_id.clone(),
        std::sync::Arc::new(events.clone()),
        cancellation,
    );
    events.push(TaskEvent::started(task_id.clone()));
    if let Some(recorder) = history {
        recorder.start_task(&task_id, "detect", None);
    }
    let mut completion = crate::history::CompletionGuard::new(
        history,
        task_id.clone(),
        events.clone(),
        task_context.cancellation_token(),
    );

    let findings = scan_embedded_findings(&request.path, &request.scanner);
    let candidate = resolve_root_candidate(&request.path, &findings, &events, &task_id);

    let mut detected_format = candidate.as_ref().and_then(|c| c.detected_format.clone());
    let mut status = "unreadable".to_string();
    let mut reason = None;
    let mut encrypted = None;
    let mut encoding = None;
    let mut encoding_confidence = None;
    let mut needs_password = false;
    let mut known_password = false;
    let mut known_encoding = None;

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
            None,
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
            .probe_with_context(&resolved.archive_path, std::sync::Arc::clone(&task_context))
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
        status = if probe.supported {
            "detected"
        } else {
            "unreadable"
        }
        .to_string();
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
                    test_report_json: None,
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
                    test_report_json: None,
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

    completion.complete();
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

pub(crate) async fn list_archive_with_listener_interactive<B: ArchiveExecutor>(
    cancellation: tokio_util::sync::CancellationToken,
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
    let task_context = backend.begin_task_with_cancellation(
        task_id.clone(),
        std::sync::Arc::new(events.clone()),
        cancellation,
    );
    events.push(TaskEvent::started(task_id.clone()));
    if let Some(recorder) = history {
        recorder.start_task(&task_id, "list", None);
    }
    let mut completion = crate::history::CompletionGuard::new(
        history,
        task_id.clone(),
        events.clone(),
        task_context.cancellation_token(),
    );

    // List and extract share the same resolver/materialization entrypoint.
    let findings = scan_embedded_findings(&request.path, &request.scanner);
    let candidate = resolve_root_candidate(&request.path, &findings, &events, &task_id)
        .unwrap_or_else(|| crate::types::ExtractionCandidate {
            path: request.path.clone(),
            relative_path: std::path::PathBuf::from(request.path.file_name().unwrap_or_default()),
            depth: 0,
            source: crate::types::CandidateSource::RootInput,
            detected_format: None,
            embedded_offset: None,
            embedded_size: None,
        });

    let mut volume_resolver = crate::volumes::VolumeResolver::new();
    let (candidate, backend_path_override, _volume_keep) = match volume_resolver.prepare(candidate)
    {
        crate::volumes::VolumePreparation::Single(candidate) => (candidate, None, None),
        crate::volumes::VolumePreparation::Resolved {
            candidate,
            archive_path,
            warnings,
            materialized,
            ..
        } => {
            for warning in warnings {
                events.push(TaskEvent {
                    task_id: task_id.clone(),
                    kind: TaskEventKind::Warning {
                        message: format!("volume warning: {warning:?}"),
                    },
                });
            }
            (candidate, Some(archive_path), Some(materialized))
        }
        crate::volumes::VolumePreparation::Incomplete { candidate, problem } => {
            return Err(smartzip_core::SmartZipError::CorruptedArchive {
                path: candidate.path,
                detail: problem.reason,
            });
        }
        crate::volumes::VolumePreparation::GroupingAmbiguous {
            candidate,
            hypotheses,
        } => {
            return Err(smartzip_core::SmartZipError::CorruptedArchive {
                path: candidate.path,
                detail: format!("grouping ambiguous: {} hypotheses", hypotheses.len()),
            });
        }
        crate::volumes::VolumePreparation::MaterializationFailed { candidate, error } => {
            return Err(smartzip_core::SmartZipError::CorruptedArchive {
                path: candidate.path,
                detail: format!("volume materialization failed: {error}"),
            });
        }
    };

    let resolved = prepare_resolved_archive(
        &candidate,
        backend_path_override,
        request.encoding_mode.clone(),
        history,
        &events,
        &task_id,
    )
    .await?;
    let password_candidates =
        load_password_candidates(passwords, request.password_candidates.clone())?;
    let mut batch_passwords = Vec::new();
    let outcome = access_archive_with_password(
        backend,
        std::sync::Arc::clone(&task_context),
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

    let listing =
        outcome
            .listing
            .clone()
            .ok_or_else(|| smartzip_core::SmartZipError::UnsupportedFormat {
                path: request.path.clone(),
                format: candidate
                    .detected_format
                    .as_ref()
                    .map(|f| f.as_str().to_string()),
            })?;

    if let Some(recorder) = history {
        // History follows the logical candidate/request identity, never the
        // temporary canonical staging path used by the backend.
        let history_input_path = &request.path;
        recorder.record_file_extraction(
            &task_id,
            crate::history::FileExtractionRow {
                input_path: history_input_path,
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
                test_report_json: None,
            },
        );
        if let (Some(hash), Some(size), EncodingMode::Override(encoding)) = (
            resolved.sample_hash.as_deref(),
            resolved.sample_size,
            &request.encoding_mode,
        ) {
            recorder.upsert_known_file_confirmed_encoding(
                crate::history::KnownFileEncodingUpsert {
                    sample_hash: hash,
                    size,
                    name: resolved.recorder_name.as_deref(),
                    offset: candidate.embedded_offset.map(|o| o as i64),
                    encoding,
                },
            );
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

    completion.complete();
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

pub(crate) use crate::extract_workflow::extract_recursive_with_listener_interactive;
