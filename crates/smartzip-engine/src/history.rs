//! Task history recording for extraction and detection workflows.
//!
//! The engine emits [`TaskEvent`]s throughout its lifecycle. When a
//! [`TaskHistoryRecorder`] is threaded into the workflow, those events are
//! also persisted to the `tasks`, `task_events`, `encoding_detections`, and
//! `embedded_archive_detections` tables described in `docs/design.md § 4`.
//!
//! **Best-effort semantics.** History writes never fail extraction. When a
//! repo call errors, the engine surfaces a `TaskEventKind::Warning` and
//! keeps going; the archive itself still extracts.
//!
//! The trait is deliberately not `Send + Sync`. The default
//! [`DbTaskHistoryRecorder`] holds `&rusqlite::Connection`, and rusqlite's
//! `Connection` is `!Sync`, so binding `Send + Sync` here would force every
//! implementation into a mutex. Callers that need to hand the recorder to
//! another thread can wrap their own implementation in whatever
//! synchronization primitive they prefer.

use crate::EncodingConfirmationContext;
use smartzip_core::{EncodingDetectionResult, EncodingMode, TaskEvent, TaskEventKind, TaskId};
use smartzip_db::{
    embedded_archive_detection::{
        EmbeddedArchiveDetectionRepository, NewEmbeddedArchiveDetection,
    },
    encoding_detection::{EncodingDetectionRepository, NewEncodingDetection},
    password::PasswordRepository,
    path_hash::path_hash,
    task::{NewTask, TaskFinish, TaskRepository, TaskStatus},
    task_event::{NewTaskEvent, TaskEventLevel, TaskEventRepository},
    timestamp::now_utc_iso8601,
};
use smartzip_scanner::{Confidence, EmbeddedArchiveFinding};
use std::path::Path;

/// Terminal status reported to [`TaskHistoryRecorder::finish`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskCompletionStatus {
    Completed,
    Partial,
    Failed,
    Cancelled,
}

impl TaskCompletionStatus {
    fn to_status(self) -> TaskStatus {
        match self {
            Self::Completed => TaskStatus::Completed,
            Self::Partial => TaskStatus::Partial,
            Self::Failed => TaskStatus::Failed,
            Self::Cancelled => TaskStatus::Cancelled,
        }
    }
}

/// Aggregates handed to [`TaskHistoryRecorder::finish`] on task completion.
///
/// These are counted by the engine as work progresses and passed as a batch
/// at the end so history writes stay off the extraction hot path.
#[derive(Debug, Clone)]
pub struct TaskOutcome<'a> {
    pub status: TaskCompletionStatus,
    pub error_code: Option<&'a str>,
    pub error_message: Option<&'a str>,
    pub password_attempts: i64,
    pub encoding_selected: Option<&'a str>,
    pub embedded_found: i64,
    pub output_path: Option<&'a Path>,
}

/// Recording hook the engine calls when a task history sink is attached.
///
/// All methods are called synchronously from the engine's async loop; they
/// should return quickly and swallow storage errors internally.
pub trait TaskHistoryRecorder {
    /// Register a new extract task. Called once at the top of extraction.
    fn start_extract(
        &self,
        task_id: &TaskId,
        input_summary: &str,
        output_path: Option<&Path>,
    );

    /// Register a new detect task. Called once at the top of `detect()`.
    fn start_detect(&self, task_id: &TaskId, path: &Path);

    /// Persist a generic engine event. Both a `task_events` row and any
    /// side-effect (e.g. bumping counters in the caller) belong here.
    ///
    /// The engine calls this for every event pushed through the sink, in
    /// order. `Started` and `Completed` are recorded for the timeline but
    /// terminal task state is written by [`Self::finish`] instead.
    fn record_event(&self, task_id: &TaskId, event: &TaskEvent);

    /// Persist a ZIP encoding assessment.
    fn record_encoding_detection(
        &self,
        task_id: &TaskId,
        archive_path: &Path,
        archive_format: Option<&str>,
        detected: &EncodingDetectionResult,
        context: Option<&EncodingConfirmationContext>,
        user_corrected: bool,
    );

    /// Persist a batch of embedded-archive findings for a single file.
    ///
    /// Called once per scanned candidate with all findings; an empty slice
    /// is a valid no-op.
    fn record_embedded_findings(
        &self,
        task_id: &TaskId,
        path: &Path,
        findings: &[EmbeddedArchiveFinding],
    );

