//! Recursive extraction workflow implementation.

use smartzip_archive::{ArchiveExecutor, ExtractArchiveRequest, NativeZipBackend};
use smartzip_core::{
    ArchiveFacts, ArchiveFormat, AttemptId, DecisionId, EncodingMode, NodeId, TaskEvent,
    TaskEventKind, TaskId,
};
use smartzip_passwords::{PasswordCandidate, PasswordService};
use smartzip_scanner::{Confidence, EmbeddedArchiveFinding, EmbeddedScanner};
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::Read;

use crate::access::prepare_resolved_archive;
use crate::backend_util::{backend_call, confidence_score};
use crate::encoding_flow::{encoding_mode_label, resolve_encoding_mode};
use crate::events::{EventSink, TaskEventListener};
use crate::interactive::{
    EmbeddedSelectionChoice, InteractiveEmbeddedPrompter, InteractiveEncodingPrompter,
    InteractiveOutputPrompter, InteractivePasswordPrompter,
};
use crate::materialize::{
    self, CollisionAction, CommitPolicy, MaterializeRequest, OutputMaterializer,
};
use crate::nested::{
    archive_stem, candidate_key, candidate_output_relative_path, discover_nested_candidates,
    discover_nested_candidates_from_inventory, output_dir_for_candidate, output_relative_path_for,
    record_skip, recyclable_nested_archive_path, recycle_archive, root_embedded_candidates,
};
use crate::password_order::password_source_label;
use crate::policy::{
    embedded_policy_from_request, ext_business_container_kind, finding_meets_min_size,
    full_root_scanner_config, should_scan_candidate_for_embedded,
};
use crate::types::{
    ArchiveRecycleHandler, CandidateSource, ExtractWorkflowRequest, ExtractWorkflowResult,
    ExtractionCandidate,
};
use crate::volumes::VolumeResolver;

/// Shared by concurrent roots; claims never hold a borrow across an await.
#[derive(Default)]
pub(crate) struct TaskDedup {
    seen: HashSet<String>,
    volumes: HashSet<String>,
    members: HashSet<std::path::PathBuf>,
    // Claim overlapping candidate sets before trial, but consume only winners.
    volume_locks: HashMap<std::path::PathBuf, std::sync::Arc<tokio::sync::Mutex<()>>>,
}

struct ExecutionNode {
    id: NodeId,
    root_id: NodeId,
    generation: u64,
    candidate: ExtractionCandidate,
}

struct StageLeaseGuard<'a> {
    execution: Option<&'a dyn crate::ExecutionStateRecorder>,
    task_id: TaskId,
    node_id: NodeId,
}

impl Drop for StageLeaseGuard<'_> {
    fn drop(&mut self) {
        if let Some(execution) = self.execution {
            execution.release_stage(&self.task_id, &self.node_id);
        }
    }
}

fn record_failure(
    failed_count: &mut usize,
    config: Option<&smartzip_config::SmartZipConfig>,
    cancellation: &crate::TaskCancellation,
) {
    *failed_count += 1;
    if config.is_some_and(|config| config.extraction.on_error == smartzip_config::OnError::Stop) {
        cancellation.stop_on_error();
    }
}

