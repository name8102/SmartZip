use crate::Result;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PasswordRecord {
    pub id: i64,
    pub value: String,
    pub source: String,
    pub pinned: bool,
    pub disabled: bool,
    pub success_count: i64,
    pub failure_count: i64,
    pub last_success_at: Option<String>,
    pub last_failure_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewPassword<'a> {
    pub value: &'a str,
    pub source: &'a str,
    pub pinned: bool,
}

pub struct PasswordRepository<'a> {
    conn: &'a Connection,
}

impl<'a> PasswordRepository<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    pub fn upsert(&self, input: NewPassword<'_>) -> Result<i64> {
        self.conn.execute(
            r#"
            INSERT INTO passwords(value, source, pinned)
            VALUES (?1, ?2, ?3)
            ON CONFLICT(value) DO UPDATE SET
                source = excluded.source,
                pinned = passwords.pinned OR excluded.pinned,
                disabled = 0,
                updated_at = CURRENT_TIMESTAMP
            "#,
            params![input.value, input.source, input.pinned as i64],
        )?;
        Ok(self.conn.query_row(
            "SELECT id FROM passwords WHERE value = ?1",
            params![input.value],
            |row| row.get(0),
        )?)
    }

    pub fn get_by_value(&self, value: &str) -> Result<Option<PasswordRecord>> {
        self.conn
            .query_row(
                "SELECT id, value, source, pinned, disabled, success_count, failure_count, last_success_at, last_failure_at FROM passwords WHERE value = ?1",
                params![value],
                map_password_record,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn ranked_candidates(&self, limit: usize) -> Result<Vec<PasswordRecord>> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT id, value, source, pinned, disabled, success_count, failure_count, last_success_at, last_failure_at
            FROM passwords
            WHERE disabled = 0
            ORDER BY pinned DESC,
                     success_count DESC,
                     COALESCE(last_success_at, '') DESC,
                     failure_count ASC,
                     id ASC
            LIMIT ?1
            "#,
        )?;
        let rows = stmt.query_map(params![limit as i64], map_password_record)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub fn record_success(&self, id: i64) -> Result<()> {
        self.conn.execute(
            r#"
            UPDATE passwords
            SET success_count = success_count + 1,
                last_success_at = CURRENT_TIMESTAMP,
                updated_at = CURRENT_TIMESTAMP
            WHERE id = ?1
            "#,
            params![id],
        )?;
        Ok(())
    }

    pub fn record_failure(&self, id: i64) -> Result<()> {
        self.conn.execute(
            r#"
            UPDATE passwords
            SET failure_count = failure_count + 1,
                last_failure_at = CURRENT_TIMESTAMP,
                updated_at = CURRENT_TIMESTAMP
            WHERE id = ?1
            "#,
            params![id],
        )?;
        Ok(())
    }

    pub fn disable(&self, id: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE passwords SET disabled = 1, updated_at = CURRENT_TIMESTAMP WHERE id = ?1",
            params![id],
        )?;
        Ok(())
    }

    pub fn delete(&self, id: i64) -> Result<()> {
        self.conn
            .execute("DELETE FROM passwords WHERE id = ?1", params![id])?;
        Ok(())
    }

    /// Record a successful match between a password and an archive shape.
    ///
    /// `archive_format` is the SmartZip format label (`"zip"`, `"rar"`, …);
    /// `filename_pattern` is the normalized archive stem (see engine callers).
    /// Rows are upserted so repeat successes bump `success_count` and refresh
    /// `last_success_at` rather than accumulating duplicates.
    pub fn record_match_success(
        &self,
        password_id: i64,
        archive_format: Option<&str>,
        filename_pattern: Option<&str>,
    ) -> Result<()> {
        let existing: Option<i64> = self
            .conn
            .query_row(
                r#"
                SELECT id FROM password_matches
                WHERE password_id = ?1
                  AND COALESCE(archive_format, '') = COALESCE(?2, '')
                  AND COALESCE(filename_pattern, '') = COALESCE(?3, '')
                "#,
                params![password_id, archive_format, filename_pattern],
                |row| row.get(0),
            )
            .optional()?;

        match existing {
            Some(id) => {
                self.conn.execute(
                    r#"
                    UPDATE password_matches
                    SET success_count = success_count + 1,
                        last_success_at = CURRENT_TIMESTAMP
                    WHERE id = ?1
                    "#,
                    params![id],
                )?;
            }
            None => {
                self.conn.execute(
                    r#"
                    INSERT INTO password_matches(
                        password_id, archive_format, filename_pattern,
                        success_count, last_success_at
                    ) VALUES (?1, ?2, ?3, 1, CURRENT_TIMESTAMP)
                    "#,
                    params![password_id, archive_format, filename_pattern],
                )?;
            }
        }
        Ok(())
    }

    /// Record a wrong-password event for the same match tuple as
    /// [`record_match_success`]. Only call this on confirmed
    /// `SmartZipError::WrongPassword` results.
    pub fn record_match_failure(
        &self,
        password_id: i64,
        archive_format: Option<&str>,
        filename_pattern: Option<&str>,
    ) -> Result<()> {
        let existing: Option<i64> = self
            .conn
            .query_row(
                r#"
                SELECT id FROM password_matches
                WHERE password_id = ?1
                  AND COALESCE(archive_format, '') = COALESCE(?2, '')
                  AND COALESCE(filename_pattern, '') = COALESCE(?3, '')
                "#,
                params![password_id, archive_format, filename_pattern],
                |row| row.get(0),
            )
            .optional()?;

        match existing {
            Some(id) => {
                self.conn.execute(
                    r#"
                    UPDATE password_matches
                    SET failure_count = failure_count + 1,
                        last_failure_at = CURRENT_TIMESTAMP
                    WHERE id = ?1
                    "#,
                    params![id],
                )?;
            }
            None => {
                self.conn.execute(
                    r#"
                    INSERT INTO password_matches(
                        password_id, archive_format, filename_pattern,
                        failure_count, last_failure_at
                    ) VALUES (?1, ?2, ?3, 1, CURRENT_TIMESTAMP)
                    "#,
                    params![password_id, archive_format, filename_pattern],
                )?;
            }
        }
        Ok(())
    }

    /// Return match rows for a password, most successful first, useful for
    /// diagnostics and future path/filename-similarity ranking.
    pub fn matches_for(&self, password_id: i64) -> Result<Vec<PasswordMatch>> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT id, password_id, archive_format, path_pattern, filename_pattern,
                   success_count, failure_count, last_success_at, last_failure_at
            FROM password_matches
            WHERE password_id = ?1
            ORDER BY success_count DESC, id ASC
            "#,
        )?;
        let rows = stmt.query_map(params![password_id], map_match)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PasswordMatch {
    pub id: i64,
    pub password_id: i64,
    pub archive_format: Option<String>,
    pub path_pattern: Option<String>,
    pub filename_pattern: Option<String>,
    pub success_count: i64,
    pub failure_count: i64,
    pub last_success_at: Option<String>,
    pub last_failure_at: Option<String>,
}

