//! Recursive extraction workflow implementation.

use smartzip_archive::{ArchiveExecutor, ExtractArchiveRequest, NativeZipBackend};
use smartzip_core::{ArchiveFacts, ArchiveFormat, EncodingMode, TaskEvent, TaskEventKind, TaskId};
use smartzip_passwords::{PasswordCandidate, PasswordService};
use smartzip_scanner::{Confidence, EmbeddedArchiveFinding, EmbeddedScanner};
use std::collections::{HashSet, VecDeque};
use std::io::Read;

use crate::backend_util::{backend_call, confidence_score};
use crate::encoding_flow::{assess_zip_encoding, encoding_mode_label, resolve_encoding_mode};
use crate::events::{EventSink, TaskEventListener};
use crate::interactive::{
    EmbeddedSelectionChoice, InteractiveEmbeddedPrompter, InteractiveEncodingPrompter,
    InteractiveOutputPrompter, InteractivePasswordPrompter,
};
use crate::materialize::{self, CommitPolicy, MaterializeRequest, OutputMaterializer};
use crate::nested::{
    archive_output_name, archive_stem, candidate_key, candidate_output_relative_path,
    discover_nested_candidates, make_collision_resolver, materialize_archive_input,
    output_dir_for_candidate, output_relative_path_for, record_skip,
    recyclable_nested_archive_path, recycle_archive, root_embedded_candidates,
};
use crate::password_order::{
    order_password_candidates, password_source_label, password_value, remember_batch_password,
};
use crate::policy::{
    embedded_policy_from_request, ext_business_container_kind, finding_meets_min_size,
    full_root_scanner_config, should_scan_candidate_for_embedded,
};
use crate::types::{
    ArchiveRecycleHandler, CandidateSource, ExtractWorkflowRequest, ExtractWorkflowResult,
    ExtractionCandidate,
};
use crate::volumes::{VolumeResolution, VolumeResolver};

/// Override how successfully processed nested archives are recycled.
///
/// This is primarily useful for deterministic tests and platform hosts
/// that provide their own recycle-bin integration.

