//! Platform-level paths and utilities (Linux, macOS, Windows).

pub mod finder_service;
pub mod system_integration;

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
        Self::try_new().expect("unable to determine platform directories")
    }

    pub fn try_new() -> std::io::Result<Self> {
        let base = directories::BaseDirs::new().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "home unavailable; provide explicit config/database paths",
            )
        })?;
        #[cfg(target_os = "linux")]
        {
            let directory = |key: &str, fallback: &str| {
                std::env::var_os(key)
                    .map(PathBuf::from)
                    .filter(|p| p.is_absolute())
                    .unwrap_or_else(|| base.home_dir().join(fallback))
                    .join("smartzip")
            };
            return Ok(Self {
                config_dir: directory("XDG_CONFIG_HOME", ".config"),
                data_dir: directory("XDG_DATA_HOME", ".local/share"),
                cache_dir: directory("XDG_CACHE_HOME", ".cache"),
            });
        }
        #[cfg(target_os = "macos")]
        return Ok(Self {
            config_dir: base.home_dir().join("Library/Application Support/SmartZip"),
            data_dir: base.home_dir().join("Library/Application Support/SmartZip"),
            cache_dir: base.home_dir().join("Library/Caches/SmartZip"),
        });
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        Ok(Self {
            config_dir: base.config_dir().join("SmartZip"),
            data_dir: base.data_dir().join("SmartZip"),
            cache_dir: base.cache_dir().join("SmartZip"),
        })
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
