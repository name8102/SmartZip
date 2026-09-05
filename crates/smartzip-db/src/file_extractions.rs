//! Persistence for the `file_extractions` history table (v3).
//!
//! One row = **one extraction action**, not one file. The root input, each
//! nested archive, each carved embedded archive, and each skipped input all
//! get their own append-only row. This is the file-grain replacement for the
//! v2 `encoding_detections` / `embedded_archive_detections` tables: encoding
//! collapses into the `encoding` / `encoding_corrected` columns and carved
//! archives are ordinary rows disambiguated by `offset`.
//!
//! The table is write-once — there is no update path. Reuse/dedup state that
//! *does* mutate lives in `known_files` instead. Callers treat repo errors as
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
    pub created_at: String,
}

pub struct FileExtractionRepository<'a> {
    conn: &'a Connection,
}

impl<'a> FileExtractionRepository<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    /// Append one extraction action. No update path exists by design.
    pub fn insert(&self, row: NewFileExtraction<'_>) -> Result<i64> {
        self.conn.execute(
            r#"
            INSERT INTO file_extractions(
                task_id, input_path, sample_hash, file_size, offset, output_path,
                has_password, password_id, status, reason, encoding,
                encoding_corrected, damaged_volumes_json, created_at, test_report_json
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
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
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Every action logged for one task, oldest first.
    pub fn list_by_task(&self, task_id: &str) -> Result<Vec<FileExtractionRecord>> {
        self.query(
            r#"
            SELECT id, task_id, input_path, sample_hash, file_size, offset,
                   output_path, has_password, password_id, status, reason,
                   encoding, encoding_corrected, damaged_volumes_json, created_at, test_report_json
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
                   encoding, encoding_corrected, damaged_volumes_json, created_at, test_report_json
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
                   encoding, encoding_corrected, damaged_volumes_json, created_at, test_report_json
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
                   encoding, encoding_corrected, damaged_volumes_json, created_at, test_report_json
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
                   encoding, encoding_corrected, damaged_volumes_json, created_at, test_report_json
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
            created_at: "2026-07-02T00:00:01Z",
        }
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