pub(crate) async fn extract_recursive_with_listener_interactive<B: ArchiveExecutor>(
    engine_scanner: &EmbeddedScanner,
    min_embedded_size_bytes: u64,
    archive_recycler: &ArchiveRecycleHandler,
    cancellation: tokio_util::sync::CancellationToken,
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
    let events = EventSink::new(listener);
    let task_context = backend.begin_task_with_cancellation(
        task_id.clone(),
        std::sync::Arc::new(events.clone()),
        cancellation.child_token(),
    );
    let nested_scanner = if request.scanner == *engine_scanner.config() {
        None
    } else {
        Some(EmbeddedScanner::new(request.scanner.clone()))
    };
    let nested_scanner = nested_scanner.as_ref().unwrap_or(engine_scanner);
    let root_scanner = EmbeddedScanner::new(full_root_scanner_config(&request.scanner));

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
    let mut completion = crate::history::CompletionGuard::new(
        history,
        task_id.clone(),
        events.clone(),
        cancellation.clone(),
    );
    let mut failed_count = 0usize;
    let mut was_cancelled = false;
    let mut committed_usage = crate::budget::Usage::default();
    let mut volume_resolver = VolumeResolver::new();
    let mut processed_volume_keys = HashSet::new();
    let mut consumed_volume_members = HashSet::new();

    loop {
        if cancellation.is_cancelled() {
            was_cancelled = true;
            break;
        }
        if task_context.is_cancelled() {
            break;
        }
        let Some(mut candidate) = queue.pop_front() else {
            break;
        };
        let original_input_path = candidate.path.clone();
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
        let absolute_input =
            std::path::absolute(&candidate.path).unwrap_or_else(|_| candidate.path.clone());
        if candidate.source != CandidateSource::EmbeddedFinding
            && consumed_volume_members.contains(&absolute_input)
        {
            record_skip(history, &task_id, &candidate, "duplicate");
            skipped.push(candidate);
            continue;
        }
        // Resolve and materialize volumes once through the shared preparation path.
        // Carved findings bypass sibling discovery, including offset-zero payloads.
        let mut volume_materialized: Option<crate::volumes::materialize::MaterializedVolumeSet> =
            None;
        let mut volume_archive_path: Option<std::path::PathBuf> = None;
        let mut volume_set_for_candidate: Option<crate::volumes::VolumeSet> = None;
        let preparation = if candidate.source != CandidateSource::EmbeddedFinding {
            let resolution = volume_resolver.resolve(&candidate);
            if let VolumeResolution::Resolved(set)
            | VolumeResolution::ResolvedWithWarnings { set, .. } = &resolution
            {
                let key = volume_set_key(set);
                if processed_volume_keys.contains(&key) {
                    continue;
                }
                processed_volume_keys.insert(key);
                for member in &set.members {
                    consumed_volume_members.insert(
                        std::path::absolute(&member.path).unwrap_or_else(|_| member.path.clone()),
                    );
                }
            }
            volume_resolver.prepare_resolution(candidate, resolution)
        } else {
            volume_resolver.prepare(candidate)
        };
        match preparation {
            crate::volumes::VolumePreparation::Single(prepared) => candidate = prepared,
            crate::volumes::VolumePreparation::Resolved {
                candidate: prepared,
                archive_path,
                set,
                warnings,
                materialized,
            } => {
                for warning in warnings {
                    events.push(TaskEvent {
                        task_id: task_id.clone(),
                        kind: TaskEventKind::Warning {
                            message: format!(
                                "volume warning for {}: {warning:?}",
                                prepared.path.display()
                            ),
                        },
                    });
                }
                candidate = prepared;
                volume_archive_path = Some(archive_path);
                volume_set_for_candidate = Some(set);
                volume_materialized = Some(materialized);
            }
            crate::volumes::VolumePreparation::Incomplete {
                candidate: failed,
                problem,
            } => {
                failed_count += 1;
                events.push(TaskEvent {
                    task_id: task_id.clone(),
                    kind: TaskEventKind::Failed {
                        error: format!(
                            "incomplete volume set for {}: {}",
                            failed.path.display(),
                            problem.reason
                        ),
                    },
                });
                if let Some(recorder) = history {
                    recorder.record_file_extraction(
                        &task_id,
                        crate::history::FileExtractionRow {
                            input_path: &original_input_path,
                            sample_hash: None,
                            file_size: None,
                            offset: failed.embedded_offset.map(|o| o as i64),
                            output_path: None,
                            has_password: false,
                            password_id: None,
                            status: "failed",
                            reason: Some("incomplete_volume"),
                            encoding: None,
                            encoding_corrected: false,
                            damaged_volumes_json: None,
                            test_report_json: None,
                        },
                    );
                }
                skipped.push(failed);
                continue;
            }
            crate::volumes::VolumePreparation::GroupingAmbiguous {
                candidate: failed,
                hypotheses,
            } => {
                failed_count += 1;
                events.push(TaskEvent {
                    task_id: task_id.clone(),
                    kind: TaskEventKind::Failed {
                        error: format!(
                            "grouping ambiguous for {}: {} hypotheses",
                            failed.path.display(),
                            hypotheses.len()
                        ),
                    },
                });
                if let Some(recorder) = history {
                    recorder.record_file_extraction(
                        &task_id,
                        crate::history::FileExtractionRow {
                            input_path: &original_input_path,
                            sample_hash: None,
                            file_size: None,
                            offset: failed.embedded_offset.map(|o| o as i64),
                            output_path: None,
                            has_password: false,
                            password_id: None,
                            status: "failed",
                            reason: Some("grouping_ambiguous"),
                            encoding: None,
                            encoding_corrected: false,
                            damaged_volumes_json: None,
                            test_report_json: None,
                        },
                    );
                }
                skipped.push(failed);
                continue;
            }
            crate::volumes::VolumePreparation::MaterializationFailed {
                candidate: failed,
                error,
            } => {
                failed_count += 1;
                events.push(TaskEvent {
                    task_id: task_id.clone(),
                    kind: TaskEventKind::Failed {
                        error: format!(
                            "volume materialization failed for {}: {error}",
                            failed.path.display()
                        ),
                    },
                });
                if let Some(recorder) = history {
                    recorder.record_file_extraction(
                        &task_id,
                        crate::history::FileExtractionRow {
                            input_path: &original_input_path,
                            sample_hash: None,
                            file_size: None,
                            offset: failed.embedded_offset.map(|o| o as i64),
                            output_path: None,
                            has_password: false,
                            password_id: None,
                            status: "failed",
                            reason: Some("materialize_failed"),
                            encoding: None,
                            encoding_corrected: false,
                            damaged_volumes_json: None,
                            test_report_json: None,
                        },
                    );
                }
                skipped.push(failed);
                continue;
            }
        }

        // Header-based detection first, then scanner confirmation
        let header_result = crate::detect::probe_file_header(&candidate.path);
        let _has_non_archive_header = {
            let mut file = match std::fs::File::open(&candidate.path) {
                Ok(f) => f,
                Err(_) => {
                    failed_count += 1;
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
            && (embedded_policy.mode == smartzip_core::EmbeddedScanMode::Ask
                || (embedded_policy.mode == smartzip_core::EmbeddedScanMode::Auto
                    && candidate.depth > 0))
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
        let header_archive = header_result
            .as_ref()
            .is_some_and(|(_, offset)| *offset == 0);
        let explicit_scan = matches!(request.scanner.mode, smartzip_scanner::ScanMode::Deep)
            || matches!(
                embedded_policy.mode,
                smartzip_core::EmbeddedScanMode::All | smartzip_core::EmbeddedScanMode::Aggressive
            );
        let findings: Vec<_> = if volume_set_for_candidate.is_none()
            && (candidate.source == CandidateSource::RootInput || !header_archive || explicit_scan)
            && should_scan_candidate_for_embedded(
                &candidate,
                &embedded_policy,
                nested_embedded_enabled,
                request.confirm_large_scan,
                &events,
                &task_id,
            ) {
            if let Some(limit) = scan_with
                .scan_limit()
                .filter(|limit| std::fs::metadata(&candidate.path).is_ok_and(|m| m.len() > *limit))
            {
                events.push(TaskEvent {
                    task_id: task_id.clone(),
                    kind: TaskEventKind::Warning {
                        message: format!(
                            "embedded scan limited to first {} bytes of {}",
                            limit,
                            candidate.path.display()
                        ),
                    },
                });
            }
            scan_with
                .scan_path(&candidate.path)
                .unwrap_or_default()
                .into_iter()
                .filter(|finding| {
                    candidate.source == CandidateSource::RootInput
                        || finding_meets_min_size(finding, &embedded_policy)
                })
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

        // Business-container skips are an efficiency policy for nested files.
        // Explicit root inputs still reach the archive backend.
        if candidate.depth > 0 && candidate.detected_format == Some(ArchiveFormat::Zip) {
            if let Some(kind) = ext_business_container_kind(&candidate.path).or_else(|| {
                crate::container::classify_zip_path(
                    volume_archive_path.as_deref().unwrap_or(&candidate.path),
                )
            }) {
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

        // For volume sets, preparation already supplied the canonical backend
        // entrypoint; candidate.path remains the logical input identity.
        let (archive_path, _archive_temp, _volume_materialized_keep) =
            if let Some(path) = volume_archive_path {
                (path, None, volume_materialized)
            } else {
                let inp = match materialize_archive_input(&candidate) {
                    Ok(input) => input,
                    Err(error) => {
                        failed_count += 1;
                        events.push(TaskEvent::failed(task_id.clone(), &error));
                        record_skip(history, &task_id, &candidate, &error.to_string());
                        skipped.push(candidate);
                        continue;
                    }
                };
                let p = inp.path.clone();
                (p, inp._temp, None)
            };
        // Shadow the earlier volume_materialized binding with the kept handle so it lives through the rest of the iteration.
        let _volume_keep = _volume_materialized_keep;

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
        // Historical fingerprints remember credentials and encoding. Extraction
        // itself always checks this invocation's actual output collision.

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
            zip_encoding_assessment = assess_zip_encoding(&archive_path, None).await;
        }
        if let Some(assessment) = &zip_encoding_assessment {
            events.push(TaskEvent {
                task_id: task_id.clone(),
                kind: TaskEventKind::EncodingDetected(assessment.context.detected.clone()),
            });
        }

        let _key = candidate_key(&candidate);
        let archive_facts = ArchiveFacts {
            container: candidate.detected_format.clone(),
            ..ArchiveFacts::default()
        };
        let output_dir = output_dir_for_candidate(&request.output_dir, &candidate);

        let mut extracted = false;
        let mut terminal_skip = false;
        let mut last_error = None;
        let mut saw_wrong_password = false;
        let mut saw_password_indeterminate = false;
        let mut password_prompt_cancelled = false;
        let mut actual_output_dir = output_dir.clone();
        // File-grain success state, recorded once after the try loop.
        let mut candidate_password_id: Option<i64> = None;
        let mut candidate_has_password = false;
        let mut candidate_encoding_used: Option<String> = None;
        // Resolve encoding once per node. A deliberate skip is a node outcome.
        let encoding_choice = tokio::select! {
            _ = cancellation.cancelled() => { None }
            result = resolve_encoding_mode(&archive_path, candidate_encoding_mode.clone(), zip_encoding_assessment.as_ref(), encoding_prompter) => result?,
        };
        let mut skip_reason = "target_exists";
        if encoding_choice.is_none() {
            terminal_skip = true;
            skip_reason = "encoding_skipped";
        }
        let total_attempts = candidate_passwords.len();
        let mut attempt_index = 0;
        let mut attempts: VecDeque<_> = candidate_passwords.into_iter().collect();
        let mut prompted = false;
        let mut saw_password_required = false;
        while !terminal_skip && !cancellation.is_cancelled() {
            let password = if let Some(password) = attempts.pop_front() {
                password
            } else if !prompted
                && (saw_wrong_password
                    || saw_password_required
                    || saw_password_indeterminate
                    || last_error.is_none())
            {
                prompted = true;
                let input = if let Some(prompter) = password_prompter {
                    tokio::select! { _ = cancellation.cancelled() => None, value = prompter.prompt(&candidate.path) => value }
                } else {
                    None
                };
                match input.filter(|value| !value.is_empty()) {
                    Some(value) => PasswordCandidate {
                        id: None,
                        value,
                        source: smartzip_passwords::PasswordSource::Manual,
                    },
                    None => {
                        password_prompt_cancelled = true;
                        break;
                    }
                }
            } else {
                break;
            };
            if task_context.is_cancelled() {
                break;
            }
            let pw_value = password_value(&password);
            attempt_index += 1;
            events.push(TaskEvent {
                task_id: task_id.clone(),
                kind: TaskEventKind::PasswordTried {
                    candidate_id: password.id,
                },
            });
            events.push(TaskEvent {
                task_id: task_id.clone(),
                kind: TaskEventKind::Progress(smartzip_core::TaskProgress::indeterminate(format!(
                    "Trying password [{}/{}] ({}) by extraction for {}",
                    attempt_index,
                    total_attempts.max(attempt_index),
                    password_source_label(&password),
                    candidate.path.display()
                ))),
            });
            let encoding = encoding_choice
                .clone()
                .expect("encoding skip exits before attempts");
            candidate_encoding_used = Some(encoding_mode_label(&encoding));
            let staged_usage = std::cell::Cell::new(committed_usage);
            let extracted_encrypted = std::cell::Cell::new(None);
            let result = output_materializer
                .materialize(
                    MaterializeRequest {
                        output_dir: output_dir.clone(),
                        archive_path: candidate.path.clone(),
                        commit_policy: CommitPolicy::FailIfExists,
                        archive_stem: Some(
                            if candidate.source == CandidateSource::EmbeddedFinding
                                && candidate.depth == 0
                            {
                                candidate
                                    .relative_path
                                    .file_name()
                                    .unwrap_or_default()
                                    .to_string_lossy()
                                    .into_owned()
                            } else {
                                archive_stem(&candidate.path).to_string_lossy().into_owned()
                            },
                        ),
                        layout_policy: request.layout_policy,
                        single_root_name_policy: request.single_root_name_policy,
                    },
                    |temp_output_dir| {
                        let archive_path = archive_path.clone();
                        let format = candidate.detected_format.clone();
                        let password = pw_value.clone();
                        let context = task_context.clone();
                        let facts = &archive_facts;
                        let limits = &request.limits;
                        let staged_usage = &staged_usage;
                        let extracted_encrypted = &extracted_encrypted;
                        async move {
                            let extracted = crate::budget::monitor(
                                &temp_output_dir,
                                limits,
                                committed_usage,
                                context.clone(),
                                backend_call(
                                    "archive-backend",
                                    "extract",
                                    &archive_path,
                                    backend.extract_with_facts_and_context(
                                        ExtractArchiveRequest {
                                            archive: archive_path.clone(),
                                            format,
                                            output_dir: temp_output_dir.clone(),
                                            password,
                                            encoding,
                                        },
                                        facts,
                                        context.clone(),
                                    ),
                                ),
                            )
                            .await?;
                            extracted_encrypted.set(extracted.encrypted);
                            staged_usage.set(crate::budget::inspect(
                                &temp_output_dir,
                                limits,
                                committed_usage,
                            )?);
                            Ok(())
                        }
                    },
                    collision_resolver.as_ref(),
                )
                .await;
            match result {
                Ok(result) => {
                    committed_usage = staged_usage.get();
                    if let Some(plan) = result.layout_plan.as_ref() {
                        for message in &plan.warnings {
                            events.push(TaskEvent {
                                task_id: task_id.clone(),
                                kind: TaskEventKind::Warning {
                                    message: message.clone(),
                                },
                            });
                        }
                    }
                    if result.output_dir != output_dir {
                        candidate.relative_path =
                            output_relative_path_for(&request.output_dir, &result.output_dir);
                    }
                    actual_output_dir = result.output_dir;
                    // Credential success needs evidence that encryption was used.
                    candidate_has_password = pw_value.as_deref().is_some_and(|p| !p.is_empty())
                        && (extracted_encrypted.get() == Some(true)
                            || saw_password_required
                            || saw_wrong_password
                            || saw_password_indeterminate
                            || (candidate.detected_format == Some(ArchiveFormat::Zip)
                                && NativeZipBackend::new()
                                    .has_encrypted_entries(&archive_path)
                                    .unwrap_or(false)));
                    if candidate_has_password {
                        candidate_password_id = passwords.record_success(&password).ok().flatten();
                        remember_batch_password(
                            &mut batch_passwords,
                            &password.value,
                            candidate_password_id,
                        );
                    }
                    extracted = true;
                    break;
                }
                Err(failure) => {
                    if failure.kind == materialize::MaterializeFailureKind::CollisionSkipped {
                        terminal_skip = true;
                        break;
                    }
                    if let Some(path) = &failure.preserved_temp_dir {
                        events.push(TaskEvent {
                            task_id: task_id.clone(),
                            kind: TaskEventKind::Warning {
                                message: format!("recovery output retained at {}", path.display()),
                            },
                        });
                    }
                    if failure.kind == materialize::MaterializeFailureKind::ExtractFailed {
                        match &failure.error {
                            smartzip_core::SmartZipError::WrongPassword { .. } => {
                                saw_wrong_password = true;
                                let _ = passwords.record_failure(&password);
                                continue;
                            }
                            smartzip_core::SmartZipError::PasswordRequired { .. }
                                if pw_value.as_deref().is_none_or(str::is_empty) =>
                            {
                                saw_password_required = true;
                                continue;
                            }
                            smartzip_core::SmartZipError::PasswordIndeterminate { .. } => {
                                saw_password_indeterminate = true;
                                last_error = Some(failure.error);
                                continue;
                            }
                            _ => {}
                        }
                    }
                    last_error = Some(failure.error);
                    break;
                }
            }
        }
        if cancellation.is_cancelled()
            || matches!(last_error, Some(smartzip_core::SmartZipError::Cancelled))
        {
            was_cancelled = true;
            record_skip(history, &task_id, &candidate, "cancelled");
            skipped.push(candidate);
            break;
        }

        if !extracted && !terminal_skip {
            if password_prompt_cancelled {
                if password_prompter.is_none() {
                    failed_count += 1;
                    let error = if saw_password_indeterminate {
                        smartzip_core::SmartZipError::PasswordIndeterminate {
                            path: candidate.path.clone(),
                        }
                    } else if saw_wrong_password {
                        smartzip_core::SmartZipError::WrongPassword {
                            path: candidate.path.clone(),
                        }
                    } else {
                        smartzip_core::SmartZipError::PasswordRequired {
                            path: candidate.path.clone(),
                        }
                    };
                    events.push(TaskEvent::failed(task_id.clone(), &error));
                }
                if let Some(recorder) = history {
                    recorder.record_file_extraction(
                        &task_id,
                        crate::history::FileExtractionRow {
                            input_path: &original_input_path,
                            sample_hash: sample_hash.as_deref(),
                            file_size: sample_size,
                            offset: candidate.embedded_offset.map(|o| o as i64),
                            output_path: None,
                            has_password: false,
                            password_id: None,
                            status: if password_prompter.is_none() {
                                "failed"
                            } else {
                                "skipped"
                            },
                            reason: Some(if saw_password_indeterminate {
                                "password_indeterminate"
                            } else if saw_wrong_password {
                                "wrong_password"
                            } else {
                                "password_required"
                            }),
                            encoding: candidate_encoding_used.as_deref(),
                            encoding_corrected: reused_confirmed_encoding
                                || matches!(request.encoding_mode, EncodingMode::Override(_)),
                            damaged_volumes_json: None,
                            test_report_json: None,
                        },
                    );
                }
            } else if let Some(error) = last_error.or_else(|| {
                saw_wrong_password.then(|| smartzip_core::SmartZipError::WrongPassword {
                    path: candidate.path.clone(),
                })
            }) {
                failed_count += 1;
                // File-grain failure: classify the reason from the error so
                // `history files --reason` can filter later.
                let reason = match &error {
                    smartzip_core::SmartZipError::PasswordIndeterminate { .. } => {
                        "password_indeterminate"
                    }
                    smartzip_core::SmartZipError::WrongPassword { .. }
                    | smartzip_core::SmartZipError::PasswordRequired { .. } => "wrong_password",
                    smartzip_core::SmartZipError::Io { .. } => "not_found",
                    smartzip_core::SmartZipError::CorruptedArchive { .. } => "corrupt",
                    _ => "backend_failed",
                };
                if let Some(recorder) = history {
                    recorder.record_file_extraction(
                        &task_id,
                        crate::history::FileExtractionRow {
                            input_path: &original_input_path,
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
                            test_report_json: None,
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
                        input_path: &original_input_path,
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
                        test_report_json: None,
                    },
                );
            }
        }
        if terminal_skip {
            record_skip(history, &task_id, &candidate, skip_reason);
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
                    input_path: &original_input_path,
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
                    test_report_json: None,
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

        // Staging usage was counted before commit/recycling, including
        // containers which will subsequently be expanded and recycled.
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
            if enqueued.len() >= request.limits.max_nested_candidates {
                failed_count += 1;
                events.push(TaskEvent::failed(
                    task_id.clone(),
                    &crate::budget::exceeded("nested candidate limit exceeded"),
                ));
                task_context.cancel();
                break;
            }
            enqueued.push(nested.clone());
            queue.push_back(nested);
        }

        // For volume sets, recycle all members that are inside the managed output root; for singles, recycle the single candidate.
        if let Some(set) = volume_set_for_candidate {
            for member in set.members {
                let synthetic = ExtractionCandidate {
                    path: member.path.clone(),
                    relative_path: member.path.clone(),
                    depth: candidate.depth,
                    source: CandidateSource::ExtractedFile,
                    detected_format: Some(set.format.clone()),
                    embedded_offset: None,
                    embedded_size: None,
                };
                if let Some(path) = recyclable_nested_archive_path(&synthetic, &request.output_dir)
                {
                    if let Err(error) =
                        recycle_archive(archive_recycler.clone(), path.clone()).await
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
        } else if let Some(path) = recyclable_nested_archive_path(&candidate, &request.output_dir) {
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

    let status = crate::history::TaskCompletionStatus::from_counts(
        processed.len(),
        failed_count,
        was_cancelled,
    );
    events.push(TaskEvent {
        task_id: task_id.clone(),
        kind: TaskEventKind::Finished {
            status: format!("{status:?}").to_ascii_lowercase(),
        },
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
        recorder.finish(
            &task_id,
            crate::history::TaskOutcome {
                status,
                output_path: Some(&request.output_dir),
            },
        );
    }

    completion.complete();
    Ok(ExtractWorkflowResult {
        status,
        failed_count,
        task_id,
        processed,
        skipped,
        enqueued,
        events: snapshot,
    })
}

fn volume_set_key(set: &crate::volumes::VolumeSet) -> String {
    let mut paths: Vec<String> = set
        .members
        .iter()
        .map(|m| m.path.display().to_string())
        .collect();
    paths.sort();
    format!("{}:{}", set.format.as_str(), paths.join("|"))
}
