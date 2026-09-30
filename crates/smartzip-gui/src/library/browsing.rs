//! Bounded, parameterized GUI queries. Searches never read plaintext passwords.
use super::*;

#[derive(Clone, Debug, Default)]
pub struct Query {
    pub text: String,
    pub status: String,
    pub offset: usize,
}
pub const PAGE_SIZE: usize = 50;
#[derive(Clone, Debug)]
pub struct Page<T> {
    pub rows: Vec<T>,
    pub has_more: bool,
}
#[derive(Clone, Debug)]
pub struct HistorySummary {
    pub task: TaskRecord,
    pub input: String,
    pub files: usize,
}

#[derive(Clone, Debug)]
pub struct RecoverySummary {
    pub id: String,
    pub inputs: Vec<PathBuf>,
    pub status: String,
    pub paused: bool,
    pub pending: usize,
    pub completed: usize,
}
/// Startup inspection is read-only: it neither claims execution ownership nor reconciles output.
pub fn recoverable_tasks(options: &LibraryOptions) -> Result<Vec<RecoverySummary>> {
    let snapshot = load_config(options)?;
    if snapshot.resolved.values.state.mode == StateMode::Off {
        return Ok(vec![]);
    }
    let db = database(options, false)?;
    let mut statement = db.connection().prepare(
        "SELECT t.id,t.inputs_json,t.status,t.paused,
        (SELECT count(*) FROM file_extractions f WHERE f.task_id=t.id AND f.status='pending'),
        (SELECT count(*) FROM file_extractions f WHERE f.task_id=t.id AND f.status='extracted')
        FROM tasks t WHERE recoverable=1 AND finished_at IS NULL ORDER BY queue_position,started_at"
    ).map_err(|e| e.to_string())?;
    let rows = statement
        .query_map([], |row| {
            let inputs: Option<String> = row.get(1)?;
            Ok(RecoverySummary {
                id: row.get(0)?,
                inputs: inputs
                    .and_then(|s| serde_json::from_str(&s).ok())
                    .unwrap_or_default(),
                status: row.get(2)?,
                paused: row.get(3)?,
                pending: row.get::<_, i64>(4)? as usize,
                completed: row.get::<_, i64>(5)? as usize,
            })
        })
        .map_err(|e| e.to_string())?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| e.to_string());
    rows
}

pub fn history_page(options: &LibraryOptions, query: &Query) -> Result<Page<HistorySummary>> {
    let db = database(options, false)?;
    let mut statement = db.connection().prepare(
        "SELECT t.id,t.kind,t.status,t.output_path,t.started_at,t.finished_at,
         COALESCE((SELECT f.input_path FROM file_extractions f WHERE f.task_id=t.id ORDER BY f.id LIMIT 1), ''),
         (SELECT count(*) FROM file_extractions f WHERE f.task_id=t.id)
         FROM tasks t WHERE (?1='' OR t.status=?1)
         AND (?2='' OR instr(lower(COALESCE(t.output_path,'')),lower(?2))>0 OR instr(t.started_at,?2)>0
         OR EXISTS(SELECT 1 FROM file_extractions f WHERE f.task_id=t.id AND instr(lower(f.input_path),lower(?2))>0))
         ORDER BY t.started_at DESC,t.id DESC LIMIT ?3 OFFSET ?4"
    ).map_err(|e| e.to_string())?;
    let mut rows = statement
        .query_map(
            (
                &query.status,
                &query.text,
                (PAGE_SIZE + 1) as i64,
                query.offset.min(i64::MAX as usize) as i64,
            ),
            |row| {
                Ok(HistorySummary {
                    task: TaskRecord {
                        id: row.get(0)?,
                        kind: row.get(1)?,
                        status: row.get(2)?,
                        output_path: row.get(3)?,
                        started_at: row.get(4)?,
                        finished_at: row.get(5)?,
                    },
                    input: row.get(6)?,
                    files: row.get::<_, i64>(7)? as usize,
                })
            },
        )
        .map_err(|e| e.to_string())?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    let has_more = rows.len() > PAGE_SIZE;
    rows.truncate(PAGE_SIZE);
    Ok(Page { rows, has_more })
}

