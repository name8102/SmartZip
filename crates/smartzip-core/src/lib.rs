//! Core domain types for SmartZip.

pub mod embedded;
pub mod error;
pub mod progress;
pub mod routing;
pub mod task;

pub use embedded::{
    BusinessContainerKind, DetectionAction, DetectionDecision, DetectionKind, EmbeddedScanMode,
    EmbeddedScanPolicy, FindingSummary, DEFAULT_MIN_EMBEDDED_FINDING_SIZE,
};
pub use error::{Result, SmartZipError};
pub use progress::{
    EncodingCandidate, EncodingDetectionResult, TaskEvent, TaskEventKind, TaskEventSink,
    TaskProgress,
};
pub use routing::{
    AdapterCapabilities, ArchiveFacts, ArchiveOperation, ArchiveRequirements,
    NegativeCapabilityKey, RejectedAdapter, RouteCandidate, RouteEvent, RoutePlan,
    TaskExecutionContext, TaskRouteContext,
};
pub use task::{ArchiveFormat, CompressionLevel, EncodingMode, TaskId};
