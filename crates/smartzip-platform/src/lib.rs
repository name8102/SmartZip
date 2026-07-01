//! Platform-level paths and utilities (Linux, macOS, Windows).

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Move a file or directory to the operating system's recycle bin/trash.
///
/// This intentionally does not fall back to permanent deletion. Callers can
/// treat an error as a non-fatal cleanup failure while preserving the source.
pub fn move_to_trash(path: impl AsRef<Path>) -> std::io::Result<()> {
    trash::delete(path.as_ref()).map_err(std::io::Error::other)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlatformPaths {
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
    pub cache_dir: PathBuf,
}

impl PlatformPaths {
    pub fn new() -> Self {
        let project = directories::ProjectDirs::from("", "", "SmartZip")
            .expect("unable to determine platform directories");
        Self {
            config_dir: project.config_dir().join("smartzip"),
            data_dir: project.data_dir().join("smartzip"),
            cache_dir: project.cache_dir().join("smartzip"),
        }
    }

    pub fn ensure_dirs(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.config_dir)?;
        std::fs::create_dir_all(&self.data_dir)?;
        std::fs::create_dir_all(&self.cache_dir)?;
        Ok(())
    }

    pub fn db_path(&self) -> PathBuf {
        self.data_dir.join("smartzip.db")
    }

    pub fn config_path(&self) -> PathBuf {
        self.config_dir.join("config.toml")
    }

    pub fn password_export_path(&self) -> PathBuf {
        self.data_dir.join("passwords.txt")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Desktop {
    Linux,
    MacOs,
    Windows,
}

pub fn desktop() -> Desktop {
    if cfg!(target_os = "linux") {
        Desktop::Linux
    } else if cfg!(target_os = "macos") {
        Desktop::MacOs
    } else {
        Desktop::Windows
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_paths_exist_or_can_be_created() {
        let paths = PlatformPaths::new();
        paths.ensure_dirs().unwrap();
    }
}