pub fn password_page(options: &LibraryOptions, query: &Query) -> Result<Page<PasswordSummary>> {
    let db = database(options, false)?;
    let mut statement = db.connection().prepare(
        "SELECT id,source,pinned,disabled,success_count,failure_count,last_success_at,last_failure_at FROM passwords
         WHERE (?1='' OR (?1='enabled' AND disabled=0) OR (?1='disabled' AND disabled=1) OR (?1='pinned' AND pinned=1))
         AND (?2='' OR instr(lower(source),lower(?2))>0 OR CAST(id AS TEXT)=?2)
         ORDER BY pinned DESC,success_count DESC,COALESCE(last_success_at,'') DESC,failure_count ASC,id ASC LIMIT ?3 OFFSET ?4"
    ).map_err(|_| "无法读取密码库".to_owned())?;
    let mut rows = statement
        .query_map(
            (
                &query.status,
                &query.text,
                (PAGE_SIZE + 1) as i64,
                query.offset.min(i64::MAX as usize) as i64,
            ),
            |row| {
                Ok(PasswordSummary {
                    id: row.get(0)?,
                    masked: "••••••••",
                    source: row.get(1)?,
                    pinned: row.get(2)?,
                    disabled: row.get(3)?,
                    success_count: row.get(4)?,
                    failure_count: row.get(5)?,
                    last_success_at: row.get(6)?,
                    last_failure_at: row.get(7)?,
                })
            },
        )
        .map_err(|_| "无法读取密码库".to_owned())?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|_| "无法读取密码库".to_owned())?;
    let has_more = rows.len() > PAGE_SIZE;
    rows.truncate(PAGE_SIZE);
    Ok(Page { rows, has_more })
}

#[derive(Clone, Copy)]
pub enum PasswordAction {
    Pin(bool),
    Enable(bool),
    Cleanup,
    Delete,
}
pub fn change_passwords(
    options: &LibraryOptions,
    ids: &[i64],
    action: PasswordAction,
) -> Result<String> {
    let db = database(options, true)?;
    let transaction = db
        .connection()
        .unchecked_transaction()
        .map_err(|_| "无法修改密码库".to_owned())?;
    let mut updated = 0;
    for id in ids {
        updated += match action {
            PasswordAction::Pin(value) => transaction.execute(
                "UPDATE passwords SET pinned=?1,updated_at=CURRENT_TIMESTAMP WHERE id=?2",
                (value, id),
            ),
            PasswordAction::Enable(value) => transaction.execute(
                "UPDATE passwords SET disabled=?1,updated_at=CURRENT_TIMESTAMP WHERE id=?2",
                (!value, id),
            ),
            PasswordAction::Cleanup => transaction.execute("UPDATE passwords SET disabled=1,updated_at=CURRENT_TIMESTAMP WHERE id=?1 AND pinned=0", [id]),
            PasswordAction::Delete => {
                transaction.execute("DELETE FROM passwords WHERE id=?1", [id])
            }
        }
        .map_err(|_| "密码修改失败，本次操作已回滚".to_owned())?;
    }
    transaction
        .commit()
        .map_err(|_| "密码修改提交失败".to_owned())?;
    Ok(format!("已更新 {updated} 条密码记录"))
}

pub fn export_diagnostics(detail: &TaskDetail, path: &Path) -> Result<()> {
    let value = serde_json::json!({"task":detail.task,"files":detail.files,"events":detail.events});
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut file = tempfile::NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
    file.write_all(
        serde_json::to_string_pretty(&value)
            .map_err(|e| e.to_string())?
            .as_bytes(),
    )
    .map_err(|e| e.to_string())?;
    file.as_file().sync_all().map_err(|e| e.to_string())?;
    file.persist_noclobber(path)
        .map_err(|_| "目标已存在或不可写，请选择新文件".to_owned())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pages_search_entire_database_and_preserve_disabled_passwords() {
        let root = tempfile::tempdir().unwrap();
        let options = super::super::tests::options(root.path(), "read-write");
        for n in 0..61 {
            add_password(
                &options,
                &format!("secret-{n}"),
                if n == 60 { "unique-source" } else { "manual" },
                false,
            )
            .unwrap();
        }
        let first = password_page(&options, &Query::default()).unwrap();
        assert_eq!(first.rows.len(), PAGE_SIZE);
        assert!(first.has_more);
        let second = password_page(
            &options,
            &Query {
                offset: PAGE_SIZE,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(second.rows.len(), 11);
        assert!(!second.has_more);
        let found = password_page(
            &options,
            &Query {
                text: "unique-source".into(),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(found.rows.len(), 1);
        assert!(!format!("{found:?}").contains("secret-60"));
        change_passwords(&options, &[found.rows[0].id], PasswordAction::Enable(false)).unwrap();
        let disabled = password_page(
            &options,
            &Query {
                status: "disabled".into(),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(disabled.rows.len(), 1);
        change_passwords(&options, &[found.rows[0].id], PasswordAction::Enable(true)).unwrap();
        assert!(password_page(
            &options,
            &Query {
                status: "disabled".into(),
                ..Default::default()
            }
        )
        .unwrap()
        .rows
        .is_empty());
        assert!(password_page(
            &options,
            &Query {
                text: "' OR 1=1 --".into(),
                ..Default::default()
            }
        )
        .unwrap()
        .rows
        .is_empty());
    }
}
