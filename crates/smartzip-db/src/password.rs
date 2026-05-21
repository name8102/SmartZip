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
}
