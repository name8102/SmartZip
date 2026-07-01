//! Persistence for the `embedded_archive_detections` history table.
//!
//! Each scan that surfaces embedded archive candidates records one row per
//! finding so later runs can quickly answer "did we already scan this file
//! and, if so, what did we see?". Rows are inserted in bulk via a single
//! transaction because a deep scan of one file can produce a handful of
//! findings and per-row commits would multiply write amplification.

use crate::Result;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq)]
pub struct NewEmbeddedArchiveDetection<'a> {
    pub file_path_hash: &'a str,
    pub format: &'a str,
    pub offset: u64,
    pub confidence: f32,
    pub size_hint: Option<u64>,
    pub created_at: &'a str,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EmbeddedArchiveDetectionRecord {
    pub id: i64,
    pub file_path_hash: String,
    pub format: String,
    pub offset: u64,
    pub confidence: f32,
    pub size_hint: Option<u64>,
    pub created_at: String,
}

pub struct EmbeddedArchiveDetectionRepository<'a> {
    conn: &'a Connection,
}

impl<'a> EmbeddedArchiveDetectionRepository<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    /// Insert a batch of findings in a single transaction.
    ///
    /// An empty `findings` slice is a no-op — the transaction is skipped,
    /// so callers can pass through scan results without needing to check.
    pub fn insert_many(&self, findings: &[NewEmbeddedArchiveDetection<'_>]) -> Result<usize> {
        if findings.is_empty() {
            return Ok(0);
        }
        self.conn.execute_batch("BEGIN")?;
        let result: rusqlite::Result<usize> = (|| {
            let mut stmt = self.conn.prepare(
                r#"
                INSERT INTO embedded_archive_detections(
                    file_path_hash, format, offset, confidence, size_hint, created_at
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                "#,
            )?;
            let mut inserted = 0;
            for finding in findings {
                stmt.execute(params![
                    finding.file_path_hash,
                    finding.format,
                    finding.offset as i64,
                    finding.confidence as f64,
                    finding.size_hint.map(|value| value as i64),
                    finding.created_at,
                ])?;
                inserted += 1;
            }
            Ok(inserted)
        })();
        match result {
            Ok(count) => {
                self.conn.execute_batch("COMMIT")?;
                Ok(count)
            }
            Err(error) => {
                let _ = self.conn.execute_batch("ROLLBACK");
                Err(error.into())
            }
        }
    }

    pub fn recent_by_hash(
        &self,
        file_path_hash: &str,
        limit: usize,
    ) -> Result<Vec<EmbeddedArchiveDetectionRecord>> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT id, file_path_hash, format, offset, confidence, size_hint, created_at
            FROM embedded_archive_detections
            WHERE file_path_hash = ?1
            ORDER BY id DESC
            LIMIT ?2
            "#,
        )?;
        let rows = stmt.query_map(params![file_path_hash, limit as i64], map_record)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }
}

fn map_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<EmbeddedArchiveDetectionRecord> {
    Ok(EmbeddedArchiveDetectionRecord {
        id: row.get(0)?,
        file_path_hash: row.get(1)?,
        format: row.get(2)?,
        offset: row.get::<_, i64>(3)? as u64,
        confidence: row.get::<_, f64>(4)? as f32,
        size_hint: row.get::<_, Option<i64>>(5)?.map(|v| v as u64),
        created_at: row.get(6)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SmartZipDb;

    #[test]
    fn insert_many_persists_all_rows() {
        let db = SmartZipDb::in_memory().unwrap();
        let repo = EmbeddedArchiveDetectionRepository::new(db.connection());
        let inserted = repo
            .insert_many(&[
                NewEmbeddedArchiveDetection {
                    file_path_hash: "hash",
                    format: "zip",
                    offset: 0,
                    confidence: 0.8,
                    size_hint: Some(1024),
                    created_at: "2026-07-02T00:00:00Z",
                },
                NewEmbeddedArchiveDetection {
                    file_path_hash: "hash",
                    format: "rar",
                    offset: 2048,
                    confidence: 0.6,
                    size_hint: None,
                    created_at: "2026-07-02T00:00:00Z",
                },
            ])
            .unwrap();
        assert_eq!(inserted, 2);
        let rows = repo.recent_by_hash("hash", 5).unwrap();
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().any(|r| r.format == "rar" && r.offset == 2048));
        assert!(rows.iter().any(|r| r.format == "zip" && r.size_hint == Some(1024)));
    }

    #[test]
    fn empty_batch_is_noop() {
        let db = SmartZipDb::in_memory().unwrap();
        let repo = EmbeddedArchiveDetectionRepository::new(db.connection());
        assert_eq!(repo.insert_many(&[]).unwrap(), 0);
    }
}