    /// Update the `tasks` row with terminal status and aggregated metrics.
    fn finish(&self, task_id: &TaskId, outcome: TaskOutcome<'_>);

    /// Record a password/archive-shape match outcome in `password_matches`.
    ///
    /// `password_id` is `None` for candidates not backed by a stored row
    /// (empty password, manual entries not yet saved); those are ignored.
    /// Only call with `success = false` on confirmed wrong-password results.
    /// Defaults to a no-op so custom recorders need not implement it.
    fn record_password_match(
        &self,
        password_id: Option<i64>,
        archive_format: Option<&str>,
        filename_pattern: Option<&str>,
        success: bool,
    ) {
        let _ = (password_id, archive_format, filename_pattern, success);
    }
}

/// Recorder that writes to a SQLite [`smartzip_db::SmartZipDb`].
///
/// The connection is borrowed for the lifetime of the recorder; it's the
/// caller's job to keep the [`smartzip_db::SmartZipDb`] alive across the
/// extract call. Repo errors turn into `stderr` warnings — they never
/// propagate.
pub struct DbTaskHistoryRecorder<'a> {
    conn: &'a rusqlite::Connection,
}

impl<'a> DbTaskHistoryRecorder<'a> {
    pub fn new(conn: &'a rusqlite::Connection) -> Self {
        Self { conn }
    }

    fn task_repo(&self) -> TaskRepository<'a> {
        TaskRepository::new(self.conn)
    }

    fn event_repo(&self) -> TaskEventRepository<'a> {
        TaskEventRepository::new(self.conn)
    }

    fn encoding_repo(&self) -> EncodingDetectionRepository<'a> {
        EncodingDetectionRepository::new(self.conn)
    }

    fn embedded_repo(&self) -> EmbeddedArchiveDetectionRepository<'a> {
        EmbeddedArchiveDetectionRepository::new(self.conn)
    }

    /// Expose the underlying [`PasswordRepository`] so callers can record
    /// filename/format match statistics through the same connection.
    pub fn passwords(&self) -> PasswordRepository<'a> {
        PasswordRepository::new(self.conn)
    }

    fn warn(context: &str, error: impl std::fmt::Display) {
        eprintln!("history warning: {context}: {error}");
    }
}

impl<'a> TaskHistoryRecorder for DbTaskHistoryRecorder<'a> {
    fn start_extract(
        &self,
        task_id: &TaskId,
        input_summary: &str,
        output_path: Option<&Path>,
    ) {
        let output = output_path.map(|p| p.to_string_lossy().into_owned());
        let started_at = now_utc_iso8601();
        if let Err(error) = self.task_repo().insert(NewTask {
            id: task_id.as_str(),
            kind: "extract",
            input_summary,
            output_path: output.as_deref(),
            started_at: &started_at,
        }) {
            Self::warn("task insert", error);
        }
    }

    fn start_detect(&self, task_id: &TaskId, path: &Path) {
        let display = path.to_string_lossy().into_owned();
        let started_at = now_utc_iso8601();
        if let Err(error) = self.task_repo().insert(NewTask {
            id: task_id.as_str(),
            kind: "detect",
            input_summary: &display,
            output_path: None,
            started_at: &started_at,
        }) {
            Self::warn("task insert", error);
        }
    }

    fn record_event(&self, task_id: &TaskId, event: &TaskEvent) {
        let (level, event_type, message, data_json) = describe_event(&event.kind);
        let created_at = now_utc_iso8601();
        if let Err(error) = self.event_repo().insert(NewTaskEvent {
            task_id: task_id.as_str(),
            level,
            event_type: &event_type,
            message: &message,
            data_json: data_json.as_deref(),
            created_at: &created_at,
        }) {
            Self::warn("event insert", error);
        }
    }

