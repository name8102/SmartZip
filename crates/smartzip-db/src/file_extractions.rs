//! Persistence for the `file_extractions` history table (v3).
//!
//! One row = **one extraction action**, not one file. The root input, each
//! nested archive, each carved embedded archive, and each skipped input all
//! get their own append-only row. This is the file-grain replacement for the
//! v2 `encoding_detections` / `embedded_archive_detections` tables: encoding
//! collapses into the `encoding` / `encoding_corrected` columns and carved
//! archives are ordinary rows disambiguated by `offset`.
//!
//! Actions remain in this history; supplemental path reports and diagnostics may be
//! attached to an existing action. Deduplication queries successful actions here.
//! Password and encoding hints live in `known_files`. Callers treat repo errors as
//! non-fatal (see the engine's best-effort recorder).

use crate::Result;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

/// Row appended for a single extraction action.
#[derive(Debug, Clone, PartialEq)]
pub struct NewFileExtraction<'a> {
    pub task_id: &'a str,
    pub input_path: &'a str,
    pub sample_hash: Option<&'a str>,
    pub file_size: Option<i64>,
    pub offset: Option<i64>,
    pub output_path: Option<&'a str>,
    pub has_password: bool,
    pub password_id: Option<i64>,
    pub status: &'a str,
    pub reason: Option<&'a str>,
    pub encoding: Option<&'a str>,
    pub encoding_corrected: bool,
    pub damaged_volumes_json: Option<&'a str>,
    pub test_report_json: Option<&'a str>,
    pub path_report_json: Option<&'a str>,
    pub path_reason: Option<&'a str>,
    pub created_at: &'a str,
}

/// Full row shape returned by list queries.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileExtractionRecord {
    pub id: i64,
    pub task_id: String,
    pub input_path: String,
    pub sample_hash: Option<String>,
    pub file_size: Option<i64>,
    pub offset: Option<i64>,
    pub output_path: Option<String>,
    pub has_password: bool,
    pub password_id: Option<i64>,
    pub status: String,
    pub reason: Option<String>,
    pub encoding: Option<String>,
    pub encoding_corrected: bool,
    pub damaged_volumes_json: Option<String>,
    pub test_report_json: Option<String>,
    pub path_report_json: Option<String>,
    pub path_reason: Option<String>,
    pub created_at: String,
}

pub struct FileExtractionRepository<'a> {
    conn: &'a Connection,
}

