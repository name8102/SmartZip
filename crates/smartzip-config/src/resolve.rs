use crate::SmartZipConfig;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    io,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResolvedConfig {
    pub values: SmartZipConfig,
    pub origins: BTreeMap<String, String>,
    pub path: Option<PathBuf>,
    pub diagnostics: Vec<String>,
}

pub fn selected_config(
    explicit: Option<&Path>,
    no_config: bool,
    environment: Option<&Path>,
    default: &Path,
    legacy: &Path,
) -> io::Result<(Option<PathBuf>, Vec<String>)> {
    if no_config {
        return Ok((None, Vec::new()));
    }
    if let Some(path) = explicit.or(environment) {
        return Ok((Some(path.to_path_buf()), Vec::new()));
    }
    let present = |path: &Path| match std::fs::metadata(path) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    };
    let current = present(default)?;
    let old = default != legacy && present(legacy)?;
    Ok(if current {
        (
            Some(default.into()),
            if old {
                vec![format!(
                    "using {}; legacy configuration also exists at {}; files are not merged",
                    default.display(),
                    legacy.display()
                )]
            } else {
                vec![]
            },
        )
    } else if old {
        (
            Some(legacy.into()),
            vec![format!(
                "using legacy configuration at {}",
                legacy.display()
            )],
        )
    } else {
        (None, vec![])
    })
}

pub(crate) fn invalid(message: impl ToString) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.to_string())
}

/// Only recognized v0 routing/limit keys migrate. Unknown keys remain errors.
pub fn migrate_value(mut value: toml::Value) -> io::Result<(toml::Value, bool)> {
    let root = value
        .as_table_mut()
        .ok_or_else(|| invalid("configuration must be a table"))?;
    if root.contains_key("schema_version") {
        return Ok((value, false));
    }
    for key in root.keys() {
        if key != "backends" && key != "extraction" {
            return Err(invalid(format!("unknown legacy configuration key: {key}")));
        }
    }
    if let Some(limits) = root.remove("extraction") {
        let _: crate::ExtractionLimits = limits.clone().try_into().map_err(invalid)?;
        root.insert("limits".into(), limits);
    }
    root.insert("schema_version".into(), 1.into());
    root.insert("defaults_version".into(), 1.into());
    Ok((value, true))
}

fn record(value: &toml::Value, prefix: &str, source: &str, origins: &mut BTreeMap<String, String>) {
    if let Some(table) = value.as_table() {
        for (key, value) in table {
            record(
                value,
                &if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{prefix}.{key}")
                },
                source,
                origins,
            );
        }
    } else {
        origins.insert(prefix.into(), source.into());
    }
}
fn merge(base: &mut toml::Value, patch: toml::Value) {
    if let (Some(base), Some(patch)) = (base.as_table_mut(), patch.as_table()) {
        for (key, value) in patch {
            if let Some(old) = base.get_mut(key) {
                merge(old, value.clone());
            } else {
                base.insert(key.clone(), value.clone());
            }
        }
    } else {
        *base = patch;
    }
}

pub fn value_at<'a>(value: &'a toml::Value, key: &str) -> Option<&'a toml::Value> {
    key.split('.').try_fold(value, |value, key| value.get(key))
}
fn patch_at(key: &str, value: toml::Value) -> toml::Value {
    key.split('.').rev().fold(value, |value, key| {
        toml::Value::Table([(key.into(), value)].into_iter().collect())
    })
}

