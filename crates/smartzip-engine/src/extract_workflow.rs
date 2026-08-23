//! Recursive extraction workflow implementation.

use smartzip_archive::{ArchiveExecutor, ExtractArchiveRequest, NativeZipBackend, TestRequest};
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
    order_password_candidates, password_attempt_index, password_source_label, password_value,
    remember_batch_password,
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
    let task_context = backend.begin_task(task_id.clone(), std::sync::Arc::new(events.clone()));
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
    let mut volume_resolver = VolumeResolver::new();
    let mut processed_volume_keys: std::collections::HashSet<String> =
        std::collections::HashSet::new();

    loop {
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
        // Resolve and materialize volumes once through the shared preparation path.
        // Embedded findings at non-zero offset still bypass sibling discovery.
        let mut volume_materialized: Option<crate::volumes::materialize::MaterializedVolumeSet> =
            None;
        let mut volume_archive_path: Option<std::path::PathBuf> = None;
        let mut volume_set_for_candidate: Option<crate::volumes::VolumeSet> = None;
        let preparation = if candidate.source == CandidateSource::RootInput {
            let resolution = volume_resolver.resolve(&candidate);
            if let VolumeResolution::Resolved(set)
            | VolumeResolution::ResolvedWithWarnings { set, .. } = &resolution
            {
                let key = volume_set_key(set);
                if processed_volume_keys.contains(&key) {
                    continue;
                }
                processed_volume_keys.insert(key);
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
                hist_saw_failure = true;
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
                hist_saw_failure = true;
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
                hist_saw_failure = true;
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

        // For volume sets, preparation already supplied the canonical backend
        // entrypoint; candidate.path remains the logical input identity.
        let (archive_path, _archive_temp, _volume_materialized_keep) =
            if let Some(path) = volume_archive_path {
                (path, None, volume_materialized)
            } else {
                let inp = materialize_archive_input(&candidate)?;
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
                                input_path: &original_input_path,
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
            let reader = NativeZipBackend::new();
            if let Ok(is_encrypted) = reader.has_encrypted_entries(&archive_path) {
                if !is_encrypted {
                    zip_encoding_assessment = assess_zip_encoding(&archive_path, None).await;
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
        let archive_facts = ArchiveFacts {
            container: candidate.detected_format.clone(),
            ..ArchiveFacts::default()
        };
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
        // The executor owns backend selection; test before extraction for every
        // archive so password failures are classified before materialization.
        let test_before_extract = true;
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
                    backend.test_with_context(
                        TestRequest {
                            archive: archive_path.clone(),
                            format: candidate.detected_format.clone(),
                            password: pw_value.clone(),
                            encoding: candidate_encoding_mode.clone(),
                        },
                        std::sync::Arc::clone(&task_context),
                    ),
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
                            zip_encoding_assessment =
                                assess_zip_encoding(&archive_path, pw_value.clone()).await;
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
                        let extract_facts = archive_facts.clone();
                        let extract_context = task_context.clone();
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
                                        backend.extract_with_facts_and_context(
                                            ExtractArchiveRequest {
                                                archive: extract_archive_path.clone(),
                                                format: extract_format,
                                                output_dir: temp_output_dir,
                                                password: extract_password,
                                                encoding: extract_encoding,
                                            },
                                            &extract_facts,
                                            std::sync::Arc::clone(&extract_context),
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
                let extract_facts = archive_facts.clone();
                let extract_context = task_context.clone();
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
                                backend.extract_with_facts_and_context(
                                    ExtractArchiveRequest {
                                        archive: extract_archive_path.clone(),
                                        format: extract_format,
                                        output_dir: temp_output_dir,
                                        password: extract_password,
                                        encoding: extract_encoding,
                                    },
                                    &extract_facts,
                                    std::sync::Arc::clone(&extract_context),
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
                                backend.test_with_context(
                                    TestRequest {
                                        archive: archive_path.clone(),
                                        format: candidate.detected_format.clone(),
                                        password: Some(pw.clone()),
                                        encoding: candidate_encoding_mode.clone(),
                                    },
                                    std::sync::Arc::clone(&task_context),
                                ),
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
                                        zip_encoding_assessment =
                                            assess_zip_encoding(&archive_path, Some(pw.clone()))
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
                                    let extract_facts = archive_facts.clone();
                                    let extract_context = task_context.clone();
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
                                                    backend.extract_with_facts_and_context(
                                                        ExtractArchiveRequest {
                                                            archive: extract_archive_path.clone(),
                                                            format: extract_format,
                                                            output_dir: temp_output_dir,
                                                            password: Some(extract_password),
                                                            encoding: extract_encoding,
                                                        },
                                                        &extract_facts,
                                                        std::sync::Arc::clone(&extract_context),
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
                            let extract_facts = archive_facts.clone();
                            let extract_context = task_context.clone();
                            let extract_encoding = resolve_encoding_mode(
                                &archive_path,
                                candidate_encoding_mode.clone(),
                                zip_encoding_assessment.as_ref(),
                                encoding_prompter,
                            )
                            .await?;
                            candidate_encoding_used = Some(encoding_mode_label(&extract_encoding));
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
                                            backend.extract_with_facts_and_context(
                                                ExtractArchiveRequest {
                                                    archive: extract_archive_path.clone(),
                                                    format: extract_format,
                                                    output_dir: temp_output_dir,
                                                    password: Some(extract_password),
                                                    encoding: extract_encoding,
                                                },
                                                &extract_facts,
                                                std::sync::Arc::clone(&extract_context),
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
                            input_path: &original_input_path,
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

fn volume_set_key(set: &crate::volumes::VolumeSet) -> String {
    let mut paths: Vec<String> = set
        .members
        .iter()
        .map(|m| m.path.display().to_string())
        .collect();
    paths.sort();
    format!("{}:{}", set.format.as_str(), paths.join("|"))
}
