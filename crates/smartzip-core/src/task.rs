use serde::{Deserialize, Serialize};
use std::fmt;

/// Stable task identifier used by GUI, CLI, logs, and database rows.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct TaskId(String);

impl TaskId {
    pub fn new() -> Self {
        Self(format!("task-{}", uuid::Uuid::new_v4()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn from_stored(value: String) -> Self {
        Self(value)
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

macro_rules! random_id {
    ($name:ident, $prefix:literal) => {
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        pub struct $name(String);

        impl $name {
            pub fn new() -> Self {
                Self(format!(concat!($prefix, "-{}"), uuid::Uuid::new_v4()))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }

            pub fn from_stored(value: String) -> Self {
                Self(value)
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

random_id!(NodeId, "node");
random_id!(AttemptId, "attempt");
random_id!(DecisionId, "decision");

/// Encoding policy used for archive entry names.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum EncodingMode {
    #[default]
    Auto,
    Override(String),
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum CompressionLevel {
    Fast,
    #[default]
    Balanced,
    Best,
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
