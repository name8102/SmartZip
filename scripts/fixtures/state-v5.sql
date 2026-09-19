-- Frozen pre-task-system schema for CLI upgrade acceptance. Synthetic data only.

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

ALTER TABLE file_extractions ADD COLUMN test_report_json TEXT;
DROP INDEX IF EXISTS idx_passwords_rank;
        CREATE INDEX idx_passwords_rank ON passwords(
            disabled, pinned DESC, success_count DESC,
            COALESCE(last_success_at, '') DESC, failure_count ASC, id ASC
        );
PRAGMA user_version = 5;