/// Override how successfully processed nested archives are recycled.
///
/// This is primarily useful for deterministic tests and platform hosts
/// that provide their own recycle-bin integration.
pub(crate) async fn extract_recursive_with_listener_interactive<B: ArchiveExecutor>(
    engine_scanner: &EmbeddedScanner,
    run_policy: Option<&crate::CompiledRunPolicy>,
    min_embedded_size_bytes: u64,
    archive_recycler: &ArchiveRecycleHandler,
    cancellation: crate::TaskCancellation,
    backend: &B,
    passwords: &PasswordService<'_>,
    request: ExtractWorkflowRequest,
    password_prompter: Option<&dyn InteractivePasswordPrompter>,
    output_prompter: Option<&dyn InteractiveOutputPrompter>,
    embedded_prompter: Option<&dyn InteractiveEmbeddedPrompter>,
    encoding_prompter: Option<&dyn InteractiveEncodingPrompter>,
    listener: Option<TaskEventListener>,
    history: Option<&dyn crate::history::TaskHistoryRecorder>,
    execution: Option<&dyn crate::ExecutionStateRecorder>,
    identity: crate::ExtractTaskIdentity,
    batch_passwords: std::rc::Rc<std::cell::RefCell<Vec<PasswordCandidate>>>,
    task_budget: std::sync::Arc<crate::budget::TaskBudget>,
    dedup: std::rc::Rc<std::cell::RefCell<TaskDedup>>,
    source_cleanup: crate::source_cleanup::SharedCleanup,
) -> smartzip_core::Result<ExtractWorkflowResult> {
    let config = run_policy.map(crate::CompiledRunPolicy::values);
    let may_prompt =
        config.is_none_or(|c| c.interaction.mode != smartzip_config::InteractionMode::Never);
    let password_prompter = password_prompter.filter(|_| passwords.allows_prompt() && may_prompt);
    let embedded_prompter = embedded_prompter.filter(|_| may_prompt);
    let policy_output = run_policy.map(|policy| crate::run_policy::PolicyOutputPrompter {
        policy,
        delegate: output_prompter,
    });
    let output_prompter = policy_output
        .as_ref()
        .map(|p| p as &dyn InteractiveOutputPrompter)
        .or(output_prompter);
    let policy_encoding = run_policy.map(|policy| crate::run_policy::PolicyEncodingPrompter {
        policy,
        delegate: encoding_prompter,
    });
    let encoding_prompter = policy_encoding
        .as_ref()
        .map(|p| p as &dyn InteractiveEncodingPrompter)
        .or(encoding_prompter);
    let delete_recycler: ArchiveRecycleHandler = std::sync::Arc::new(std::fs::remove_file);
    let archive_recycler = if config
        .is_some_and(|c| c.extraction.cleanup.nested_archives == smartzip_config::Cleanup::Delete)
    {
        &delete_recycler
    } else {
        archive_recycler
    };
    let task_id = identity.task_id;
    let legacy_history = if execution.is_none_or(|e| !e.durable()) {
        history
    } else {
        None
    };
    let events = EventSink::new(listener);
    let task_context = backend.begin_task_with_cancellation(
        task_id.clone(),
        std::sync::Arc::new(events.clone()),
        cancellation.token().child_token(),
    );
    let nested_scanner = if request.scanner == *engine_scanner.config() {
        None
    } else {
        Some(EmbeddedScanner::new(request.scanner.clone()))
    };
    let nested_scanner = nested_scanner.as_ref().unwrap_or(engine_scanner);
    let root_scanner = config
        .is_none_or(|c| c.extraction.embedded.root != smartzip_config::RootScan::Off)
        .then(|| EmbeddedScanner::new(full_root_scanner_config(&request.scanner)));

    events.push(TaskEvent::started(task_id.clone()));
    if let Some(policy) = run_policy {
        policy.emit_plan(&events, &task_id);
    }
    let mut queue = VecDeque::new();
    let mut processed = Vec::new();
    let mut skipped = Vec::new();
    let mut enqueued = Vec::new();
    let output_materializer = OutputMaterializer::default();
    let root_input_total = request.inputs.len();
    let mut root_input_started = 0usize;
    let mut embedded_policy = embedded_policy_from_request(&request);
    embedded_policy.min_finding_size_bytes = min_embedded_size_bytes;
    let nested_embedded_enabled = config.is_none_or(|c| {
        c.extraction.recursion.enabled
            && c.extraction.embedded.nested != smartzip_config::NestedScan::Off
    }) && !matches!(
        embedded_policy.mode,
        smartzip_core::EmbeddedScanMode::Ignore
    );
    let mut embedded_extract_all = false;

    if identity.roots.len() != request.inputs.len() {
        return Err(smartzip_core::SmartZipError::ResourceLimit {
            detail: "task root identity count does not match input count".into(),
        });
    }
    for (input, root) in request.inputs.iter().zip(identity.roots) {
        if root.candidate.path != *input {
            return Err(smartzip_core::SmartZipError::ResourceLimit {
                detail: "task root identity does not match its input".into(),
            });
        }
        queue.push_back(ExecutionNode {
            id: root.node_id,
            root_id: root.root_id,
            generation: root.generation,
            candidate: root.candidate,
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
    // History: register the task up-front and accumulate metrics as the
    // loop runs. All history writes are best-effort — a repo error becomes
    // a Warning event through the recorder and never aborts extraction.
    if let Some(recorder) = legacy_history {
        recorder.start_extract(&task_id, Some(&request.output_dir));
    }
    let mut completion = crate::history::CompletionGuard::new(
        legacy_history,
        task_id.clone(),
        events.clone(),
        cancellation.user_token(),
    );
    let mut failed_count = 0usize;
    let mut was_cancelled = false;
    let mut volume_resolver = VolumeResolver::new();

    'nodes: loop {
        if failed_count > 0
            && config.is_some_and(|c| c.extraction.on_error == smartzip_config::OnError::Stop)
        {
            if let Some(policy) = run_policy {
                policy.decision(
                    &events,
                    &task_id,
                    "scheduler",
                    "stop",
                    "error_in_previous_candidate",
                    "extraction.on_error",
                );
            }
            break;
        }
        if cancellation.is_cancelled() {
            was_cancelled = cancellation.is_user_cancelled();
            break;
        }
        if task_context.is_cancelled() {
            was_cancelled = cancellation.is_user_cancelled();
            break;
        }
        let Some(node) = queue.pop_front() else {
            break;
        };
        let _stage_lease = StageLeaseGuard {
            execution,
            task_id: task_id.clone(),
            node_id: node.id.clone(),
        };
        let mut candidate = node.candidate;
        let attempt_id = AttemptId::new();
        if !enter_stage(
            execution,
            &task_id,
            &node.id,
            node.generation,
            "ready",
            crate::coordinator::Stage::ResolveInputs,
            Some(&attempt_id),
            &cancellation,
        )
        .await?
        {
            was_cancelled |= cancellation.is_user_cancelled();
            finish_interrupted_node(
                execution,
                &task_id,
                &node.id,
                node.generation,
                &cancellation,
            )
            .await?;
            skipped.push(candidate);
            break 'nodes;
        }
        let original_input_path = candidate.path.clone();
        let key = candidate_key(&candidate);
        let is_new = dedup.borrow_mut().seen.insert(key);
        // Split the merged skip so each reason lands its own file_extractions
        // row (duplicate within this run / over recursion limit / a non-first
        // volume of a split set).
        if !is_new {
            record_skip(legacy_history, &task_id, &candidate, "duplicate");
            finish_execution(
                execution,
                &task_id,
                &node.id,
                node.generation,
                "skipped",
                Some("duplicate"),
                None,
                false,
            )
            .await?;
            skipped.push(candidate);
            continue;
        }
        if candidate.depth > request.recursion_limit {
            record_skip(legacy_history, &task_id, &candidate, "recursion_limit");
            finish_execution(
                execution,
                &task_id,
                &node.id,
                node.generation,
                "skipped",
                Some("recursion_limit"),
                None,
                false,
            )
            .await?;
            skipped.push(candidate);
            continue;
        }
        let absolute_input =
            std::path::absolute(&candidate.path).unwrap_or_else(|_| candidate.path.clone());
        if candidate.source != CandidateSource::EmbeddedFinding
            && dedup.borrow().members.contains(&absolute_input)
        {
            record_skip(legacy_history, &task_id, &candidate, "duplicate");
            finish_execution(
                execution,
                &task_id,
                &node.id,
                node.generation,
                "skipped",
                Some("duplicate"),
                None,
                false,
            )
            .await?;
            skipped.push(candidate);
            continue;
        }
        let original_candidate = candidate.clone();
        let resolution = if config.is_some_and(|c| !c.extraction.volumes.auto_discover) {
            crate::volumes::VolumeResolution::Single
        } else {
            volume_resolver.resolve(&candidate)
        };
        let sets: Vec<_> = match &resolution {
            crate::volumes::VolumeResolution::GroupingAmbiguous { hypotheses } => hypotheses
                .iter()
                .filter_map(|h| h.trial_set.as_ref())
                .collect(),
            _ => resolution.resolved_set().into_iter().collect(),
        };
        let mut member_paths: Vec<_> = sets
            .iter()
            .flat_map(|set| &set.members)
            .map(|member| std::path::absolute(&member.path).unwrap_or_else(|_| member.path.clone()))
            .collect();
        member_paths.sort();
        member_paths.dedup();
        let volume_locks: Vec<_> = {
            let mut dedup = dedup.borrow_mut();
            member_paths
                .into_iter()
                .map(|path| dedup.volume_locks.entry(path).or_default().clone())
                .collect()
        };
        let needs_volume_lease = !volume_locks.is_empty();
        if needs_volume_lease {
            if let Some(execution) = execution {
                // A root waiting on a sibling must not retain resources that
                // the sibling needs to finish its extraction stage.
                execution.release_stage(&task_id, &node.id);
            }
        }
        let mut _volume_guards = Vec::new();
        // Stable path order prevents lock cycles between overlapping groups;
        // unrelated archive groups retain their existing concurrency.
        for lock in volume_locks {
            let guard = tokio::select! {
                guard = lock.lock_owned() => guard,
                _ = cancellation.cancelled() => {
                    was_cancelled |= cancellation.is_user_cancelled();
                    finish_interrupted_node(execution, &task_id, &node.id, node.generation, &cancellation).await?;
                    skipped.push(candidate);
                    break 'nodes;
                }
            };
            _volume_guards.push(guard);
        }
        // Another root may have selected this member while we waited.
        if candidate.source != CandidateSource::EmbeddedFinding
            && dedup.borrow().members.contains(&absolute_input)
        {
            record_skip(legacy_history, &task_id, &candidate, "duplicate");
            finish_execution(
                execution,
                &task_id,
                &node.id,
                node.generation,
                "skipped",
                Some("duplicate"),
                None,
                false,
            )
            .await?;
            skipped.push(candidate);
            continue;
        }
        if needs_volume_lease
            && !enter_stage(
                execution,
                &task_id,
                &node.id,
                node.generation,
                "running",
                crate::coordinator::Stage::ResolveInputs,
                Some(&attempt_id),
                &cancellation,
            )
            .await?
        {
            was_cancelled |= cancellation.is_user_cancelled();
            finish_interrupted_node(
                execution,
                &task_id,
                &node.id,
                node.generation,
                &cancellation,
            )
            .await?;
            skipped.push(candidate);
            break 'nodes;
        }
        let mut remaining_groups = VecDeque::new();
        if let crate::volumes::VolumeResolution::GroupingAmbiguous { hypotheses } = &resolution {
            let mut seen = HashSet::new();
            for hypothesis in hypotheses {
                if let Some(set) = &hypothesis.trial_set {
                    if seen.insert(volume_set_key(set)) {
                        remaining_groups.push_back(
                            crate::volumes::VolumeResolution::ResolvedWithWarnings {
                                set: set.clone(),
                                warnings: hypothesis.warnings.clone(),
                            },
                        );
                    }
                }
            }
        }
        let group_trials = !remaining_groups.is_empty();
        let all_groups = remaining_groups.clone();
        let mut group_passwords = Vec::new();
        let mut group_password_needed = false;
        let mut next_resolution = Some(remaining_groups.pop_front().unwrap_or(resolution));
        let mut group_attempt = 1usize;
        'groupings: loop {
            // Resolve and materialize volumes once through the shared preparation path.
            // Carved findings bypass sibling discovery, including offset-zero payloads.
            let mut volume_materialized: Option<
                crate::volumes::materialize::MaterializedVolumeSet,
            > = None;
            let mut volume_archive_path: Option<std::path::PathBuf> = None;
            let mut volume_set_for_candidate: Option<crate::volumes::VolumeSet> = None;
            let preparation = if config.is_some_and(|c| !c.extraction.volumes.auto_discover) {
                crate::volumes::VolumePreparation::Single(candidate)
            } else if candidate.source != CandidateSource::EmbeddedFinding {
                let resolution = next_resolution.take().expect("volume trial resolution");
                if let Some(set) = resolution.resolved_set().filter(|_| !group_trials) {
                    let key = volume_set_key(set);
                    let new_volume = dedup.borrow_mut().volumes.insert(key);
                    if !new_volume {
                        record_skip(legacy_history, &task_id, &candidate, "duplicate");
                        finish_execution(
                            execution,
                            &task_id,
                            &node.id,
                            node.generation,
                            "skipped",
                            Some("duplicate"),
                            None,
                            false,
                        )
                        .await?;
                        skipped.push(candidate);
                        continue 'nodes;
                    }
                    for member in &set.members {
                        dedup.borrow_mut().members.insert(
                            std::path::absolute(&member.path)
                                .unwrap_or_else(|_| member.path.clone()),
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
                    record_failure(&mut failed_count, config, &cancellation);
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
                    if let Some(recorder) = legacy_history {
                        recorder.record_file_extraction(
                            &task_id,
                            crate::history::FileExtractionRow::failed(
                                &original_input_path,
                                failed.embedded_offset,
                                "incomplete_volume",
                            ),
                        );
                    }
                    finish_execution(
                        execution,
                        &task_id,
                        &node.id,
                        node.generation,
                        "failed",
                        Some("incomplete_volume"),
                        None,
                        false,
                    )
                    .await?;
                    skipped.push(failed);
                    continue 'nodes;
                }
                crate::volumes::VolumePreparation::GroupingAmbiguous {
                    candidate: failed,
                    hypotheses,
                } => {
                    record_failure(&mut failed_count, config, &cancellation);
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
                    if let Some(recorder) = legacy_history {
                        recorder.record_file_extraction(
                            &task_id,
                            crate::history::FileExtractionRow::failed(
                                &original_input_path,
                                failed.embedded_offset,
                                "grouping_ambiguous",
                            ),
                        );
                    }
                    finish_execution(
                        execution,
                        &task_id,
                        &node.id,
                        node.generation,
                        "failed",
                        Some("grouping_ambiguous"),
                        None,
                        false,
                    )
                    .await?;
                    skipped.push(failed);
                    continue 'nodes;
                }
                crate::volumes::VolumePreparation::MaterializationFailed {
                    candidate: failed,
                    error,
                } => {
                    record_failure(&mut failed_count, config, &cancellation);
                    events.push(TaskEvent {
                        task_id: task_id.clone(),
                        kind: TaskEventKind::Failed {
                            error: format!(
                                "volume materialization failed for {}: {error}",
                                failed.path.display()
                            ),
                        },
                    });
                    if let Some(recorder) = legacy_history {
                        recorder.record_file_extraction(
                            &task_id,
                            crate::history::FileExtractionRow::failed(
                                &original_input_path,
                                failed.embedded_offset,
                                "materialize_failed",
                            ),
                        );
                    }
                    finish_execution(
                        execution,
                        &task_id,
                        &node.id,
                        node.generation,
                        "failed",
                        Some("materialize_failed"),
                        None,
                        false,
                    )
                    .await?;
                    skipped.push(failed);
                    continue 'nodes;
                }
            }

            let source_paths = if source_cleanup.is_some()
                && original_candidate.source == CandidateSource::RootInput
            {
                volume_set_for_candidate
                    .as_ref()
                    .map(|set| {
                        set.members
                            .iter()
                            .map(|member| member.path.clone())
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_else(|| vec![original_input_path.clone()])
            } else {
                Vec::new()
            };
            if let Some(cleanup) = &source_cleanup {
                cleanup.borrow_mut().capture(&source_paths);
            }

            // Header-based detection first, then scanner confirmation
            if let Some(policy) = run_policy {
                embedded_policy.mode = policy.embedded_mode(candidate.depth == 0);
            }
            let header_result = if config.is_some()
                && embedded_policy.mode == smartzip_core::EmbeddedScanMode::Ignore
            {
                None
            } else {
                crate::detect::probe_file_header(&candidate.path)
            };
            let _has_non_archive_header = {
                let mut file = match std::fs::File::open(&candidate.path) {
                    Ok(f) => f,
                    Err(_) => {
                        record_failure(&mut failed_count, config, &cancellation);
                        record_skip(legacy_history, &task_id, &candidate, "not_found");
                        finish_execution(
                            execution,
                            &task_id,
                            &node.id,
                            node.generation,
                            "failed",
                            Some("not_found"),
                            None,
                            false,
                        )
                        .await?;
                        skipped.push(candidate);
                        continue 'nodes;
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
                    let decision_id = begin_decision(
                        execution,
                        &task_id,
                        &node.id,
                        node.generation,
                        crate::coordinator::Stage::ResolveInputs,
                        "embedded_selection",
                        &candidate.path.display().to_string(),
                    )
                    .await?;
                    let selection = tokio::select! {
                        _ = cancellation.cancelled() => None,
                        value = prompter.prompt(&candidate.path, &decision) => Some(value),
                    };
                    if !finish_decision(
                        execution,
                        &task_id,
                        &node.id,
                        node.generation,
                        crate::coordinator::Stage::ResolveInputs,
                        &decision_id,
                        &cancellation,
                    )
                    .await?
                        || cancellation.is_cancelled()
                    {
                        was_cancelled |= cancellation.is_user_cancelled();
                        finish_interrupted_node(
                            execution,
                            &task_id,
                            &node.id,
                            node.generation,
                            &cancellation,
                        )
                        .await?;
                        skipped.push(candidate);
                        break 'nodes;
                    }
                    selection
                } else {
                    None
                };
                match selection {
                    Some(EmbeddedSelectionChoice::Extract) => {}
                    Some(EmbeddedSelectionChoice::ExtractAll) => embedded_extract_all = true,
                    Some(EmbeddedSelectionChoice::Skip) | None => {
                        record_skip(legacy_history, &task_id, &candidate, "not_found");
                        finish_execution(
                            execution,
                            &task_id,
                            &node.id,
                            node.generation,
                            "skipped",
                            Some("not_found"),
                            None,
                            false,
                        )
                        .await?;
                        skipped.push(candidate);
                        continue 'nodes;
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
                root_scanner.as_ref().unwrap_or(nested_scanner)
            } else {
                nested_scanner
            };
            let header_archive = header_result
                .as_ref()
                .is_some_and(|(_, offset)| *offset == 0);
            let explicit_scan = matches!(request.scanner.mode, smartzip_scanner::ScanMode::Deep)
                || matches!(
                    embedded_policy.mode,
                    smartzip_core::EmbeddedScanMode::All
                        | smartzip_core::EmbeddedScanMode::Aggressive
                );
            if !enter_stage(
                execution,
                &task_id,
                &node.id,
                node.generation,
                "running",
                crate::coordinator::Stage::ScanEmbedded,
                Some(&attempt_id),
                &cancellation,
            )
            .await?
            {
                was_cancelled |= cancellation.is_user_cancelled();
                finish_interrupted_node(
                    execution,
                    &task_id,
                    &node.id,
                    node.generation,
                    &cancellation,
                )
                .await?;
                skipped.push(candidate);
                break 'nodes;
            }
            let findings: Vec<_> = if volume_set_for_candidate.is_none()
                && (candidate.source == CandidateSource::RootInput
                    || !header_archive
                    || explicit_scan)
                && should_scan_candidate_for_embedded(
                    &candidate,
                    &embedded_policy,
                    nested_embedded_enabled,
                ) {
                if let Some(limit) = scan_with.scan_limit().filter(|limit| {
                    std::fs::metadata(&candidate.path).is_ok_and(|m| m.len() > *limit)
                }) {
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
                let scan = crate::access::scan_file(
                    &candidate.path,
                    scan_with.config().clone(),
                    cancellation.token().clone(),
                )
                .await;
                let findings = match scan {
                    Ok(findings) => findings,
                    Err(smartzip_core::SmartZipError::Cancelled) if cancellation.is_cancelled() => {
                        was_cancelled |= cancellation.is_user_cancelled();
                        finish_interrupted_node(
                            execution,
                            &task_id,
                            &node.id,
                            node.generation,
                            &cancellation,
                        )
                        .await?;
                        skipped.push(candidate);
                        break 'nodes;
                    }
                    Err(error) => return Err(error),
                };
                findings
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
                let mut ready_archives = Vec::new();
                for embedded_candidate in root_findings {
                    if !task_budget.reserve_nested(request.limits.max_nested_candidates) {
                        record_failure(&mut failed_count, config, &cancellation);
                        events.push(TaskEvent::failed(
                            task_id.clone(),
                            &crate::budget::exceeded("nested candidate limit exceeded"),
                        ));
                        finish_execution(
                            execution,
                            &task_id,
                            &node.id,
                            node.generation,
                            "failed",
                            Some("nested_candidate_limit"),
                            None,
                            false,
                        )
                        .await?;
                        skipped.push(candidate);
                        break 'nodes;
                    }
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
                    let child_id = NodeId::new();
                    let inserted = if let Some(execution) = execution {
                        execution
                            .enqueue_child(
                                &task_id,
                                &child_id,
                                &node.id,
                                &node.root_id,
                                &embedded_candidate,
                                0,
                            )
                            .await?
                            || cancellation.is_cancelled()
                    } else {
                        true
                    };
                    if inserted {
                        enqueued.push(embedded_candidate.clone());
                        ready_archives.push(ExecutionNode {
                            id: child_id,
                            root_id: node.root_id.clone(),
                            generation: 0,
                            candidate: embedded_candidate,
                        });
                    } else {
                        task_budget.release_nested();
                    }
                }
                // These are ready archives from the current input, not nested
                // discovery work. Extract them before scanning later inputs.
                for archive in ready_archives.into_iter().rev() {
                    queue.push_front(archive);
                }
                finish_execution_outcome(
                    execution,
                    &task_id,
                    &node.id,
                    node.generation,
                    crate::NodeOutcome::terminal(
                        "skipped",
                        Some("expanded_to_embedded"),
                        None,
                        false,
                    ),
                )
                .await?;
                continue 'nodes;
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
                            let decision_id = begin_decision(
                                execution,
                                &task_id,
                                &node.id,
                                node.generation,
                                crate::coordinator::Stage::ScanEmbedded,
                                "embedded_selection",
                                &candidate.path.display().to_string(),
                            )
                            .await?;
                            let selection = tokio::select! {
                                _ = cancellation.cancelled() => None,
                                value = prompter.prompt(&candidate.path, &decision) => Some(value),
                            };
                            if !finish_decision(
                                execution,
                                &task_id,
                                &node.id,
                                node.generation,
                                crate::coordinator::Stage::ScanEmbedded,
                                &decision_id,
                                &cancellation,
                            )
                            .await?
                                || cancellation.is_cancelled()
                            {
                                was_cancelled |= cancellation.is_user_cancelled();
                                finish_interrupted_node(
                                    execution,
                                    &task_id,
                                    &node.id,
                                    node.generation,
                                    &cancellation,
                                )
                                .await?;
                                skipped.push(candidate);
                                break 'nodes;
                            }
                            selection
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
                                    record_skip(legacy_history, &task_id, &candidate, "not_found");
                                    finish_execution(
                                        execution,
                                        &task_id,
                                        &node.id,
                                        node.generation,
                                        "skipped",
                                        Some("not_found"),
                                        None,
                                        false,
                                    )
                                    .await?;
                                    skipped.push(candidate);
                                    continue 'nodes;
                                }
                            },
                            None => {
                                record_skip(legacy_history, &task_id, &candidate, "not_found");
                                finish_execution(
                                    execution,
                                    &task_id,
                                    &node.id,
                                    node.generation,
                                    "skipped",
                                    Some("not_found"),
                                    None,
                                    false,
                                )
                                .await?;
                                skipped.push(candidate);
                                continue 'nodes;
                            }
                        }
                    }
                    _ => {
                        record_skip(legacy_history, &task_id, &candidate, "not_found");
                        finish_execution(
                            execution,
                            &task_id,
                            &node.id,
                            node.generation,
                            "skipped",
                            Some("not_found"),
                            None,
                            false,
                        )
                        .await?;
                        skipped.push(candidate);
                        continue 'nodes;
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
                record_skip(legacy_history, &task_id, &candidate, "not_found");
                finish_execution_outcome(
                    execution,
                    &task_id,
                    &node.id,
                    node.generation,
                    crate::NodeOutcome::terminal("skipped", Some("not_found"), None, false),
                )
                .await?;
                skipped.push(candidate);
                continue 'nodes;
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
                    record_skip(legacy_history, &task_id, &candidate, "business_container");
                    finish_execution_outcome(
                        execution,
                        &task_id,
                        &node.id,
                        node.generation,
                        crate::NodeOutcome::terminal(
                            "skipped",
                            Some("business_container"),
                            None,
                            false,
                        ),
                    )
                    .await?;
                    skipped.push(candidate);
                    continue 'nodes;
                }
            }

            // Preparation owns carved/canonical input guards and returns facts only.
            if !enter_stage(
                execution,
                &task_id,
                &node.id,
                node.generation,
                "running",
                crate::coordinator::Stage::PrepareAccess,
                Some(&attempt_id),
                &cancellation,
            )
            .await?
            {
                was_cancelled |= cancellation.is_user_cancelled();
                finish_interrupted_node(
                    execution,
                    &task_id,
                    &node.id,
                    node.generation,
                    &cancellation,
                )
                .await?;
                skipped.push(candidate);
                break 'nodes;
            }
            let prepared = match prepare_resolved_archive(
                &candidate,
                volume_archive_path.zip(volume_materialized),
                Some(&request.output_dir),
                request.encoding_mode.clone(),
                history,
                run_policy,
            )
            .await
            {
                Ok(prepared) => prepared,
                Err(error) => {
                    record_failure(&mut failed_count, config, &cancellation);
                    events.push(TaskEvent::failed(task_id.clone(), &error));
                    record_skip(legacy_history, &task_id, &candidate, &error.to_string());
                    finish_execution_outcome(
                        execution,
                        &task_id,
                        &node.id,
                        node.generation,
                        crate::NodeOutcome::terminal(
                            "failed",
                            Some("prepare_access_failed"),
                            None,
                            false,
                        ),
                    )
                    .await?;
                    skipped.push(candidate);
                    continue 'nodes;
                }
            };
            let archive_path = &prepared.archive_path;
            let sample_hash = &prepared.sample_hash;
            let sample_size = prepared.sample_size;
            let known_hit = &prepared.known_hit;
            let candidate_encoding_mode = &prepared.encoding_mode;
            let reused_confirmed_encoding = prepared.reused_confirmed_encoding;
            let zip_encoding_assessment = &prepared.zip_encoding_assessment;

            if !request.force
                && config.is_some_and(|c| c.extraction.reuse.skip_completed)
                && history
                    .zip(sample_hash.as_deref())
                    .zip(sample_size)
                    .is_some_and(|((h, hash), size)| h.was_extracted(hash, size))
            {
                record_skip(legacy_history, &task_id, &candidate, "already_extracted");
                events.push(TaskEvent {
                    task_id: task_id.clone(),
                    kind: TaskEventKind::Decision {
                        stage: "reuse".into(),
                        action: "skip".into(),
                        reason: "already_extracted".into(),
                        policy_key: "extraction.reuse.skip_completed".into(),
                        source: "history".into(),
                        detail: Some(candidate.path.display().to_string()),
                    },
                });
                finish_execution_outcome(
                    execution,
                    &task_id,
                    &node.id,
                    node.generation,
                    crate::NodeOutcome::terminal("skipped", Some("already_extracted"), None, false),
                )
                .await?;
                skipped.push(candidate);
                continue 'nodes;
            }

            // Password try order: command-line/manual > exact known-file hit >
            // passwords accepted earlier in this batch > empty/database
            // fallback. Values are deduplicated while preserving that order.
            let known_password = known_hit
                .as_ref()
                .and_then(|h| h.password_id)
                .and_then(|id| passwords.candidate_by_id(id).ok().flatten());
            let candidate_passwords = {
                let batch_passwords = batch_passwords.borrow();
                let base: Vec<_> = group_passwords
                    .iter()
                    .cloned()
                    .chain(password_candidates.iter().cloned())
                    .collect();
                passwords.order_candidates(&base, known_password.as_ref(), &batch_passwords)
            };

            if candidate.source == CandidateSource::RootInput {
                if group_attempt == 1 {
                    root_input_started += 1;
                }
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

            if let Some(assessment) = zip_encoding_assessment {
                events.push(TaskEvent {
                    task_id: task_id.clone(),
                    kind: TaskEventKind::EncodingDetected(assessment.context.detected.clone()),
                });
            }

            let archive_facts = ArchiveFacts {
                container: candidate.detected_format.clone(),
                ..ArchiveFacts::default()
            };
            let output_dir = output_dir_for_candidate(&request.output_dir, &candidate);

            let mut extracted = false;
            let mut terminal_skip = false;
            let mut last_error = None;
            let mut group_retry_allowed = false;
            let mut restart_groups = false;
            let mut saw_wrong_password = false;
            let mut saw_password_indeterminate = false;
            let mut password_prompt_cancelled = false;
            let mut actual_output_dir = output_dir.clone();
            // File-grain success state, recorded once after the try loop.
            let mut candidate_password_id: Option<i64> = None;
            let mut candidate_has_password = false;
            let mut candidate_encoding_used: Option<String> = None;
            // Resolve encoding once per node. A deliberate skip is a node outcome.
            if !enter_stage(
                execution,
                &task_id,
                &node.id,
                node.generation,
                "running",
                crate::coordinator::Stage::AnalyzeEncoding,
                Some(&attempt_id),
                &cancellation,
            )
            .await?
            {
                was_cancelled |= cancellation.is_user_cancelled();
                finish_interrupted_node(
                    execution,
                    &task_id,
                    &node.id,
                    node.generation,
                    &cancellation,
                )
                .await?;
                skipped.push(candidate);
                break 'nodes;
            }
            let encoding_decision = if zip_encoding_assessment
                .as_ref()
                .is_some_and(|assessment| assessment.should_confirm)
                && encoding_prompter.is_some()
            {
                Some(
                    begin_decision(
                        execution,
                        &task_id,
                        &node.id,
                        node.generation,
                        crate::coordinator::Stage::AnalyzeEncoding,
                        "encoding",
                        &candidate.path.display().to_string(),
                    )
                    .await?,
                )
            } else {
                None
            };
            let encoding_choice = tokio::select! {
                _ = cancellation.cancelled() => { None }
                result = resolve_encoding_mode(&archive_path, candidate_encoding_mode.clone(), zip_encoding_assessment.as_ref(), encoding_prompter) => result?,
            };
            if let Some(decision_id) = encoding_decision {
                if !finish_decision(
                    execution,
                    &task_id,
                    &node.id,
                    node.generation,
                    crate::coordinator::Stage::AnalyzeEncoding,
                    &decision_id,
                    &cancellation,
                )
                .await?
                    || cancellation.is_cancelled()
                {
                    was_cancelled |= cancellation.is_user_cancelled();
                    finish_interrupted_node(
                        execution,
                        &task_id,
                        &node.id,
                        node.generation,
                        &cancellation,
                    )
                    .await?;
                    skipped.push(candidate);
                    break 'nodes;
                }
            }
            let mut skip_reason = "target_exists";
            if encoding_choice.is_none() {
                terminal_skip = true;
                skip_reason = "encoding_skipped";
            }
            let total_attempts = candidate_passwords.len();
            let mut attempt_index = 0;
            let mut attempts: VecDeque<_> = candidate_passwords.into_iter().collect();
            let mut saw_password_required = false;
            let committed_output_usage = std::cell::Cell::new(crate::budget::Usage::default());
            let committed_output_files = std::cell::RefCell::new(None);
            while !terminal_skip && !cancellation.is_cancelled() {
                let extraction_attempt_id = AttemptId::new();
                if !enter_stage(
                    execution,
                    &task_id,
                    &node.id,
                    node.generation,
                    "running",
                    crate::coordinator::Stage::ExtractAttempt,
                    Some(&extraction_attempt_id),
                    &cancellation,
                )
                .await?
                {
                    was_cancelled |= cancellation.is_user_cancelled();
                    finish_interrupted_node(
                        execution,
                        &task_id,
                        &node.id,
                        node.generation,
                        &cancellation,
                    )
                    .await?;
                    skipped.push(candidate);
                    break 'nodes;
                }
                let password = if let Some(password) = attempts.pop_front() {
                    password
                } else if saw_wrong_password
                    || saw_password_required
                    || saw_password_indeterminate
                    || (group_trials && group_password_needed)
                    || last_error.is_none()
                {
                    // Try the existing credentials against every grouping
                    // before asking the user for another password.
                    if group_trials && !remaining_groups.is_empty() {
                        password_prompt_cancelled = true;
                        break;
                    }
                    let input = if let Some(prompter) = password_prompter {
                        let decision_id = begin_decision(
                            execution,
                            &task_id,
                            &node.id,
                            node.generation,
                            crate::coordinator::Stage::ExtractAttempt,
                            "password",
                            &candidate.path.display().to_string(),
                        )
                        .await?;
                        let input = tokio::select! { _ = cancellation.cancelled() => None, value = prompter.prompt(&candidate.path) => value };
                        if !finish_decision(
                            execution,
                            &task_id,
                            &node.id,
                            node.generation,
                            crate::coordinator::Stage::ExtractAttempt,
                            &decision_id,
                            &cancellation,
                        )
                        .await?
                            || cancellation.is_cancelled()
                        {
                            was_cancelled |= cancellation.is_user_cancelled();
                            finish_interrupted_node(
                                execution,
                                &task_id,
                                &node.id,
                                node.generation,
                                &cancellation,
                            )
                            .await?;
                            skipped.push(candidate);
                            break 'nodes;
                        }
                        input
                    } else {
                        None
                    };
                    match input.filter(|value| !value.is_empty()) {
                        Some(value) => {
                            let password = PasswordCandidate {
                                id: None,
                                value,
                                source: smartzip_passwords::PasswordSource::Manual,
                            };
                            if all_groups.len() > 1 {
                                group_passwords.push(password);
                                restart_groups = true;
                                break;
                            }
                            password
                        }
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
                let pw_value = Some(password.value.clone());
                attempt_index += 1;
                events.push(TaskEvent {
                    task_id: task_id.clone(),
                    kind: TaskEventKind::PasswordTried {
                        candidate_id: password.id,
                    },
                });
                events.push(TaskEvent {
                    task_id: task_id.clone(),
                    kind: TaskEventKind::Progress(smartzip_core::TaskProgress::indeterminate(
                        format!(
                            "Trying password [{}/{}] ({}) by extraction for {}",
                            attempt_index,
                            total_attempts.max(attempt_index),
                            password_source_label(&password),
                            candidate.path.display()
                        ),
                    )),
                });
                let encoding = encoding_choice
                    .clone()
                    .expect("encoding skip exits before attempts");
                candidate_encoding_used = Some(encoding_mode_label(&encoding));
                let mut budget_reservation = task_budget.reserve_attempt();
                let attempt_output_usage = std::cell::Cell::new(crate::budget::Usage::default());
                let attempt_output_inventory = std::cell::RefCell::new(None);
                let extracted_encrypted = std::cell::Cell::new(None);
                let committed_has_password = std::cell::Cell::new(false);
                let committed_password_id = std::cell::Cell::new(None);
                let result = output_materializer
                    .prepare(
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
                            let budget_reservation = &budget_reservation;
                            let extracted_encrypted = &extracted_encrypted;
                            let attempt_output_usage = &attempt_output_usage;
                            let attempt_output_inventory = &attempt_output_inventory;
                            let task_id = task_id.clone();
                            let node_id = node.id.clone();
                            let generation = node.generation;
                            async move {
                                if let Some(execution) = execution {
                                    if !execution
                                        .record_staging(
                                            &task_id,
                                            &node_id,
                                            generation,
                                            &temp_output_dir,
                                        )
                                        .await?
                                    {
                                        return Err(smartzip_core::SmartZipError::BackendFailed {
                                            backend: "state-store".into(),
                                            exit_code: None,
                                            stderr: format!(
                                                "could not persist staging ownership for node {node_id}"
                                            ),
                                        });
                                    }
                                }
                                let (extracted, inventory) = crate::budget::monitor_task(
                                    &temp_output_dir,
                                    limits,
                                    budget_reservation,
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
                                attempt_output_usage.set(inventory.usage);
                                *attempt_output_inventory.borrow_mut() = Some(inventory);
                                extracted_encrypted.set(extracted.encrypted);
                                Ok(())
                            }
                        },
                    )
                    .await;
                let result = match result {
                    Ok(staged) => {
                        if !enter_stage(
                            execution,
                            &task_id,
                            &node.id,
                            node.generation,
                            "running",
                            crate::coordinator::Stage::InspectAndPlan,
                            Some(&extraction_attempt_id),
                            &cancellation,
                        )
                        .await?
                        {
                            was_cancelled |= cancellation.is_user_cancelled();
                            finish_interrupted_node(
                                execution,
                                &task_id,
                                &node.id,
                                node.generation,
                                &cancellation,
                            )
                            .await?;
                            skipped.push(candidate);
                            break 'nodes;
                        }
                        match staged.collision_request() {
                            Ok(collision) => {
                                let decision = if let (Some(prompter), Some(collision)) =
                                    (output_prompter, collision)
                                {
                                    let decision_id = begin_decision(
                                        execution,
                                        &task_id,
                                        &node.id,
                                        node.generation,
                                        crate::coordinator::Stage::InspectAndPlan,
                                        "output_collision",
                                        &collision.target_path.display().to_string(),
                                    )
                                    .await?;
                                    let action = tokio::select! {
                                        _ = cancellation.cancelled() => None,
                                        value = prompter.prompt(
                                            collision.archive_path.clone(),
                                            collision.target_path.clone(),
                                        ) => Some(match value {
                                            crate::interactive::OutputCollisionStrategy::Skip => CollisionAction::Skip,
                                            crate::interactive::OutputCollisionStrategy::Overwrite => CollisionAction::Overwrite,
                                            crate::interactive::OutputCollisionStrategy::Rename => CollisionAction::Rename,
                                        }),
                                    };
                                    if !finish_decision(
                                        execution,
                                        &task_id,
                                        &node.id,
                                        node.generation,
                                        crate::coordinator::Stage::InspectAndPlan,
                                        &decision_id,
                                        &cancellation,
                                    )
                                    .await?
                                        || cancellation.is_cancelled()
                                    {
                                        was_cancelled |= cancellation.is_user_cancelled();
                                        finish_interrupted_node(
                                            execution,
                                            &task_id,
                                            &node.id,
                                            node.generation,
                                            &cancellation,
                                        )
                                        .await?;
                                        skipped.push(candidate);
                                        break 'nodes;
                                    }
                                    action.map(|action| (collision, action))
                                } else {
                                    None
                                };
                                if !enter_stage(
                                    execution,
                                    &task_id,
                                    &node.id,
                                    node.generation,
                                    "running",
                                    crate::coordinator::Stage::Commit,
                                    Some(&extraction_attempt_id),
                                    &cancellation,
                                )
                                .await?
                                {
                                    was_cancelled |= cancellation.is_user_cancelled();
                                    finish_interrupted_node(
                                        execution,
                                        &task_id,
                                        &node.id,
                                        node.generation,
                                        &cancellation,
                                    )
                                    .await?;
                                    skipped.push(candidate);
                                    break 'nodes;
                                }
                                let has_password = pw_value
                                    .as_deref()
                                    .is_some_and(|p| !p.is_empty())
                                    && (extracted_encrypted.get() == Some(true)
                                        || saw_password_required
                                        || saw_wrong_password
                                        || saw_password_indeterminate
                                        || (candidate.detected_format == Some(ArchiveFormat::Zip)
                                            && NativeZipBackend::new()
                                                .has_encrypted_entries(archive_path)
                                                .unwrap_or(false)));
                                let password_id = if has_password { password.id } else { None };
                                committed_has_password.set(has_password);
                                committed_password_id.set(password_id);
                                let success = crate::CommitSuccessFacts {
                                    sample_hash: sample_hash.clone(),
                                    file_size: sample_size,
                                    embedded_offset: candidate.embedded_offset,
                                    has_password,
                                    password_id,
                                    encoding: candidate_encoding_used.clone(),
                                    encoding_corrected: reused_confirmed_encoding
                                        || matches!(
                                            request.encoding_mode,
                                            EncodingMode::Override(_)
                                        ),
                                };
                                match staged.prepare_commit(decision, success) {
                                    Ok(mut prepared) => {
                                        prepared.set_output_usage(attempt_output_usage.get());
                                        let intent = prepared.intent().cloned();
                                        if let (Some(execution), Some(intent)) =
                                            (execution, intent.as_ref())
                                        {
                                            if !execution
                                                .begin_commit(
                                                    &task_id,
                                                    &node.id,
                                                    node.generation,
                                                    intent,
                                                )
                                                .await?
                                            {
                                                return Err(
                                                    smartzip_core::SmartZipError::BackendFailed {
                                                        backend: "task-coordinator".into(),
                                                        exit_code: None,
                                                        stderr: format!(
                                                "could not persist commit intent for node {}",
                                                node.id
                                            ),
                                                    },
                                                );
                                            }
                                        }
                                        match prepared.commit_with_recovery(
                                            execution.is_some_and(|e| e.durable()),
                                        ) {
                                            Ok(published) => {
                                                committed_output_usage
                                                    .set(budget_reservation.commit());
                                                if let (Some(execution), Some(intent)) =
                                                    (execution, published.intent())
                                                {
                                                    if !execution
                                                        .commit_published(
                                                            &task_id,
                                                            &node.id,
                                                            node.generation,
                                                            intent,
                                                        )
                                                        .await?
                                                    {
                                                        return Err(
                                                    smartzip_core::SmartZipError::BackendFailed {
                                                        backend: "task-coordinator".into(),
                                                        exit_code: None,
                                                        stderr: format!(
                                                            "could not persist published commit for node {}",
                                                            node.id
                                                        ),
                                                    },
                                                );
                                                    }
                                                }
                                                Ok(published.finalize())
                                            }
                                            Err(failure) => {
                                                if intent.as_ref().is_some_and(|intent| {
                                                    std::fs::symlink_metadata(&intent.marker_path)
                                                        .is_ok()
                                                }) {
                                                    return Err(failure.error);
                                                }
                                                if let (Some(execution), Some(_)) =
                                                    (execution, intent)
                                                {
                                                    let _ = execution
                                                        .abort_commit(
                                                            &task_id,
                                                            &node.id,
                                                            node.generation,
                                                        )
                                                        .await?;
                                                }
                                                Err(failure)
                                            }
                                        }
                                    }
                                    Err(failure) => Err(failure),
                                }
                            }
                            Err(failure) => Err(failure),
                        }
                    }
                    Err(failure) => Err(failure),
                };
                match result {
                    Ok(result) => {
                        *committed_output_files.borrow_mut() = attempt_output_inventory
                            .borrow()
                            .as_ref()
                            .and_then(|inventory| inventory.published_files(&result.layout_plan));
                        for message in &result.layout_plan.warnings {
                            events.push(TaskEvent {
                                task_id: task_id.clone(),
                                kind: TaskEventKind::Warning {
                                    message: message.clone(),
                                },
                            });
                        }
                        if result.output_dir != output_dir {
                            candidate.relative_path =
                                output_relative_path_for(&request.output_dir, &result.output_dir);
                        }
                        actual_output_dir = result.output_dir;
                        candidate_has_password = committed_has_password.get();
                        candidate_password_id = committed_password_id.get();
                        if candidate_has_password {
                            if let Ok(Some(password_id)) = passwords.record_success(&password) {
                                candidate_password_id = Some(password_id);
                            }
                            passwords.remember_batch(
                                &mut batch_passwords.borrow_mut(),
                                &password.value,
                                candidate_password_id,
                            );
                        }
                        extracted = true;
                        break;
                    }
                    Err(failure) => {
                        group_retry_allowed = false;
                        if failure.kind == materialize::MaterializeFailureKind::CollisionSkipped {
                            terminal_skip = true;
                            break;
                        }
                        if let Some(path) = &failure.preserved_temp_dir {
                            events.push(TaskEvent {
                                task_id: task_id.clone(),
                                kind: TaskEventKind::Warning {
                                    message: format!(
                                        "recovery output retained at {}",
                                        path.display()
                                    ),
                                },
                            });
                        }
                        if failure.preserved_temp_dir.is_some() {
                            last_error = Some(failure.error);
                            break;
                        }
                        if failure.kind == materialize::MaterializeFailureKind::ExtractFailed {
                            group_retry_allowed = retryable_group_error(&failure.error);
                            match &failure.error {
                                smartzip_core::SmartZipError::WrongPassword { .. } => {
                                    saw_wrong_password = true;
                                    if !group_trials {
                                        let _ = passwords.record_failure(&password);
                                    }
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
                        if group_trials
                            && group_retry_allowed
                            && remaining_groups.is_empty()
                            && group_password_needed
                            && password_prompter.is_some()
                        {
                            attempts.clear();
                            continue;
                        }
                        break;
                    }
                }
            }
            if restart_groups && !cancellation.is_cancelled() {
                remaining_groups = all_groups.clone();
                next_resolution = remaining_groups.pop_front();
                candidate = original_candidate.clone();
                group_attempt += 1;
                continue 'groupings;
            }
            if !extracted
                && cancellation.is_cancelled()
                && (last_error.is_none()
                    || matches!(
                        last_error.as_ref(),
                        Some(smartzip_core::SmartZipError::Cancelled)
                    ))
            {
                was_cancelled |= cancellation.is_user_cancelled();
                finish_interrupted_node(
                    execution,
                    &task_id,
                    &node.id,
                    node.generation,
                    &cancellation,
                )
                .await?;
                skipped.push(candidate);
                break 'nodes;
            }
            if !extracted && !terminal_skip && group_retry_allowed && !cancellation.is_cancelled() {
                if let Some(resolution) = remaining_groups.pop_front() {
                    group_password_needed |=
                        saw_wrong_password || saw_password_required || saw_password_indeterminate;
                    events.push(TaskEvent {
                        task_id: task_id.clone(),
                        kind: TaskEventKind::Warning {
                            message: format!("volume grouping attempt {group_attempt} failed for {}; trying next grouping", original_input_path.display()),
                        },
                    });
                    next_resolution = Some(resolution);
                    candidate = original_candidate.clone();
                    group_attempt += 1;
                    continue 'groupings;
                }
            }
            if extracted {
                if let (Some(execution), Some(set)) = (execution, &volume_set_for_candidate) {
                    execution.volume_selected(
                        &node.id,
                        set.members.iter().map(|m| m.path.clone()).collect(),
                    );
                }
            }
            if extracted && group_trials {
                if let Some(set) = &volume_set_for_candidate {
                    let mut dedup = dedup.borrow_mut();
                    dedup.volumes.insert(volume_set_key(set));
                    for member in &set.members {
                        dedup.members.insert(
                            std::path::absolute(&member.path)
                                .unwrap_or_else(|_| member.path.clone()),
                        );
                    }
                }
                events.push(TaskEvent {
                    task_id: task_id.clone(),
                    kind: TaskEventKind::Warning {
                        message: format!("volume grouping attempt {group_attempt} succeeded for {}; discarded {} remaining groupings", original_input_path.display(), remaining_groups.len()),
                    },
                });
            }
            let cancelled_now = cancellation.is_user_cancelled()
                || (matches!(last_error, Some(smartzip_core::SmartZipError::Cancelled))
                    && !cancellation.stopped_on_error());
            if cancelled_now && !extracted {
                was_cancelled = true;
                record_skip(legacy_history, &task_id, &candidate, "cancelled");
                finish_execution_outcome(
                    execution,
                    &task_id,
                    &node.id,
                    node.generation,
                    crate::NodeOutcome {
                        sample_hash: sample_hash.clone(),
                        file_size: sample_size,
                        embedded_offset: candidate.embedded_offset,
                        encoding: candidate_encoding_used.clone(),
                        encoding_corrected: reused_confirmed_encoding
                            || matches!(request.encoding_mode, EncodingMode::Override(_)),
                        ..crate::NodeOutcome::terminal("cancelled", Some("cancelled"), None, false)
                    },
                )
                .await?;
                skipped.push(candidate);
                break 'nodes;
            }
            was_cancelled |= cancelled_now;

            let mut node_terminal_status = "skipped";
            let mut node_terminal_reason = "password_required";
            if !extracted && !terminal_skip {
                if password_prompt_cancelled {
                    node_terminal_reason = if saw_password_indeterminate {
                        "password_indeterminate"
                    } else if saw_wrong_password {
                        "wrong_password"
                    } else {
                        "password_required"
                    };
                    if password_prompter.is_none() {
                        node_terminal_status = "failed";
                        record_failure(&mut failed_count, config, &cancellation);
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
                    if let Some(recorder) = legacy_history {
                        recorder.record_file_extraction(
                            &task_id,
                            crate::history::FileExtractionRow {
                                sample_hash: sample_hash.as_deref(),
                                file_size: sample_size,
                                encoding: candidate_encoding_used.as_deref(),
                                encoding_corrected: reused_confirmed_encoding
                                    || matches!(request.encoding_mode, EncodingMode::Override(_)),
                                ..{
                                    let reason = if saw_password_indeterminate {
                                        "password_indeterminate"
                                    } else if saw_wrong_password {
                                        "wrong_password"
                                    } else {
                                        "password_required"
                                    };
                                    if password_prompter.is_none() {
                                        crate::history::FileExtractionRow::failed(
                                            &original_input_path,
                                            candidate.embedded_offset,
                                            reason,
                                        )
                                    } else {
                                        crate::history::FileExtractionRow::skipped(
                                            &original_input_path,
                                            candidate.embedded_offset,
                                            reason,
                                        )
                                    }
                                }
                            },
                        );
                    }
                } else if let Some(error) = last_error.or_else(|| {
                    saw_wrong_password.then(|| smartzip_core::SmartZipError::WrongPassword {
                        path: candidate.path.clone(),
                    })
                }) {
                    node_terminal_status = "failed";
                    record_failure(&mut failed_count, config, &cancellation);
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
                    node_terminal_reason = reason;
                    if let Some(recorder) = legacy_history {
                        recorder.record_file_extraction(
                            &task_id,
                            crate::history::FileExtractionRow {
                                sample_hash: sample_hash.as_deref(),
                                file_size: sample_size,
                                has_password: candidate_has_password,
                                password_id: candidate_password_id,
                                encoding: candidate_encoding_used.as_deref(),
                                encoding_corrected: reused_confirmed_encoding
                                    || matches!(request.encoding_mode, EncodingMode::Override(_)),
                                ..crate::history::FileExtractionRow::failed(
                                    &original_input_path,
                                    candidate.embedded_offset,
                                    reason,
                                )
                            },
                        );
                    }
                    let event = TaskEvent::failed(task_id.clone(), &error);
                    events.push(event);
                } else if let Some(recorder) = legacy_history {
                    // No error and not extracted: candidates were tried but none
                    // opened it (e.g. needed a password we never got). Record a
                    // skip with `password_required` rather than a failure.
                    recorder.record_file_extraction(
                        &task_id,
                        crate::history::FileExtractionRow {
                            sample_hash: sample_hash.as_deref(),
                            file_size: sample_size,
                            has_password: candidate_has_password,
                            password_id: candidate_password_id,
                            encoding: candidate_encoding_used.as_deref(),
                            encoding_corrected: reused_confirmed_encoding
                                || matches!(request.encoding_mode, EncodingMode::Override(_)),
                            ..crate::history::FileExtractionRow::skipped(
                                &original_input_path,
                                candidate.embedded_offset,
                                "password_required",
                            )
                        },
                    );
                }
            }
            if terminal_skip {
                record_skip(legacy_history, &task_id, &candidate, skip_reason);
                finish_execution_outcome(
                    execution,
                    &task_id,
                    &node.id,
                    node.generation,
                    crate::NodeOutcome {
                        sample_hash: sample_hash.clone(),
                        file_size: sample_size,
                        embedded_offset: candidate.embedded_offset,
                        encoding: candidate_encoding_used.clone(),
                        encoding_corrected: reused_confirmed_encoding
                            || matches!(request.encoding_mode, EncodingMode::Override(_)),
                        ..crate::NodeOutcome::terminal("skipped", Some(skip_reason), None, false)
                    },
                )
                .await?;
                skipped.push(candidate);
                continue 'nodes;
            }
            if !extracted {
                if let Some(policy) = run_policy {
                    let c = policy.values();
                    let key = if skip_reason == "encoding_skipped"
                        && c.extraction.encoding.on_suspicious
                            == smartzip_config::SuspiciousEncoding::Ask
                    {
                        Some("extraction.encoding.on_suspicious")
                    } else if terminal_skip
                        && skip_reason == "target_exists"
                        && c.extraction.output.on_conflict == smartzip_config::Conflict::Ask
                    {
                        Some("extraction.output.on_conflict")
                    } else {
                        None
                    };
                    if let Some(key) = key {
                        policy.decision(
                            &events,
                            &task_id,
                            "confirmation",
                            "skip",
                            "needs_decision",
                            key,
                        );
                    }
                }
                finish_execution_outcome(
                    execution,
                    &task_id,
                    &node.id,
                    node.generation,
                    crate::NodeOutcome {
                        sample_hash: sample_hash.clone(),
                        file_size: sample_size,
                        embedded_offset: candidate.embedded_offset,
                        has_password: candidate_has_password,
                        password_id: candidate_password_id,
                        encoding: candidate_encoding_used.clone(),
                        encoding_corrected: reused_confirmed_encoding
                            || matches!(request.encoding_mode, EncodingMode::Override(_)),
                        ..crate::NodeOutcome::terminal(
                            node_terminal_status,
                            Some(node_terminal_reason),
                            None,
                            false,
                        )
                    },
                )
                .await?;
                skipped.push(candidate);
                continue 'nodes;
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
            // reuse its password; dedup queries the history row above.
            if let Some(recorder) = legacy_history {
                recorder.record_file_extraction(
                    &task_id,
                    crate::history::FileExtractionRow {
                        sample_hash: sample_hash.as_deref(),
                        file_size: sample_size,
                        has_password: candidate_has_password,
                        password_id: candidate_password_id,
                        encoding: candidate_encoding_used.as_deref(),
                        encoding_corrected: reused_confirmed_encoding
                            || matches!(request.encoding_mode, EncodingMode::Override(_)),
                        ..crate::history::FileExtractionRow::extracted(
                            &original_input_path,
                            candidate.embedded_offset,
                            &actual_output_dir,
                        )
                    },
                );
            }
            if let Some(recorder) = history {
                if let (Some(hash), Some(size)) = (sample_hash.as_deref(), sample_size) {
                    let name = &prepared.recorder_name;
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
            let successful_outcome = || crate::NodeOutcome {
                sample_hash: sample_hash.clone(),
                file_size: sample_size,
                embedded_offset: candidate.embedded_offset,
                has_password: candidate_has_password,
                password_id: candidate_password_id,
                encoding: candidate_encoding_used.clone(),
                encoding_corrected: reused_confirmed_encoding
                    || matches!(request.encoding_mode, EncodingMode::Override(_)),
                output_files: committed_output_usage.get().files,
                output_bytes: committed_output_usage.get().bytes,
                ..crate::NodeOutcome::terminal("extracted", None, Some(&actual_output_dir), true)
            };
            if let Some(cleanup) = &source_cleanup {
                cleanup.borrow_mut().committed(
                    &source_paths,
                    &actual_output_dir,
                    committed_output_usage.get().files,
                );
            }
            processed.push(candidate.clone());
            if !enter_stage(
                execution,
                &task_id,
                &node.id,
                node.generation,
                "running",
                crate::coordinator::Stage::DiscoverChildren,
                Some(&attempt_id),
                &cancellation,
            )
            .await?
            {
                was_cancelled |= cancellation.is_user_cancelled();
                finish_execution_outcome(
                    execution,
                    &task_id,
                    &node.id,
                    node.generation,
                    successful_outcome(),
                )
                .await?;
                break 'nodes;
            }
            let output_relative_path = candidate_output_relative_path(&candidate);
            let mut discovery_policy = embedded_policy.clone();
            if let Some(policy) = run_policy {
                discovery_policy.mode = policy.embedded_mode(false);
            }
            let nested_candidates = if !was_cancelled
                && (config.is_none() || candidate.depth < request.recursion_limit)
            {
                let scan_unrecognized = config.is_none_or(|c| {
                    matches!(
                        c.extraction.embedded.nested,
                        smartzip_config::NestedScan::Aggressive | smartzip_config::NestedScan::All
                    )
                });
                if let Some(files) = committed_output_files.borrow().as_deref() {
                    discover_nested_candidates_from_inventory(
                        nested_scanner,
                        &actual_output_dir,
                        files,
                        candidate.depth + 1,
                        &output_relative_path,
                        &discovery_policy,
                        nested_embedded_enabled,
                        scan_unrecognized,
                        cancellation.token(),
                    )
                } else {
                    discover_nested_candidates(
                        nested_scanner,
                        &actual_output_dir,
                        candidate.depth + 1,
                        &output_relative_path,
                        &discovery_policy,
                        nested_embedded_enabled,
                        scan_unrecognized,
                        cancellation.token(),
                    )
                }
            } else {
                Vec::new()
            };
            for nested in nested_candidates {
                if !task_budget.reserve_nested(request.limits.max_nested_candidates) {
                    record_failure(&mut failed_count, config, &cancellation);
                    events.push(TaskEvent::failed(
                        task_id.clone(),
                        &crate::budget::exceeded("nested candidate limit exceeded"),
                    ));
                    finish_execution_outcome(
                        execution,
                        &task_id,
                        &node.id,
                        node.generation,
                        successful_outcome(),
                    )
                    .await?;
                    break 'nodes;
                }
                let child_id = NodeId::new();
                let inserted = if let Some(execution) = execution {
                    execution
                        .enqueue_child(&task_id, &child_id, &node.id, &node.root_id, &nested, 0)
                        .await?
                } else {
                    true
                };
                if inserted {
                    enqueued.push(nested.clone());
                    queue.push_back(ExecutionNode {
                        id: child_id,
                        root_id: node.root_id.clone(),
                        generation: 0,
                        candidate: nested,
                    });
                } else {
                    task_budget.release_nested();
                }
            }

            // For volume sets, recycle all members that are inside the managed output root; for singles, recycle the single candidate.
            if !was_cancelled
                && candidate.source != CandidateSource::RootInput
                && config.is_none_or(|c| {
                    c.extraction.cleanup.nested_archives != smartzip_config::Cleanup::Keep
                })
            {
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
                        if let Some(path) =
                            recyclable_nested_archive_path(&synthetic, &request.output_dir)
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
                } else if let Some(path) =
                    recyclable_nested_archive_path(&candidate, &request.output_dir)
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
            finish_execution_outcome(
                execution,
                &task_id,
                &node.id,
                node.generation,
                successful_outcome(),
            )
            .await?;
            break 'groupings;
        }
    }

    while let Some(node) = queue.pop_front() {
        let (status, reason) = if was_cancelled || cancellation.is_user_cancelled() {
            ("cancelled", "cancelled")
        } else {
            ("skipped", "task_stopped")
        };
        finish_execution(
            execution,
            &task_id,
            &node.id,
            node.generation,
            status,
            Some(reason),
            None,
            false,
        )
        .await?;
    }

    was_cancelled |= cancellation.is_user_cancelled();
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
    if let Some(recorder) = legacy_history {
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

fn retryable_group_error(error: &smartzip_core::SmartZipError) -> bool {
    use smartzip_core::SmartZipError::*;
    matches!(
        error,
        CorruptedArchive { .. }
            | WrongPassword { .. }
            | PasswordRequired { .. }
            | PasswordIndeterminate { .. }
            | UnsupportedFormat { .. }
            | UnsupportedContainer { .. }
            | UnsupportedCodec { .. }
            | BackendProtocolError { .. }
    )
}

async fn enter_stage(
    execution: Option<&dyn crate::ExecutionStateRecorder>,
    task_id: &TaskId,
    node_id: &NodeId,
    generation: u64,
    from: &str,
    stage: crate::coordinator::Stage,
    attempt_id: Option<&AttemptId>,
    cancellation: &crate::TaskCancellation,
) -> smartzip_core::Result<bool> {
    let Some(execution) = execution else {
        return Ok(!cancellation.is_cancelled());
    };
    let entered = execution
        .transition(
            task_id,
            node_id,
            generation,
            from,
            "running",
            stage,
            attempt_id,
            cancellation.token(),
        )
        .await;
    match entered {
        Ok(true) => Ok(true),
        Err(smartzip_core::SmartZipError::Cancelled) if cancellation.is_cancelled() => Ok(false),
        Ok(false) => Err(smartzip_core::SmartZipError::BackendFailed {
            backend: "task-coordinator".into(),
            exit_code: None,
            stderr: format!("stale stage result for node {node_id} generation {generation}"),
        }),
        Err(error) => Err(error),
    }
}

async fn finish_interrupted_node(
    execution: Option<&dyn crate::ExecutionStateRecorder>,
    task_id: &TaskId,
    node_id: &NodeId,
    generation: u64,
    cancellation: &crate::TaskCancellation,
) -> smartzip_core::Result<()> {
    let (status, reason) = if cancellation.is_user_cancelled() {
        ("cancelled", "cancelled")
    } else {
        ("skipped", "task_stopped")
    };
    finish_execution(
        execution,
        task_id,
        node_id,
        generation,
        status,
        Some(reason),
        None,
        false,
    )
    .await
}

async fn finish_execution(
    execution: Option<&dyn crate::ExecutionStateRecorder>,
    task_id: &TaskId,
    node_id: &NodeId,
    generation: u64,
    status: &str,
    reason: Option<&str>,
    output_path: Option<&std::path::Path>,
    committed: bool,
) -> smartzip_core::Result<()> {
    finish_execution_outcome(
        execution,
        task_id,
        node_id,
        generation,
        crate::NodeOutcome::terminal(status, reason, output_path, committed),
    )
    .await
}

async fn finish_execution_outcome(
    execution: Option<&dyn crate::ExecutionStateRecorder>,
    task_id: &TaskId,
    node_id: &NodeId,
    generation: u64,
    outcome: crate::NodeOutcome,
) -> smartzip_core::Result<()> {
    let Some(execution) = execution else {
        return Ok(());
    };
    if execution
        .finish_node(task_id, node_id, generation, outcome)
        .await?
    {
        Ok(())
    } else {
        Err(smartzip_core::SmartZipError::BackendFailed {
            backend: "task-coordinator".into(),
            exit_code: None,
            stderr: format!("stale terminal result for node {node_id} generation {generation}"),
        })
    }
}

async fn begin_decision(
    execution: Option<&dyn crate::ExecutionStateRecorder>,
    task_id: &TaskId,
    node_id: &NodeId,
    generation: u64,
    stage: crate::coordinator::Stage,
    kind: &str,
    evidence: &str,
) -> smartzip_core::Result<DecisionId> {
    let decision_id = DecisionId::new();
    if let Some(execution) = execution {
        if !execution
            .wait_for_decision(
                task_id,
                node_id,
                generation,
                stage,
                &decision_id,
                kind,
                evidence,
            )
            .await?
        {
            return Err(smartzip_core::SmartZipError::BackendFailed {
                backend: "task-coordinator".into(),
                exit_code: None,
                stderr: format!("could not persist decision for node {node_id}"),
            });
        }
    }
    Ok(decision_id)
}

async fn finish_decision(
    execution: Option<&dyn crate::ExecutionStateRecorder>,
    task_id: &TaskId,
    node_id: &NodeId,
    generation: u64,
    stage: crate::coordinator::Stage,
    decision_id: &DecisionId,
    cancellation: &crate::TaskCancellation,
) -> smartzip_core::Result<bool> {
    let Some(execution) = execution else {
        return Ok(true);
    };
    if !execution
        .accept_decision(task_id, node_id, generation, decision_id)
        .await?
    {
        return Err(smartzip_core::SmartZipError::BackendFailed {
            backend: "task-coordinator".into(),
            exit_code: None,
            stderr: format!("stale decision reply for node {node_id}"),
        });
    }
    enter_stage(
        Some(execution),
        task_id,
        node_id,
        generation,
        "ready",
        stage,
        None,
        cancellation,
    )
    .await
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
