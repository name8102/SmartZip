//! Read-only integrity testing, password attempts, and independent diagnosis.
use crate::backend_util::backend_call;
use crate::events::{EventSink, TaskEventListener};
use crate::history::{FileExtractionRow, TaskCompletionStatus, TaskHistoryRecorder, TaskOutcome};
use crate::interactive::InteractivePasswordPrompter;
use crate::test_reduce::reduce;
use serde::{Deserialize, Serialize};
use smartzip_archive::diagnostic::{self, DiagnosticControl};
use smartzip_archive::integrity::*;
use smartzip_archive::volumes::{VolumeFamily, VolumeSet};
use smartzip_archive::{ArchiveExecutor, TestRequest, TestResult};
use smartzip_core::{EncodingMode, Result, SmartZipError, TaskEvent, TaskEventKind, TaskId};
use smartzip_passwords::{
    PasswordCandidate, PasswordCandidateRequest, PasswordService, PasswordSource,
};
use smartzip_scanner::{EmbeddedScanner, ScannerConfig};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnoseMode {
    #[default]
    Auto,
    Off,
}

#[derive(Debug, Clone)]
pub struct TestWorkflowRequest {
    pub paths: Vec<PathBuf>,
    pub encoding: EncodingMode,
    pub scanner: ScannerConfig,
    pub password_candidates: PasswordCandidateRequest,
    pub diagnose: DiagnoseMode,
    pub diagnostic_timeout: Option<Duration>,
    pub control: DiagnosticControl,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestWorkflowResult {
    pub schema_version: u32,
    pub command: String,
    pub task_id: TaskId,
    pub files: Vec<TestArchiveReport>,
    pub events: Vec<TaskEvent>,
    pub exit_code: i32,
}

pub(crate) async fn run<B: ArchiveExecutor>(
    backend: &B,
    passwords: &PasswordService<'_>,
    request: TestWorkflowRequest,
    prompter: Option<&dyn InteractivePasswordPrompter>,
    listener: Option<TaskEventListener>,
    history: Option<&dyn TaskHistoryRecorder>,
) -> Result<TestWorkflowResult> {
    if request.paths.is_empty() {
        return Err(SmartZipError::io(
            None,
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "test requires at least one input",
            ),
        ));
    }
    let task_id = TaskId::new();
    let events = EventSink::new(listener);
    let context = backend.begin_task(task_id.clone(), Arc::new(events.clone()));
    events.push(TaskEvent::started(task_id.clone()));
    if let Some(history) = history {
        history.start_task(&task_id, "test", None);
    }
    let mut candidates = match passwords.ranked_candidates(request.password_candidates.clone()) {
        Ok(candidates) => candidates,
        Err(error) => {
            warning(
                &events,
                &task_id,
                format!("password candidates unavailable: {error}"),
            );
            request
                .password_candidates
                .manual
                .iter()
                .map(|value| PasswordCandidate {
                    id: None,
                    value: value.clone(),
                    source: PasswordSource::Manual,
                })
                .collect()
        }
    };
    // One no-password integrity pass establishes whether a supplied candidate
    // is actually needed. A clean unencrypted archive finishes on this pass.
    if request.password_candidates.include_empty {
        candidates.retain(|candidate| !candidate.value.is_empty());
        candidates.insert(
            0,
            PasswordCandidate {
                id: None,
                value: String::new(),
                source: PasswordSource::Empty,
            },
        );
    }
    let mut files: Vec<TestArchiveReport> = Vec::new();
    for path in &request.paths {
        let set = match VolumeSet::collect(path) {
            Ok(mut set) => {
                if set.format.is_none() {
                    set.format = crate::nested::format_from_extension(&set.entrypoint);
                }
                set
            }
            Err(error) => {
                let missing = if error.kind() == std::io::ErrorKind::NotFound {
                    vec![path.clone()]
                } else {
                    Vec::new()
                };
                let unreadable = if missing.is_empty() {
                    vec![path.clone()]
                } else {
                    Vec::new()
                };
                VolumeSet {
                    family: VolumeFamily::Single,
                    format: None,
                    entrypoint: path.clone(),
                    members: Vec::new(),
                    missing,
                    unreadable,
                    issues: vec![error.to_string()],
                    ambiguous: false,
                }
            }
        };
        if let Some(existing) = files.iter_mut().find(|f| f.entrypoint == set.entrypoint) {
            if !existing.input_paths.contains(path) {
                existing.input_paths.push(path.clone());
            }
            continue;
        }
        files.push(TestArchiveReport::new(set, path.clone()));
    }
    let mut pass_id = 0u32;
    for report in &mut files {
        let mut password_id = None;
        let mut verified_candidate = None;
        if request.control.is_cancelled() {
            report
                .stop_reasons
                .push("cancelled before this group was tested".into());
            reduce(report);
            if let Some(history) = history {
                record_file(history, &task_id, report, None, &request.encoding);
            }
            continue;
        }
        if report.volumes.format.is_none() && report.entrypoint.is_file() {
            let scanner = EmbeddedScanner::new(request.scanner.clone());
            match scanner.scan_path(&report.entrypoint) {
                Ok(findings) => {
                    report.volumes.format = findings
                        .iter()
                        .find(|f| f.offset == 0)
                        .map(|f| f.format.clone());
                    if findings.iter().any(|f| f.offset > 0) {
                        report.stop_reasons.push(
                            "embedded findings are outside this test's top-level archive scope"
                                .into(),
                        );
                    }
                }
                Err(error) => report.stop_reasons.push(format!("format scan: {error}")),
            }
        }
        let mut chosen = None;
        let mut password_was_required = false;
        if !report.volumes.ambiguous
            && report.entrypoint.is_file()
            && !report.unreadable_volumes.contains(&report.entrypoint)
        {
            let mut attempts = candidates.clone();
            if attempts.is_empty() {
                report.password_status = PasswordStatus::Required;
                report
                    .stop_reasons
                    .push("no password candidates and empty attempt disabled".into());
            }
            let mut prompted = false;
            let mut index = 0;
            loop {
                let candidate = if let Some(candidate) = attempts.get(index) {
                    candidate.clone()
                } else if password_was_required && !prompted && prompter.is_some() {
                    prompted = true;
                    let Some(prompter) = prompter else { break };
                    let value = tokio::select! { biased; _=cancelled(&request.control)=>None, value=prompter.prompt(&report.entrypoint)=>value };
                    match value.filter(|p| !p.is_empty()) {
                        Some(value) if !attempts.iter().any(|p| p.value == value) => {
                            PasswordCandidate {
                                id: None,
                                value,
                                source: PasswordSource::Manual,
                            }
                        }
                        _ => break,
                    }
                } else {
                    break;
                };
                index += 1;
                events.push(TaskEvent {
                    task_id: task_id.clone(),
                    kind: TaskEventKind::PasswordTried {
                        candidate_id: candidate.id,
                    },
                });
                pass_id = pass_id.saturating_add(1);
                phase(&events, &task_id, report, pass_id, "integrity");
                let test_request = backend_request(report, &request.encoding, Some(&candidate));
                let result = tokio::select! { biased;
                    _=cancelled(&request.control)=>Err(SmartZipError::Cancelled),
                    result=backend_call("archive-executor","testing",&report.entrypoint,backend.test_with_context(test_request,context.clone()))=>result,
                };
                let result = executed_result(result);
                let failure = result.diagnostics.failure;
                let ok = result.ok;
                let encrypted = result.encrypted;
                chosen = Some(candidate.clone());
                append_pass(report, result, pass_id, "integrity");
                if failure == Some(TestFailure::Cancelled) {
                    request.control.cancel();
                    report
                        .stop_reasons
                        .push("cancelled during backend test; partial report retained".into());
                    break;
                }
                if ok {
                    report.password_status =
                        if candidate.value.is_empty() || encrypted == Some(false) {
                            PasswordStatus::NotNeeded
                        } else if encrypted == Some(true) || password_was_required {
                            PasswordStatus::Verified
                        } else {
                            PasswordStatus::Indeterminate
                        };
                    if report.password_status == PasswordStatus::Verified
                        && !candidate.value.is_empty()
                    {
                        verified_candidate = Some(candidate.clone());
                    }
                    break;
                }
                match failure {
                    Some(TestFailure::PasswordRequired) => {
                        password_was_required = true;
                        report.password_status = PasswordStatus::Required;
                    }
                    Some(TestFailure::PasswordRejected) => {
                        password_was_required = true;
                        report.password_status = PasswordStatus::Rejected;
                    }
                    Some(TestFailure::PasswordIndeterminate) => {
                        password_was_required = true;
                        report.password_status = PasswordStatus::Indeterminate;
                    }
                    _ => {
                        report.password_status =
                            if encrypted == Some(false) || candidate.value.is_empty() {
                                PasswordStatus::NotNeeded
                            } else {
                                PasswordStatus::Indeterminate
                            };
                        break;
                    }
                }
                // A corrupt encryption/check field can resemble a rejected
                // credential. Test never penalizes a library candidate here.
                if index >= 128 {
                    report
                        .stop_reasons
                        .push("password attempt budget reached".into());
                    break;
                }
                if index >= attempts.len() && prompted {
                    break;
                }
                if index > attempts.len() {
                    attempts.push(candidate);
                }
            }
        } else {
            report
                .stop_reasons
                .push("backend entrypoint is missing, unreadable, or ambiguous".into());
        }
        reduce(report);
        if request.diagnose == DiagnoseMode::Auto
            && report.integrity != Integrity::Intact
            && !request.control.is_cancelled()
        {
            let mut control = request.control.clone();
            control.deadline = request
                .diagnostic_timeout
                .and_then(|duration| Instant::now().checked_add(duration));
            pass_id = pass_id.saturating_add(1);
            phase(&events, &task_id, report, pass_id, "local_diagnosis");
            let set = report.volumes.clone();
            let failed_files = if report.password_status == PasswordStatus::Indeterminate
                && password_was_required
            {
                Vec::new()
            } else {
                report.damaged_files.clone()
            };
            let local_control = control.clone();
            let local = tokio::task::spawn_blocking(move || {
                diagnostic::inspect(&set, &failed_files, &local_control, pass_id)
            })
            .await;
            match local {
                Ok(local) => {
                    report.evidence.extend(local.evidence);
                    report.checked_scopes.extend(local.checked_scopes);
                    report.missing_volumes.extend(local.missing);
                    report.unreadable_volumes.extend(local.unreadable);
                    report.stop_reasons.extend(local.stop_reasons);
                    if local.encrypted == Some(false) && !password_was_required {
                        report.password_status = PasswordStatus::NotNeeded;
                    }
                }
                Err(error) => report
                    .stop_reasons
                    .push(format!("local diagnostic worker failed: {error}")),
            }
            reduce(report);
            let previous = report.passes.last().map(|pass| pass.diagnostics.clone());
            // No second full read when credentials are unresolved, input is
            // ambiguous, or an added reader cannot reach the entrypoint.
            if !password_was_required
                && control.check().is_ok()
                && report.entrypoint.is_file()
                && !report.volumes.ambiguous
            {
                if let Some(previous) = previous {
                    pass_id = pass_id.saturating_add(1);
                    phase(&events, &task_id, report, pass_id, "diagnostic_backend");
                    let test_request = backend_request(report, &request.encoding, chosen.as_ref());
                    let result = tokio::select! { biased;
                        _=cancelled(&control)=>Err(SmartZipError::Cancelled),
                        _=deadline(control.deadline)=>{report.stop_reasons.push("diagnostic timeout reached before backend completion".into());Ok(None)},
                        result=backend.diagnose_test_with_context(test_request,&previous,report.volumes.family!=VolumeFamily::Single,context.clone())=>result,
                    };
                    match result {
                        Ok(Some(result))=>{
                            if result.ok && report.passes.iter().any(|p|!p.ok) {report.stop_reasons.push("backend results disagree; earlier integrity evidence retained".into());}
                            append_pass(report,result,pass_id,"diagnostic");
                        }
                        Ok(None)=>report.stop_reasons.push("no additional backend diagnostic result (unavailable, forced backend, or budget)".into()),
                        Err(SmartZipError::Cancelled)=>{request.control.cancel();report.stop_reasons.push("cancelled during diagnostic backend; previous evidence retained".into());}
                        Err(error)=>report.stop_reasons.push(format!("additional diagnostic backend: {error}")),
                    }
                }
            }
            if control.check().is_err() && !request.control.is_cancelled() {
                report
                    .stop_reasons
                    .push("diagnostic time budget exhausted; unchecked ranges retained".into());
            }
        }
        let changed = report
            .volumes
            .members
            .iter()
            .filter(|m| !m.unchanged())
            .map(|m| PhysicalRange {
                volume: m.path.clone(),
                offset: 0,
                length: m.size,
            })
            .collect::<Vec<_>>();
        if !changed.is_empty() {
            for evidence in &mut report.evidence {
                evidence.strength = EvidenceStrength::Observation;
            }
            report.password_status = PasswordStatus::Indeterminate;
            report.evidence.push(TestEvidence {id:"input-changed".into(),kind:EvidenceKind::InputChanged,strength:EvidenceStrength::Observation,source:"volume-snapshot".into(),pass_id,ranges:changed,reference_ranges:Vec::new(),metadata_trust:MetadataTrust::Unverified,affected_entries:Vec::new(),summary:"volume identity/size/mtime changed during test; cross-phase conclusions invalidated".into()});
            report.stop_reasons.push("input_changed".into());
        }
        reduce(report);
        if report.integrity == Integrity::Intact {
            if let Some(candidate) = verified_candidate {
                match passwords.record_success(&candidate) {
                    Ok(id) => password_id = id,
                    Err(error) => warning(
                        &events,
                        &task_id,
                        format!("password success could not be recorded: {error}"),
                    ),
                }
            }
        }
        if let Some(history) = history {
            record_file(history, &task_id, report, password_id, &request.encoding);
        }
    }
    let intact = files
        .iter()
        .filter(|report| report.integrity == Integrity::Intact)
        .count();
    let exit_code = if request.control.is_cancelled() {
        130
    } else if intact == files.len() {
        0
    } else if intact == 0 {
        1
    } else {
        2
    };
    events.push(TaskEvent {
        task_id: task_id.clone(),
        kind: if exit_code == 0 {
            TaskEventKind::Completed
        } else {
            TaskEventKind::Failed {
                error: format!(
                    "test completed with {intact}/{} intact groups (exit {exit_code})",
                    files.len()
                ),
            }
        },
    });
    if let Some(history) = history {
        for event in events.snapshot() {
            history.record_event(&task_id, &event);
        }
        history.finish(
            &task_id,
            TaskOutcome {
                status: match exit_code {
                    0 => TaskCompletionStatus::Completed,
                    2 => TaskCompletionStatus::Partial,
                    130 => TaskCompletionStatus::Cancelled,
                    _ => TaskCompletionStatus::Failed,
                },
                output_path: None,
            },
        );
    }
    Ok(TestWorkflowResult {
        schema_version: 1,
        command: "test".into(),
        task_id,
        files,
        events: events.snapshot(),
        exit_code,
    })
}

