use serde::{Deserialize, Serialize};
use smartzip_core::{ArchiveFormat, EncodingMode};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchiveProbe {
    pub path: PathBuf,
    pub format: Option<ArchiveFormat>,
    pub encrypted: Option<bool>,
    pub supported: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListRequest {
    pub archive: PathBuf,
    pub format: Option<ArchiveFormat>,
    pub password: Option<String>,
    pub encoding: EncodingMode,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchiveEntry {
    #[serde(default)]
    pub native: Option<NativeEntry>,
    pub path: PathBuf,
    #[serde(default)]
    pub raw_name: Vec<u8>,
    #[serde(default)]
    pub compressed_size: Option<u64>,
    #[serde(default)]
    pub uncompressed_size: Option<u64>,
    pub is_dir: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchiveListing {
    pub format: Option<ArchiveFormat>,
    pub entries: Vec<ArchiveEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestRequest {
    pub archive: PathBuf,
    pub format: Option<ArchiveFormat>,
    pub password: Option<String>,
    pub encoding: EncodingMode,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestResult {
    pub ok: bool,
    pub encrypted: Option<bool>,
    #[serde(default)]
    pub diagnostics: crate::integrity::BackendTestDiagnostics,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtractArchiveRequest {
    #[serde(default)]
    pub selection: Option<Vec<EntryRef>>,
    pub archive: PathBuf,
    pub format: Option<ArchiveFormat>,
    pub output_dir: PathBuf,
    pub password: Option<String>,
    pub encoding: EncodingMode,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtractArchiveResult {
    #[serde(default)]
    pub manifest: Option<OutputManifest>,
    pub output_dir: PathBuf,
    /// Encryption evidence from the extraction's existing metadata/preflight.
    #[serde(default)]
    pub encrypted: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackendCommandOutput {
    pub status: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

/// Member identity survives name/encoding changes and is checked on reopen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntryRef {
    pub catalog_id: String,
    pub index: u32,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EntryKind {
    File,
    Directory,
    SymbolicLink,
    HardLink,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeEntry {
    pub reference: EntryRef,
    pub kind: EntryKind,
    pub encrypted: Option<bool>,
    pub method: Option<String>,
    pub crc: Option<u32>,
    pub block: Option<u64>,
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputManifest {
    pub files: Vec<OutputFile>,
    pub entries: u64,
    pub bytes: u64,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputFile {
    pub relative_path: PathBuf,
    pub size: u64,
}
