//! Blocking persistence operations. Call from a worker, never the GPUI render thread.
//! Password values deliberately never appear in list models or errors.
#[path = "library/browsing.rs"]
mod browsing;
pub use browsing::*;

use smartzip_config::{ResolvedConfig, StateMode};
use smartzip_db::{
    file_extractions::{FileExtractionRecord, FileExtractionRepository},
    password::{NewPassword, PasswordRepository},
    task::{TaskRecord, TaskRepository},
    task_event::{TaskEventRecord, TaskEventRepository},
    SmartZipDb,
};
use smartzip_platform::PlatformPaths;
use std::{
    io::Write,
    path::{Path, PathBuf},
};

pub type Result<T> = std::result::Result<T, String>;

#[derive(Clone, Debug, Default)]
pub struct LibraryOptions {
    pub config_path: Option<PathBuf>,
    pub no_config: bool,
    pub database_path: Option<PathBuf>,
}

#[derive(Clone, Debug)]
pub struct ConfigSnapshot {
    pub resolved: ResolvedConfig,
    pub edit_path: PathBuf,
}

#[derive(Clone, Debug)]
pub struct TaskDetail {
    pub inputs: Vec<PathBuf>,
    pub task: TaskRecord,
    pub files: Vec<FileExtractionRecord>,
    pub events: Vec<TaskEventRecord>,
}

/// Fixed-length mask avoids revealing even a password's length.
#[derive(Clone, Debug)]
pub struct PasswordSummary {
    pub id: i64,
    pub masked: &'static str,
    pub source: String,
    pub pinned: bool,
    pub disabled: bool,
    pub success_count: i64,
    pub failure_count: i64,
    pub last_success_at: Option<String>,
    pub last_failure_at: Option<String>,
}

fn config_selection(options: &LibraryOptions) -> Result<(Option<PathBuf>, PathBuf)> {
    let environment = std::env::var_os("SMARTZIP_CONFIG").map(PathBuf::from);
    let explicit = options.config_path.as_ref().or(environment.as_ref());
    if let Some(path) = explicit.filter(|_| !options.no_config) {
        return Ok((Some(path.clone()), path.clone()));
    }
    let default = PlatformPaths::try_new()
        .map_err(|e| e.to_string())?
        .config_path();
    let selected = smartzip_config::selected_config(None, options.no_config, None, &default)
        .map_err(|e| e.to_string())?;
    let edit_path = selected.clone().unwrap_or(default);
    Ok((selected, edit_path))
}

pub fn load_config(options: &LibraryOptions) -> Result<ConfigSnapshot> {
    let (selected, edit_path) = config_selection(options)?;
    let resolved = ResolvedConfig::load(selected.as_deref()).map_err(|e| e.to_string())?;
    Ok(ConfigSnapshot {
        resolved,
        edit_path,
    })
}

fn database(options: &LibraryOptions, write: bool) -> Result<SmartZipDb> {
    let snapshot = load_config(options)?;
    let state = &snapshot.resolved.values.state;
    if state.mode == StateMode::Off {
        return Err("状态存储已关闭 (state.mode = off)".into());
    }
    if write && state.mode == StateMode::ReadOnly {
        return Err("状态存储为只读，无法修改".into());
    }
    let path = match options.database_path.as_ref().or(state.database.as_ref()) {
        Some(path) => path.clone(),
        None => PlatformPaths::try_new()
            .map_err(|e| e.to_string())?
            .db_path(),
    };
    if write {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        SmartZipDb::open(path).map_err(|e| e.to_string())
    } else {
        // Browsing does not create a database, run migrations, or change permissions.
        if !path.try_exists().map_err(|e| e.to_string())? && state.mode == StateMode::ReadWrite {
            return SmartZipDb::in_memory().map_err(|e| e.to_string());
        }
        SmartZipDb::open_read_only(path).map_err(|e| e.to_string())
    }
}

pub fn load_task_detail(options: &LibraryOptions, id: &str) -> Result<TaskDetail> {
    let db = database(options, false)?;
    let task = TaskRepository::new(db.connection())
        .find_by_id(id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "未找到该历史任务".to_string())?;
    let inputs_json: Option<String> = db
        .connection()
        .query_row("SELECT inputs_json FROM tasks WHERE id=?1", [id], |r| {
            r.get(0)
        })
        .map_err(|e| e.to_string())?;
    let inputs = inputs_json
        .as_deref()
        .and_then(|text| serde_json::from_str(text).ok())
        .unwrap_or_default();
    Ok(TaskDetail {
        inputs,
        task,
        files: FileExtractionRepository::new(db.connection())
            .list_by_task(id)
            .map_err(|e| e.to_string())?,
        events: TaskEventRepository::new(db.connection())
            .list_by_task(id)
            .map_err(|e| e.to_string())?,
    })
}

