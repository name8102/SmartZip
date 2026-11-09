//! Password and confirmed-encoding hints keyed by file identity.
//! Successful extraction and deduplication use `file_extractions` history.

use crate::Result;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KnownFile {
    pub sample_hash: String,
    pub size: i64,
    pub password_id: Option<i64>,
    pub confirmed_encoding: Option<String>,
}

pub struct KnownFileRepository<'a> {
    conn: &'a Connection,
}

impl<'a> KnownFileRepository<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    pub fn find(&self, sample_hash: &str, size: i64) -> Result<Option<KnownFile>> {
        self.conn
            .query_row(
                "SELECT sample_hash, size, password_id, confirmed_encoding \
                 FROM known_files WHERE sample_hash = ?1 AND size = ?2",
                params![sample_hash, size],
                |row| {
                    Ok(KnownFile {
                        sample_hash: row.get(0)?,
                        size: row.get(1)?,
                        password_id: row.get(2)?,
                        confirmed_encoding: row.get(3)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    /// Update verified hints together, keeping a prior hint when no replacement is supplied.
    /// A success without either hint belongs only in extraction history.
    pub fn upsert_hints(
        &self,
        sample_hash: &str,
        size: i64,
        password_id: Option<i64>,
        confirmed_encoding: Option<&str>,
    ) -> Result<()> {
        if password_id.is_none() && confirmed_encoding.is_none() {
            return Ok(());
        }
        self.conn.execute(
            "INSERT INTO known_files(sample_hash, size, password_id, confirmed_encoding) \
             VALUES (?1, ?2, ?3, ?4) \
             ON CONFLICT(sample_hash, size) DO UPDATE SET \
             password_id = COALESCE(excluded.password_id, known_files.password_id), \
             confirmed_encoding = COALESCE(excluded.confirmed_encoding, known_files.confirmed_encoding)",
            params![sample_hash, size, password_id, confirmed_encoding],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SmartZipDb;

    #[test]
    fn remembers_hints_together_and_preserves_unspecified_values() {
        let db = SmartZipDb::in_memory().unwrap();
        let password = crate::password::PasswordRepository::new(db.connection())
            .upsert(crate::password::NewPassword {
                value: "pw",
                source: "test",
                pinned: false,
            })
            .unwrap();
        let repo = KnownFileRepository::new(db.connection());
        repo.upsert_hints("h", 100, Some(password), Some("gbk"))
            .unwrap();
        repo.upsert_hints("h", 100, None, Some("gb18030")).unwrap();
        repo.upsert_hints("h", 100, Some(password), None).unwrap();
        let known = repo.find("h", 100).unwrap().unwrap();
        assert_eq!(known.password_id, Some(password));
        assert_eq!(known.confirmed_encoding.as_deref(), Some("gb18030"));
        assert!(repo.find("h", 101).unwrap().is_none());
    }

    #[test]
    fn success_without_hints_does_not_create_a_redundant_row() {
        let db = SmartZipDb::in_memory().unwrap();
        let repo = KnownFileRepository::new(db.connection());
        repo.upsert_hints("h", 100, None, None).unwrap();
        assert!(repo.find("h", 100).unwrap().is_none());
    }
}