impl ResolvedConfig {
    pub fn load(path: Option<&Path>) -> io::Result<Self> {
        let mut value = toml::Value::try_from(SmartZipConfig::default()).map_err(invalid)?;
        let mut origins = BTreeMap::new();
        record(&value, "", "defaults-v1", &mut origins);
        let mut diagnostics = Vec::new();
        let path = path.map(|p| std::path::absolute(p)).transpose()?;
        if let Some(path) = &path {
            let raw = std::fs::read_to_string(path)?;
            let parsed = raw
                .parse::<toml::Value>()
                .map_err(|e| invalid(format!("{}: {e}", path.display())))?;
            let (mut patch, migrated) = migrate_value(parsed)?;
            if migrated {
                diagnostics.push(
                    "legacy-v0 migrated in memory; use config migrate --apply to save".into(),
                );
            }
            // Resolve file paths before CLI patches. Bare backend names stay PATH lookups.
            for key in ["state.database", "extraction.output.directory"] {
                if let Some(p) = value_at(&patch, key).and_then(toml::Value::as_str) {
                    if p.is_empty() {
                        return Err(invalid(format!("{key} cannot be empty")));
                    }
                    let resolved = if Path::new(p).is_relative() {
                        path.parent().unwrap().join(p)
                    } else {
                        p.into()
                    };
                    merge(
                        &mut patch,
                        patch_at(key, resolved.to_string_lossy().into_owned().into()),
                    );
                }
            }
            if let Some(items) = patch
                .get_mut("backends")
                .and_then(|v| v.get_mut("installations"))
                .and_then(toml::Value::as_array_mut)
            {
                for item in items {
                    if let Some(executable) = item.get_mut("executable") {
                        if let Some(p) = executable.as_str() {
                            if p.is_empty() {
                                return Err(invalid(
                                    "backends.installations.executable cannot be empty",
                                ));
                            }
                            if Path::new(p).is_relative() && (p.contains('/') || p.contains('\\')) {
                                *executable = path
                                    .parent()
                                    .unwrap()
                                    .join(p)
                                    .to_string_lossy()
                                    .into_owned()
                                    .into();
                            }
                        }
                    }
                }
            }
            record(
                &patch,
                "",
                &format!("file:{}", path.display()),
                &mut origins,
            );
            merge(&mut value, patch);
        }
        let values: SmartZipConfig = value.try_into().map_err(invalid)?;
        values.validate()?;
        Ok(Self {
            values,
            origins,
            path,
            diagnostics,
        })
    }

    pub fn apply(&mut self, patches: &[(String, toml::Value)]) -> io::Result<()> {
        let mut value = toml::Value::try_from(&self.values).map_err(invalid)?;
        let mut seen = std::collections::HashSet::new();
        for (key, patch) in patches {
            if patch.is_table() {
                return Err(invalid("runtime overrides must use dotted leaf keys"));
            }
            if !seen.insert(key) {
                return Err(invalid(format!("conflicting explicit overrides: {key}")));
            }
            if key == "schema_version"
                || key == "defaults_version"
                || key == "backends"
                || key.starts_with("backends.")
            {
                return Err(invalid(format!("not a runtime override: {key}")));
            }
            let patch = patch_at(key, patch.clone());
            merge(&mut value, patch);
        }
        let values: SmartZipConfig = value.try_into().map_err(invalid)?;
        values.validate()?;
        self.values = values;
        for (key, _) in patches {
            self.origins.insert(key.clone(), "command_line".into());
        }
        Ok(())
    }

    pub fn explanation(&self) -> BTreeMap<String, String> {
        let mut inactive = BTreeMap::new();
        if !self.values.extraction.recursion.enabled
            || self.values.extraction.recursion.max_depth == 0
        {
            inactive.insert(
                "extraction.embedded.nested".into(),
                "inactive: recursion disabled".into(),
            );
        }
        if self.values.extraction.reuse.skip_completed {
            inactive.insert("extraction.reuse.skip_completed".into(), "suppressed: legacy cache lacks output and policy completion evidence; archive will be processed".into());
        }
        if self.values.state.mode != crate::StateMode::ReadWrite {
            for key in [
                "passwords.save_success",
                "passwords.record_statistics",
                "state.history",
            ] {
                inactive.insert(key.into(), "suppressed_by_state_mode".into());
            }
        }
        if self.values.state.mode == crate::StateMode::ReadOnly
            && self.values.state.known_files == crate::StateMode::ReadWrite
        {
            inactive.insert(
                "state.known_files".into(),
                "effective read-only: suppressed_by_state_mode".into(),
            );
        }
        if self.values.passwords.mode != crate::PasswordMode::Auto {
            inactive.insert(
                "passwords.sources".into(),
                format!(
                    "restricted by passwords.mode={:?}",
                    self.values.passwords.mode
                ),
            );
        }
        if self.values.state.mode == crate::StateMode::Off {
            for key in [
                "state.database",
                "state.known_files",
                "passwords.sources.known",
                "passwords.sources.database",
            ] {
                inactive.insert(key.into(), "suppressed_by_state_mode".into());
            }
        }
        inactive
    }
}
