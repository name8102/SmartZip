use serde::{Deserialize, Serialize};
use smartzip_core::{ArchiveFormat, CompressionLevel, EncodingMode};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackendCapabilities {
    pub can_extract: Vec<ArchiveFormat>,
    pub can_compress: Vec<ArchiveFormat>,
    pub supports_passwords: bool,
    pub supports_listing: bool,
    pub supports_test: bool,
}

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestResult {
    pub ok: bool,
    pub encrypted: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtractArchiveRequest {
    pub archive: PathBuf,
    pub format: Option<ArchiveFormat>,
    pub output_dir: PathBuf,
    pub password: Option<String>,
    pub encoding: EncodingMode,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtractArchiveResult {
    pub output_dir: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompressArchiveRequest {
    pub inputs: Vec<PathBuf>,
    pub output: PathBuf,
    pub format: ArchiveFormat,
    pub level: CompressionLevel,
    pub password: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompressArchiveResult {
    pub output: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackendCommandOutput {
    pub status: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtractionLimits {
    pub max_entries: usize,
    pub max_single_entry_bytes: u64,
    pub max_total_output_bytes: u64,
    pub max_compression_ratio: u32,
}

impl Default for ExtractionLimits {
    fn default() -> Self {
        Self {
            max_entries: 100_000,
            max_single_entry_bytes: 10 * 1024 * 1024 * 1024,
            max_total_output_bytes: 100 * 1024 * 1024 * 1024,
            max_compression_ratio: 100,
        }
    }
}
