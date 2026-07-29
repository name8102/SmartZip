//! Extract/detect/list orchestration (scheduler over capability modules).

use smartzip_archive::{ArchiveBackend, ExtractArchiveRequest, NativeZipBackend, TestRequest};
use smartzip_core::{ArchiveFormat, EncodingMode, TaskEvent, TaskEventKind, TaskId};
use smartzip_passwords::{PasswordCandidate, PasswordService};
use smartzip_scanner::{Confidence, EmbeddedArchiveFinding, EmbeddedScanner};
use std::collections::{HashSet, VecDeque};
use std::io::Read;

use crate::access::{
    access_archive_with_password, prepare_resolved_archive, resolve_root_candidate,
    scan_embedded_findings,
};
use crate::backend_util::{
    backend_call, confidence_score, extraction_progress_callback, map_detect_error,
};
use crate::encoding_flow::{assess_zip_encoding, encoding_mode_label, resolve_encoding_mode};
use crate::events::{EventSink, TaskEventListener};
use crate::interactive::{
    EmbeddedSelectionChoice, InteractiveEmbeddedPrompter, InteractiveEncodingPrompter,
    InteractiveOutputPrompter, InteractivePasswordPrompter,
};
use crate::materialize::{self, CommitPolicy, MaterializeRequest, OutputMaterializer};
use crate::nested::{
    archive_output_name, archive_stem, candidate_key, candidate_output_relative_path,
    discover_nested_candidates, is_first_volume, make_collision_resolver,
    materialize_archive_input, output_dir_for_candidate, output_relative_path_for, record_skip,
    recyclable_nested_archive_path, recycle_archive, root_embedded_candidates,
};
use crate::password_order::{
    load_password_candidates, order_password_candidates, password_attempt_index,
    password_source_label, password_value, remember_batch_password,
};
use crate::policy::{
    default_root_scanner_config, embedded_policy_from_request, ext_business_container_kind,
    finding_meets_min_size, full_root_scanner_config, should_scan_candidate_for_embedded,
};
use crate::types::{
    ArchiveRecycleHandler, CandidateSource, DetectRequest, DetectResult, ExtractWorkflowRequest,
    ExtractWorkflowResult, ExtractionCandidate, FileAwareDetectResult, InspectRequest,
    ListArchiveRequest, ListArchiveResult,
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

pub(crate) async fn inspect_file_with_listener<B: ArchiveBackend>(
    min_embedded_size_bytes: u64,
    backend: &B,
    _passwords: &PasswordService<'_>,
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
        min_embedded_size_bytes,
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
    let findings = scan_embedded_findings(&request.path, &request.scanner, min_embedded_size_bytes);

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
        let resolved =
            prepare_resolved_archive(&candidate, EncodingMode::Auto, history, &events, &task_id)
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

pub(crate) async fn list_archive_with_listener_interactive<B: ArchiveBackend>(
    min_embedded_size_bytes: u64,
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
        min_embedded_size_bytes,
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
    let password_candidates =
        load_password_candidates(passwords, request.password_candidates.clone())?;
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

pub(crate) async fn extract_recursive_with_listener_interactive<B: ArchiveBackend>(
    engine_scanner: &EmbeddedScanner,
    min_embedded_size_bytes: u64,
    archive_recycler: &ArchiveRecycleHandler,
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
    let nested_scanner = if request.scanner == *engine_scanner.config() {
        None
    } else {
        Some(EmbeddedScanner::new(request.scanner.clone()))
    };
    let nested_scanner = nested_scanner.as_ref().unwrap_or(engine_scanner);
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
    embedded_policy.min_finding_size_bytes = min_embedded_size_bytes;
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
                kind: TaskEventKind::Progress(smartzip_core::TaskProgress::indeterminate(format!(
                    "Processing input [{}/{}]: {}",
                    root_input_started,
                    root_input_total,
                    candidate.path.display()
                ))),
            });
        } else {
            events.push(TaskEvent {
                task_id: task_id.clone(),
                kind: TaskEventKind::Progress(smartzip_core::TaskProgress::indeterminate(format!(
                    "Processing nested archive at depth {}: {}",
                    candidate.depth,
                    candidate.path.display()
                ))),
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
        let test_before_extract =
            backend.should_test_before_extract(&archive_path, candidate.detected_format.as_ref());
        let total_password_attempts = candidate_passwords.len();
        for password in &candidate_passwords {
            let pw_value = password_value(password);
            let attempt_index = password_attempt_index(password, &candidate_passwords);
            events.push(TaskEvent {
                task_id: task_id.clone(),
                kind: TaskEventKind::Progress(smartzip_core::TaskProgress::indeterminate(format!(
                    "Trying password [{}/{}] ({}) for {}",
                    attempt_index,
                    total_password_attempts,
                    password_source_label(password),
                    candidate.path.display()
                ))),
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
                        candidate_password_id = matched_password_id;
                        candidate_has_password =
                            pw_value.as_deref().map(|v| !v.is_empty()).unwrap_or(false);

                        if zip_encoding_assessment.is_none()
                            && candidate_encoding_mode == EncodingMode::Auto
                            && candidate.detected_format == Some(ArchiveFormat::Zip)
                        {
                            let native_zip = NativeZipBackend::new();
                            zip_encoding_assessment =
                                assess_zip_encoding(&native_zip, &archive_path, pw_value.clone())
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
                        if matches!(&error, smartzip_core::SmartZipError::WrongPassword { .. }) {
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
                        candidate_password_id = matched_password_id;
                        candidate_has_password =
                            pw_value.as_deref().map(|v| !v.is_empty()).unwrap_or(false);
                        if result.output_dir != output_dir {
                            candidate.relative_path =
                                output_relative_path_for(&request.output_dir, &result.output_dir);
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
                                            smartzip_core::TaskProgress::indeterminate(format!(
                                                "Interactive password accepted for {}",
                                                candidate.path.display()
                                            )),
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
                            candidate_encoding_used = Some(encoding_mode_label(&extract_encoding));
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
                                                    password: Some(extract_password),
                                                    encoding: extract_encoding,
                                                },
                                                Some(extraction_progress),
                                            ),
                                        )
                                        .await
                                        .map(|_| ())
                                    },
                                    output_prompter.map(|p| make_collision_resolver(p)).as_ref(),
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
                                            smartzip_core::TaskProgress::indeterminate(format!(
                                                "Interactive password accepted for {}",
                                                candidate.path.display()
                                            )),
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
            if let Err(error) = recycle_archive(archive_recycler.clone(), path.clone()).await {
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