pub fn read_password(options: &LibraryOptions, id: i64) -> Result<String> {
    let db = database(options, false)?;
    PasswordRepository::new(db.connection())
        .get_by_id(id)
        .map_err(|_| "无法读取密码库".to_string())?
        .map(|password| password.value)
        .ok_or_else(|| "未找到该密码".to_string())
}

pub fn add_password(
    options: &LibraryOptions,
    value: &str,
    source: &str,
    pinned: bool,
) -> Result<i64> {
    if value.is_empty() {
        return Err("密码不能为空".into());
    }
    let db = database(options, true)?;
    PasswordRepository::new(db.connection())
        .upsert(NewPassword {
            value,
            source,
            pinned,
        })
        .map_err(|_| "无法保存密码".into())
}

pub fn import_passwords(options: &LibraryOptions, path: &Path, source: &str) -> Result<u64> {
    let db = database(options, true)?;
    let reader = std::io::BufReader::new(
        std::fs::File::open(path).map_err(|_| "无法读取密码文件".to_string())?,
    );
    PasswordRepository::new(db.connection())
        .import_lines(reader, source)
        .map_err(|_| "密码导入失败；本次导入已回滚，请检查文件编码".into())
}

/// Export is an explicit reveal action. Never overwrite an existing file or symlink.
/// tempfile creates a private (0600 on Unix) sibling and publishes it atomically.
pub fn export_passwords(options: &LibraryOptions, path: &Path) -> Result<usize> {
    let db = database(options, false)?;
    let rows = PasswordRepository::new(db.connection())
        .ranked_candidates(i64::MAX as usize)
        .map_err(|_| "无法读取密码库".to_string())?;
    if rows.iter().any(|p| p.value.contains(['\n', '\r'])) {
        return Err("存在包含换行的密码，无法无损导出为逐行文本".into());
    }
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut file =
        tempfile::NamedTempFile::new_in(parent).map_err(|_| "无法创建导出文件".to_string())?;
    for row in &rows {
        writeln!(file, "{}", row.value).map_err(|_| "密码导出写入失败".to_string())?;
    }
    file.as_file()
        .sync_all()
        .map_err(|_| "密码导出同步失败".to_string())?;
    file.persist_noclobber(path)
        .map_err(|_| "密码导出失败：目标已存在或不可写，请选择新文件".to_string())?;
    Ok(rows.len())
}

/// Matches CLI ranking and stale cleanup: pinned passwords are always retained.
/// `apply=false` is read-only. `apply=true` recomputes candidates before disabling.
pub fn cleanup_passwords(
    options: &LibraryOptions,
    max_passwords: usize,
    stale_days: Option<u64>,
    apply: bool,
) -> Result<Vec<i64>> {
    let cutoff = stale_days
        .map(|days| {
            i64::try_from(days)
                .ok()
                .and_then(chrono::Duration::try_days)
                .and_then(|duration| chrono::Utc::now().checked_sub_signed(duration))
                .map(|date| date.format("%Y-%m-%d %H:%M:%S").to_string())
                .ok_or_else(|| "过期天数超出支持范围".to_string())
        })
        .transpose()?;
    let db = database(options, apply)?;
    let repo = PasswordRepository::new(db.connection());
    let rows = repo
        .ranked_candidates(i64::MAX as usize)
        .map_err(|_| "无法读取密码库".to_string())?;
    let ids: Vec<i64> = rows
        .iter()
        .enumerate()
        .filter(|(index, row)| {
            !row.pinned
                && (*index >= max_passwords
                    || cutoff.as_ref().is_some_and(|cutoff| {
                        row.last_success_at
                            .as_ref()
                            .is_none_or(|date| date < cutoff)
                    }))
        })
        .map(|(_, row)| row.id)
        .collect();
    if apply {
        repo.disable_many(&ids)
            .map_err(|_| "密码清理失败；本次清理已回滚".to_string())?;
    }
    Ok(ids)
}

