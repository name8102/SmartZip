//! Persistence for the `encoding_detections` history table.
//!
//! Each archive that goes through ZIP encoding detection records the
//! selected encoding, the raw candidate list (as JSON), and whether the
//! user overrode the automatic choice. The `archive_path_hash` column is
//! populated from [`crate::path_hash::hash_path`] so paths can be looked
//! up without leaking them to the database in cleartext.

use crate::Result;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq)]
pub struct NewEncodingDetection<'a> {
    pub archive_path_hash: &'a str,
    pub archive_format: Option<&'a str>,
    pub selected_encoding: &'a str,
    pub confidence: f32,
    pub user_corrected: bool,
    pub candidates_json: &'a str,
    pub created_at: &'a str,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EncodingDetectionRecord {
    pub id: i64,
    pub archive_path_hash: String,
    pub archive_format: Option<String>,
    pub selected_encoding: String,
    pub confidence: f32,
    pub user_corrected: bool,
    pub candidates_json: String,
    pub created_at: String,
}

pub struct EncodingDetectionRepository<'a> {
    conn: &'a Connection,
}

impl<'a> EncodingDetectionRepository<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    pub fn insert(&self, detection: NewEncodingDetection<'_>) -> Result<i64> {
        self.conn.execute(
            r#"
            INSERT INTO encoding_detections(
                archive_path_hash,
                archive_format,
                selected_encoding,
                confidence,
                user_corrected,
                candidates_json,
                created_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            "#,
            params![
                detection.archive_path_hash,
                detection.archive_format,
                detection.selected_encoding,
                detection.confidence as f64,
                detection.user_corrected as i64,
                detection.candidates_json,
                detection.created_at,
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn recent_by_hash(
        &self,
        archive_path_hash: &str,
        limit: usize,
    ) -> Result<Vec<EncodingDetectionRecord>> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT id, archive_path_hash, archive_format, selected_encoding,
                   confidence, user_corrected, candidates_json, created_at
            FROM encoding_detections
            WHERE archive_path_hash = ?1
            ORDER BY id DESC
            LIMIT ?2
            "#,
        )?;
        let rows = stmt.query_map(params![archive_path_hash, limit as i64], map_record)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }
}

fn map_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<EncodingDetectionRecord> {
    Ok(EncodingDetectionRecord {
        id: row.get(0)?,
        archive_path_hash: row.get(1)?,
        archive_format: row.get(2)?,
        selected_encoding: row.get(3)?,
        confidence: row.get::<_, f64>(4)? as f32,
        user_corrected: row.get::<_, i64>(5)? != 0,
        candidates_json: row.get(6)?,
        created_at: row.get(7)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SmartZipDb;

    #[test]
    fn insert_and_lookup_by_hash() {
        let db = SmartZipDb::in_memory().unwrap();
        let repo = EncodingDetectionRepository::new(db.connection());
        repo.insert(NewEncodingDetection {
            archive_path_hash: "abc",
            archive_format: Some("zip"),
            selected_encoding: "GB18030",
            confidence: 0.87,
            user_corrected: false,
            candidates_json: r#"[{"name":"GB18030","confidence":0.87}]"#,
            created_at: "2026-07-02T00:00:00Z",
        })
        .unwrap();
        let rows = repo.recent_by_hash("abc", 5).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].selected_encoding, "GB18030");
        assert!((rows[0].confidence - 0.87).abs() < 1e-6);
    }

    #[test]
    fn recent_returns_most_recent_first() {
        let db = SmartZipDb::in_memory().unwrap();
        let repo = EncodingDetectionRepository::new(db.connection());
        for (encoding, at) in [
            ("GBK", "2026-07-02T00:00:00Z"),
            ("GB18030", "2026-07-02T00:00:01Z"),
        ] {
            repo.insert(NewEncodingDetection {
                archive_path_hash: "same",
                archive_format: Some("zip"),
                selected_encoding: encoding,
                confidence: 0.5,
                user_corrected: false,
                candidates_json: "[]",
                created_at: at,
            })
            .unwrap();
        }
        let rows = repo.recent_by_hash("same", 5).unwrap();
        assert_eq!(rows[0].selected_encoding, "GB18030");
        assert_eq!(rows[1].selected_encoding, "GBK");
    }
}
