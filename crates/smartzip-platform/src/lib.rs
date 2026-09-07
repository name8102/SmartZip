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

    pub fn legacy() -> std::io::Result<Self> {
        let project = directories::ProjectDirs::from("", "", "SmartZip").ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "unable to determine legacy directories",
            )
        })?;
        Ok(Self {
            config_dir: project.config_dir().join("smartzip"),
            data_dir: project.data_dir().join("smartzip"),
            cache_dir: project.cache_dir().join("smartzip"),
        })
    }

    pub fn select_database(&self, legacy: &Self) -> std::io::Result<(PathBuf, Option<String>)> {
        let path = self.db_path();
        let old = legacy.db_path();
        if path != old && old.try_exists()? {
            if path.try_exists()? {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    format!(
                        "both {} and {} exist; select with --db",
                        path.display(),
                        old.display()
                    ),
                ));
            }
            return Ok((
                old.clone(),
                Some(format!("using legacy database: {}", old.display())),
            ));
        }
        Ok((path, None))
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
    fn database_selection_never_hides_an_existing_legacy_store() {
        let root = tempfile::tempdir().unwrap();
        let paths = |name: &str| PlatformPaths {
            config_dir: root.path().join(name),
            data_dir: root.path().join(name),
            cache_dir: root.path().join(name),
        };
        let current = paths("current");
        let old = paths("legacy");
        std::fs::create_dir_all(&old.data_dir).unwrap();
        std::fs::write(old.db_path(), b"existing database").unwrap();
        assert_eq!(current.select_database(&old).unwrap().0, old.db_path());
        assert!(!current.data_dir.exists());
        std::fs::create_dir_all(&current.data_dir).unwrap();
        std::fs::write(current.db_path(), b"another database").unwrap();
        assert!(current.select_database(&old).is_err());
    }
}
