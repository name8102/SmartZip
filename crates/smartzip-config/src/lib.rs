//! Application routing configuration via TOML.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AdapterFamily {
    SevenZipCli,
    UnrarCli,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
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
            if installation.executable.as_os_str().is_empty()
                || installation.executable.to_string_lossy().contains('\0')
            {
                return Err("backend executable must be a nonempty valid path".into());
            }
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ExtractionLimits {
    pub max_files: u64,
    pub max_output_bytes: u64,
    pub min_free_bytes: u64,
    pub max_nested_candidates: usize,
}
impl Default for ExtractionLimits {
    fn default() -> Self {
        Self {
            max_files: 100_000,
            max_output_bytes: 20 * 1024 * 1024 * 1024,
            min_free_bytes: 512 * 1024 * 1024,
            max_nested_candidates: 10_000,
        }
    }
}

mod model;
mod resolve;
mod store;
pub use model::*;
pub use resolve::*;
pub use store::*;
