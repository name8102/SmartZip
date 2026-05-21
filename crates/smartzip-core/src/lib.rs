//! Core domain types for SmartZip.

pub mod error;
pub mod progress;
pub mod task;

pub use error::{Result, SmartZipError};
pub use progress::{
    EncodingCandidate, EncodingDetectionResult, TaskEvent, TaskEventKind, TaskProgress,
};
pub use task::{ArchiveFormat, CompressionLevel, EncodingMode, TaskId, TaskKind};
