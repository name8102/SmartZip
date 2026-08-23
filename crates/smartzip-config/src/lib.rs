//! Application routing configuration via TOML.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AdapterFamily {
    SevenZipCli,
    UnrarCli,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackendInstallation {
    pub id: String,
    pub family: AdapterFamily,
    pub executable: PathBuf,
    #[serde(default)]
    pub declared_version: Option<String>,
    #[serde(default = "enabled_by_default")]
    pub enabled: bool,
    #[serde(default)]
    pub priority: i32,
}

fn enabled_by_default() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackendConfig {
    #[serde(default = "enabled_by_default")]
    pub auto_discover: bool,
    #[serde(default)]
    pub installations: Vec<BackendInstallation>,
}

impl Default for BackendConfig {
    fn default() -> Self {
        Self {
            auto_discover: true,
            installations: Vec::new(),
        }
    }
}

impl BackendConfig {
    pub fn validate(&self) -> std::result::Result<(), String> {
        let mut ids = std::collections::HashSet::new();
        for installation in &self.installations {
            if installation.id.trim().is_empty() {
                return Err("backend installation ID cannot be empty".into());
            }
            if !ids.insert(&installation.id) {
                return Err(format!(
                    "duplicate backend installation ID: {}",
                    installation.id
                ));
            }
        }
        Ok(())
    }
}

/// The CLI currently consumes only backend routing configuration. Keep the
/// wrapper so config files have one stable root without carrying dead defaults.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SmartZipConfig {
    #[serde(default)]
    pub backends: BackendConfig,
}

impl Default for SmartZipConfig {
    fn default() -> Self {
        Self {
            backends: BackendConfig::default(),
        }
    }
}

impl SmartZipConfig {
    pub fn load(path: impl AsRef<Path>) -> std::io::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        toml::from_str(&content)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_backend_configuration() {
        let path =
            std::env::temp_dir().join(format!("smartzip-config-{}.toml", std::process::id()));
        std::fs::write(
            &path,
            "[backends]\nauto_discover = false\n[[backends.installations]]\nid = 'local-7z'\nfamily = 'seven-zip-cli'\nexecutable = '/opt/bin/7z'\npriority = 10\n",
        )
        .unwrap();
        let loaded = SmartZipConfig::load(&path).unwrap();
        assert!(!loaded.backends.auto_discover);
        assert_eq!(loaded.backends.installations[0].id, "local-7z");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn duplicate_backend_ids_are_rejected() {
        let installation = BackendInstallation {
            id: "duplicate".into(),
            family: AdapterFamily::SevenZipCli,
            executable: PathBuf::from("7z"),
            declared_version: None,
            enabled: true,
            priority: 0,
        };
        let config = BackendConfig {
            installations: vec![installation.clone(), installation],
            ..BackendConfig::default()
        };
        assert!(config.validate().unwrap_err().contains("duplicate"));
    }
}
