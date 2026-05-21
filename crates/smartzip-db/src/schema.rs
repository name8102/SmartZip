use rusqlite::Connection;

pub fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        r#"
        PRAGMA foreign_keys = ON;

        CREATE TABLE IF NOT EXISTS schema_migrations (
            version INTEGER PRIMARY KEY,
            applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
        );

        CREATE TABLE IF NOT EXISTS passwords (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            value TEXT NOT NULL UNIQUE,
            source TEXT NOT NULL,
            pinned INTEGER NOT NULL DEFAULT 0,
            disabled INTEGER NOT NULL DEFAULT 0,
            success_count INTEGER NOT NULL DEFAULT 0,
            failure_count INTEGER NOT NULL DEFAULT 0,
            created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
            updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
            last_success_at TEXT,
            last_failure_at TEXT
        );

        CREATE INDEX IF NOT EXISTS idx_passwords_rank
            ON passwords(disabled, pinned, success_count, last_success_at);

        CREATE TABLE IF NOT EXISTS password_matches (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            password_id INTEGER NOT NULL REFERENCES passwords(id) ON DELETE CASCADE,
            archive_format TEXT,
            path_pattern TEXT,
            filename_pattern TEXT,
            success_count INTEGER NOT NULL DEFAULT 0,
            failure_count INTEGER NOT NULL DEFAULT 0,
            last_success_at TEXT,
            last_failure_at TEXT
        );

        CREATE INDEX IF NOT EXISTS idx_password_matches_password
            ON password_matches(password_id);
        CREATE INDEX IF NOT EXISTS idx_password_matches_filename
            ON password_matches(filename_pattern);
        CREATE INDEX IF NOT EXISTS idx_password_matches_path
            ON password_matches(path_pattern);

        INSERT OR IGNORE INTO schema_migrations(version) VALUES (1);
        "#,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migration_creates_passwords_table() {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='passwords'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }
}
