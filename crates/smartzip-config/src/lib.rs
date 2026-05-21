//! Application configuration via TOML.

use serde::{Deserialize, Serialize};
use smartzip_core::{ArchiveFormat, CompressionLevel};
use smartzip_scanner::ScannerConfig;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SmartZipConfig {
    pub default_format: ArchiveFormat,
    pub default_level: CompressionLevel,
    pub scanner: ScannerConfig,
    pub delete_source_on_success: bool,
    pub delete_source_to_trash: bool,
    pub log_level: LogLevel,
    pub gui: GuiConfig,
}

impl Default for SmartZipConfig {
    fn default() -> Self {
        Self {
            default_format: ArchiveFormat::Zip,
            default_level: CompressionLevel::Balanced,
            scanner: ScannerConfig::default(),
            delete_source_on_success: false,
            delete_source_to_trash: true,
            log_level: LogLevel::Info,
            gui: GuiConfig::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GuiConfig {
    pub dark_mode: Option<bool>,
    pub locale: String,
    pub show_password_hint: bool,
}

impl Default for GuiConfig {
    fn default() -> Self {
        Self {
            dark_mode: None,
            locale: "zh_CN".into(),
            show_password_hint: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LogLevel {
    Off,
    Error,
    Info,
    Debug,
}

impl SmartZipConfig {
    pub fn load(path: impl AsRef<Path>) -> std::io::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        toml::from_str(&content)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
    }

    pub fn save(&self, path: impl AsRef<Path>) -> std::io::Result<()> {
        let content = toml::to_string_pretty(self)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
        if let Some(parent) = path.as_ref().parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, content)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_config() {
        let config = SmartZipConfig::default();
        let path =
            std::env::temp_dir().join(format!("smartzip-config-{}.toml", std::process::id()));
        config.save(&path).unwrap();
        let loaded = SmartZipConfig::load(&path).unwrap();
        assert_eq!(config, loaded);
        let _ = std::fs::remove_file(path);
    }
}
