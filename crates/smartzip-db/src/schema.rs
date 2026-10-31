use rusqlite::{Connection, TransactionBehavior};

/// The only supported database schema version.
pub const LATEST_VERSION: u32 = 8;

const SCHEMA: &str = r#"
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

CREATE TABLE task_events (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
            level TEXT NOT NULL,
            event_type TEXT NOT NULL,
            message TEXT NOT NULL,
            data_json TEXT,
            created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
            node_id TEXT,
            stage TEXT,
            attempt_id TEXT,
            sequence INTEGER
        );

CREATE TABLE tasks (
            id TEXT PRIMARY KEY,
            kind TEXT NOT NULL,
            status TEXT NOT NULL,
            output_path TEXT,
            started_at TEXT NOT NULL,
            finished_at TEXT,
            inputs_json TEXT,
            priority INTEGER NOT NULL DEFAULT 1,
            queue_position INTEGER,
            config_snapshot_json TEXT,
            config_revision INTEGER NOT NULL DEFAULT 0,
            paused INTEGER NOT NULL DEFAULT 0,
            owner_epoch INTEGER NOT NULL DEFAULT 0,
            recoverable INTEGER NOT NULL DEFAULT 0,
            committed_output_files INTEGER NOT NULL DEFAULT 0,
            committed_output_bytes INTEGER NOT NULL DEFAULT 0,
            nested_candidate_count INTEGER NOT NULL DEFAULT 0
        );

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
            created_at           TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
            test_report_json TEXT,
            node_id TEXT,
            parent_node_id TEXT,
            root_node_id TEXT,
            generation INTEGER,
            stage TEXT,
            execution_state TEXT,
            input_ref_json TEXT,
            input_version_json TEXT,
            artifact_refs_json TEXT,
            decision_json TEXT,
            commit_json TEXT,
            config_revision INTEGER,
            attempt_id TEXT
        );

CREATE TABLE known_files (
            sample_hash        TEXT NOT NULL,
            size               INTEGER NOT NULL,
            names_offsets_json TEXT NOT NULL DEFAULT '[]',
            password_id        INTEGER REFERENCES passwords(id) ON DELETE SET NULL,
            confirmed_encoding TEXT,
            PRIMARY KEY (sample_hash, size)
        );

CREATE TABLE execution_owner (
            id INTEGER PRIMARY KEY CHECK (id = 1),
            epoch INTEGER NOT NULL,
            session_id TEXT NOT NULL,
            acquired_at TEXT NOT NULL
        );

CREATE INDEX idx_task_events_task
            ON task_events(task_id, created_at);

CREATE INDEX idx_tasks_started_at ON tasks(started_at);

CREATE INDEX idx_tasks_status ON tasks(status);

CREATE INDEX idx_file_extractions_task   ON file_extractions(task_id);

CREATE INDEX idx_file_extractions_status ON file_extractions(status);

CREATE INDEX idx_file_extractions_dedup  ON file_extractions(sample_hash, file_size, created_at);

CREATE INDEX idx_passwords_rank ON passwords(
            disabled, pinned DESC, success_count DESC,
            COALESCE(last_success_at, '') DESC, failure_count ASC, id ASC
        );

CREATE UNIQUE INDEX idx_file_extractions_node_generation
            ON file_extractions(task_id, node_id, generation)
            WHERE node_id IS NOT NULL;

CREATE INDEX idx_file_extractions_ready
            ON file_extractions(task_id, execution_state, stage)
            WHERE node_id IS NOT NULL;

CREATE UNIQUE INDEX idx_task_events_sequence
            ON task_events(task_id, sequence)
            WHERE sequence IS NOT NULL;
"#;

/// Initialize an empty database atomically, or open an existing current database.
pub fn initialize(conn: &mut Connection) -> crate::Result<()> {
    conn.pragma_update(None, "foreign_keys", "ON")?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let version = current_version(&tx)?;
    if version == 0 {
        let objects: i64 = tx.query_row(
            "SELECT count(*) FROM sqlite_master WHERE name NOT LIKE 'sqlite_%'",
            [],
            |row| row.get(0),
        )?;
        if objects != 0 {
            return Err(crate::DbError::UnsupportedSchema(version));
        }
        tx.execute_batch(SCHEMA)?;
        tx.pragma_update(None, "user_version", LATEST_VERSION)?;
    } else if version != LATEST_VERSION {
        return Err(crate::DbError::UnsupportedSchema(version));
    }
    tx.commit()?;
    Ok(())
}

/// Check the schema version without changing the database.
pub fn validate(conn: &Connection) -> crate::Result<()> {
    let version = current_version(conn)?;
    if version != LATEST_VERSION {
        return Err(crate::DbError::UnsupportedSchema(version));
    }
    Ok(())
}

pub fn current_version(conn: &Connection) -> rusqlite::Result<u32> {
    conn.pragma_query_value(None, "user_version", |row| row.get(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initializes_and_reopens_current_schema() {
        let mut conn = Connection::open_in_memory().unwrap();
        initialize(&mut conn).unwrap();
        assert_eq!(current_version(&conn).unwrap(), LATEST_VERSION);
        conn.execute(
            "INSERT INTO passwords(value, source) VALUES ('secret', 'manual')",
            [],
        )
        .unwrap();
        initialize(&mut conn).unwrap();
        let value: String = conn
            .query_row("SELECT value FROM passwords", [], |row| row.get(0))
            .unwrap();
        assert_eq!(value, "secret");
        validate(&conn).unwrap();
    }

    #[test]
    fn read_only_open_checks_version() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        {
            let db = crate::SmartZipDb::open(&path).unwrap();
            db.connection()
                .execute(
                    "INSERT INTO passwords(value, source) VALUES ('secret', 'manual')",
                    [],
                )
                .unwrap();
        }
        let db = crate::SmartZipDb::open_read_only(&path).unwrap();
        assert_eq!(current_version(db.connection()).unwrap(), LATEST_VERSION);
        drop(db);
        let conn = Connection::open(&path).unwrap();
        conn.pragma_update(None, "user_version", 5).unwrap();
        drop(conn);
        assert!(matches!(
            crate::SmartZipDb::open_read_only(&path),
            Err(crate::DbError::UnsupportedSchema(5))
        ));
        let conn = Connection::open(&path).unwrap();
        let value: String = conn
            .query_row("SELECT value FROM passwords", [], |row| row.get(0))
            .unwrap();
        assert_eq!(value, "secret");
    }

    #[test]
    fn rejects_old_and_unknown_databases_without_modification() {
        for version in [0, 5, 7, 9] {
            let mut conn = Connection::open_in_memory().unwrap();
            conn.execute_batch(
                "CREATE TABLE keep_data(value TEXT); INSERT INTO keep_data VALUES ('preserved');",
            )
            .unwrap();
            conn.pragma_update(None, "user_version", version).unwrap();
            assert!(
                matches!(initialize(&mut conn), Err(crate::DbError::UnsupportedSchema(v)) if v == version)
            );
            assert!(validate(&conn).is_err());
            assert_eq!(current_version(&conn).unwrap(), version);
            let value: String = conn
                .query_row("SELECT value FROM keep_data", [], |row| row.get(0))
                .unwrap();
            assert_eq!(value, "preserved");
        }
    }
}