fn backend_request(
    report: &TestArchiveReport,
    encoding: &EncodingMode,
    candidate: Option<&PasswordCandidate>,
) -> TestRequest {
    TestRequest {
        archive: report.entrypoint.clone(),
        format: report.volumes.format.clone(),
        password: candidate
            .filter(|c| !c.value.is_empty())
            .map(|c| c.value.clone()),
        encoding: encoding.clone(),
    }
}

fn executed_result(result: Result<TestResult>) -> TestResult {
    match result {
        Ok(result) => result,
        Err(error) => {
            let failure = match &error {
                SmartZipError::Cancelled => TestFailure::Cancelled,
                SmartZipError::PasswordRequired { .. } => TestFailure::PasswordRequired,
                SmartZipError::WrongPassword { .. } => TestFailure::PasswordRejected,
                SmartZipError::CorruptedArchive { .. } => TestFailure::Corruption,
                SmartZipError::Io { .. } => TestFailure::Io,
                _ => TestFailure::Unknown,
            };
            TestResult {
                ok: false,
                encrypted: None,
                diagnostics: BackendTestDiagnostics {
                    failure: Some(failure),
                    stderr: error.to_string(),
                    ..BackendTestDiagnostics::default()
                },
            }
        }
    }
}

fn append_pass(report: &mut TestArchiveReport, result: TestResult, pass_id: u32, purpose: &str) {
    for file in &result.diagnostics.damaged_files {
        if !report.damaged_files.contains(file) {
            report.damaged_files.push(file.clone());
        }
    }
    if result.diagnostics.output_truncated {
        report
            .stop_reasons
            .push("backend output budget reached; some text diagnostics were discarded".into());
    }
    if result.ok {
        report.checked_scopes.push(CheckedScope {
            source: result.diagnostics.adapter_id.clone(),
            pass_id,
            description: "complete backend integrity test".into(),
            ranges: report
                .volumes
                .members
                .iter()
                .map(|m| PhysicalRange {
                    volume: m.path.clone(),
                    offset: 0,
                    length: m.size,
                })
                .collect(),
            passed: true,
        });
    }
    report.passes.push(TestPass {
        pass_id,
        purpose: purpose.into(),
        ok: result.ok,
        diagnostics: result.diagnostics,
    });
}

