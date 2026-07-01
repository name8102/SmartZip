//! Persistence for the `tasks` history table.
//!
//! Rows are created when a task starts and updated on completion with a
//! terminal status, aggregated password-attempt counts, selected encoding,
//! and embedded-finding tallies. The engine wires this up through a
//! recorder trait; callers should treat repo errors as non-fatal and
//! surface them as warnings rather than aborting extraction.

use crate::Result;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

/// Terminal status recorded when a task finishes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskStatus {
    Running,
    Completed,
    Partial,
    Failed,
    Cancelled,
}

impl TaskStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Partial => "partial",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

/// Row inserted at task start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewTask<'a> {
    pub id: &'a str,
    pub kind: &'a str,
    pub input_summary: &'a str,
    pub output_path: Option<&'a str>,
    pub started_at: &'a str,
}

/// Fields updated when a task finishes.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TaskFinish<'a> {
    pub status: Option<TaskStatus>,
    pub finished_at: Option<&'a str>,
    pub error_code: Option<&'a str>,
    pub error_message: Option<&'a str>,
    pub password_attempts: Option<i64>,
    pub encoding_selected: Option<&'a str>,
    pub embedded_found: Option<i64>,
    pub output_path: Option<&'a str>,
}

/// Full row shape returned by list/find queries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskRecord {
    pub id: String,
    pub kind: String,
    pub status: String,
    pub input_summary: String,
    pub output_path: Option<String>,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub password_attempts: i64,
    pub encoding_selected: Option<String>,
    pub embedded_found: i64,
}

pub struct TaskRepository<'a> {
    conn: &'a Connection,
}

impl<'a> TaskRepository<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    /// Insert a fresh task row in the `running` state.
    pub fn insert(&self, new_task: NewTask<'_>) -> Result<()> {
        self.conn.execute(
            r#"
            INSERT INTO tasks(id, kind, status, input_summary, output_path, started_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6)
            "#,
            params![
                new_task.id,
                new_task.kind,
                TaskStatus::Running.as_str(),
                new_task.input_summary,
                new_task.output_path,
                new_task.started_at,
            ],
        )?;
        Ok(())
    }

    /// Update the fields recorded at task completion.
    ///
    /// Only fields set to `Some` are touched; the rest keep their prior
    /// value. This lets the engine drip metrics onto the row as they
    /// become known without a separate write per column.
    pub fn finish(&self, id: &str, finish: TaskFinish<'_>) -> Result<()> {
        let mut sets: Vec<&'static str> = Vec::new();
        let mut values: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

        if let Some(status) = finish.status {
            sets.push("status = ?");
            values.push(Box::new(status.as_str().to_string()));
        }
        if let Some(finished_at) = finish.finished_at {
            sets.push("finished_at = ?");
            values.push(Box::new(finished_at.to_string()));
        }
        if let Some(error_code) = finish.error_code {
            sets.push("error_code = ?");
            values.push(Box::new(error_code.to_string()));
        }
        if let Some(error_message) = finish.error_message {
            sets.push("error_message = ?");
            values.push(Box::new(error_message.to_string()));
        }
        if let Some(attempts) = finish.password_attempts {
            sets.push("password_attempts = ?");
            values.push(Box::new(attempts));
        }
        if let Some(encoding) = finish.encoding_selected {
            sets.push("encoding_selected = ?");
            values.push(Box::new(encoding.to_string()));
        }
        if let Some(count) = finish.embedded_found {
            sets.push("embedded_found = ?");
            values.push(Box::new(count));
        }
        if let Some(output_path) = finish.output_path {
            sets.push("output_path = ?");
            values.push(Box::new(output_path.to_string()));
        }

        if sets.is_empty() {
            return Ok(());
        }

        let sql = format!("UPDATE tasks SET {} WHERE id = ?", sets.join(", "));
        values.push(Box::new(id.to_string()));

        let params: Vec<&dyn rusqlite::ToSql> = values.iter().map(|b| b.as_ref()).collect();
        self.conn.execute(&sql, params.as_slice())?;
        Ok(())
    }

    /// Return the most recent tasks, newest first.
    pub fn recent(&self, limit: usize) -> Result<Vec<TaskRecord>> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT id, kind, status, input_summary, output_path, started_at,
                   finished_at, error_code, error_message, password_attempts,
                   encoding_selected, embedded_found
            FROM tasks
            ORDER BY started_at DESC, id DESC
            LIMIT ?1
            "#,
        )?;
        let rows = stmt.query_map(params![limit as i64], map_task_record)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub fn find_by_id(&self, id: &str) -> Result<Option<TaskRecord>> {
        self.conn
            .query_row(
                r#"
                SELECT id, kind, status, input_summary, output_path, started_at,
                       finished_at, error_code, error_message, password_attempts,
                       encoding_selected, embedded_found
                FROM tasks
                WHERE id = ?1
                "#,
                params![id],
                map_task_record,
            )
            .optional()
            .map_err(Into::into)
    }
}

