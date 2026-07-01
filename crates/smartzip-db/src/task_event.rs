//! Persistence for the `task_events` history table.
//!
//! Each engine event lands here with a `level` (info / warn / error), an
//! `event_type` matching the [`TaskEventKind`] variant name, a
//! human-readable `message`, and an optional serialized JSON payload for
//! structured details. Consumers (the CLI `history show` command and the
//! future GUI history pane) reconstruct a timeline by ordering rows by
//! `created_at` within a task.

use crate::Result;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

/// Severity classification used when persisting an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskEventLevel {
    Info,
    Warn,
    Error,
}

impl TaskEventLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
        }
    }
}

/// Row inserted for each event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewTaskEvent<'a> {
    pub task_id: &'a str,
    pub level: TaskEventLevel,
    pub event_type: &'a str,
    pub message: &'a str,
    pub data_json: Option<&'a str>,
    pub created_at: &'a str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskEventRecord {
    pub id: i64,
    pub task_id: String,
    pub level: String,
    pub event_type: String,
    pub message: String,
    pub data_json: Option<String>,
    pub created_at: String,
}

pub struct TaskEventRepository<'a> {
    conn: &'a Connection,
}

impl<'a> TaskEventRepository<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    pub fn insert(&self, event: NewTaskEvent<'_>) -> Result<i64> {
        self.conn.execute(
            r#"
            INSERT INTO task_events(task_id, level, event_type, message, data_json, created_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6)
            "#,
            params![
                event.task_id,
                event.level.as_str(),
                event.event_type,
                event.message,
                event.data_json,
                event.created_at,
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Fetch every event for a task, oldest first.
    pub fn list_by_task(&self, task_id: &str) -> Result<Vec<TaskEventRecord>> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT id, task_id, level, event_type, message, data_json, created_at
            FROM task_events
            WHERE task_id = ?1
            ORDER BY created_at ASC, id ASC
            "#,
        )?;
        let rows = stmt.query_map(params![task_id], map_event)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }
}

fn map_event(row: &rusqlite::Row<'_>) -> rusqlite::Result<TaskEventRecord> {
    Ok(TaskEventRecord {
        id: row.get(0)?,
        task_id: row.get(1)?,
        level: row.get(2)?,
        event_type: row.get(3)?,
        message: row.get(4)?,
        data_json: row.get(5)?,
        created_at: row.get(6)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task::{NewTask, TaskRepository};
    use crate::SmartZipDb;

    #[test]
    fn insert_and_list_preserves_order() {
        let db = SmartZipDb::in_memory().unwrap();
        // Task rows must exist before events can reference them.
        TaskRepository::new(db.connection())
            .insert(NewTask {
                id: "task-events",
                kind: "extract",
                input_summary: "a.zip",
                output_path: None,
                started_at: "2026-07-02T00:00:00Z",
            })
            .unwrap();

        let repo = TaskEventRepository::new(db.connection());
        for (idx, (ty, msg, at)) in [
            ("Started", "started", "2026-07-02T00:00:01Z"),
            ("Progress", "extracting", "2026-07-02T00:00:02Z"),
            ("Completed", "completed", "2026-07-02T00:00:03Z"),
        ]
        .iter()
        .enumerate()
        {
            repo.insert(NewTaskEvent {
                task_id: "task-events",
                level: if idx == 0 {
                    TaskEventLevel::Info
                } else {
                    TaskEventLevel::Info
                },
                event_type: ty,
                message: msg,
                data_json: None,
                created_at: at,
            })
            .unwrap();
        }
        let events = repo.list_by_task("task-events").unwrap();
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].event_type, "Started");
        assert_eq!(events[2].event_type, "Completed");
    }

    #[test]
    fn cascade_delete_removes_events_when_task_removed() {
        let db = SmartZipDb::in_memory().unwrap();
        TaskRepository::new(db.connection())
            .insert(NewTask {
                id: "task-cascade",
                kind: "extract",
                input_summary: "a.zip",
                output_path: None,
                started_at: "2026-07-02T00:00:00Z",
            })
            .unwrap();
        TaskEventRepository::new(db.connection())
            .insert(NewTaskEvent {
                task_id: "task-cascade",
                level: TaskEventLevel::Info,
                event_type: "Started",
                message: "s",
                data_json: None,
                created_at: "2026-07-02T00:00:01Z",
            })
            .unwrap();
        db.connection()
            .execute("DELETE FROM tasks WHERE id = 'task-cascade'", [])
            .unwrap();
        let events = TaskEventRepository::new(db.connection())
            .list_by_task("task-cascade")
            .unwrap();
        assert!(events.is_empty(), "cascade delete should clear events");
    }
}