pub fn set_config_fields(
    options: &LibraryOptions,
    patches: &[(String, Option<String>)],
) -> Result<ConfigSnapshot> {
    if options.no_config {
        return Err("当前禁用配置文件，请先启用配置".into());
    }
    let snapshot = load_config(options)?;
    smartzip_config::edit_config_fields(&snapshot.edit_path, patches).map_err(|e| e.to_string())?;
    load_config(options)
}

pub fn init_config(options: &LibraryOptions, full: bool) -> Result<ConfigSnapshot> {
    if options.no_config {
        return Err("当前禁用配置文件，请先启用配置".into());
    }
    let (_, edit_path) = config_selection(options)?;
    smartzip_config::init_config(&edit_path, full).map_err(|e| e.to_string())?;
    load_config(options)
}

#[cfg(test)]
mod tests {
    use super::*;
    pub(super) fn options(root: &Path, mode: &str) -> LibraryOptions {
        let config = root.join("config.toml");
        std::fs::write(
            &config,
            format!("schema_version = 1\ndefaults_version = 1\n[state]\nmode = \"{mode}\"\n"),
        )
        .unwrap();
        LibraryOptions {
            config_path: Some(config),
            database_path: Some(root.join("state.db")),
            no_config: false,
        }
    }
    #[test]
    fn state_policy_prevents_writes_and_reading_never_creates_database() {
        let root = tempfile::tempdir().unwrap();
        let options = options(root.path(), "read-write");
        assert!(history_page(&options, &Query::default())
            .unwrap()
            .rows
            .is_empty());
        assert!(!options.database_path.as_ref().unwrap().exists());
        let id = add_password(&options, "secret", "manual", true).unwrap();
        set_config_fields(
            &options,
            &[("state.mode".into(), Some("\"read-only\"".into()))],
        )
        .unwrap();
        assert_eq!(
            password_page(&options, &Query::default())
                .unwrap()
                .rows
                .len(),
            1
        );
        assert!(change_passwords(&options, &[id], PasswordAction::Delete).is_err());
        set_config_fields(&options, &[("state.mode".into(), Some("\"off\"".into()))]).unwrap();
        assert!(password_page(&options, &Query::default()).is_err());
        assert!(add_password(&options, "secret", "manual", false).is_err());
    }
    #[test]
    fn passwords_are_masked_import_is_atomic_and_export_preserves_existing_files() {
        let root = tempfile::tempdir().unwrap();
        let options = options(root.path(), "read-write");
        let id = add_password(&options, "private-value", "manual", true).unwrap();
        let rows = password_page(&options, &Query::default()).unwrap().rows;
        assert!(!format!("{rows:?}").contains("private-value"));
        let input = root.path().join("input.txt");
        std::fs::write(&input, b"rolled-back\n\xff").unwrap();
        assert!(import_passwords(&options, &input, "import").is_err());
        assert_eq!(
            password_page(&options, &Query::default())
                .unwrap()
                .rows
                .len(),
            1
        );
        let output = root.path().join("output.txt");
        assert_eq!(export_passwords(&options, &output).unwrap(), 1);
        assert_eq!(std::fs::read_to_string(&output).unwrap(), "private-value\n");
        assert!(export_passwords(&options, &output).is_err());
        assert_eq!(std::fs::read_to_string(&output).unwrap(), "private-value\n");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&output).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        assert!(cleanup_passwords(&options, 0, Some(1), true)
            .unwrap()
            .is_empty());
        change_passwords(&options, &[id], PasswordAction::Delete).unwrap();
        assert!(password_page(&options, &Query::default())
            .unwrap()
            .rows
            .is_empty());
    }

    #[test]
    fn password_reveal_is_by_id_and_respects_read_policy() {
        let root = tempfile::tempdir().unwrap();
        let options = options(root.path(), "read-write");
        let id = add_password(&options, "private-value", "manual", true).unwrap();
        let rows = password_page(&options, &Query::default()).unwrap().rows;
        assert_eq!(rows[0].id, id);
        assert_eq!(rows[0].masked, "••••••••");
        assert_eq!(read_password(&options, id).unwrap(), "private-value");
        assert!(read_password(&options, id + 100).is_err());

        set_config_fields(
            &options,
            &[("state.mode".into(), Some("\"read-only\"".into()))],
        )
        .unwrap();
        assert_eq!(read_password(&options, id).unwrap(), "private-value");
        set_config_fields(&options, &[("state.mode".into(), Some("\"off\"".into()))]).unwrap();
        assert!(read_password(&options, id).is_err());
    }

    #[test]
    fn history_reads_real_task_files_and_events() {
        let root = tempfile::tempdir().unwrap();
        let options = options(root.path(), "read-write");
        let db = database(&options, true).unwrap();
        TaskRepository::new(db.connection())
            .insert(smartzip_db::task::NewTask {
                id: "recorded",
                kind: "extract",
                output_path: Some("/output"),
                started_at: "2026-09-12T01:00:00Z",
            })
            .unwrap();
        TaskEventRepository::new(db.connection())
            .insert(smartzip_db::task_event::NewTaskEvent {
                task_id: "recorded",
                level: smartzip_db::task_event::TaskEventLevel::Info,
                event_type: "started",
                message: "开始",
                data_json: None,
                created_at: "2026-09-12T01:00:00Z",
            })
            .unwrap();
        db.connection().execute("INSERT INTO file_extractions(task_id, input_path, status, created_at) VALUES ('recorded', '/input.zip', 'extracted', '2026-09-12T01:00:00Z')", []).unwrap();
        drop(db);
        assert_eq!(
            history_page(&options, &Query::default()).unwrap().rows[0]
                .task
                .id,
            "recorded"
        );
        let detail = load_task_detail(&options, "recorded").unwrap();
        assert_eq!(detail.files[0].input_path, "/input.zip");
        assert_eq!(detail.events[0].message, "开始");
    }
    #[test]
    fn initialization_works_for_an_explicit_missing_file() {
        let root = tempfile::tempdir().unwrap();
        let options = LibraryOptions {
            config_path: Some(root.path().join("new/config.toml")),
            ..LibraryOptions::default()
        };
        assert!(load_config(&options).is_err());
        assert!(init_config(&options, false).is_ok());
        assert!(init_config(&options, false).is_err());
    }

    #[test]
    fn cleanup_preview_keeps_state_and_apply_disables_only_unpinned_candidates() {
        let root = tempfile::tempdir().unwrap();
        let options = options(root.path(), "read-write");
        let pinned = add_password(&options, "pinned", "manual", true).unwrap();
        let stale = add_password(&options, "stale", "manual", false).unwrap();
        assert_eq!(
            cleanup_passwords(&options, 100, Some(1), false).unwrap(),
            vec![stale]
        );
        assert_eq!(
            password_page(&options, &Query::default())
                .unwrap()
                .rows
                .len(),
            2
        );
        assert_eq!(
            cleanup_passwords(&options, 100, Some(1), true).unwrap(),
            vec![stale]
        );
        let remaining = password_page(
            &options,
            &Query {
                status: "enabled".into(),
                ..Query::default()
            },
        )
        .unwrap()
        .rows;
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].id, pinned);
        assert!(cleanup_passwords(&options, 0, Some(u64::MAX), true).is_err());
    }

    #[test]
    fn grouped_configuration_save_is_atomic_and_returns_resolved_snapshot() {
        let root = tempfile::tempdir().unwrap();
        let options = options(root.path(), "read-write");
        let patches = vec![
            (
                "extraction.output.destination".into(),
                Some("'directory'".into()),
            ),
            (
                "extraction.output.directory".into(),
                Some("'output'".into()),
            ),
        ];
        let snapshot = set_config_fields(&options, &patches).unwrap();
        assert_eq!(
            snapshot.resolved.values.extraction.output.directory,
            Some(root.path().join("output"))
        );
        let original = std::fs::read(&snapshot.edit_path).unwrap();
        assert!(set_config_fields(
            &options,
            &[
                ("state.history".into(), Some("false".into())),
                ("extraction.output.directory".into(), None),
            ]
        )
        .is_err());
        assert_eq!(std::fs::read(&snapshot.edit_path).unwrap(), original);
        let disabled = LibraryOptions {
            no_config: true,
            ..options
        };
        assert!(set_config_fields(&disabled, &patches).is_err());
        assert_eq!(std::fs::read(&snapshot.edit_path).unwrap(), original);
    }

    #[test]
    fn invalid_configuration_does_not_replace_valid_file() {
        let root = tempfile::tempdir().unwrap();
        let options = options(root.path(), "read-write");
        let original = std::fs::read(options.config_path.as_ref().unwrap()).unwrap();
        assert!(set_config_fields(
            &options,
            &[("state.mode".into(), Some("\"invalid\"".into()))]
        )
        .is_err());
        assert_eq!(
            std::fs::read(options.config_path.as_ref().unwrap()).unwrap(),
            original
        );
    }
}
