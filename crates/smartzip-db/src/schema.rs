use rusqlite::Connection;
use rusqlite_migration::{Migrations, M};

/// Latest schema version this build knows how to produce.
pub const LATEST_VERSION: u32 = 3;

const MIGRATIONS_SLICE: &[M<'static>] = &[
    M::up(
        r#"
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
    ),
    M::up(
        r#"
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
    ),
    M::up(
        r#"
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
    ),
];

static MIGRATIONS: Migrations<'static> = Migrations::from_slice(MIGRATIONS_SLICE);

/// Apply any pending schema migrations to `conn`.
///
/// Uses `rusqlite_migration` with `user_version` as the version tracker,
/// replacing the previous hand-rolled `schema_migrations` table, `BEGIN`/`COMMIT`
/// plumbing, and manual `MigrationStep` dispatch. This removes a non-product
/// persistence framework in favour of a maintained crate.
///
/// For backwards compatibility with databases created before the migration
/// to `rusqlite_migration`, any existing `schema_migrations` table is
/// detected and its highest `version` is promoted to `PRAGMA user_version`
/// before invoking `to_latest`. This ensures a legacy v1/v2 database does
/// not re-apply migrations and wipe data (notably v3's `DROP TABLE tasks`).
pub fn migrate(conn: &mut Connection) -> crate::Result<()> {
    // Keep foreign-keys enforcement consistent with the previous implementation.
    // rusqlite_migration docs discourage `PRAGMA foreign_keys` inside the
    // migration SQL itself (transactions), so set it outside.
    conn.pragma_update(None, "foreign_keys", "ON")?;

    // Promote legacy `schema_migrations` version to `user_version` if needed.
    // This is a one-time bridge for DBs created with the old hand-rolled logic.
    // Fail-closed: any error while inspecting the legacy state is propagated
    // instead of being silently interpreted as version 0, which would cause
    // destructive re-execution of migrations (e.g. v3 DROP TABLE).
    let has_legacy: i64 = conn.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='schema_migrations'",
        [],
        |row| row.get::<_, i64>(0),
    )?;
    let has_legacy = has_legacy > 0;
    if has_legacy {
        let legacy_version: i64 = conn.query_row(
            "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
            [],
            |row| row.get(0),
        )?;
        let user_version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if user_version == 0 && legacy_version > 0 {
            conn.pragma_update(None, "user_version", legacy_version)?;
        }
    }

    MIGRATIONS.to_latest(conn)?;

    // After successful migration, the legacy table is no longer needed.
    // Propagate any error instead of silently swallowing it.
    if has_legacy {
        conn.execute_batch("DROP TABLE IF EXISTS schema_migrations;")?;
    }

    Ok(())
}

