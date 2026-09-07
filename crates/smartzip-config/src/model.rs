use crate::{BackendConfig, ExtractionLimits};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const SCHEMA_VERSION: u32 = 1;
pub const DEFAULTS_VERSION: u32 = 1;
pub const DEFAULT_RECURSION_DEPTH: u8 = 3;
pub const DEFAULT_PASSWORD_LIMIT: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OnError {
    Continue,
    Stop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RootScan {
    Off,
    Auto,
    Ask,
    All,
    Largest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NestedScan {
    Off,
    Auto,
    Ask,
    Aggressive,
    All,
    Largest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SuspiciousEncoding {
    Ask,
    Skip,
    Accept,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Destination {
    FirstInputParent,
    Directory,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Layout {
    Conservative,
    Smart,
    Raw,
    FlatSingle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SingleRootName {
    Auto,
    Archive,
    Inner,
    PreserveBoth,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Conflict {
    Ask,
    Skip,
    Rename,
    Overwrite,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Cleanup {
    Keep,
    Trash,
    Delete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PasswordMode {
    Auto,
    Manual,
    Off,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PasswordSource {
    Manual,
    Known,
    Batch,
    Empty,
    Database,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StateMode {
    Off,
    ReadOnly,
    ReadWrite,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum InteractionMode {
    Auto,
    Always,
    Never,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LogLevel {
    Off,
    Error,
    Warn,
    Info,
    Debug,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Recursion {
    pub enabled: bool,
    pub max_depth: u8,
}
impl Default for Recursion {
    fn default() -> Self {
        Self {
            enabled: true,
            max_depth: DEFAULT_RECURSION_DEPTH,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Embedded {
    pub root: RootScan,
    pub nested: NestedScan,
    pub dominant_min_ratio: f32,
}
impl Default for Embedded {
    fn default() -> Self {
        Self {
            root: RootScan::Auto,
            nested: NestedScan::Auto,
            dominant_min_ratio: 0.70,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Volumes {
    pub auto_discover: bool,
}
impl Default for Volumes {
    fn default() -> Self {
        Self {
            auto_discover: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Encoding {
    pub mode: String,
    pub on_suspicious: SuspiciousEncoding,
}
impl Default for Encoding {
    fn default() -> Self {
        Self {
            mode: "auto".into(),
            on_suspicious: SuspiciousEncoding::Ask,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Output {
    pub destination: Destination,
    pub directory: Option<PathBuf>,
    pub layout: Layout,
    pub single_root_name: SingleRootName,
    pub on_conflict: Conflict,
}
impl Default for Output {
    fn default() -> Self {
        Self {
            destination: Destination::FirstInputParent,
            directory: None,
            layout: Layout::Conservative,
            single_root_name: SingleRootName::Auto,
            on_conflict: Conflict::Ask,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Reuse {
    pub skip_completed: bool,
    pub password_hint: bool,
    pub encoding_hint: bool,
}
impl Default for Reuse {
    fn default() -> Self {
        Self {
            skip_completed: false,
            password_hint: true,
            encoding_hint: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CleanupPolicy {
    pub nested_archives: Cleanup,
}
impl Default for CleanupPolicy {
    fn default() -> Self {
        Self {
            nested_archives: Cleanup::Trash,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Extraction {
    pub on_error: OnError,
    pub recursion: Recursion,
    pub embedded: Embedded,
    pub volumes: Volumes,
    pub encoding: Encoding,
    pub output: Output,
    pub reuse: Reuse,
    pub cleanup: CleanupPolicy,
}
impl Default for Extraction {
    fn default() -> Self {
        Self {
            on_error: OnError::Continue,
            recursion: Recursion::default(),
            embedded: Embedded::default(),
            volumes: Volumes::default(),
            encoding: Encoding::default(),
            output: Output::default(),
            reuse: Reuse::default(),
            cleanup: CleanupPolicy::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Passwords {
    pub mode: PasswordMode,
    pub sources: Vec<PasswordSource>,
    pub database_limit: usize,
    pub save_success: bool,
    pub record_statistics: bool,
}
impl Default for Passwords {
    fn default() -> Self {
        Self {
            mode: PasswordMode::Auto,
            sources: vec![
                PasswordSource::Manual,
                PasswordSource::Known,
                PasswordSource::Batch,
                PasswordSource::Empty,
                PasswordSource::Database,
            ],
            database_limit: DEFAULT_PASSWORD_LIMIT,
            save_success: true,
            record_statistics: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct State {
    pub database: Option<PathBuf>,
    pub mode: StateMode,
    pub history: bool,
    pub known_files: StateMode,
}
impl Default for State {
    fn default() -> Self {
        Self {
            database: None,
            mode: StateMode::ReadWrite,
            history: true,
            known_files: StateMode::ReadWrite,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Interaction {
    pub mode: InteractionMode,
}
impl Default for Interaction {
    fn default() -> Self {
        Self {
            mode: InteractionMode::Auto,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Logging {
    pub level: LogLevel,
    pub file: bool,
}
impl Default for Logging {
    fn default() -> Self {
        Self {
            level: LogLevel::Info,
            file: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SmartZipConfig {
    pub schema_version: u32,
    pub defaults_version: u32,
    pub extraction: Extraction,
    pub passwords: Passwords,
    pub state: State,
    pub interaction: Interaction,
    pub logging: Logging,
    pub limits: ExtractionLimits,
    pub backends: BackendConfig,
}
impl Default for SmartZipConfig {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            defaults_version: DEFAULTS_VERSION,
            extraction: Extraction::default(),
            passwords: Passwords::default(),
            state: State::default(),
            interaction: Interaction::default(),
            logging: Logging::default(),
            limits: ExtractionLimits::default(),
            backends: BackendConfig::default(),
        }
    }
}

impl SmartZipConfig {
    pub fn load(path: impl AsRef<Path>) -> std::io::Result<Self> {
        Ok(ResolvedConfig::load(Some(path.as_ref()))?.values)
    }
    pub fn validate(&self) -> std::io::Result<()> {
        let fail = |message: &str| {
            Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                message,
            ))
        };
        if self.schema_version != SCHEMA_VERSION {
            return fail("unsupported schema_version");
        }
        if self.defaults_version != DEFAULTS_VERSION {
            return fail("unsupported defaults_version");
        }
        for (key, path) in [
            ("state.database", self.state.database.as_ref()),
            (
                "extraction.output.directory",
                self.extraction.output.directory.as_ref(),
            ),
        ] {
            if path.is_some_and(|p| p.as_os_str().is_empty() || p.to_string_lossy().contains('\0'))
            {
                return fail(&format!("{key} must be a nonempty valid path"));
            }
        }
        self.backends.validate().map_err(std::io::Error::other)?;
        let ratio = self.extraction.embedded.dominant_min_ratio;
        if !ratio.is_finite() || !(0.0..=1.0).contains(&ratio) {
            return fail("extraction.embedded.dominant_min_ratio must be between 0 and 1");
        }
        if self.extraction.output.destination == Destination::Directory
            && self
                .extraction
                .output
                .directory
                .as_ref()
                .is_none_or(|p| p.as_os_str().is_empty())
        {
            return fail("extraction.output.directory is required for destination=directory");
        }
        let encoding = self.extraction.encoding.mode.to_ascii_lowercase();
        if ![
            "auto",
            "backend",
            "utf-8",
            "gb18030",
            "gbk",
            "big5",
            "shift_jis",
            "euc-jp",
            "euc-kr",
        ]
        .contains(&encoding.as_str())
        {
            return fail("unsupported extraction.encoding.mode");
        }
        for (index, source) in self.passwords.sources.iter().enumerate() {
            if self.passwords.sources[..index].contains(source) {
                return fail("duplicate passwords.sources entry");
            }
        }
        if self.logging.file {
            return fail("unsupported_option: logging.file=true (file logging is not implemented)");
        }
        Ok(())
    }
}
use crate::ResolvedConfig;
