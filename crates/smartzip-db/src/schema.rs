use rusqlite::{params, Connection};

/// Latest schema version this build knows how to produce.
pub const LATEST_VERSION: u32 = 4;

/// Apply any pending schema migrations to `conn`.
///
/// Migrations are versioned and idempotent: `schema_migrations` records which
/// versions have been applied, and only the missing steps run. Each step is
/// wrapped in a transaction so an interrupted upgrade leaves the database on
/// the previous version rather than in a half-applied state.
pub fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        r#"
        PRAGMA foreign_keys = ON;

        CREATE TABLE IF NOT EXISTS schema_migrations (
            version INTEGER PRIMARY KEY,
            applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
        );
        "#,
    )?;

    let current = current_version(conn)?;
    for step in MIGRATIONS {
        if step.version <= current {
            continue;
        }
        apply_step(conn, step)?;
    }
    Ok(())
}

/// Return the highest applied schema version, or 0 for a fresh database.
pub fn current_version(conn: &Connection) -> rusqlite::Result<u32> {
    let version: Option<i64> = conn
        .query_row(
            "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
            [],
            |row| row.get(0),
        )
        .ok();
    Ok(version.unwrap_or(0) as u32)
}

struct MigrationStep {
    version: u32,
    sql: &'static str,
}

const MIGRATIONS: &[MigrationStep] = &[
    MigrationStep {
        version: 1,
        sql: r#"
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
        "#,
    },
    MigrationStep {
        version: 2,
        sql: r#"
        CREATE TABLE IF NOT EXISTS tasks (
            id TEXT PRIMARY KEY,
            kind TEXT NOT NULL,
            status TEXT NOT NULL,
            input_summary TEXT NOT NULL,
            output_path TEXT,
            started_at TEXT NOT NULL,
            finished_at TEXT,
            error_code TEXT,
            error_message TEXT,
            password_attempts INTEGER NOT NULL DEFAULT 0,
            encoding_selected TEXT,
            embedded_found INTEGER NOT NULL DEFAULT 0
        );

        CREATE INDEX IF NOT EXISTS idx_tasks_started_at ON tasks(started_at);
        CREATE INDEX IF NOT EXISTS idx_tasks_status ON tasks(status);

        CREATE TABLE IF NOT EXISTS task_events (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
            level TEXT NOT NULL,
            event_type TEXT NOT NULL,
            message TEXT NOT NULL,
            data_json TEXT,
            created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
        );

        CREATE INDEX IF NOT EXISTS idx_task_events_task
            ON task_events(task_id, created_at);

        CREATE TABLE IF NOT EXISTS encoding_detections (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            archive_path_hash TEXT NOT NULL,
            archive_format TEXT,
            selected_encoding TEXT NOT NULL,
            confidence REAL NOT NULL,
            user_corrected INTEGER NOT NULL DEFAULT 0,
            candidates_json TEXT NOT NULL,
            created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
        );

        CREATE INDEX IF NOT EXISTS idx_encoding_hash
            ON encoding_detections(archive_path_hash);

        CREATE TABLE IF NOT EXISTS embedded_archive_detections (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            file_path_hash TEXT NOT NULL,
            format TEXT NOT NULL,
            offset INTEGER NOT NULL,
            confidence REAL NOT NULL,
            size_hint INTEGER,
            created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
        );

        CREATE INDEX IF NOT EXISTS idx_embedded_file_hash
            ON embedded_archive_detections(file_path_hash);
        "#,
    },
    MigrationStep {
        version: 3,
        sql: r#"
        -- v3: file-grain history. The v2 detection/match tables never held
        -- decision-driving data, so drop them outright rather than migrate.
        DROP TABLE IF EXISTS encoding_detections;
        DROP TABLE IF EXISTS embedded_archive_detections;
        DROP TABLE IF EXISTS password_matches;

        -- Slim `tasks` to a pure operation-level parent (method A). Old rows
        -- carry no meaningful history, so rebuild rather than ALTER-drop each
        -- column. task_events cascades on tasks(id), but since we only DROP an
        -- empty table that has no dependent rows, no event data is lost.
        DROP TABLE IF EXISTS tasks;
        CREATE TABLE tasks (
            id TEXT PRIMARY KEY,
            kind TEXT NOT NULL,
            status TEXT NOT NULL,
            output_path TEXT,
            started_at TEXT NOT NULL,
            finished_at TEXT
        );
        CREATE INDEX idx_tasks_started_at ON tasks(started_at);
        CREATE INDEX idx_tasks_status ON tasks(status);

        CREATE TABLE file_extractions (
            id                   INTEGER PRIMARY KEY AUTOINCREMENT,
            task_id              TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
            input_path           TEXT NOT NULL,
            sample_hash          TEXT,
            file_size            INTEGER,
            offset               INTEGER,
            output_path          TEXT,
            has_password         INTEGER NOT NULL DEFAULT 0,
            password_id          INTEGER REFERENCES passwords(id) ON DELETE SET NULL,
            status               TEXT NOT NULL,
            reason               TEXT,
            encoding             TEXT,
            encoding_corrected   INTEGER NOT NULL DEFAULT 0,
            damaged_volumes_json TEXT,
            created_at           TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
        );
        CREATE INDEX idx_file_extractions_task   ON file_extractions(task_id);
        CREATE INDEX idx_file_extractions_status ON file_extractions(status);
        CREATE INDEX idx_file_extractions_dedup  ON file_extractions(sample_hash, file_size, created_at);

        CREATE TABLE known_files (
            sample_hash        TEXT NOT NULL,
            size               INTEGER NOT NULL,
            names_offsets_json TEXT NOT NULL DEFAULT '[]',
            password_id        INTEGER REFERENCES passwords(id) ON DELETE SET NULL,
            confirmed_encoding TEXT,
            last_extract_at    TEXT,
            PRIMARY KEY (sample_hash, size)
        );
        "#,
    },
    MigrationStep {
        version: 4,
        sql: "ALTER TABLE file_extractions ADD COLUMN test_report_json TEXT;",
    },
];