/// Return the highest applied schema version, or 0 for a fresh database.
///
/// Reads `PRAGMA user_version` (the `rusqlite_migration` tracker) and falls
/// back to the legacy `schema_migrations` table for DBs that have not yet
/// been promoted. This keeps `current_version` meaningful during the
/// transition period.
pub fn current_version(conn: &Connection) -> rusqlite::Result<u32> {
    let user_version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if user_version != 0 {
        return Ok(user_version as u32);
    }
    // Fallback for legacy DBs that still use `schema_migrations`.
    // Fail-closed: propagate any error (e.g. missing version column) instead
    // of silently returning 0.
    let legacy: i64 = conn.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
        [],
        |row| row.get(0),
    )?;
    Ok(legacy as u32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;

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
        let mut conn = Connection::open_in_memory().unwrap();
        migrate(&mut conn).unwrap();
        assert!(table_exists(&conn, "passwords"));
    }

    #[test]
    fn migration_creates_all_v3_tables() {
        let mut conn = Connection::open_in_memory().unwrap();
        migrate(&mut conn).unwrap();
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
        let mut conn = Connection::open_in_memory().unwrap();
        migrate(&mut conn).unwrap();
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
        let mut conn = Connection::open_in_memory().unwrap();
        migrate(&mut conn).unwrap();
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
        let mut conn = Connection::open_in_memory().unwrap();
        migrate(&mut conn).unwrap();
        migrate(&mut conn).unwrap();
        // `rusqlite_migration` tracks via `user_version`, not `schema_migrations`.
        assert_eq!(current_version(&conn).unwrap(), LATEST_VERSION);
        // Legacy table should have been cleaned up.
        assert!(!table_exists(&conn, "schema_migrations"));
    }

    #[test]
    fn upgrades_v1_database_to_latest() {
        let mut conn = Connection::open_in_memory().unwrap();
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

        migrate(&mut conn).unwrap();
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
        // Legacy promotion should have removed the old table.
        assert!(!table_exists(&conn, "schema_migrations"));
    }

    #[test]
    fn upgrades_v2_database_to_latest() {
        let mut conn = Connection::open_in_memory().unwrap();
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
            CREATE TABLE tasks (
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
            CREATE TABLE task_events (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
                level TEXT NOT NULL,
                event_type TEXT NOT NULL,
                message TEXT NOT NULL,
                data_json TEXT,
                created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
            );
            CREATE TABLE encoding_detections (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                archive_path_hash TEXT NOT NULL,
                archive_format TEXT,
                selected_encoding TEXT NOT NULL,
                confidence REAL NOT NULL,
                user_corrected INTEGER NOT NULL DEFAULT 0,
                candidates_json TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
            );
            CREATE TABLE embedded_archive_detections (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                file_path_hash TEXT NOT NULL,
                format TEXT NOT NULL,
                offset INTEGER NOT NULL,
                confidence REAL NOT NULL,
                size_hint INTEGER,
                created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
            );
            CREATE TABLE password_matches (
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
            INSERT INTO schema_migrations(version) VALUES (2);
            "#,
        )
        .unwrap();
        assert_eq!(current_version(&conn).unwrap(), 2);
        migrate(&mut conn).unwrap();
        assert_eq!(current_version(&conn).unwrap(), LATEST_VERSION);
        assert!(table_exists(&conn, "file_extractions"));
        assert!(table_exists(&conn, "known_files"));
        assert!(!table_exists(&conn, "encoding_detections"));
        assert!(!table_exists(&conn, "password_matches"));
        // v3 should have slimmed tasks
        let cols: Vec<String> = conn
            .prepare("PRAGMA table_info(tasks)")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(
            cols,
            vec![
                "id",
                "kind",
                "status",
                "output_path",
                "started_at",
                "finished_at"
            ]
        );
    }

    #[test]
    fn legacy_v3_promotes_to_user_version() {
        let mut conn = Connection::open_in_memory().unwrap();
        // Simulate a DB that already went through the old hand-rolled v3
        // (has file_extractions/known_files and schema_migrations=3).
        conn.execute_batch(
            r#"
            CREATE TABLE schema_migrations (version INTEGER PRIMARY KEY, applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
            CREATE TABLE passwords (id INTEGER PRIMARY KEY AUTOINCREMENT, value TEXT NOT NULL UNIQUE, source TEXT NOT NULL);
            CREATE TABLE tasks (id TEXT PRIMARY KEY, kind TEXT NOT NULL, status TEXT NOT NULL, output_path TEXT, started_at TEXT NOT NULL, finished_at TEXT);
            CREATE TABLE task_events (id INTEGER PRIMARY KEY AUTOINCREMENT, task_id TEXT NOT NULL, level TEXT NOT NULL, event_type TEXT NOT NULL, message TEXT NOT NULL);
            CREATE TABLE file_extractions (id INTEGER PRIMARY KEY AUTOINCREMENT, task_id TEXT NOT NULL, input_path TEXT NOT NULL, status TEXT NOT NULL);
            CREATE TABLE known_files (sample_hash TEXT NOT NULL, size INTEGER NOT NULL, PRIMARY KEY (sample_hash, size));
            INSERT INTO schema_migrations(version) VALUES (3);
            "#,
        )
        .unwrap();
        // user_version is still 0 before migrate
        let uv: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(uv, 0);
        migrate(&mut conn).unwrap();
        let uv2: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(uv2, 3);
        assert_eq!(current_version(&conn).unwrap(), 3);
        assert!(!table_exists(&conn, "schema_migrations"));
    }

    #[test]
    fn password_rows_survive_migration() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE schema_migrations (version INTEGER PRIMARY KEY, applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
            CREATE TABLE passwords (id INTEGER PRIMARY KEY AUTOINCREMENT, value TEXT NOT NULL UNIQUE, source TEXT NOT NULL, pinned INTEGER NOT NULL DEFAULT 0, disabled INTEGER NOT NULL DEFAULT 0, success_count INTEGER NOT NULL DEFAULT 0, failure_count INTEGER NOT NULL DEFAULT 0, created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP, updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP, last_success_at TEXT, last_failure_at TEXT);
            INSERT INTO passwords(value, source) VALUES ('secret123', 'manual');
            INSERT INTO schema_migrations(version) VALUES (1);
            "#,
        )
        .unwrap();
        migrate(&mut conn).unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT count(*) FROM passwords WHERE value='secret123'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
        assert_eq!(current_version(&conn).unwrap(), LATEST_VERSION);
    }

    #[test]
    fn migration_failure_does_not_advance_version() {
        use rusqlite_migration::{Migrations, M};
        let mut conn = Connection::open_in_memory().unwrap();
        // First bring to v1 normally
        migrate(&mut conn).unwrap();
        let before: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        // Now try a failing migration on a fresh Migrations set that would be v4
        let failing = Migrations::new(vec![
            M::up("CREATE TABLE t1 (id INTEGER PRIMARY KEY);"),
            M::up("CREATE TABLE t2 (id INTEGER PRIMARY KEY);"),
            M::up("CREATE TABLE t3 (id INTEGER PRIMARY KEY);"),
            M::up("THIS IS NOT VALID SQL"),
        ]);
        // The DB is at version 3, so the next migration (v4) will fail.
        let res = failing.to_latest(&mut conn);
        assert!(res.is_err(), "failing migration should error");
        let after: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(before, after, "user_version must not advance on failure");
        assert_eq!(after, 3);
    }
    #[test]
    fn legacy_missing_version_column_fails_closed() {
        let mut conn = Connection::open_in_memory().unwrap();
        // Malformed legacy table: has schema_migrations but no version column
        conn.execute_batch(
            "CREATE TABLE schema_migrations (id INTEGER PRIMARY KEY, applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP); INSERT INTO schema_migrations(id, applied_at) VALUES (1, '2024-01-01');",
        )
        .unwrap();
        assert!(table_exists(&conn, "schema_migrations"));
        let before_uv: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(before_uv, 0);
        let res = migrate(&mut conn);
        assert!(
            res.is_err(),
            "migrate should fail closed when legacy schema is malformed, got {res:?}"
        );
        let after_uv: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            after_uv, 0,
            "user_version must not advance when legacy bridge fails"
        );
        // No SmartZip tables should have been created (the failing bridge
        // should have prevented any of the v1/v2/v3 migrations from running).
        assert!(
            !table_exists(&conn, "passwords"),
            "passwords should not be created after a bridge failure"
        );
        assert!(
            !table_exists(&conn, "tasks"),
            "tasks should not be created after a bridge failure"
        );
    }
}
