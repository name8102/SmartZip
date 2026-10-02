//! Versioned, serializable naming policy and complete extraction mapping.
use serde::{Deserialize, Serialize};

pub const PATH_MAPPING_VERSION: u32 = 1;

/// Relative archive syntax that explicitly declares the extraction root.
/// Empty, absolute, parent-traversal and ordinary names never qualify.
pub fn is_root_directory_alias(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.contains('\\')
        && path
            .split('/')
            .all(|component| component.is_empty() || component == ".")
        && path.split('/').any(|component| component == ".")
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PathMode {
    #[default]
    Native,
    Portable,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LengthMetric {
    Utf8Bytes,
    Utf16Units,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyConfidence {
    Known,
    Conservative,
    Unknown,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComponentBudget {
    pub limit: usize,
    pub metric: LengthMetric,
    pub source: String,
    pub confidence: PolicyConfidence,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TargetIdentity {
    pub canonical_root: String,
    pub volume_id: String,
    pub root_id: String,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PathAccessStrategy {
    PosixDirRelative,
    WindowsVerbatim,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NameComparison {
    pub case_sensitive: bool,
    pub normalization_sensitive: bool,
    pub confidence: PolicyConfidence,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TargetPathPolicy {
    pub version: u32,
    pub mode: PathMode,
    pub target: TargetIdentity,
    pub fs_kind: String,
    pub component: ComponentBudget,
    pub access: PathAccessStrategy,
    pub comparison: NameComparison,
    pub windows_names: bool,
    /// The complete currently supported API chain, not just the controlled writer.
    pub full_path_limit: Option<usize>,
}
impl TargetPathPolicy {
    pub fn measure(&self, name: &str) -> usize {
        match self.component.metric {
            LengthMetric::Utf8Bytes => name.len(),
            LengthMetric::Utf16Units => name.encode_utf16().count(),
        }
    }
    pub fn fits_component(&self, name: &str) -> bool {
        self.measure(name) <= self.component.limit
            && (self.mode != PathMode::Portable || name.len() <= 255)
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceNameKind {
    ZipCentralDirectory,
    ZipUnicodeExtra,
    Decoded,
    BackendText,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PathMappingReason {
    InvalidCharacters,
    TrailingCharacters,
    ReservedName,
    NameTooLong,
    NameCollision,
    ExtensionShortened,
    ChangedAncestor,
    CreationFallback,
    LayoutRenamed,
    FullPathShortened,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PathMappingEntry {
    pub id: u64,
    pub source: String,
    pub display_name: String,
    pub raw_name: Option<Vec<u8>>,
    pub source_kind: SourceNameKind,
    pub staging_relative: String,
    pub final_relative: String,
    pub is_dir: bool,
    pub reasons: Vec<PathMappingReason>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PathMappingReport {
    pub version: u32,
    pub digest: String,
    pub policy: TargetPathPolicy,
    pub entries: Vec<PathMappingEntry>,
    pub tentative: bool,
    #[serde(default)]
    pub manifest_digest: String,
    #[serde(default)]
    pub archive_identity: String,
    #[serde(default)]
    pub adapter_id: String,
    #[serde(default)]
    pub archive_path: String,
    #[serde(default)]
    pub node_id: Option<String>,
    #[serde(default)]
    pub generation: Option<u64>,
}
impl PathMappingReport {
    pub fn changed_count(&self) -> usize {
        self.entries
            .iter()
            .filter(|e| !e.reasons.is_empty())
            .count()
    }
    pub fn refresh_digest(&mut self) {
        struct DigestWriter(blake3::Hasher);
        impl std::io::Write for DigestWriter {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.update(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut writer = DigestWriter(blake3::Hasher::new());
        serde_json::to_writer(
            &mut writer,
            &(
                self.version,
                &self.policy,
                &self.entries,
                &self.manifest_digest,
                &self.archive_identity,
                &self.adapter_id,
                &self.archive_path,
                &self.node_id,
                self.generation,
            ),
        )
        .expect("mapping data is serializable");
        self.digest = writer.0.finalize().to_hex().to_string();
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PathConstraintReason {
    NameTooLong,
    PathTooLong,
    InvalidName,
    NameCollision,
    PathRemapUnsupported,
    PathConstraintUnknown,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PathStage {
    Preflight,
    Create,
    Layout,
    Commit,
    Cleanup,
}
impl PathConstraintReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NameTooLong => "name_too_long",
            Self::PathTooLong => "path_too_long",
            Self::InvalidName => "invalid_name",
            Self::NameCollision => "name_collision",
            Self::PathRemapUnsupported => "path_remap_unsupported",
            Self::PathConstraintUnknown => "path_constraint_unknown",
        }
    }
}
impl PathStage {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Preflight => "preflight",
            Self::Create => "create",
            Self::Layout => "layout",
            Self::Commit => "commit",
            Self::Cleanup => "cleanup",
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PathDiagnostic {
    pub reason: PathConstraintReason,
    pub stage: PathStage,
    pub scope: String,
    pub entry_id: Option<u64>,
    pub component_index: Option<usize>,
    pub display_name: Option<String>,
    pub raw_name: Option<Vec<u8>>,
    pub source_kind: Option<SourceNameKind>,
    pub policy: Option<TargetPathPolicy>,
    pub measured: Option<usize>,
    pub limit: Option<usize>,
    pub metric: Option<LengthMetric>,
    pub candidate: Option<String>,
    pub actual_mapping: Option<String>,
    pub os_error_code: Option<i32>,
    pub detail: String,
}
impl PathDiagnostic {
    pub fn new(reason: PathConstraintReason, stage: PathStage, detail: impl Into<String>) -> Self {
        Self {
            reason,
            stage,
            scope: "component".into(),
            entry_id: None,
            component_index: None,
            display_name: None,
            raw_name: None,
            source_kind: None,
            policy: None,
            measured: None,
            limit: None,
            metric: None,
            candidate: None,
            actual_mapping: None,
            os_error_code: None,
            detail: detail.into(),
        }
    }
    pub fn error(self) -> crate::SmartZipError {
        crate::SmartZipError::PathConstraint {
            diagnostic: Box::new(self),
            source: None,
        }
    }
    pub fn io_error(mut self, source: std::io::Error) -> crate::SmartZipError {
        self.os_error_code = source.raw_os_error();
        crate::SmartZipError::PathConstraint {
            diagnostic: Box::new(self),
            source: Some(source),
        }
    }
}
