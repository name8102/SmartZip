//! Persistence for the `known_files` dedup/reuse index (v3).
//!
//! Exactly one row per `(sample_hash, size)` (UPSERT). This is the mutable
//! counterpart to the append-only `file_extractions` log: the log keeps every
//! action (answering "why did this fail last time?"), while this index keeps a
//! single up-to-date entry per physical file and serves only the matching hot
//! path — dedup skip, confirmed-encoding reuse, and password reuse.
//!
//! Two write paths with different merge rules:
//! - [`KnownFileRepository::upsert_extract`] (called after a successful
//!   extract) writes `last_extract_at` + `password_id` and appends the
//!   observed name/offset pair, but never touches `confirmed_encoding`.
//! - [`KnownFileRepository::upsert_confirmed_encoding`] (called when the user
//!   manually confirms an encoding) overwrites `confirmed_encoding` and appends
//!   the name/offset pair, but never writes `last_extract_at` — a detect-time
//!   guess must not register as a successful extraction.

use crate::Result;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

/// A `(name, offset)` pair observed for a known file.
///
/// `offset` is `None` for a whole-file archive and `Some` for a carved
/// embedded archive; the pair is what distinguishes multiple embedded members
/// sharing one host file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NameOffset {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub offset: Option<i64>,
}

/// Full row shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KnownFile {
    pub sample_hash: String,
    pub size: i64,
    pub names_offsets: Vec<NameOffset>,
    pub password_id: Option<i64>,
    pub confirmed_encoding: Option<String>,
    pub last_extract_at: Option<String>,
}

pub struct KnownFileRepository<'a> {
    conn: &'a Connection,
}

impl<'a> KnownFileRepository<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    /// Look up the single index entry for a physical file. One query returns
    /// everything the extract hot path needs: reuse password, reuse confirmed
    /// encoding, and the dedup timestamp.
    pub fn find(&self, sample_hash: &str, size: i64) -> Result<Option<KnownFile>> {
        self.conn
            .query_row(
                r#"
                SELECT sample_hash, size, names_offsets_json, password_id,
                       confirmed_encoding, last_extract_at
                FROM known_files
                WHERE sample_hash = ?1 AND size = ?2
                "#,
                params![sample_hash, size],
                map_known_file,
            )
            .optional()
            .map_err(Into::into)
    }

    /// Record a successful extraction: write `last_extract_at` and (when
    /// known) `password_id`, and append the observed name/offset pair.
    /// Leaves `confirmed_encoding` untouched — an extract never overrides a
    /// user-confirmed encoding.
    pub fn upsert_extract(
        &self,
        sample_hash: &str,
        size: i64,
        name_offset: Option<NameOffset>,
        password_id: Option<i64>,
        last_extract_at: &str,
    ) -> Result<()> {
        let existing = self.find(sample_hash, size)?;
        let names = merge_name_offset(existing.as_ref(), name_offset);
        let names_json = serde_json::to_string(&names)?;
        // COALESCE(?new, existing) keeps a prior password when this run had
        // none (e.g. reused a cached candidate but recorded no id).
        self.conn.execute(
            r#"
            INSERT INTO known_files(
                sample_hash, size, names_offsets_json, password_id, last_extract_at
            ) VALUES (?1, ?2, ?3, ?4, ?5)
            ON CONFLICT(sample_hash, size) DO UPDATE SET
                names_offsets_json = excluded.names_offsets_json,
                password_id = COALESCE(excluded.password_id, known_files.password_id),
                last_extract_at = excluded.last_extract_at
            "#,
            params![sample_hash, size, names_json, password_id, last_extract_at],
        )?;
        Ok(())
    }

    /// Record a user-confirmed encoding: overwrite `confirmed_encoding` and
    /// append the name/offset pair. Never writes `last_extract_at` — a
    /// confirmation is not an extraction.
    pub fn upsert_confirmed_encoding(
        &self,
        sample_hash: &str,
        size: i64,
        name_offset: Option<NameOffset>,
        confirmed_encoding: &str,
    ) -> Result<()> {
        let existing = self.find(sample_hash, size)?;
        let names = merge_name_offset(existing.as_ref(), name_offset);
        let names_json = serde_json::to_string(&names)?;
        self.conn.execute(
            r#"
            INSERT INTO known_files(
                sample_hash, size, names_offsets_json, confirmed_encoding
            ) VALUES (?1, ?2, ?3, ?4)
            ON CONFLICT(sample_hash, size) DO UPDATE SET
                names_offsets_json = excluded.names_offsets_json,
                confirmed_encoding = excluded.confirmed_encoding
            "#,
            params![sample_hash, size, names_json, confirmed_encoding],
        )?;
        Ok(())
    }

    /// Whether this file counts as a dedup hit: it was extracted before
    /// (`last_extract_at` non-null) and that time is at or after
    /// `window_start` (an ISO-8601 lower bound the caller computes from the
    /// configured window). Comparison is lexicographic, which is correct for
    /// zero-padded UTC ISO-8601.
    pub fn dedup_hit(&self, sample_hash: &str, size: i64, window_start: &str) -> Result<bool> {
        let hit: Option<i64> = self
            .conn
            .query_row(
                r#"
                SELECT 1 FROM known_files
                WHERE sample_hash = ?1 AND size = ?2
                  AND last_extract_at IS NOT NULL
                  AND last_extract_at >= ?3
                "#,
                params![sample_hash, size, window_start],
                |row| row.get(0),
            )
            .optional()?;
        Ok(hit.is_some())
    }
}

