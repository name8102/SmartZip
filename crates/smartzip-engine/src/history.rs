//! Task history recording for extraction workflows (v3, file-grain).
//!
//! The engine emits [`TaskEvent`]s throughout its lifecycle. When a
//! [`TaskHistoryRecorder`] is threaded into the workflow, those events are
//! persisted to the `tasks` and `task_events` tables, and every extraction
//! *action* (one per input, nested archive, carved embedded archive, or
//! skip) is logged to `file_extractions`. The `known_files` dedup/reuse index
//! is consulted before extraction and updated after a success.
//!
//! **Best-effort semantics.** History writes never fail extraction. When a
//! repo call errors, the engine surfaces a `TaskEventKind::Warning` and keeps
//! going; the archive itself still extracts.
//!
//! The trait is deliberately not `Send + Sync`. The default
//! [`DbTaskHistoryRecorder`] holds `&rusqlite::Connection`, and rusqlite's
//! `Connection` is `!Sync`, so binding `Send + Sync` here would force every
//! implementation into a mutex. Callers that need to hand the recorder to
//! another thread can wrap their own implementation in whatever
//! synchronization primitive they prefer.

use smartzip_core::EncodingMode;
use smartzip_core::{TaskEvent, TaskEventKind, TaskId};
use smartzip_db::{
    file_extractions::{FileExtractionRepository, NewFileExtraction},
    known_files::{KnownFileRepository, NameOffset},
    password::PasswordRepository,
    task::{NewTask, TaskFinish, TaskRepository, TaskStatus},
    task_event::{NewTaskEvent, TaskEventLevel, TaskEventRepository},
    timestamp::now_utc_iso8601,
};
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
/// In the v3 file-grain model the per-file detail lives in `file_extractions`,
/// so the parent `tasks` row only carries the denormalized terminal status and
/// the operation-level output root.
#[derive(Debug, Clone)]
pub struct TaskOutcome<'a> {
    pub status: TaskCompletionStatus,
    pub output_path: Option<&'a Path>,
}

/// One logged extraction action, passed to
/// [`TaskHistoryRecorder::record_file_extraction`].
///
/// Paths are borrowed and stringified by the recorder. `sample_hash` / `size`
/// are `None` when the content couldn't be sampled (e.g. an unknown-length
/// carve); such rows never participate in dedup.
#[derive(Debug, Clone)]
pub struct FileExtractionRow<'a> {
    pub input_path: &'a Path,
    pub sample_hash: Option<&'a str>,
    pub file_size: Option<i64>,
    pub offset: Option<i64>,
    pub output_path: Option<&'a Path>,
    pub has_password: bool,
    pub password_id: Option<i64>,
    pub status: &'a str,
    pub reason: Option<&'a str>,
    pub encoding: Option<&'a str>,
    pub encoding_corrected: bool,
    pub damaged_volumes_json: Option<&'a str>,
}

/// Reuse hints returned by [`TaskHistoryRecorder::lookup_known_file`].
#[derive(Debug, Clone, Default)]
pub struct KnownFileHit {
    pub password_id: Option<i64>,
    pub confirmed_encoding: Option<String>,
    pub last_extract_at: Option<String>,
}

/// Arguments for [`TaskHistoryRecorder::upsert_known_file_extract`], recorded
/// after a successful extraction.
#[derive(Debug, Clone)]
pub struct KnownFileUpsert<'a> {
    pub sample_hash: &'a str,
    pub size: i64,
    pub name: Option<&'a str>,
    pub offset: Option<i64>,
    pub password_id: Option<i64>,
}

/// A user-confirmed encoding for an exact known file. Unlike an extract
/// upsert, this overwrites `confirmed_encoding` and does not touch
/// `last_extract_at`.
#[derive(Debug, Clone)]
pub struct KnownFileEncodingUpsert<'a> {
    pub sample_hash: &'a str,
    pub size: i64,
    pub name: Option<&'a str>,
    pub offset: Option<i64>,
    pub encoding: &'a str,
}

/// Recording hook the engine calls when a task history sink is attached.
///
/// All methods are called synchronously from the engine's async loop; they
/// should return quickly and swallow storage errors internally.
pub trait TaskHistoryRecorder {
    /// Register a new task. `kind` is one of the CLI/engine operation names
    /// (`extract`, `detect`, `list`, `test`, ...). `output_path` is only
    /// meaningful for operations that materialize files.
    fn start_task(&self, task_id: &TaskId, kind: &str, output_path: Option<&Path>);

    /// Register a new extract task. Called once at the top of extraction.
    fn start_extract(&self, task_id: &TaskId, output_path: Option<&Path>) {
        self.start_task(task_id, "extract", output_path)
    }

    /// Register a new detect task. Called once at the top of `detect()`.
    fn start_detect(&self, task_id: &TaskId, path: &Path) {
        let _ = path;
        self.start_task(task_id, "detect", None)
    }

    /// Persist a generic engine event into `task_events`.
    fn record_event(&self, task_id: &TaskId, event: &TaskEvent);

