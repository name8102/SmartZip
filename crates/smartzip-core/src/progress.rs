use crate::error::SmartZipError;
use crate::task::{ArchiveFormat, EncodingMode, TaskId};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Progress value for an operation. `percent == None` means indeterminate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskProgress {
    pub percent: Option<f32>,
    pub message: String,
}

impl TaskProgress {
    pub fn indeterminate(message: impl Into<String>) -> Self {
        Self {
            percent: None,
            message: message.into(),
        }
    }

    pub fn percent(percent: f32, message: impl Into<String>) -> Self {
        Self {
            percent: Some(percent.clamp(0.0, 100.0)),
            message: message.into(),
        }
    }
}

/// Encoding detection summary surfaced to GUI/CLI.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EncodingDetectionResult {
    pub selected: EncodingMode,
    pub confidence: f32,
    pub candidates: Vec<EncodingCandidate>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EncodingCandidate {
    pub name: String,
    pub confidence: f32,
}

/// Core task event payload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum TaskEventKind {
    Started,
    Progress(TaskProgress),
    PasswordTried {
        candidate_id: Option<i64>,
    },
    EncodingDetected(EncodingDetectionResult),
    EmbeddedArchiveFound {
        offset: u64,
        size: Option<u64>,
        format: ArchiveFormat,
        confidence: f32,
        description: String,
    },
    OutputCreated {
        path: PathBuf,
    },
    Warning {
        message: String,
    },
    Failed {
        error: String,
    },
    Completed,
}

/// Event emitted by core operations. Errors are stringified to keep this type
/// serializable for database persistence and UI transport.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskEvent {
    pub task_id: TaskId,
    pub kind: TaskEventKind,
}

impl TaskEvent {
    pub fn started(task_id: TaskId) -> Self {
        Self {
            task_id,
            kind: TaskEventKind::Started,
        }
    }

    pub fn failed(task_id: TaskId, error: &SmartZipError) -> Self {
        Self {
            task_id,
            kind: TaskEventKind::Failed {
                error: error.to_string(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_percent_is_clamped() {
        assert_eq!(TaskProgress::percent(120.0, "done").percent, Some(100.0));
        assert_eq!(TaskProgress::percent(-1.0, "start").percent, Some(0.0));
    }
}