fn map_task_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<TaskRecord> {
    Ok(TaskRecord {
        id: row.get(0)?,
        kind: row.get(1)?,
        status: row.get(2)?,
        input_summary: row.get(3)?,
        output_path: row.get(4)?,
        started_at: row.get(5)?,
        finished_at: row.get(6)?,
        error_code: row.get(7)?,
        error_message: row.get(8)?,
        password_attempts: row.get(9)?,
        encoding_selected: row.get(10)?,
        embedded_found: row.get(11)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SmartZipDb;

    fn open() -> SmartZipDb {
        SmartZipDb::in_memory().unwrap()
    }

    #[test]
    fn insert_and_find_task() {
        let db = open();
        let repo = TaskRepository::new(db.connection());
        repo.insert(NewTask {
            id: "task-1",
            kind: "extract",
            input_summary: "a.zip, b.zip",
            output_path: Some("/tmp/out"),
            started_at: "2026-07-02T00:00:00Z",
        })
        .unwrap();

        let record = repo.find_by_id("task-1").unwrap().unwrap();
        assert_eq!(record.status, "running");
        assert_eq!(record.kind, "extract");
        assert_eq!(record.output_path.as_deref(), Some("/tmp/out"));
    }

    #[test]
    fn finish_updates_only_supplied_fields() {
        let db = open();
        let repo = TaskRepository::new(db.connection());
        repo.insert(NewTask {
            id: "task-2",
            kind: "extract",
            input_summary: "a.zip",
            output_path: None,
            started_at: "2026-07-02T00:00:00Z",
        })
        .unwrap();

        repo.finish(
            "task-2",
            TaskFinish {
                status: Some(TaskStatus::Completed),
                finished_at: Some("2026-07-02T00:00:05Z"),
                password_attempts: Some(3),
                encoding_selected: Some("gb18030"),
                embedded_found: Some(2),
                ..TaskFinish::default()
            },
        )
        .unwrap();

        let record = repo.find_by_id("task-2").unwrap().unwrap();
        assert_eq!(record.status, "completed");
        assert_eq!(record.finished_at.as_deref(), Some("2026-07-02T00:00:05Z"));
        assert_eq!(record.password_attempts, 3);
        assert_eq!(record.encoding_selected.as_deref(), Some("gb18030"));
        assert_eq!(record.embedded_found, 2);
        assert!(record.error_code.is_none());
    }

    #[test]
    fn recent_returns_newest_first() {
        let db = open();
        let repo = TaskRepository::new(db.connection());
        for (id, started_at) in [
            ("task-a", "2026-07-01T00:00:00Z"),
            ("task-b", "2026-07-02T00:00:00Z"),
            ("task-c", "2026-06-30T00:00:00Z"),
        ] {
            repo.insert(NewTask {
                id,
                kind: "extract",
                input_summary: "x.zip",
                output_path: None,
                started_at,
            })
            .unwrap();
        }
        let recent = repo.recent(10).unwrap();
        assert_eq!(recent[0].id, "task-b");
        assert_eq!(recent[1].id, "task-a");
        assert_eq!(recent[2].id, "task-c");
    }
}
