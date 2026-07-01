# Database History Persistence — Implementation Plan

## Ordered Steps

1. Versioned schema migrations
   - Refactor `crates/smartzip-db/src/schema.rs` to iterate migrations by
     `MAX(version) FROM schema_migrations`.
   - v1 = current three tables. v2 adds `tasks`, `task_events`,
     `encoding_detections`, `embedded_archive_detections` and their indexes,
     verbatim from `docs/design.md § 4.3-4.6`.
   - Add a migration test that opens a v1 in-memory DB, applies v2, and
     asserts every new table + index exists and `schema_migrations` reports
     version 2.

2. Repositories
   - `crates/smartzip-db/src/task.rs`:
     `TaskRepository { insert(NewTask), update_finish(TaskFinish),
     mark_failed(id, code, message), recent(limit) }`.
   - `crates/smartzip-db/src/task_event.rs`:
     `TaskEventRepository { insert(NewTaskEvent), list_by_task(task_id) }`.
     `data_json` holds serialized `TaskEventKind`; `level` maps from
     `TaskEventKind` (Warning/Failed = warn/error, others = info).
   - `crates/smartzip-db/src/encoding_detection.rs`:
     `EncodingDetectionRepository { insert(NewEncodingDetection),
     recent_by_hash(hash, limit) }`.
   - `crates/smartzip-db/src/embedded_archive_detection.rs`:
     `EmbeddedArchiveDetectionRepository { insert_many, recent_by_hash }`.
   - `crates/smartzip-db/src/path_hash.rs`: `pub fn hash_path(&Path) -> String`
     using SHA-256 over the canonicalized path (falls back to raw bytes if
     canonicalization fails).
   - Extend `PasswordRepository` with `record_match_success` /
     `record_match_failure` targeting `password_matches`.
   - Each repository ships with `#[cfg(test)]` round-trip coverage.

3. Recorder trait
   - `crates/smartzip-engine/src/history.rs`:
     ```rust
     #[derive(Debug, Clone)]
     pub struct TaskHistorySummary {
         pub kind: TaskKind,
         pub input_summary: String,
         pub output_path: Option<PathBuf>,
     }

     #[derive(Debug, Clone, Copy, PartialEq, Eq)]
     pub enum TaskHistoryStatus { Completed, Partial, Failed, Cancelled }

     #[derive(Debug, Clone)]
     pub struct TaskHistoryOutcome {
         pub status: TaskHistoryStatus,
         pub finished_at: OffsetDateTime,
         pub error_code: Option<String>,
         pub error_message: Option<String>,
         pub password_attempts: u32,
         pub encoding_selected: Option<String>,
         pub embedded_found: u32,
     }

     pub trait TaskHistoryRecorder: Send + Sync {
         fn on_start(&self, task_id: &TaskId, summary: &TaskHistorySummary);
         fn on_event(&self, task_id: &TaskId, event: &TaskEvent);
         fn on_finish(&self, task_id: &TaskId, outcome: &TaskHistoryOutcome);
     }
     ```
   - `DbTaskHistoryRecorder<'a>` bundles the four repositories.
   - All DB errors become `TaskEventKind::Warning` events via the engine's
     `EventSink` — never propagated as extraction failures.

4. Engine wiring
   - Add a `history: Option<&dyn TaskHistoryRecorder>` parameter to the
     deepest extract entry-point; wrap it in a small `RecorderGuard` that
     runs `on_finish` on drop.
   - `EventSink::push` forwards each event to `history.on_event` when set.
   - `on_start` receives a summary built from `request.inputs`.
   - Password attempts, embedded-finding count, and encoding decisions are
     tallied inside the loop and dropped into the outcome.
   - `SmartZipEngine::detect` records `TaskKind::Detect` history and
     per-finding rows.
   - `password_matches` writes happen whenever `record_password_success` or
     `record_password_failure` fires with a known `PasswordCandidate.id`.