fn apply_step(conn: &Connection, step: &MigrationStep) -> rusqlite::Result<()> {
    conn.execute_batch("BEGIN")?;
    let result = (|| -> rusqlite::Result<()> {
        conn.execute_batch(step.sql)?;
        conn.execute(
            "INSERT OR IGNORE INTO schema_migrations(version) VALUES (?1)",
            params![step.version as i64],
        )?;
        Ok(())
    })();
    match result {
        Ok(()) => {
            conn.execute_batch("COMMIT")?;
            Ok(())
        }
        Err(error) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table_exists(conn: &Connection, name: &str) -> bool {
        let count: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='table' AND name = ?1",
                params![name],
                |row| row.get(0),
            )
            .unwrap();
        count > 0
    }

    #[test]
    fn migration_creates_passwords_table() {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        assert!(table_exists(&conn, "passwords"));
    }

    #[test]
    fn migration_creates_all_v3_tables() {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        for name in [
            "passwords",
            "tasks",
            "task_events",
            "file_extractions",
            "known_files",
        ] {
            assert!(table_exists(&conn, name), "table {name} missing");
        }
        assert_eq!(current_version(&conn).unwrap(), LATEST_VERSION);
    }

    #[test]
    fn v3_drops_superseded_v2_tables() {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        for name in [
            "password_matches",
            "encoding_detections",
            "embedded_archive_detections",
        ] {
            assert!(!table_exists(&conn, name), "table {name} should be dropped");
        }
    }

    #[test]
    fn v3_tasks_table_is_slim() {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        let columns: Vec<String> = {
            let mut stmt = conn.prepare("PRAGMA table_info(tasks)").unwrap();
            let rows = stmt
                .query_map([], |row| row.get::<_, String>(1))
                .unwrap()
                .map(|r| r.unwrap())
                .collect();
            rows
        };
        assert_eq!(
            columns,
            vec![
                "id",
                "kind",
                "status",
                "output_path",
                "started_at",
                "finished_at"
            ],
        );
    }

    #[test]
    fn migration_is_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        migrate(&conn).unwrap();
        let applied: i64 = conn
            .query_row("SELECT count(*) FROM schema_migrations", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(applied, LATEST_VERSION as i64);
    }

    #[test]
    fn v4_preserves_v3_history_and_defaults_old_reports_to_null() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE schema_migrations (version INTEGER PRIMARY KEY)")
            .unwrap();
        for step in &MIGRATIONS[..3] {
            apply_step(&conn, step).unwrap();
        }
        conn.execute_batch(r#"
            INSERT INTO tasks (id, kind, status, started_at) VALUES ('old', 'extract', 'completed', '2026-07-03');
            INSERT INTO file_extractions (task_id, input_path, status, damaged_volumes_json)
                VALUES ('old', 'archive.zip', 'extracted', '["legacy.part2.rar"]');
            INSERT INTO known_files (sample_hash, size, last_extract_at) VALUES ('hash', 42, '2026-07-03');
        "#).unwrap();
        migrate(&conn).unwrap();
        migrate(&conn).unwrap();
        let row: (String, String, Option<String>) = conn
            .query_row(
                "SELECT input_path, damaged_volumes_json, test_report_json FROM file_extractions",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            row,
            ("archive.zip".into(), "[\"legacy.part2.rar\"]".into(), None)
        );
        let last: String = conn
            .query_row("SELECT last_extract_at FROM known_files", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(last, "2026-07-03");
        assert_eq!(current_version(&conn).unwrap(), 4);
    }

    #[test]
    fn upgrades_v1_database_to_latest() {
        let conn = Connection::open_in_memory().unwrap();
        // Simulate an older database that only has v1 applied.
        conn.execute_batch(
            r#"
            CREATE TABLE schema_migrations (
                version INTEGER PRIMARY KEY,
                applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
            );
            CREATE TABLE passwords (
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
            INSERT INTO schema_migrations(version) VALUES (1);
            "#,
        )
        .unwrap();
        assert_eq!(current_version(&conn).unwrap(), 1);
        assert!(!table_exists(&conn, "tasks"));

        migrate(&conn).unwrap();
        assert_eq!(current_version(&conn).unwrap(), LATEST_VERSION);
        // v3 end state: slim tasks + file-grain tables present, the v2
        // detection/match tables dropped along the way.
        assert!(table_exists(&conn, "tasks"));
        assert!(table_exists(&conn, "task_events"));
        assert!(table_exists(&conn, "file_extractions"));
        assert!(table_exists(&conn, "known_files"));
        assert!(!table_exists(&conn, "encoding_detections"));
        assert!(!table_exists(&conn, "embedded_archive_detections"));
        assert!(!table_exists(&conn, "password_matches"));
    }
}
