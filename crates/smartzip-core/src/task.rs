use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static NEXT_TASK_SEQUENCE: AtomicU64 = AtomicU64::new(1);

/// Stable task identifier used by GUI, CLI, logs, and database rows.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TaskId(String);

impl TaskId {
    pub fn new() -> Self {
        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_millis())
            .unwrap_or_default();
        let sequence = NEXT_TASK_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        Self(format!("task-{millis}-{sequence}"))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for TaskId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for TaskId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Top-level operation types SmartZip can execute.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskKind {
    Extract,
    Compress,
    Detect,
    Open,
}

/// Encoding policy used for archive entry names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum EncodingMode {
    Auto,
    Override(String),
}

impl Default for EncodingMode {
    fn default() -> Self {
        Self::Auto
    }
}

/// Archive formats SmartZip recognizes at the domain layer.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ArchiveFormat {
    Zip,
    SevenZip,
    Rar,
    Tar,
    Gzip,
    Bzip2,
    Xz,
    Cab,
    Iso,
    Dmg,
    Zstd,
    Lz4,
    Lzma,
    Unknown(String),
}

impl ArchiveFormat {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Zip => "zip",
            Self::SevenZip => "7z",
            Self::Rar => "rar",
            Self::Tar => "tar",
            Self::Gzip => "gzip",
            Self::Bzip2 => "bzip2",
            Self::Xz => "xz",
            Self::Cab => "cab",
            Self::Iso => "iso",
            Self::Dmg => "dmg",
            Self::Zstd => "zstd",
            Self::Lz4 => "lz4",
            Self::Lzma => "lzma",
            Self::Unknown(value) => value.as_str(),
        }
    }
}

/// User-facing compression level preset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CompressionLevel {
    Fast,
    Balanced,
    Best,
}

impl Default for CompressionLevel {
    fn default() -> Self {
        Self::Balanced
    }
}

/// Request to extract one or more archives.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtractRequest {
    pub inputs: Vec<PathBuf>,
    pub output_dir: Option<PathBuf>,
    pub encoding: EncodingMode,
    pub scan_embedded: bool,
    pub delete_source_on_success: bool,
    pub recursion_limit: u8,
}

/// Request to compress files or directories into one or more archives.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompressRequest {
    pub inputs: Vec<PathBuf>,
    pub output: Option<PathBuf>,
    pub format: ArchiveFormat,
    pub level: CompressionLevel,
    pub password: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_task_ids_are_unique() {
        let first = TaskId::new();
        let second = TaskId::new();
        assert_ne!(first, second);
        assert!(first.as_str().starts_with("task-"));
    }

    #[test]
    fn archive_format_exposes_stable_names() {
        assert_eq!(ArchiveFormat::SevenZip.as_str(), "7z");
        assert_eq!(ArchiveFormat::Unknown("apk".into()).as_str(), "apk");
    }
}
