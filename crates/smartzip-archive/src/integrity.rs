//! Evidence shared by archive readers, the test workflow, and its consumers.
//!
//! A suspect group is a disjunction, never a list of independently bad volumes.
use crate::volumes::VolumeSet;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Integrity {
    Intact,
    Corrupt,
    Incomplete,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Coverage {
    Complete,
    Partial,
    #[default]
    None,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Localization {
    Exact,
    Partial,
    #[default]
    Unknown,
    NotApplicable,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PasswordStatus {
    NotNeeded,
    Verified,
    Required,
    Rejected,
    #[default]
    Indeterminate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TestFailure {
    Corruption,
    MissingVolume,
    PasswordRequired,
    PasswordRejected,
    PasswordIndeterminate,
    Io,
    Unknown,
    Cancelled,
}

/// Raw external output is bounded, untrusted diagnostic text. It cannot, by
/// itself, promote a physical volume to confirmed damage.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackendTestDiagnostics {
    pub adapter_id: String,
    pub family: String,
    pub version: Option<String>,
    pub exit_code: Option<i32>,
    pub failure: Option<TestFailure>,
    pub coverage: Coverage,
    pub damaged_files: Vec<String>,
    pub stdout: String,
    pub stderr: String,
    pub output_truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhysicalRange {
    pub volume: PathBuf,
    pub offset: u64,
    pub length: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
    HeaderChecksum,
    PackedChecksum,
    DataChecksum,
    MetadataConflict,
    StructuralTruncation,
    BackendVolumeChecksum,
    EntryChecksum,
    DecodeError,
    MissingReference,
    ReadError,
    AmbiguousSequence,
    InputChanged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceStrength {
    Confirmed,
    Suspect,
    Observation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MetadataTrust {
    ChecksumVerified,
    Structural,
    Unverified,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestEvidence {
    pub id: String,
    pub kind: EvidenceKind,
    pub strength: EvidenceStrength,
    pub source: String,
    pub pass_id: u32,
    pub ranges: Vec<PhysicalRange>,
    pub reference_ranges: Vec<PhysicalRange>,
    pub metadata_trust: MetadataTrust,
    pub affected_entries: Vec<String>,
    pub summary: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfirmedVolume {
    pub path: PathBuf,
    pub ranges: Vec<PhysicalRange>,
    pub evidence_ids: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SuspectRelation {
    OneOrMore,
    Possible,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SuspectGroup {
    pub members: Vec<PathBuf>,
    pub relation: SuspectRelation,
    pub evidence_ids: Vec<String>,
    pub affected_entries: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckedScope {
    pub source: String,
    pub pass_id: u32,
    pub description: String,
    pub ranges: Vec<PhysicalRange>,
    pub passed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestPass {
    pub pass_id: u32,
    pub purpose: String,
    pub ok: bool,
    pub diagnostics: BackendTestDiagnostics,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestArchiveReport {
    pub schema_version: u32,
    pub input_paths: Vec<PathBuf>,
    pub entrypoint: PathBuf,
    pub volumes: VolumeSet,
    pub integrity: Integrity,
    pub coverage: Coverage,
    pub localization: Localization,
    pub password_status: PasswordStatus,
    pub confirmed_volumes: Vec<ConfirmedVolume>,
    pub suspect_groups: Vec<SuspectGroup>,
    pub missing_volumes: Vec<PathBuf>,
    pub unreadable_volumes: Vec<PathBuf>,
    pub unchecked_volumes: Vec<PathBuf>,
    pub checked_scopes: Vec<CheckedScope>,
    pub damaged_files: Vec<String>,
    pub evidence: Vec<TestEvidence>,
    pub stop_reasons: Vec<String>,
    pub passes: Vec<TestPass>,
}

impl TestArchiveReport {
    pub fn new(volumes: VolumeSet, input: PathBuf) -> Self {
        Self {
            schema_version: 1,
            input_paths: vec![input],
            entrypoint: volumes.entrypoint.clone(),
            missing_volumes: volumes.missing.clone(),
            unreadable_volumes: volumes.unreadable.clone(),
            unchecked_volumes: volumes.members.iter().map(|m| m.path.clone()).collect(),
            stop_reasons: volumes.issues.clone(),
            volumes,
            integrity: Integrity::Unknown,
            coverage: Coverage::None,
            localization: Localization::Unknown,
            password_status: PasswordStatus::Indeterminate,
            confirmed_volumes: Vec::new(),
            suspect_groups: Vec::new(),
            checked_scopes: Vec::new(),
            damaged_files: Vec::new(),
            evidence: Vec::new(),
            passes: Vec::new(),
        }
    }
}