    fn record_encoding_detection(
        &self,
        _task_id: &TaskId,
        archive_path: &Path,
        archive_format: Option<&str>,
        detected: &EncodingDetectionResult,
        _context: Option<&EncodingConfirmationContext>,
        user_corrected: bool,
    ) {
        let hash = path_hash(archive_path);
        let selected = match &detected.selected {
            EncodingMode::Auto => "auto".to_string(),
            EncodingMode::Override(name) => name.clone(),
        };
        let candidates_json = match serde_json::to_string(&detected.candidates) {
            Ok(s) => s,
            Err(error) => {
                Self::warn("candidates_json", error);
                "[]".to_string()
            }
        };
        let created_at = now_utc_iso8601();
        if let Err(error) = self.encoding_repo().insert(NewEncodingDetection {
            archive_path_hash: &hash,
            archive_format,
            selected_encoding: &selected,
            confidence: detected.confidence,
            user_corrected,
            candidates_json: &candidates_json,
            created_at: &created_at,
        }) {
            Self::warn("encoding_detection insert", error);
        }
    }

    fn record_embedded_findings(
        &self,
        _task_id: &TaskId,
        path: &Path,
        findings: &[EmbeddedArchiveFinding],
    ) {
        if findings.is_empty() {
            return;
        }
        let hash = path_hash(path);
        let created_at = now_utc_iso8601();
        let rows: Vec<NewEmbeddedArchiveDetection<'_>> = findings
            .iter()
            .map(|finding| NewEmbeddedArchiveDetection {
                file_path_hash: &hash,
                format: finding.format.as_str(),
                offset: finding.offset,
                confidence: confidence_score(finding.confidence),
                size_hint: finding.size,
                created_at: &created_at,
            })
            .collect();
        if let Err(error) = self.embedded_repo().insert_many(&rows) {
            Self::warn("embedded_archive_detection insert", error);
        }
    }

    fn record_password_match(
        &self,
        password_id: Option<i64>,
        archive_format: Option<&str>,
        filename_pattern: Option<&str>,
        success: bool,
    ) {
        let Some(id) = password_id else {
            return;
        };
        let repo = self.passwords();
        let result = if success {
            repo.record_match_success(id, archive_format, filename_pattern)
        } else {
            repo.record_match_failure(id, archive_format, filename_pattern)
        };
        if let Err(error) = result {
            Self::warn("password_match", error);
        }
    }

    fn finish(&self, task_id: &TaskId, outcome: TaskOutcome<'_>) {
        let finished_at = now_utc_iso8601();
        let output = outcome.output_path.map(|p| p.to_string_lossy().into_owned());
        let finish = TaskFinish {
            status: Some(outcome.status.to_status()),
            finished_at: Some(&finished_at),
            error_code: outcome.error_code,
            error_message: outcome.error_message,
            password_attempts: Some(outcome.password_attempts),
            encoding_selected: outcome.encoding_selected,
            embedded_found: Some(outcome.embedded_found),
            output_path: output.as_deref(),
        };
        if let Err(error) = self.task_repo().finish(task_id.as_str(), finish) {
            Self::warn("task finish", error);
        }
    }
}