fn phase(
    events: &EventSink,
    task_id: &TaskId,
    report: &TestArchiveReport,
    pass_id: u32,
    phase: &str,
) {
    events.push(TaskEvent {
        task_id: task_id.clone(),
        kind: TaskEventKind::TestPhase {
            path: report.entrypoint.clone(),
            pass_id,
            phase: phase.into(),
        },
    });
}
fn warning(events: &EventSink, task_id: &TaskId, message: String) {
    events.push(TaskEvent {
        task_id: task_id.clone(),
        kind: TaskEventKind::Warning { message },
    });
}

async fn cancelled(control: &DiagnosticControl) {
    loop {
        if control.is_cancelled() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}
async fn deadline(limit: Option<Instant>) {
    if let Some(limit) = limit {
        tokio::time::sleep_until(tokio::time::Instant::from_std(limit)).await;
    } else {
        std::future::pending::<()>().await;
    }
}

fn record_file(
    history: &dyn TaskHistoryRecorder,
    task_id: &TaskId,
    report: &TestArchiveReport,
    password_id: Option<i64>,
    encoding: &EncodingMode,
) {
    let serialized = serde_json::to_string(report).ok();
    let confirmed = serde_json::to_string(
        &report
            .confirmed_volumes
            .iter()
            .map(|v| &v.path)
            .collect::<Vec<_>>(),
    )
    .ok();
    let (status, reason) = match report.integrity {
        Integrity::Intact => ("intact", None),
        Integrity::Corrupt => ("corrupt", Some("integrity_failed")),
        Integrity::Incomplete => ("partial", Some("incomplete")),
        Integrity::Unknown if report.password_status == PasswordStatus::Required => {
            ("skipped", Some("password_required"))
        }
        Integrity::Unknown => ("failed", Some("unknown")),
    };
    history.record_file_extraction(
        task_id,
        FileExtractionRow {
            input_path: Path::new(&report.entrypoint),
            sample_hash: None,
            file_size: report
                .volumes
                .byte_len()
                .and_then(|n| i64::try_from(n).ok()),
            offset: None,
            output_path: None,
            has_password: report.password_status == PasswordStatus::Verified,
            password_id,
            status,
            reason,
            encoding: match encoding {
                EncodingMode::Auto => None,
                EncodingMode::Override(name) => Some(name),
            },
            encoding_corrected: false,
            damaged_volumes_json: confirmed.as_deref(),
            test_report_json: serialized.as_deref(),
        },
    );
}
