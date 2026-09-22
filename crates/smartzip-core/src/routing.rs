use crate::progress::{TaskEvent, TaskEventKind, TaskEventSink};
use crate::task::TaskId;
use crate::ArchiveFormat;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Archive operation considered by backend routing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArchiveOperation {
    Probe,
    List,
    Test,
    Extract,
    Compress,
}

/// Concrete adapter metadata used by the router. This is deliberately limited
/// to decisions made by current production callers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdapterCapabilities {
    pub operations: Vec<ArchiveOperation>,
    pub read_containers: Vec<ArchiveFormat>,
    pub compress_containers: Vec<ArchiveFormat>,
    pub supports_passwords: bool,
    pub supports_charset_override: bool,
}

impl AdapterCapabilities {
    pub fn supports(&self, operation: ArchiveOperation, container: Option<&ArchiveFormat>) -> bool {
        let containers = match operation {
            ArchiveOperation::Compress => &self.compress_containers,
            _ => &self.read_containers,
        };
        self.operations.contains(&operation)
            && container.is_none_or(|container| containers.contains(container))
    }
}

/// Archive facts consumed by routing. Codec observations are concrete strings
/// because they are only needed to scope task-local unsupported-codec cache hits.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchiveFacts {
    pub container: Option<ArchiveFormat>,
    #[serde(default)]
    pub codecs: Vec<String>,
}

/// Request requirements used by current list/test/extract callers.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchiveRequirements {
    pub password: bool,
    pub charset_override: bool,
    #[serde(default)]
    pub codecs: Vec<String>,
}

impl ArchiveRequirements {
    pub fn from_facts(facts: &ArchiveFacts) -> Self {
        Self {
            codecs: facts.codecs.clone(),
            ..Self::default()
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouteCandidate {
    pub adapter_id: String,
    pub priority: i32,
    #[serde(default)]
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RejectedAdapter {
    pub adapter_id: String,
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoutePlan {
    pub operation: ArchiveOperation,
    pub container: Option<ArchiveFormat>,
    pub requirements: ArchiveRequirements,
    pub candidates: Vec<RouteCandidate>,
    pub rejected: Vec<RejectedAdapter>,
    pub forced_adapter: Option<String>,
}

/// Definitive task-local incompatibility. It is never persisted.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NegativeCapabilityKey {
    pub adapter_id: String,
    pub operation: ArchiveOperation,
    pub container: Option<ArchiveFormat>,
    pub codec: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct TaskRouteContext {
    unsupported: HashMap<NegativeCapabilityKey, String>,
}

impl TaskRouteContext {
    pub fn record(&mut self, key: NegativeCapabilityKey, reason: impl Into<String>) {
        self.unsupported.insert(key, reason.into());
    }

    pub fn rejection(&self, key: &NegativeCapabilityKey) -> Option<&str> {
        self.unsupported.get(key).map(String::as_str)
    }
}

/// Per-workflow routing context. It keeps route observations and negative
/// decisions isolated when one router serves concurrent tasks.
pub struct TaskExecutionContext {
    task_id: TaskId,
    sink: Arc<dyn TaskEventSink>,
    route: Arc<Mutex<TaskRouteContext>>,
    cancellation: tokio_util::sync::CancellationToken,
}

struct DiscardingTaskEventSink;

impl TaskEventSink for DiscardingTaskEventSink {
    fn push(&self, _event: TaskEvent) {}
}

impl TaskExecutionContext {
    pub fn detached() -> Self {
        Self::new(TaskId::new(), Arc::new(DiscardingTaskEventSink))
    }

    pub fn new(task_id: TaskId, sink: Arc<dyn TaskEventSink>) -> Self {
        Self {
            task_id,
            sink,
            route: Arc::new(Mutex::new(TaskRouteContext::default())),
            cancellation: tokio_util::sync::CancellationToken::new(),
        }
    }

    pub fn with_cancellation(mut self, cancellation: tokio_util::sync::CancellationToken) -> Self {
        self.cancellation = cancellation;
        self
    }

    /// Share this task's negative capability cache while routing a root's events
    /// and cancellation independently. Separate tasks still have separate caches.
    pub fn scoped(
        &self,
        sink: Arc<dyn TaskEventSink>,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> Self {
        Self {
            task_id: self.task_id.clone(),
            route: self.route.clone(),
            sink,
            cancellation,
        }
    }

    pub fn emit_progress(&self, progress: crate::TaskProgress) {
        self.sink.push(TaskEvent {
            task_id: self.task_id.clone(),
            kind: TaskEventKind::Progress(progress),
        });
    }

    pub fn cancellation_token(&self) -> tokio_util::sync::CancellationToken {
        self.cancellation.clone()
    }

    pub fn cancel(&self) {
        self.cancellation.cancel();
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancellation.is_cancelled()
    }

    pub fn record_rejection(&self, key: NegativeCapabilityKey, reason: impl Into<String>) {
        self.route
            .lock()
            .expect("task route context lock poisoned")
            .record(key, reason);
    }

    pub fn rejection(&self, key: &NegativeCapabilityKey) -> Option<String> {
        self.route
            .lock()
            .expect("task route context lock poisoned")
            .rejection(key)
            .map(str::to_owned)
    }

    pub fn emit_route(&self, event: RouteEvent) {
        self.sink.push(TaskEvent {
            task_id: self.task_id.clone(),
            kind: TaskEventKind::Route(event),
        });
    }
}

/// Structured routing diagnostics. Payloads intentionally contain no command arguments.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RouteEvent {
    RoutePlanned { plan: RoutePlan },
    BackendAttemptStarted { adapter_id: String },
    BackendAttemptFailed { adapter_id: String, class: String },
    BackendAttemptCleaned { adapter_id: String },
    BackendSelected { adapter_id: String },
    RouteExhausted { attempted: Vec<String> },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scoped_context_shares_cache_but_keeps_root_cancellation_independent() {
        let task = TaskExecutionContext::detached();
        let first = task.scoped(
            Arc::new(DiscardingTaskEventSink),
            task.cancellation_token().child_token(),
        );
        let second = task.scoped(
            Arc::new(DiscardingTaskEventSink),
            task.cancellation_token().child_token(),
        );
        let key = NegativeCapabilityKey {
            adapter_id: "backend".into(),
            operation: ArchiveOperation::Extract,
            container: None,
            codec: None,
        };
        first.record_rejection(key.clone(), "unsupported codec");
        assert_eq!(second.rejection(&key).as_deref(), Some("unsupported codec"));
        assert!(TaskExecutionContext::detached().rejection(&key).is_none());
        first.cancel();
        assert!(!second.is_cancelled());
        assert!(!task.is_cancelled());
        task.cancel();
        assert!(second.is_cancelled());
    }
}
