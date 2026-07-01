# Database History Persistence Design

## Layered View

```
smartzip-cli ──► SmartZipEngine::extract_recursive_*
                         │
                         ├── PasswordService       (existing)
                         ├── ArchiveBackend        (existing)
                         └── TaskHistoryRecorder   (new, optional injection)
                                    │
                                    ▼
                         DbTaskHistoryRecorder
                                    │
                                    ▼
             ┌───────────────────────────────────────────────┐
             │ TaskRepository                                │
             │ TaskEventRepository                           │
             │ EncodingDetectionRepository                   │
             │ EmbeddedArchiveDetectionRepository            │
             │ PasswordRepository::record_match_*  (extended)│
             └───────────────────────────────────────────────┘
                                    │
                                    ▼
                              SQLite (v2 schema)
```

## Data Flow: Extract

1. CLI 构造 `DbTaskHistoryRecorder`（除非 `--no-history`）并注入 engine。
2. Engine 在开始时调用 `recorder.start_extract(task_id, summary)`，写入
   `tasks` 行状态为 `running`。
3. `EventSink` 在原有 listener 广播的基础上，再把每条事件转发给 recorder；
   recorder 负责：
   - 将事件序列化写入 `task_events`（`data_json` = 事件 payload）。
   - 特化处理：
     - `EncodingDetected` → 追加一行 `encoding_detections`（键 = 当前 archive
       路径哈希 + format）。
     - `EmbeddedArchiveFound` → 累积到当前 candidate 的 batch，在下一次
       candidate 切换或 flush 时批量写入 `embedded_archive_detections`。
     - `PasswordTried` → 递增内存中的 `password_attempts` 计数。
     - `Completed` / `Failed` → 记录终态。
4. Engine 主循环退出时调用 `recorder.finish(TaskOutcome { status, error_code,
   error_message, output_path, password_attempts, encoding_selected, embedded_found })`；
   recorder 用一次 `UPDATE tasks SET …` 收尾。
5. `RecorderGuard`（Drop 实现）保证任何 panic 或提前 return 都会把 tasks 行
   标为 `failed`；正常路径下 `finish` 会消费 guard，避免二次写入。

## Data Flow: Detect

- 与 extract 同结构，`TaskKind::Detect`；`embedded_found` 由扫描结果决定；
  `finish` 前把每个 finding 单独写入 `embedded_archive_detections`。

## Path Hashing

- 输入路径先 `std::fs::canonicalize`；失败（文件不存在等）时使用原始
  `Path::as_os_str().as_encoded_bytes()`。
- 哈希 = `sha2::Sha256` 输出的十六进制字符串。
- 单独的 `path_hash::hash_path(path: &Path) -> String` 工具函数，位于
  `smartzip-db::path_hash`。

## Schema Changes

`schema.rs` 拆成 step-driven 结构：

```rust
struct Migration { version: i64, sql: &'static str }
const MIGRATIONS: &[Migration] = &[
    Migration { version: 1, sql: V1_SQL },
    Migration { version: 2, sql: V2_SQL },
];
```

`migrate` 读取 `MAX(version)`，按序执行 v2 之后的 step，每步在一个事务里执行
DDL + `INSERT OR IGNORE INTO schema_migrations`。

`V2_SQL` 完整对照 `docs/design.md § 4.3–4.6`：
- `tasks` / `task_events` / `encoding_detections` / `embedded_archive_detections`
  及其索引。
- `password_matches` schema 保持 v1，无需变更（v1 已经建表）。

## Repository API 概览

```rust
// TaskRepository
pub struct NewTask<'a> { pub id: &'a str, pub kind: TaskKind, pub input_summary: &'a str }
pub struct TaskFinish<'a> {
    pub id: &'a str,
    pub status: TaskStatus,
    pub output_path: Option<&'a str>,
    pub error_code: Option<&'a str>,
    pub error_message: Option<&'a str>,
    pub password_attempts: i64,
    pub encoding_selected: Option<&'a str>,
    pub embedded_found: i64,
}
impl TaskRepository<'_> {
    fn insert(&self, input: NewTask<'_>) -> Result<()>;
    fn finish(&self, input: TaskFinish<'_>) -> Result<()>;
    fn mark_failed(&self, id: &str, error: &str) -> Result<()>;
    fn list_recent(&self, limit: usize) -> Result<Vec<TaskRow>>;
    fn get(&self, id: &str) -> Result<Option<TaskRow>>;
}

// TaskEventRepository
pub struct NewTaskEvent<'a> {
    pub task_id: &'a str,
    pub level: EventLevel, // Info / Warn / Error
    pub event_type: &'a str,
    pub message: &'a str,
    pub data_json: Option<&'a str>,
}
impl TaskEventRepository<'_> {
    fn insert(&self, event: NewTaskEvent<'_>) -> Result<()>;
    fn list_by_task(&self, task_id: &str) -> Result<Vec<TaskEventRow>>;
}

// EncodingDetectionRepository / EmbeddedArchiveDetectionRepository：类似的 insert + query
```

## Engine Wiring

- 新增 `crates/smartzip-engine/src/history.rs`。
- `SmartZipEngine::extract_recursive_with_listener_interactive` 增加一个尾部
  参数 `recorder: Option<Arc<dyn TaskHistoryRecorder>>`。所有更短的 overload
  都传 `None` 保证向后兼容。
- `SmartZipEngine::detect` 也增加同名可选参数（通过便利方法 `detect_with_recorder`）。
- Recorder 的接口本身返回 `Result<(), DbError>`，Engine 内部把错误吞为 `Warning`
  事件；不重新 `?` 传出。

## CLI Wiring

- `Cli::db` 保持；新增 `--no-history: bool`。
- 在 `open_db` 之后构造 `DbTaskHistoryRecorder<'db>` 的 `Arc`。`--no-history`
  下不构造。
- `Command::History { …Subcmd }`:
  - `list --limit N` (default 20, max 200) → 打印 `task-id, kind, status, started_at, output_path`。
  - `show <task-id>` → 打印任务概要 + 按时间排序的事件。
- 输出格式：默认表格；`--json` 输出结构化。

## Error / Warning Semantics

- Recorder 写入错误不写入自身表（避免死循环）。
- 每次失败会通过 `TaskEventListener` 广播 `Warning { message: "history: <err>" }`
  一次；再次失败同类事件会 rate-limit（首个失败后 1 秒内相同错误合并）。
- Engine 返回值语义不变。

## Trade-offs

- **同步 rusqlite**：保持简单，代价是记录会阻塞事件路径。写入使用短事务，
  在实测数据集上估计每条事件耗时 <1ms，可接受。
- **路径只存哈希**：牺牲了未来"按路径回溯"的便利，但换来隐私默认安全，
  历史行也可以脱敏导出。
- **不引入 async channel**：直接同步调用 recorder，避免 back-pressure 复杂度。
- **guarded finish**：`Drop` 里做 fallback 写入并不能覆盖 SQLite 连接被 poison
  的场景，但已能覆盖大多数错误退出路径。

## Risks

- `rusqlite::Connection` 不是 `Sync`。Recorder 内部持有 `Arc<Mutex<Connection>>`
  的读写锁，或每个方法参数带 `&Connection`。设计选后者：Recorder 存
  `&'a SmartZipDb` 引用；线程访问模型与现有 `PasswordRepository` 保持一致。
- 若 CLI 进程崩溃在 recorder 之外，`tasks` 行会留在 `running` 状态。后续可
  加启动时 sweep（`UPDATE tasks SET status='failed' WHERE status='running'`），
  但本 task 不做。
