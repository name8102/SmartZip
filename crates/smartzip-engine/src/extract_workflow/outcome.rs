//! One exit for candidate history, failure events and result accounting.
use std::path::Path;

use smartzip_core::{TaskEvent, TaskEventKind, TaskId};

use crate::{
    events::EventSink,
    history::{FileExtractionRow, KnownFileEncodingUpsert, KnownFileUpsert, TaskHistoryRecorder},
    ExtractionCandidate,
};

#[derive(Default)]
pub(super) struct CandidateDetails<'a> {
    pub input_path: Option<&'a Path>,
    pub sample_hash: Option<&'a str>,
    pub file_size: Option<i64>,
    pub has_password: bool,
    pub password_id: Option<i64>,
    pub encoding: Option<&'a str>,
    pub encoding_corrected: bool,
    pub confirmed_encoding: Option<&'a str>,
}

pub(super) enum CandidateOutcome<'a> {
    Skipped(&'a str),
    Failed { reason: &'a str, error: String },
    Extracted(&'a Path),
    // Preserve the legacy history contract: these preparation errors count as
    // failures in the task summary but were stored as skipped file actions.
    Unreadable,
    CarveFailed(String),
}

pub(super) struct CandidateResults<'a> {
    history: Option<&'a dyn TaskHistoryRecorder>,
    task_id: &'a TaskId,
    events: &'a EventSink,
    pub processed: Vec<ExtractionCandidate>,
    pub skipped: Vec<ExtractionCandidate>,
    pub failed_count: usize,
}

impl<'a> CandidateResults<'a> {
    pub fn new(
        history: Option<&'a dyn TaskHistoryRecorder>,
        task_id: &'a TaskId,
        events: &'a EventSink,
    ) -> Self {
        Self {
            history,
            task_id,
            events,
            processed: vec![],
            skipped: vec![],
            failed_count: 0,
        }
    }

    pub fn skip(&mut self, candidate: ExtractionCandidate, reason: &str) {
        self.finish(
            candidate,
            CandidateDetails::default(),
            CandidateOutcome::Skipped(reason),
        );
    }

    /// Task-wide limits can fail after a candidate was already committed.
    pub fn fail_task(&mut self, error: String) {
        self.failed_count += 1;
        self.events.push(TaskEvent {
            task_id: self.task_id.clone(),
            kind: TaskEventKind::Failed { error },
        });
    }

    pub fn finish(
        &mut self,
        candidate: ExtractionCandidate,
        details: CandidateDetails<'_>,
        outcome: CandidateOutcome<'_>,
    ) {
        let (status, reason, output) = match &outcome {
            CandidateOutcome::Skipped(reason) => ("skipped", Some(*reason), None),
            CandidateOutcome::Failed { reason, error } => {
                self.fail_task(error.clone());
                ("failed", Some(*reason), None)
            }
            CandidateOutcome::Unreadable => {
                self.failed_count += 1;
                ("skipped", Some("not_found"), None)
            }
            CandidateOutcome::CarveFailed(error) => {
                self.fail_task(error.clone());
                ("skipped", Some(error.as_str()), None)
            }
            CandidateOutcome::Extracted(path) => {
                self.events.push(TaskEvent {
                    task_id: self.task_id.clone(),
                    kind: TaskEventKind::OutputCreated {
                        path: path.to_path_buf(),
                    },
                });
                ("extracted", None, Some(*path))
            }
        };
        if let Some(recorder) = self.history {
            recorder.record_file_extraction(
                self.task_id,
                FileExtractionRow {
                    input_path: details.input_path.unwrap_or(&candidate.path),
                    sample_hash: details.sample_hash,
                    file_size: details.file_size,
                    offset: candidate.embedded_offset.map(|o| o as i64),
                    output_path: output,
                    has_password: details.has_password,
                    password_id: details.password_id,
                    status,
                    reason,
                    encoding: details.encoding,
                    encoding_corrected: details.encoding_corrected,
                    damaged_volumes_json: None,
                    test_report_json: None,
                },
            );
            if output.is_some() {
                if let (Some(hash), Some(size)) = (details.sample_hash, details.file_size) {
                    let name = candidate.path.file_name().map(|n| n.to_string_lossy());
                    recorder.upsert_known_file_extract(KnownFileUpsert {
                        sample_hash: hash,
                        size,
                        name: name.as_deref(),
                        offset: candidate.embedded_offset.map(|o| o as i64),
                        password_id: details.password_id,
                    });
                    if let Some(encoding) = details.confirmed_encoding {
                        recorder.upsert_known_file_confirmed_encoding(KnownFileEncodingUpsert {
                            sample_hash: hash,
                            size,
                            name: name.as_deref(),
                            offset: candidate.embedded_offset.map(|o| o as i64),
                            encoding,
                        });
                    }
                }
            }
        }
        if output.is_some() {
            self.processed.push(candidate);
        } else {
            self.skipped.push(candidate);
        }
    }
}