    /// Append one row to `file_extractions` (one extraction action).
    fn record_file_extraction(&self, task_id: &TaskId, row: FileExtractionRow<'_>);

    /// Look up the `known_files` reuse entry for a physical file.
    ///
    /// Returns `None` when the file was never seen. Defaults to `None` so
    /// custom recorders that don't back a `known_files` table need not
    /// implement it.
    fn lookup_known_file(&self, _sample_hash: &str, _size: i64) -> Option<KnownFileHit> {
        None
    }

    /// Record a successful extraction into the `known_files` index (writes
    /// `last_extract_at` + `password_id`, appends the name/offset pair).
    /// Defaults to a no-op.
    fn upsert_known_file_extract(&self, _upsert: KnownFileUpsert<'_>) {}

    /// Persist a command-line/user-confirmed encoding for future reuse.
    fn upsert_known_file_confirmed_encoding(&self, _upsert: KnownFileEncodingUpsert<'_>) {}

    /// Update the `tasks` row with terminal status and output root.
    fn finish(&self, task_id: &TaskId, outcome: TaskOutcome<'_>);
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

    fn file_repo(&self) -> FileExtractionRepository<'a> {
        FileExtractionRepository::new(self.conn)
    }

    fn known_repo(&self) -> KnownFileRepository<'a> {
        KnownFileRepository::new(self.conn)
    }

    /// Expose the underlying [`PasswordRepository`] so callers can share the
    /// same connection for password statistics.
    pub fn passwords(&self) -> PasswordRepository<'a> {
        PasswordRepository::new(self.conn)
    }

    fn warn(context: &str, error: impl std::fmt::Display) {
        eprintln!("history warning: {context}: {error}");
    }
}

impl<'a> TaskHistoryRecorder for DbTaskHistoryRecorder<'a> {
    fn start_task(&self, task_id: &TaskId, kind: &str, output_path: Option<&Path>) {
        let output = output_path.map(|p| p.to_string_lossy().into_owned());
        let started_at = now_utc_iso8601();
        if let Err(error) = self.task_repo().insert(NewTask {
            id: task_id.as_str(),
            kind,
            output_path: output.as_deref(),
            started_at: &started_at,
        }) {
            Self::warn("task insert", error);
        }
    }

    fn start_extract(&self, task_id: &TaskId, output_path: Option<&Path>) {
        self.start_task(task_id, "extract", output_path);
    }

    fn start_detect(&self, task_id: &TaskId, _path: &Path) {
        self.start_task(task_id, "detect", None);
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

    fn record_file_extraction(&self, task_id: &TaskId, row: FileExtractionRow<'_>) {
        let input = row.input_path.to_string_lossy().into_owned();
        let output = row.output_path.map(|p| p.to_string_lossy().into_owned());
        let created_at = now_utc_iso8601();
        if let Err(error) = self.file_repo().insert(NewFileExtraction {
            task_id: task_id.as_str(),
            input_path: &input,
            sample_hash: row.sample_hash,
            file_size: row.file_size,
            offset: row.offset,
            output_path: output.as_deref(),
            has_password: row.has_password,
            password_id: row.password_id,
            status: row.status,
            reason: row.reason,
            encoding: row.encoding,
            encoding_corrected: row.encoding_corrected,
            damaged_volumes_json: row.damaged_volumes_json,
            created_at: &created_at,
        }) {
            Self::warn("file_extraction insert", error);
        }
    }

    fn lookup_known_file(&self, sample_hash: &str, size: i64) -> Option<KnownFileHit> {
        match self.known_repo().find(sample_hash, size) {
            Ok(Some(known)) => Some(KnownFileHit {
                password_id: known.password_id,
                confirmed_encoding: known.confirmed_encoding,
                last_extract_at: known.last_extract_at,
            }),
            Ok(None) => None,
            Err(error) => {
                Self::warn("known_file lookup", error);
                None
            }
        }
    }

    fn upsert_known_file_extract(&self, upsert: KnownFileUpsert<'_>) {
        let name_offset = upsert.name.map(|name| NameOffset {
            name: name.to_string(),
            offset: upsert.offset,
        });
        let last_extract_at = now_utc_iso8601();
        if let Err(error) = self.known_repo().upsert_extract(
            upsert.sample_hash,
            upsert.size,
            name_offset,
            upsert.password_id,
            &last_extract_at,
        ) {
            Self::warn("known_file upsert", error);
        }
    }

    fn upsert_known_file_confirmed_encoding(&self, upsert: KnownFileEncodingUpsert<'_>) {
        let name_offset = upsert.name.map(|name| NameOffset {
            name: name.to_string(),
            offset: upsert.offset,
        });
        if let Err(error) = self.known_repo().upsert_confirmed_encoding(
            upsert.sample_hash,
            upsert.size,
            name_offset,
            upsert.encoding,
        ) {
            Self::warn("known_file confirmed encoding upsert", error);
        }
    }

    fn finish(&self, task_id: &TaskId, outcome: TaskOutcome<'_>) {
        let finished_at = now_utc_iso8601();
        let output = outcome
            .output_path
            .map(|p| p.to_string_lossy().into_owned());
        let finish = TaskFinish {
            status: Some(outcome.status.to_status()),
            finished_at: Some(&finished_at),
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
        TaskEventKind::Route(route) => (
            TaskEventLevel::Info,
            "Route".into(),
            format!("{route:?}"),
            serde_json::to_string(route).ok(),
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
        TaskEventKind::EmbeddedArchiveCarved { source, offset, .. } => (
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
            format!("{}: {file_size} bytes > {threshold}", path.display()),
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
        TaskEventKind::Failed { error } => {
            (TaskEventLevel::Error, "Failed".into(), error.clone(), None)
        }
        TaskEventKind::Completed => (
            TaskEventLevel::Info,
            "Completed".into(),
            "task completed".into(),
            None,
        ),
    }
}
