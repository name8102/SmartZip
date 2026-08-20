use crate::progress::{TaskEvent, TaskEventKind, TaskEventSink};
use crate::task::TaskId;
use crate::ArchiveFormat;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex};

/// Archive operation considered by capability routing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArchiveOperation {
    Probe,
    List,
    Test,
    Extract,
    Compress,
}

/// A configured support claim. Unknown remains routable but ranks below known support.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SupportState {
    Supported,
    Unsupported,
    Conditional { conditions: Vec<String> },
    Unknown,
}

/// A normalized, namespaced capability identifier such as `codec:zstd`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct CapabilityId(String);

impl CapabilityId {
    pub fn new(value: impl Into<String>) -> std::result::Result<Self, String> {
        let value = value.into().trim().to_ascii_lowercase();
        let valid = value.split_once(':').is_some_and(|(namespace, name)| {
            !namespace.is_empty()
                && !name.is_empty()
                && value.bytes().all(|byte| {
                    byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"-_.:".contains(&byte)
                })
        });
        if valid {
            Ok(Self(value))
        } else {
            Err(format!("capability identifier must be namespaced: {value}"))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for CapabilityId {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

impl fmt::Display for CapabilityId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Scope for one persisted capability claim. Empty operation/container means any.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityRule {
    pub capability: CapabilityId,
    /// Internal profile-layer rank. It is assigned during composition, not persisted.
    #[serde(skip)]
    pub precedence: u8,
    #[serde(default)]
    pub operations: Vec<ArchiveOperation>,
    #[serde(default)]
    pub containers: Vec<ArchiveFormat>,
    pub support: SupportState,
    #[serde(default)]
    pub evidence: Option<String>,
}

impl CapabilityRule {
    fn applies(&self, operation: ArchiveOperation, container: Option<&ArchiveFormat>) -> bool {
        (self.operations.is_empty() || self.operations.contains(&operation))
            && (self.containers.is_empty()
                || container.is_some_and(|format| self.containers.contains(format)))
    }

    fn specificity(&self) -> u8 {
        u8::from(!self.operations.is_empty()) + u8::from(!self.containers.is_empty())
    }
}

/// Persisted multidimensional claims for one adapter or profile layer.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackendCapabilityProfile {
    #[serde(default)]
    pub rules: Vec<CapabilityRule>,
}

impl BackendCapabilityProfile {
    pub fn validate(&self) -> std::result::Result<(), String> {
        for (index, left) in self.rules.iter().enumerate() {
            for right in &self.rules[index + 1..] {
                if left.capability == right.capability
                    && left.precedence == right.precedence
                    && left.specificity() == right.specificity()
                    && scopes_overlap(&left.operations, &right.operations)
                    && scopes_overlap(&left.containers, &right.containers)
                    && left.support != right.support
                {
                    return Err(format!(
                        "ambiguous rules for capability {} at equal specificity",
                        left.capability
                    ));
                }
            }
        }
        Ok(())
    }

    /// Compose family, version, and installation layers in increasing precedence.
    pub fn compose(
        family: &Self,
        version: Option<&Self>,
        installation: Option<&Self>,
    ) -> std::result::Result<Self, String> {
        family.validate()?;
        if let Some(profile) = version {
            profile.validate()?;
        }
        if let Some(profile) = installation {
            profile.validate()?;
        }
        let mut rules = with_precedence(&family.rules, 0);
        if let Some(profile) = version {
            rules.extend(with_precedence(&profile.rules, 1));
        }
        if let Some(profile) = installation {
            rules.extend(with_precedence(&profile.rules, 2));
        }
        Ok(Self { rules })
    }

    pub fn support(
        &self,
        capability: &CapabilityId,
        operation: ArchiveOperation,
        container: Option<&ArchiveFormat>,
    ) -> SupportState {
        self.rules
            .iter()
            .enumerate()
            .filter(|(_, rule)| {
                &rule.capability == capability && rule.applies(operation, container)
            })
            .max_by_key(|(index, rule)| (rule.precedence, rule.specificity(), *index))
            .map(|(_, rule)| rule.support.clone())
            .unwrap_or(SupportState::Unknown)
    }
}

fn scopes_overlap<T: PartialEq>(left: &[T], right: &[T]) -> bool {
    left.is_empty() || right.is_empty() || left.iter().any(|value| right.contains(value))
}

fn with_precedence(rules: &[CapabilityRule], precedence: u8) -> Vec<CapabilityRule> {
    rules
        .iter()
        .cloned()
        .map(|mut rule| {
            rule.precedence = precedence;
            rule
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchiveFact<T> {
    pub value: T,
    pub source: String,
}

/// Observed archive properties. Facts do not contain backend policy.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchiveFacts {
    pub container: Option<ArchiveFact<ArchiveFormat>>,
    #[serde(default)]
    pub codecs: Vec<ArchiveFact<CapabilityId>>,
    #[serde(default)]
    pub filters: Vec<ArchiveFact<CapabilityId>>,
    pub encrypted: Option<ArchiveFact<bool>>,
    pub solid: Option<ArchiveFact<bool>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequirementClass {
    Required,
    Preferred,
    Diagnostic,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchiveRequirement {
    pub capability: CapabilityId,
    pub class: RequirementClass,
    #[serde(default)]
    pub conditions: Vec<String>,
    pub reason: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchiveRequirements {
    #[serde(default)]
    pub items: Vec<ArchiveRequirement>,
}

impl ArchiveRequirements {
    pub fn from_facts(facts: &ArchiveFacts) -> Self {
        let mut items = Vec::new();
        for fact in facts.codecs.iter().chain(&facts.filters) {
            items.push(ArchiveRequirement {
                capability: fact.value.clone(),
                class: RequirementClass::Required,
                conditions: Vec::new(),
                reason: format!("observed by {}", fact.source),
            });
        }
        Self { items }
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
    pub requirements: ArchiveRequirements,
    pub candidates: Vec<RouteCandidate>,
    pub rejected: Vec<RejectedAdapter>,
    pub forced_adapter: Option<String>,
}

/// Definitive, task-local incompatibility key. This type is never persisted.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NegativeCapabilityKey {
    pub adapter_id: String,
    pub operation: ArchiveOperation,
    pub container: Option<ArchiveFormat>,
    pub capability: Option<CapabilityId>,
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
/// capability decisions isolated when one router serves concurrent tasks.
pub struct TaskExecutionContext {
    task_id: TaskId,
    sink: Arc<dyn TaskEventSink>,
    route: Mutex<TaskRouteContext>,
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
            route: Mutex::new(TaskRouteContext::default()),
            cancellation: tokio_util::sync::CancellationToken::new(),
        }
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

    fn rule(support: SupportState) -> CapabilityRule {
        CapabilityRule {
            capability: CapabilityId::new("codec:zstd").unwrap(),
            precedence: 0,
            operations: vec![ArchiveOperation::Extract],
            containers: vec![ArchiveFormat::SevenZip],
            support,
            evidence: None,
        }
    }

    #[test]
    fn installation_profile_overrides_version_and_family() {
        let family = BackendCapabilityProfile {
            rules: vec![rule(SupportState::Unknown)],
        };
        let version = BackendCapabilityProfile {
            rules: vec![rule(SupportState::Unsupported)],
        };
        let installation = BackendCapabilityProfile {
            rules: vec![rule(SupportState::Supported)],
        };
        let composed =
            BackendCapabilityProfile::compose(&family, Some(&version), Some(&installation))
                .unwrap();
        assert_eq!(
            composed.support(
                &CapabilityId::new("codec:zstd").unwrap(),
                ArchiveOperation::Extract,
                Some(&ArchiveFormat::SevenZip),
            ),
            SupportState::Supported
        );
    }

    #[test]
    fn profile_layer_precedence_beats_scope_specificity() {
        let family = BackendCapabilityProfile {
            rules: vec![rule(SupportState::Unsupported)],
        };
        let installation = BackendCapabilityProfile {
            rules: vec![CapabilityRule {
                capability: CapabilityId::new("codec:zstd").unwrap(),
                precedence: 0,
                operations: Vec::new(),
                containers: Vec::new(),
                support: SupportState::Supported,
                evidence: None,
            }],
        };
        let composed =
            BackendCapabilityProfile::compose(&family, None, Some(&installation)).unwrap();
        assert_eq!(
            composed.support(
                &CapabilityId::new("codec:zstd").unwrap(),
                ArchiveOperation::Extract,
                Some(&ArchiveFormat::SevenZip),
            ),
            SupportState::Supported
        );
    }

    #[test]
    fn equal_scope_conflicts_are_rejected() {
        let profile = BackendCapabilityProfile {
            rules: vec![
                rule(SupportState::Supported),
                rule(SupportState::Unsupported),
            ],
        };
        assert!(profile.validate().unwrap_err().contains("ambiguous"));
    }

    #[test]
    fn capability_ids_must_be_namespaced() {
        assert!(CapabilityId::new("zstd").is_err());
        assert!(serde_json::from_str::<CapabilityId>("\"zstd\"").is_err());
        assert_eq!(
            CapabilityId::new("Codec:ZSTD").unwrap().as_str(),
            "codec:zstd"
        );
    }
}