fn map_match(row: &rusqlite::Row<'_>) -> rusqlite::Result<PasswordMatch> {
    Ok(PasswordMatch {
        id: row.get(0)?,
        password_id: row.get(1)?,
        archive_format: row.get(2)?,
        path_pattern: row.get(3)?,
        filename_pattern: row.get(4)?,
        success_count: row.get(5)?,
        failure_count: row.get(6)?,
        last_success_at: row.get(7)?,
        last_failure_at: row.get(8)?,
    })
}

fn map_password_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<PasswordRecord> {
    Ok(PasswordRecord {
        id: row.get(0)?,
        value: row.get(1)?,
        source: row.get(2)?,
        pinned: row.get::<_, i64>(3)? != 0,
        disabled: row.get::<_, i64>(4)? != 0,
        success_count: row.get(5)?,
        failure_count: row.get(6)?,
        last_success_at: row.get(7)?,
        last_failure_at: row.get(8)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SmartZipDb;

    #[test]
    fn upsert_and_rank_passwords() {
        let db = SmartZipDb::in_memory().unwrap();
        let repo = PasswordRepository::new(db.connection());

        let first = repo
            .upsert(NewPassword {
                value: "密码一",
                source: "manual",
                pinned: false,
            })
            .unwrap();
        let second = repo
            .upsert(NewPassword {
                value: "password-two",
                source: "clipboard",
                pinned: true,
            })
            .unwrap();
        repo.record_success(first).unwrap();
        repo.record_success(first).unwrap();
        repo.record_failure(first).unwrap();

        let ranked = repo.ranked_candidates(10).unwrap();
        assert_eq!(ranked[0].id, second);
        assert_eq!(ranked[1].id, first);
        assert_eq!(ranked[1].value, "密码一");
    }

    #[test]
    fn record_match_success_upserts_row_and_bumps_count() {
        let db = SmartZipDb::in_memory().unwrap();
        let repo = PasswordRepository::new(db.connection());
        let id = repo
            .upsert(NewPassword {
                value: "abc123",
                source: "manual",
                pinned: false,
            })
            .unwrap();

        repo.record_match_success(id, Some("zip"), Some("photos"))
            .unwrap();
        repo.record_match_success(id, Some("zip"), Some("photos"))
            .unwrap();
        // Different tuple → distinct row.
        repo.record_match_success(id, Some("rar"), Some("photos"))
            .unwrap();

        let matches = repo.matches_for(id).unwrap();
        assert_eq!(matches.len(), 2);
        let zip = matches
            .iter()
            .find(|m| m.archive_format.as_deref() == Some("zip"))
            .unwrap();
        assert_eq!(zip.success_count, 2);
        assert!(zip.last_success_at.is_some());
        let rar = matches
            .iter()
            .find(|m| m.archive_format.as_deref() == Some("rar"))
            .unwrap();
        assert_eq!(rar.success_count, 1);
    }

    #[test]
    fn record_match_failure_bumps_failure_column() {
        let db = SmartZipDb::in_memory().unwrap();
        let repo = PasswordRepository::new(db.connection());
        let id = repo
            .upsert(NewPassword {
                value: "abc123",
                source: "manual",
                pinned: false,
            })
            .unwrap();
        repo.record_match_failure(id, Some("zip"), Some("photos"))
            .unwrap();
        repo.record_match_failure(id, Some("zip"), Some("photos"))
            .unwrap();
        let matches = repo.matches_for(id).unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].failure_count, 2);
        assert_eq!(matches[0].success_count, 0);
    }
}
