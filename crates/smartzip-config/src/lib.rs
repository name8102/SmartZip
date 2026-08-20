//! Application configuration via TOML.

use serde::{Deserialize, Serialize};
use smartzip_core::{ArchiveFormat, BackendCapabilityProfile, CompressionLevel};
use smartzip_scanner::ScannerConfig;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AdapterFamily {
    NativeZip,
    SevenZipCli,
    UnrarCli,
    Custom(String),
}

impl AdapterFamily {
    pub fn profile_key(&self) -> String {
        match self {
            Self::NativeZip => "native-zip".into(),
            Self::SevenZipCli => "sevenzip-cli".into(),
            Self::UnrarCli => "unrar-cli".into(),
            Self::Custom(value) => value.clone(),
        }
    }
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
    #[serde(default)]
    pub profile: BackendCapabilityProfile,
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
    #[serde(default)]
    pub family_profiles: BTreeMap<String, BackendCapabilityProfile>,
    #[serde(default)]
    pub version_profiles: BTreeMap<String, BackendCapabilityProfile>,
}

impl Default for BackendConfig {
    fn default() -> Self {
        Self {
            auto_discover: true,
            installations: Vec::new(),
            family_profiles: BTreeMap::new(),
            version_profiles: BTreeMap::new(),
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
            installation.profile.validate()?;
        }
        for profile in self
            .family_profiles
            .values()
            .chain(self.version_profiles.values())
        {
            profile.validate()?;
        }
        Ok(())
    }

    pub fn profile_for(
        &self,
        installation: &BackendInstallation,
    ) -> std::result::Result<BackendCapabilityProfile, String> {
        self.validate()?;
        let family = self
            .family_profiles
            .get(&installation.family.profile_key())
            .cloned()
            .unwrap_or_default();
        let version_key = installation
            .declared_version
            .as_ref()
            .map(|version| format!("{}@{version}", installation.family.profile_key()));
        let version = version_key
            .as_ref()
            .and_then(|key| self.version_profiles.get(key));
        BackendCapabilityProfile::compose(&family, version, Some(&installation.profile))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LayoutConfig {
    pub policy: String,
    pub single_root_name: String,
    pub ignore_metadata_entries: bool,
    pub preserve_archive_context_for_root: bool,
    pub preserve_archive_context_for_nested: bool,
}

impl Default for LayoutConfig {
    fn default() -> Self {
        Self {
            policy: "conservative".into(),
            single_root_name: "auto".into(),
            ignore_metadata_entries: true,
            preserve_archive_context_for_root: true,
            preserve_archive_context_for_nested: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SmartZipConfig {
    pub default_format: ArchiveFormat,
    pub default_level: CompressionLevel,
    pub scanner: ScannerConfig,
    pub delete_source_on_success: bool,
    pub delete_source_to_trash: bool,
    pub log_level: LogLevel,
    pub gui: GuiConfig,
    pub layout: LayoutConfig,
    #[serde(default)]
    pub backends: BackendConfig,
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
            layout: LayoutConfig::default(),
            backends: BackendConfig::default(),
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
        // Use atomic-write-file to avoid truncated config on crash/power loss:
        // write to a temp file in the same directory and atomically rename,
        // guaranteeing the result is either the old file or the new file.
        let mut file = atomic_write_file::AtomicWriteFile::open(path.as_ref())?;
        use std::io::Write as _;
        file.write_all(content.as_bytes())?;
        file.commit()?;
        Ok(())
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

    #[test]
    fn backend_profiles_round_trip_and_compose_by_precedence() {
        use smartzip_core::{ArchiveOperation, CapabilityId, CapabilityRule, SupportState};

        let capability = CapabilityId::new("codec:zstd").unwrap();
        let make_profile = |support| BackendCapabilityProfile {
            rules: vec![CapabilityRule {
                capability: capability.clone(),
                precedence: 0,
                operations: vec![ArchiveOperation::Extract],
                containers: vec![ArchiveFormat::SevenZip],
                support,
                evidence: None,
            }],
        };
        let installation = BackendInstallation {
            id: "local-7z".into(),
            family: AdapterFamily::SevenZipCli,
            executable: PathBuf::from("/opt/bin/7z"),
            declared_version: Some("24.09".into()),
            enabled: true,
            priority: 10,
            profile: make_profile(SupportState::Supported),
        };
        let mut backends = BackendConfig::default();
        backends
            .family_profiles
            .insert("sevenzip-cli".into(), make_profile(SupportState::Unknown));
        backends.version_profiles.insert(
            "sevenzip-cli@24.09".into(),
            make_profile(SupportState::Unsupported),
        );
        backends.installations.push(installation.clone());

        let profile = backends.profile_for(&installation).unwrap();
        assert_eq!(
            profile.support(
                &capability,
                ArchiveOperation::Extract,
                Some(&ArchiveFormat::SevenZip),
            ),
            SupportState::Supported
        );
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
            profile: BackendCapabilityProfile::default(),
        };
        let config = BackendConfig {
            installations: vec![installation.clone(), installation],
            ..BackendConfig::default()
        };
        assert!(config.validate().unwrap_err().contains("duplicate"));
    }
}