/// Append `incoming` to the existing pairs, skipping exact duplicates.
fn merge_name_offset(
    existing: Option<&KnownFile>,
    incoming: Option<NameOffset>,
) -> Vec<NameOffset> {
    let mut names = existing
        .map(|k| k.names_offsets.clone())
        .unwrap_or_default();
    if let Some(pair) = incoming {
        if !names.contains(&pair) {
            names.push(pair);
        }
    }
    names
}

fn map_known_file(row: &rusqlite::Row<'_>) -> rusqlite::Result<KnownFile> {
    let names_json: String = row.get(2)?;
    let names_offsets = serde_json::from_str(&names_json).unwrap_or_default();
    Ok(KnownFile {
        sample_hash: row.get(0)?,
        size: row.get(1)?,
        names_offsets,
        password_id: row.get(3)?,
        confirmed_encoding: row.get(4)?,
        last_extract_at: row.get(5)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SmartZipDb;

    fn no(name: &str, offset: Option<i64>) -> NameOffset {
        NameOffset {
            name: name.to_string(),
            offset,
        }
    }

    /// Insert a password so `known_files.password_id` FK references resolve.
    fn seed_password(db: &SmartZipDb, value: &str) -> i64 {
        crate::password::PasswordRepository::new(db.connection())
            .upsert(crate::password::NewPassword {
                value,
                source: "test",
                pinned: false,
            })
            .unwrap()
    }

    #[test]
    fn upsert_extract_creates_then_appends_names() {
        let db = SmartZipDb::in_memory().unwrap();
        let pw = seed_password(&db, "pw-1");
        let repo = KnownFileRepository::new(db.connection());
        repo.upsert_extract(
            "h",
            100,
            Some(no("a.zip", None)),
            Some(pw),
            "2026-07-02T00:00:00Z",
        )
        .unwrap();
        repo.upsert_extract(
            "h",
            100,
            Some(no("inner", Some(2048))),
            None,
            "2026-07-03T00:00:00Z",
        )
        .unwrap();

        let got = repo.find("h", 100).unwrap().unwrap();
        assert_eq!(
            got.names_offsets,
            vec![no("a.zip", None), no("inner", Some(2048))]
        );
        // password_id is preserved from the first write via COALESCE.
        assert_eq!(got.password_id, Some(1));
        assert_eq!(got.last_extract_at.as_deref(), Some("2026-07-03T00:00:00Z"));
    }

    #[test]
    fn duplicate_name_offset_not_appended_twice() {
        let db = SmartZipDb::in_memory().unwrap();
        let repo = KnownFileRepository::new(db.connection());
        repo.upsert_extract("h", 1, Some(no("a", None)), None, "2026-07-02T00:00:00Z")
            .unwrap();
        repo.upsert_extract("h", 1, Some(no("a", None)), None, "2026-07-02T00:00:01Z")
            .unwrap();
        let got = repo.find("h", 1).unwrap().unwrap();
        assert_eq!(got.names_offsets.len(), 1);
    }

    #[test]
    fn extract_never_overwrites_confirmed_encoding() {
        let db = SmartZipDb::in_memory().unwrap();
        let pw = seed_password(&db, "pw-1");
        let repo = KnownFileRepository::new(db.connection());
        repo.upsert_confirmed_encoding("h", 1, Some(no("a", None)), "gb18030")
            .unwrap();
        repo.upsert_extract("h", 1, None, Some(pw), "2026-07-02T00:00:00Z")
            .unwrap();
        let got = repo.find("h", 1).unwrap().unwrap();
        assert_eq!(got.confirmed_encoding.as_deref(), Some("gb18030"));
        assert_eq!(got.last_extract_at.as_deref(), Some("2026-07-02T00:00:00Z"));
        assert_eq!(got.password_id, Some(pw));
    }

    #[test]
    fn confirmed_encoding_overwrites_prior_value() {
        let db = SmartZipDb::in_memory().unwrap();
        let repo = KnownFileRepository::new(db.connection());
        repo.upsert_confirmed_encoding("h", 1, None, "gbk").unwrap();
        repo.upsert_confirmed_encoding("h", 1, None, "gb18030")
            .unwrap();
        let got = repo.find("h", 1).unwrap().unwrap();
        assert_eq!(got.confirmed_encoding.as_deref(), Some("gb18030"));
    }

    #[test]
    fn confirmed_encoding_does_not_set_last_extract_at() {
        let db = SmartZipDb::in_memory().unwrap();
        let repo = KnownFileRepository::new(db.connection());
        repo.upsert_confirmed_encoding("h", 1, None, "gbk").unwrap();
        let got = repo.find("h", 1).unwrap().unwrap();
        assert!(got.last_extract_at.is_none());
    }

    #[test]
    fn dedup_hit_respects_window() {
        let db = SmartZipDb::in_memory().unwrap();
        let repo = KnownFileRepository::new(db.connection());
        repo.upsert_extract("h", 1, None, None, "2026-06-15T00:00:00Z")
            .unwrap();
        // Window starting 2026-06-01: the extract is inside → hit.
        assert!(repo.dedup_hit("h", 1, "2026-06-01T00:00:00Z").unwrap());
        // Window starting 2026-07-01: the extract is older → miss.
        assert!(!repo.dedup_hit("h", 1, "2026-07-01T00:00:00Z").unwrap());
    }

    #[test]
    fn dedup_miss_when_only_encoding_confirmed() {
        let db = SmartZipDb::in_memory().unwrap();
        let repo = KnownFileRepository::new(db.connection());
        // Confirmed encoding but never extracted → not a dedup hit.
        repo.upsert_confirmed_encoding("h", 1, None, "gbk").unwrap();
        assert!(!repo.dedup_hit("h", 1, "2000-01-01T00:00:00Z").unwrap());
    }
}