5. CLI
   - `smartzip-cli/src/main.rs`:
     - Add `--no-history` global flag (default off).
     - Construct `DbTaskHistoryRecorder` in `extract` and `detect` and pass
       it through when history is enabled.
     - Print `task-id: <id>` after every run so users can quote it.
   - New `smartzip history` subcommand:
     - `list [--limit N]` → recent tasks (id, kind, status, started_at,
       finished_at, input summary).
     - `show <task-id>` → header + event log; `--json` also supported.

6. Tests
   - `smartzip-db`: unit tests per new repository + migration test.
   - `smartzip-engine`:
     `tests/history_persistence.rs` runs an extract against an existing
     fixture with a real `SmartZipDb` and asserts:
       - one `tasks` row with `status='completed'`,
       - `task_events` contains at least Started + Completed,
       - `encoding_detections` populated for the ZIP fixture,
       - `embedded_archive_detections` populated for the embedded fixture.
   - `smartzip-cli`: existing snapshot suite gains a `history` invocation.

7. Docs alignment
   - Drop the "尚未实现" annotations for the four tables in
     `docs/design.md § 4` and the mirrored lines in
     `docs/implementation-progress.md`.
   - The remaining "target-state" annotations (password sets, batch imports)
     are kept because they are out of scope for this task.

## Validation

- `cargo test -p smartzip-db` → 23 passed (repos, migration v1→v2, path_hash, timestamp)
- `cargo test -p smartzip-engine` → 175 lib + 9 embedded_integration + 3 history_integration + 75 smartzip_integration passed
- `cargo test -p smartzip-cli` → 5 passed
- `cargo build --workspace` → clean

Pre-existing, unrelated: `smartzip-passwords` unit test
`candidates_include_empty_manual_clipboard_and_db_without_duplicates` fails on
the clean tree at HEAD too (it contradicts its sibling
`manual_passwords_disable_database_fallback`). Left untouched — out of scope.

## Implementation Notes (as-built, 2026-07-02)

Deviations from the sketch above, kept for accuracy:

- **Trait method names.** Shipped as `start_extract` / `start_detect` /
  `record_event` / `record_encoding_detection` / `record_embedded_findings` /
  `record_password_match` / `finish` on `TaskHistoryRecorder`, rather than the
  `on_start` / `on_event` / `on_finish` sketch. Detection-table writes need
  archive-path context that a generic `on_event` does not carry, so those got
  dedicated methods called inline where the path is known.
- **Not `Send + Sync`.** `DbTaskHistoryRecorder` holds `&rusqlite::Connection`
  (which is `!Sync`), so the trait is intentionally left without those bounds.
  The extract future is only `.await`-ed (never `spawn`-ed) and already holds a
  non-`Sync` `&PasswordService`, so this changes nothing for the CLI.
- **No `RecorderGuard` drop hook.** `finish` is called explicitly before the
  single `Ok(...)` return. The function has one success exit and errors
  propagate before any history row is opened, so a drop guard added no value.
- **Event replay, not push-forwarding.** `EventSink` is left unchanged; the
  full event timeline is replayed into `task_events` from `events.snapshot()`
  at the end. Detection rows + `password_matches` are written inline during the
  loop where path context exists.
- **Timestamps.** Added `smartzip_db::timestamp::now_utc_iso8601` (dependency-
  free civil-from-days formatter) so multi-row inserts in one task share one
  monotonic UTC clock instead of relying on per-row `CURRENT_TIMESTAMP`.
- **`filename_pattern`.** `password_matches.filename_pattern` is the lowercased
  archive stem with digit runs collapsed to `#`
  (`engine::history::normalize_filename_pattern`), so `dump_2024_01.zip` and
  `dump_2024_02.zip` share a pattern.
- **New workspace dep.** `sha2 = "0.10"` for `path_hash`.

## Non-Goals

- Named password sets / `password_sets` / batch imports (Phase P3-1).
- Cancellation and resume plumbing.
- History retention / GC policies.
- GUI history tab (crate still at prototype stage).
- Encrypted-at-rest DB.

## Exit Condition

- Every `docs/design.md § 4` table has a live migration + repository +
  writer path.
- CLI `smartzip history list` / `show` return non-empty rows after an
  extract or detect.
- Existing test suites still pass; new tests cover the persistence path.