/// Map a [`TaskEventKind`] to the row shape stored in `task_events`.
fn describe_event(kind: &TaskEventKind) -> (TaskEventLevel, String, String, Option<String>) {
    match kind {
        TaskEventKind::Started => (
            TaskEventLevel::Info,
            "Started".into(),
            "task started".into(),
            None,
        ),
        TaskEventKind::Progress(progress) => (
            TaskEventLevel::Info,
            "Progress".into(),
            progress.message.clone(),
            serde_json::to_string(progress).ok(),
        ),
        TaskEventKind::PasswordTried { candidate_id } => (
            TaskEventLevel::Info,
            "PasswordTried".into(),
            match candidate_id {
                Some(id) => format!("password candidate {id}"),
                None => "password candidate".into(),
            },
            serde_json::to_string(&serde_json::json!({ "candidate_id": candidate_id })).ok(),
        ),
        TaskEventKind::EncodingDetected(result) => {
            let name = match &result.selected {
                EncodingMode::Auto => "auto".to_string(),
                EncodingMode::Override(n) => n.clone(),
            };
            (
                TaskEventLevel::Info,
                "EncodingDetected".into(),
                format!("selected {name} @ {:.0}%", result.confidence * 100.0),
                serde_json::to_string(result).ok(),
            )
        }
        TaskEventKind::EmbeddedArchiveFound {
            offset,
            format,
            description,
            ..
        } => (
            TaskEventLevel::Info,
            "EmbeddedArchiveFound".into(),
            format!("{} @ 0x{:X} — {description}", format.as_str(), offset),
            serde_json::to_string(kind).ok(),
        ),
        TaskEventKind::EmbeddedArchiveSelected { format, offset, .. } => (
            TaskEventLevel::Info,
            "EmbeddedArchiveSelected".into(),
            format!("{} @ 0x{:X}", format.as_str(), offset),
            serde_json::to_string(kind).ok(),
        ),
        TaskEventKind::EmbeddedArchiveCarved {
            source, offset, ..
        } => (
            TaskEventLevel::Info,
            "EmbeddedArchiveCarved".into(),
            format!("{} @ 0x{:X}", source.display(), offset),
            serde_json::to_string(kind).ok(),
        ),
        TaskEventKind::EmbeddedArchiveSelectionRequired {
            path,
            findings_count,
        } => (
            TaskEventLevel::Warn,
            "EmbeddedArchiveSelectionRequired".into(),
            format!("{} findings for {}", findings_count, path.display()),
            serde_json::to_string(kind).ok(),
        ),
        TaskEventKind::LargeEmbeddedScanConfirmationRequired {
            path,
            file_size,
            threshold,
        } => (
            TaskEventLevel::Warn,
            "LargeEmbeddedScanConfirmationRequired".into(),
            format!(
                "{}: {file_size} bytes > {threshold}",
                path.display()
            ),
            serde_json::to_string(kind).ok(),
        ),
        TaskEventKind::BusinessContainerSkipped { path, kind: k } => (
            TaskEventLevel::Info,
            "BusinessContainerSkipped".into(),
            format!("{} ({})", path.display(), k),
            serde_json::to_string(kind).ok(),
        ),
        TaskEventKind::OutputCreated { path } => (
            TaskEventLevel::Info,
            "OutputCreated".into(),
            path.display().to_string(),
            None,
        ),
        TaskEventKind::Warning { message } => (
            TaskEventLevel::Warn,
            "Warning".into(),
            message.clone(),
            None,
        ),
        TaskEventKind::Failed { error } => (
            TaskEventLevel::Error,
            "Failed".into(),
            error.clone(),
            None,
        ),
        TaskEventKind::Completed => (
            TaskEventLevel::Info,
            "Completed".into(),
            "task completed".into(),
            None,
        ),
    }
}

fn confidence_score(confidence: Confidence) -> f32 {
    match confidence {
        Confidence::Low => 0.4,
        Confidence::Medium => 0.7,
        Confidence::High => 0.95,
    }
}

/// Normalize an archive filename into a stable pattern suitable for
/// `password_matches.filename_pattern`.
///
/// - lowercase
/// - drop the extension
/// - collapse consecutive digits into a single `#` so that `dump_2024_01.zip`
///   and `dump_2024_02.zip` share a pattern.
///
/// Kept in this module (not in `smartzip-db`) because it's engine-side
/// heuristic policy rather than schema.
pub fn normalize_filename_pattern(path: &Path) -> Option<String> {
    let stem = path.file_stem()?.to_string_lossy().to_ascii_lowercase();
    if stem.is_empty() {
        return None;
    }
    let mut out = String::with_capacity(stem.len());
    let mut in_digits = false;
    for ch in stem.chars() {
        if ch.is_ascii_digit() {
            if !in_digits {
                out.push('#');
                in_digits = true;
            }
        } else {
            out.push(ch);
            in_digits = false;
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn normalize_collapses_digit_runs() {
        assert_eq!(
            normalize_filename_pattern(&PathBuf::from("Dump_2024_01.zip")).as_deref(),
            Some("dump_#_#"),
        );
        assert_eq!(
            normalize_filename_pattern(&PathBuf::from("Photos-Trip.7z")).as_deref(),
            Some("photos-trip"),
        );
    }

    #[test]
    fn normalize_handles_dotfile_stem() {
        // `Path::file_stem` on ".zip" returns Some(".zip"); we accept that as
        // a usable pattern rather than special-casing hidden files.
        assert_eq!(
            normalize_filename_pattern(&PathBuf::from(".zip")).as_deref(),
            Some(".zip"),
        );
    }

    #[test]
    fn normalize_returns_none_when_path_has_no_stem() {
        // Trailing slash — no filename component at all.
        assert_eq!(normalize_filename_pattern(&PathBuf::from("/")), None);
    }
}
