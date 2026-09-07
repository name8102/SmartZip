use crate::{resolve::invalid, ResolvedConfig, SmartZipConfig};
use std::{
    io::{self, Write},
    path::{Path, PathBuf},
};
use toml_edit::{DocumentMut, Item, TableLike};

struct Lock(PathBuf);
impl Drop for Lock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn editable(path: &Path) -> io::Result<std::fs::Metadata> {
    let metadata = std::fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.permissions().readonly()
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "managed, linked, or read-only configuration: {}",
                path.display()
            ),
        ));
    }
    Ok(metadata)
}

fn write_atomic(path: &Path, original: Option<&str>, contents: &str) -> io::Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let lock_path = path.with_extension("toml.lock");
    let _file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&lock_path)?;
    let _lock = Lock(lock_path);
    let permissions = if let Some(original) = original {
        let permissions = editable(path)?.permissions();
        if std::fs::read_to_string(path)? != original {
            return Err(invalid("configuration changed while editing; retry"));
        }
        Some(permissions)
    } else {
        None
    };
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    if let Some(permissions) = permissions {
        temp.as_file().set_permissions(permissions)?;
    }
    temp.write_all(contents.as_bytes())?;
    temp.as_file().sync_all()?;
    if let Some(original) = original {
        editable(path)?;
        if std::fs::read_to_string(path)? != original {
            return Err(invalid("configuration changed while editing; retry"));
        }
        temp.persist(path).map_err(|e| e.error)?;
    } else {
        temp.persist_noclobber(path).map_err(|e| e.error)?;
    }
    #[cfg(unix)]
    std::fs::File::open(parent)?.sync_all()?;
    Ok(())
}

pub fn init_config(path: &Path, full: bool) -> io::Result<()> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    let text = if full {
        format!(
            "# SmartZip defaults v1. Root inputs are always preserved.\n{}",
            toml::to_string_pretty(&SmartZipConfig::default()).map_err(invalid)?
        )
    } else {
        "# Omitted fields inherit defaults-v1.\nschema_version = 1\ndefaults_version = 1\n".into()
    };
    write_atomic(path, None, &text)
}

pub fn edit_config(path: &Path, key: &str, value: Option<&str>) -> io::Result<()> {
    editable(path)?;
    let original = std::fs::read_to_string(path)?;
    let mut doc = original.parse::<DocumentMut>().map_err(invalid)?;
    if doc.get("schema_version").is_none() {
        return Err(invalid("migrate legacy configuration before editing"));
    }
    if key == "schema_version" || key == "defaults_version" {
        return Err(invalid(
            "version fields cannot be edited with config set/unset",
        ));
    }
    let defaults = toml::Value::try_from(SmartZipConfig::default()).map_err(invalid)?;
    if crate::value_at(&defaults, key).is_none()
        && !["state.database", "extraction.output.directory"].contains(&key)
    {
        return Err(invalid(format!("unknown editable key: {key}")));
    }
    let keys: Vec<_> = key.split('.').collect();
    let mut table: &mut dyn TableLike = doc.as_table_mut();
    for part in &keys[..keys.len() - 1] {
        if !table.contains_key(part) {
            table.insert(part, Item::Table(toml_edit::Table::new()));
        }
        table = table
            .get_mut(part)
            .and_then(Item::as_table_like_mut)
            .ok_or_else(|| invalid(format!("not a table: {part}")))?;
    }
    let leaf = keys[keys.len() - 1];
    if let Some(value) = value {
        let parsed = format!("value = {value}")
            .parse::<DocumentMut>()
            .map_err(invalid)?;
        let mut item = parsed["value"].clone();
        if let (Some(old), Some(new)) = (
            table.get(leaf).and_then(Item::as_value),
            item.as_value_mut(),
        ) {
            *new.decor_mut() = old.decor().clone();
        }
        table.insert(leaf, item);
    } else {
        table.remove(leaf);
    }
    let content = doc.to_string();
    let config: SmartZipConfig = toml::from_str(&content).map_err(invalid)?;
    config.validate()?;
    write_atomic(path, Some(&original), &content)
}

pub fn migrate_config(path: &Path, apply: bool) -> io::Result<String> {
    let original = std::fs::read_to_string(path)?;
    let mut doc = original.parse::<DocumentMut>().map_err(invalid)?;
    ResolvedConfig::load(Some(path))?;
    if doc.get("schema_version").is_some() {
        return Ok(original);
    }
    if let Some(limits) = doc.remove("extraction") {
        doc.insert("limits", limits);
    }
    doc["schema_version"] = toml_edit::value(1);
    doc["defaults_version"] = toml_edit::value(1);
    let content = doc.to_string();
    if apply {
        editable(path)?;
        let backup = path.with_extension("toml.v0.bak");
        write_atomic(&backup, None, &original)?;
        write_atomic(path, Some(&original), &content)?;
    }
    Ok(content)
}