impl<'a> FileExtractionRepository<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    /// Whether this identity has a successful extraction in the existing history.
    /// Listing/testing and the location or existence of outputs do not count.
    pub fn was_extracted(&self, hash: &str, size: i64) -> Result<bool> {
        Ok(self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM file_extractions f JOIN tasks t ON t.id = f.task_id
             WHERE f.sample_hash = ?1 AND f.file_size = ?2
             AND f.status = 'extracted' AND t.kind = 'extract')",
            params![hash, size],
            |row| row.get(0),
        )?)
    }

    /// Append one extraction action.
    pub fn insert(&self, row: NewFileExtraction<'_>) -> Result<i64> {
        self.conn.execute(
            r#"
            INSERT INTO file_extractions(
                task_id, input_path, sample_hash, file_size, offset, output_path,
                has_password, password_id, status, reason, encoding,
                encoding_corrected, damaged_volumes_json, created_at, test_report_json, path_report_json, path_reason
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)
            "#,
            params![
                row.task_id,
                row.input_path,
                row.sample_hash,
                row.file_size,
                row.offset,
                row.output_path,
                row.has_password as i64,
                row.password_id,
                row.status,
                row.reason,
                row.encoding,
                row.encoding_corrected as i64,
                row.damaged_volumes_json,
                row.created_at,
                row.test_report_json,
                row.path_report_json,
                row.path_reason,
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Save the complete report independently of the bounded event log.
    /// Legacy actions have no node identity; only the newest such action is updated.
    pub fn set_path_outcome(
        &self,
        task_id: &str,
        node_id: Option<&str>,
        generation: u64,
        report_json: Option<&str>,
        path_reason: Option<&str>,
    ) -> Result<bool> {
        if let Some(report) = report_json {
            let _: serde_json::Value = serde_json::from_str(report)?;
        }
        let path_reason = path_reason.filter(|reason| is_path_reason(reason));
        let generation = i64::try_from(generation)
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
        let changed = self.conn.execute(
            "UPDATE file_extractions SET path_report_json=COALESCE(?1,path_report_json), \
             path_reason=COALESCE(?2,path_reason) WHERE id=(SELECT id FROM file_extractions \
             WHERE task_id=?3 AND ((?4 IS NOT NULL AND node_id=?4 AND generation=?5) \
             OR (?4 IS NULL AND node_id IS NULL)) ORDER BY id DESC LIMIT 1)",
            params![report_json, path_reason, task_id, node_id, generation],
        )?;
        Ok(changed == 1)
    }

    /// Attach supplemental outcome to the exact successfully inserted history action.
    pub fn set_path_outcome_by_id(
        &self,
        record_id: i64,
        report_json: Option<&str>,
        path_reason: Option<&str>,
    ) -> Result<bool> {
        if let Some(report) = report_json {
            let _: serde_json::Value = serde_json::from_str(report)?;
        }
        let path_reason = path_reason.filter(|reason| is_path_reason(reason));
        Ok(self.conn.execute(
            "UPDATE file_extractions SET path_report_json=COALESCE(?1,path_report_json), \
             path_reason=COALESCE(?2,path_reason) WHERE id=?3",
            params![report_json, path_reason, record_id],
        )? == 1)
    }

    /// A bounded page of report entries. The complete JSON remains in the database.
    pub fn path_report_page(
        &self,
        record_id: i64,
        offset: usize,
        limit: usize,
    ) -> Result<Vec<serde_json::Value>> {
        let mut statement = self.conn.prepare(
            "SELECT entries.value FROM file_extractions f, \
             json_each(f.path_report_json, '$.entries') entries \
             WHERE f.id=?1 ORDER BY CAST(entries.key AS INTEGER) LIMIT ?2 OFFSET ?3",
        )?;
        let texts = statement
            .query_map(
                params![
                    record_id,
                    limit.min(1000) as i64,
                    offset.min(i64::MAX as usize) as i64
                ],
                |row| row.get::<_, String>(0),
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        texts
            .into_iter()
            .map(|text| serde_json::from_str(&text).map_err(Into::into))
            .collect()
    }

    /// Every action logged for one task, oldest first.
    pub fn list_by_task(&self, task_id: &str) -> Result<Vec<FileExtractionRecord>> {
        self.query(
            r#"
            SELECT id, task_id, input_path, sample_hash, file_size, offset,
                   output_path, has_password, password_id, status, reason,
                   encoding, encoding_corrected, damaged_volumes_json, created_at, test_report_json, path_report_json, path_reason
            FROM file_extractions
            WHERE task_id = ?1
            ORDER BY id ASC
            "#,
            params![task_id],
        )
    }

    /// Most recent actions across all tasks, newest first.
    pub fn recent(&self, limit: usize) -> Result<Vec<FileExtractionRecord>> {
        self.query(
            r#"
            SELECT id, task_id, input_path, sample_hash, file_size, offset,
                   output_path, has_password, password_id, status, reason,
                   encoding, encoding_corrected, damaged_volumes_json, created_at, test_report_json, path_report_json, path_reason
            FROM file_extractions
            ORDER BY id DESC
            LIMIT ?1
            "#,
            params![limit as i64],
        )
    }

    /// Recent actions filtered by terminal status (uses `idx_..._status`).
    pub fn list_by_status(&self, status: &str, limit: usize) -> Result<Vec<FileExtractionRecord>> {
        self.query(
            r#"
            SELECT id, task_id, input_path, sample_hash, file_size, offset,
                   output_path, has_password, password_id, status, reason,
                   encoding, encoding_corrected, damaged_volumes_json, created_at, test_report_json, path_report_json, path_reason
            FROM file_extractions
            WHERE status = ?1
            ORDER BY id DESC
            LIMIT ?2
            "#,
            params![status, limit as i64],
        )
    }

    /// Recent actions filtered by skip/failure reason.
    pub fn list_by_reason(&self, reason: &str, limit: usize) -> Result<Vec<FileExtractionRecord>> {
        self.query(
            r#"
            SELECT id, task_id, input_path, sample_hash, file_size, offset,
                   output_path, has_password, password_id, status, reason,
                   encoding, encoding_corrected, damaged_volumes_json, created_at, test_report_json, path_report_json, path_reason
            FROM file_extractions
            WHERE reason = ?1
            ORDER BY id DESC
            LIMIT ?2
            "#,
            params![reason, limit as i64],
        )
    }

    /// Recent actions matching both status and reason filters.
    pub fn list_by_status_and_reason(
        &self,
        status: &str,
        reason: &str,
        limit: usize,
    ) -> Result<Vec<FileExtractionRecord>> {
        self.query(
            r#"
            SELECT id, task_id, input_path, sample_hash, file_size, offset,
                   output_path, has_password, password_id, status, reason,
                   encoding, encoding_corrected, damaged_volumes_json, created_at, test_report_json, path_report_json, path_reason
            FROM file_extractions
            WHERE status = ?1 AND reason = ?2
            ORDER BY id DESC
            LIMIT ?3
            "#,
            params![status, reason, limit as i64],
        )
    }

    fn query(&self, sql: &str, params: impl rusqlite::Params) -> Result<Vec<FileExtractionRecord>> {
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map(params, map_record)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }
}

fn is_path_reason(reason: &str) -> bool {
    matches!(
        reason,
        "name_too_long"
            | "path_too_long"
            | "invalid_name"
            | "name_collision"
            | "path_remap_unsupported"
            | "path_constraint_unknown"
    )
}

fn map_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<FileExtractionRecord> {
    Ok(FileExtractionRecord {
        id: row.get(0)?,
        task_id: row.get(1)?,
        input_path: row.get(2)?,
        sample_hash: row.get(3)?,
        file_size: row.get(4)?,
        offset: row.get(5)?,
        output_path: row.get(6)?,
        has_password: row.get::<_, i64>(7)? != 0,
        password_id: row.get(8)?,
        status: row.get(9)?,
        reason: row.get(10)?,
        encoding: row.get(11)?,
        encoding_corrected: row.get::<_, i64>(12)? != 0,
        damaged_volumes_json: row.get(13)?,
        test_report_json: row.get(15)?,
        path_report_json: row.get(16)?,
        path_reason: row.get(17)?,
        created_at: row.get(14)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task::{NewTask, TaskRepository};
    use crate::SmartZipDb;

    fn seed_task(db: &SmartZipDb, id: &str) {
        TaskRepository::new(db.connection())
            .insert(NewTask {
                id,
                kind: "extract",
                output_path: None,
                started_at: "2026-07-02T00:00:00Z",
            })
            .unwrap();
    }

    fn base<'a>(task_id: &'a str, input: &'a str, status: &'a str) -> NewFileExtraction<'a> {
        NewFileExtraction {
            task_id,
            input_path: input,
            sample_hash: None,
            file_size: None,
            offset: None,
            output_path: None,
            has_password: false,
            password_id: None,
            status,
            reason: None,
            encoding: None,
            encoding_corrected: false,
            damaged_volumes_json: None,
            test_report_json: None,
            path_report_json: None,
            path_reason: None,
            created_at: "2026-07-02T00:00:01Z",
        }
    }

    #[test]
    fn reuse_uses_only_successful_extraction_history_without_output_checks() {
        let db = SmartZipDb::in_memory().unwrap();
        seed_task(&db, "extract");
        TaskRepository::new(db.connection())
            .insert(NewTask {
                id: "list",
                kind: "list",
                output_path: None,
                started_at: "2026-07-02T00:00:00Z",
            })
            .unwrap();
        let repo = FileExtractionRepository::new(db.connection());
        for (task, status) in [
            ("list", "extracted"),
            ("extract", "failed"),
            ("extract", "skipped"),
        ] {
            repo.insert(NewFileExtraction {
                sample_hash: Some("h"),
                file_size: Some(42),
                ..base(task, "/a.zip", status)
            })
            .unwrap();
        }
        assert!(!repo.was_extracted("h", 42).unwrap());
        repo.insert(NewFileExtraction {
            sample_hash: Some("h"),
            file_size: Some(42),
            output_path: Some("/missing/moved-output"),
            ..base("extract", "/a.zip", "extracted")
        })
        .unwrap();
        assert!(repo.was_extracted("h", 42).unwrap());
        assert!(!repo.was_extracted("other", 42).unwrap());
        assert!(!repo.was_extracted("h", 43).unwrap());
        db.connection()
            .execute("DELETE FROM tasks WHERE id = 'extract'", [])
            .unwrap();
        assert!(!repo.was_extracted("h", 42).unwrap());
    }

    #[test]
    fn insert_and_list_by_task_preserves_order() {
        let db = SmartZipDb::in_memory().unwrap();
        seed_task(&db, "t1");
        let repo = FileExtractionRepository::new(db.connection());
        repo.insert(base("t1", "/a.zip", "extracted")).unwrap();
        repo.insert(NewFileExtraction {
            offset: Some(4096),
            status: "extracted",
            ..base("t1", "/a.zip", "extracted")
        })
        .unwrap();

        let rows = repo.list_by_task("t1").unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].offset, None);
        assert_eq!(rows[1].offset, Some(4096));
    }

    #[test]
    fn filter_by_status_and_reason() {
        let db = SmartZipDb::in_memory().unwrap();
        seed_task(&db, "t1");
        let repo = FileExtractionRepository::new(db.connection());
        repo.insert(base("t1", "/ok.zip", "extracted")).unwrap();
        repo.insert(NewFileExtraction {
            reason: Some("duplicate"),
            ..base("t1", "/dup.zip", "skipped")
        })
        .unwrap();
        repo.insert(NewFileExtraction {
            reason: Some("wrong_password"),
            ..base("t1", "/bad.zip", "failed")
        })
        .unwrap();

        assert_eq!(repo.list_by_status("skipped", 10).unwrap().len(), 1);
        assert_eq!(repo.list_by_status("extracted", 10).unwrap().len(), 1);
        let by_reason = repo.list_by_reason("wrong_password", 10).unwrap();
        assert_eq!(by_reason.len(), 1);
        assert_eq!(by_reason[0].input_path, "/bad.zip");
        let combined = repo
            .list_by_status_and_reason("skipped", "duplicate", 10)
            .unwrap();
        assert_eq!(combined.len(), 1);
        assert_eq!(combined[0].input_path, "/dup.zip");
    }

    #[test]
    fn full_mapping_survives_without_events_and_pages_without_truncation() {
        let db = SmartZipDb::in_memory().unwrap();
        seed_task(&db, "task");
        let repo = FileExtractionRepository::new(db.connection());
        let id = repo
            .insert(base("task", "/input.zip", "extracted"))
            .unwrap();
        let entries = (0..4100).map(|id| serde_json::json!({"id":id,"display_name":format!("original-{id}"),"final_relative":format!("final-{id}")})).collect::<Vec<_>>();
        let report = serde_json::json!({"entries":entries}).to_string();
        assert!(repo
            .set_path_outcome("task", None, 0, Some(&report), Some("name_too_long"))
            .unwrap());
        let record = repo.list_by_task("task").unwrap().pop().unwrap();
        assert_eq!(record.path_report_json.as_deref(), Some(report.as_str()));
        assert_eq!(record.path_reason.as_deref(), Some("name_too_long"));
        let page = repo.path_report_page(id, 4095, 50).unwrap();
        assert_eq!(page.len(), 5);
        assert_eq!(page.last().unwrap()["id"], 4099);
        assert!(!repo
            .set_path_outcome("task", Some("unknown"), 0, Some(&report), None)
            .unwrap());
        assert!(repo
            .set_path_outcome("task", None, 0, Some("invalid"), None)
            .is_err());
    }

    #[test]
    fn supplemental_path_outcome_targets_only_the_inserted_legacy_action() {
        let db = SmartZipDb::in_memory().unwrap();
        seed_task(&db, "task");
        let repo = FileExtractionRepository::new(db.connection());
        let previous = repo
            .insert(base("task", "/previous.zip", "extracted"))
            .unwrap();
        let current = repo
            .insert(base("task", "/current.zip", "extracted"))
            .unwrap();
        assert!(repo
            .set_path_outcome_by_id(previous, None, Some("wrong_password"))
            .unwrap());
        assert!(repo
            .set_path_outcome_by_id(current, Some("{\"digest\":\"current-report\"}"), None)
            .unwrap());
        assert!(repo
            .set_path_outcome_by_id(current, None, Some("name_too_long"))
            .unwrap());
        assert!(!repo
            .set_path_outcome_by_id(current + 100, Some("{}"), None)
            .unwrap());
        let records = repo.list_by_task("task").unwrap();
        assert_eq!(records[0].id, previous);
        assert!(records[0].path_report_json.is_none());
        assert!(records[0].path_reason.is_none());
        assert_eq!(
            records[1].path_report_json.as_deref(),
            Some("{\"digest\":\"current-report\"}")
        );
        assert_eq!(records[1].path_reason.as_deref(), Some("name_too_long"));
    }

    #[test]
    fn recent_is_newest_first() {
        let db = SmartZipDb::in_memory().unwrap();
        seed_task(&db, "t1");
        let repo = FileExtractionRepository::new(db.connection());
        let first = repo.insert(base("t1", "/one.zip", "extracted")).unwrap();
        let second = repo.insert(base("t1", "/two.zip", "extracted")).unwrap();
        let recent = repo.recent(10).unwrap();
        assert_eq!(recent[0].id, second);
        assert_eq!(recent[1].id, first);
    }

    #[test]
    fn cascade_delete_removes_rows_with_task() {
        let db = SmartZipDb::in_memory().unwrap();
        seed_task(&db, "t1");
        let repo = FileExtractionRepository::new(db.connection());
        repo.insert(base("t1", "/a.zip", "extracted")).unwrap();
        db.connection()
            .execute("DELETE FROM tasks WHERE id = 't1'", [])
            .unwrap();
        assert!(repo.list_by_task("t1").unwrap().is_empty());
    }
}
