# Implementation Plan — File-Grain History (v3)

三阶段推进：db → engine → cli+验证。旧表无有效数据，直接建 v3 新结构，不回填。

---

## 阶段一：db 层

**Schema (migration v3, `smartzip-db/src/schema.rs`)**

- `LATEST_VERSION = 3`，新增一个 MigrationStep(version 3)：
  - `DROP TABLE encoding_detections;`
  - `DROP TABLE embedded_archive_detections;`
  - `DROP TABLE password_matches;`
  - 瘦身 `tasks`（方案 A）：rebuild 为 `id / kind / status / output_path / started_at / finished_at`。旧表无数据，直接 `DROP TABLE tasks` + `CREATE TABLE tasks (...)`（不做 INSERT 迁移）。保留 `idx_tasks_started_at`、`idx_tasks_status`。
  - 建 `file_extractions`（见下）+ 索引。
  - 建 `known_files`（见下）+ `UNIQUE(sample_hash, size)`。
- migration 保持事务包裹、idempotent（沿用 `apply_step`）。

```sql
CREATE TABLE file_extractions (
    id                  INTEGER PRIMARY KEY AUTOINCREMENT,
    task_id             TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    input_path          TEXT NOT NULL,
    sample_hash         TEXT,
    file_size           INTEGER,
    offset              INTEGER,
    output_path         TEXT,
    has_password        INTEGER NOT NULL DEFAULT 0,
    password_id         INTEGER REFERENCES passwords(id) ON DELETE SET NULL,
    status              TEXT NOT NULL,
    reason              TEXT,
    encoding            TEXT,
    encoding_corrected  INTEGER NOT NULL DEFAULT 0,
    damaged_volumes_json TEXT,
    created_at          TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
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
```

**Repos**

- `file_extractions.rs`（新）：`insert(NewFileExtraction)`、`recent(limit)`、`list_by_task(task_id)`、`list_by_status(status, limit)`、`list_by_reason(reason, limit)`。纯 append，无 update。
- `known_files.rs`（新）：
  - `find(sample_hash, size) -> Option<KnownFile>`（一次查询同时拿 `password_id` + `confirmed_encoding` + `last_extract_at`）。
  - `upsert_extract(...)`：写 `last_extract_at` + `password_id`，追加 `names_offsets_json`。合并语义：不覆盖已存在的 `confirmed_encoding`。
  - `upsert_confirmed_encoding(...)`：人工确认编码时写 `confirmed_encoding`（**覆盖**旧值），不写 `last_extract_at`。追加 name+offset。
  - `dedup_hit(sample_hash, size, window) -> bool`：`last_extract_at` 非空且在时间窗内。
- 删除 `password.rs` 中 `record_match_success/failure/matches_for/PasswordMatch/map_match`（password_matches 已 DROP）。`upsert/ranked_candidates/record_success/record_failure/disable/delete/get_by_value` 保留。
- 删除 `encoding_detection.rs`、`embedded_archive_detection.rs` 两个模块（连同 `lib.rs` 的 mod 声明）。

**sample_hash 函数**（`smartzip-db/src/sample_hash.rs`，新）

- `sample_hash(path) -> Option<(String, u64)>`：返回 (hash, size)。
  - `size = metadata.len()`。
  - `< 128KB`：全量读入 → BLAKE3。
  - `>= 128KB`：读前 64KB + 后 64KB（两次 seek）→ BLAKE3(head ‖ tail)，size 混入 hash 或与 hash 一并返回参与判等。
- `sample_hash_segment(path, offset, size) -> Option<(String, u64)>`：carve 档，对 `[offset, offset+size)` 做同样头尾采样。**size 未知（None）时返回 None**（不参与去重）。
- 依赖：`blake3` crate（加入 `smartzip-db/Cargo.toml`）。

**测试（阶段一）**：migration 建全表 + DROP 生效、`known_files` UPSERT 合并语义（编码不被覆盖 vs 人工覆盖）、`sample_hash` 大小文件分支 + segment、去重窗口命中/过期。

---

## 阶段二：engine 层

**recorder trait 改造(`smartzip-engine/src/history.rs`)**

- 删除 `record_password_match` 方法 + `record_password_match_success` helper + `normalize_filename_pattern`（password_matches 已删）。
- 删除 `record_encoding_detection` / `record_embedded_findings`（对应表已 DROP）。
- 新增 per-file 方法：
  ```rust
  fn record_file_extraction(&self, task_id: &TaskId, row: FileExtractionRow<'_>);
  fn lookup_known_file(&self, sample_hash: &str, size: u64) -> Option<KnownFileHit>;
  fn upsert_known_file_extract(&self, hit: KnownFileUpsert<'_>);
  ```
- `TaskOutcome` 精简：删 `encoding_selected` / `embedded_found`（下沉 file 行）；`tasks` 只留聚合 `status`。

**extract 主循环改造(`smartzip-engine/src/lib.rs`)**

1. **拆分跳过打 reason**：现 `lib.rs:422-428` 的 `!is_new || depth>limit || !is_first_volume` 合并 `skipped.push` → 拆成带 reason 的分支（`duplicate` / `recursion_limit` / `not_first_volume`）；业务容器跳过 → `business_container`；每种跳过各写一条 `file_extractions` 行（status=`skipped`+reason）。
2. **解压前查 known_files**：对 root/nested 档算 `sample_hash` → `lookup_known_file`：
   - 命中 `confirmed_encoding` 且命令行未指定编码 → 采用（置顶）。
   - 命中 `last_extract_at` 在窗口内且非 `--force` → 跳过，status=`skipped`+reason=`duplicate`，发提示事件。
3. **求密码候选顺序注入**：候选队列 = 命令行 `--password` + known_files.password_id（置顶不独占）+ 当前批次交互成功密码 + [通配符层 TODO] + `ranked_candidates` 兜底，去重后依次试。交互成功时立即写密码库并更新任务内候选缓存，不等 task 结束。
4. **per-file 记录**：在 `processed.push(candidate)`（`lib.rs:~1357`）处，`actual_output_dir` 在手 → `record_file_extraction`（input_path、sample_hash、size、offset、output_path=actual_output_dir、has_password、password_id、status=`extracted`、encoding、encoding_corrected）。carve 档用 `sample_hash_segment`。
5. **成功后 UPSERT known_files**：`upsert_known_file_extract`（写 last_extract_at + password_id，追加 name+offset）。
6. **失败/部分**：失败分支写 status=`failed`+reason（`wrong_password` / `corrupt` / `not_found`）；"需密码未拿到" → `skipped`+`password_required`。
7. 删除 `hist_encoding_selected` / `hist_embedded_found` / `hist_last_output` 聚合累积（改由 file 行承载）；`tasks.status` 仍按 processed/failure 聚合。

**测试(阶段二)**：`history_integration.rs` 重写——per-file 行数与 status/reason 正确、carve 档 offset 记录、known_files 去重跳过 + `--force` 绕过、confirmed_encoding 复用、密码顺序注入；同批多文件只交互一次并复用刚成功的密码。

---

## 阶段三：cli + 验证

**extract 接线(`smartzip-cli/src/main.rs`)**

- `DbTaskHistoryRecorder` 注入不变；适配新 trait 方法。
- extract 加 `--force`（忽略去重跳过）。去重窗口默认 1 月，预留配置读取接口（本次硬编码 + TODO 注释）。

**history 命令(两模式 + 下钻 + 过滤)**

- `history tasks`（默认）：读 `tasks`，列 kind/status/时间/output_path。
- `history files`：读 `file_extractions`，列 input_path/status/reason/encoding/output_path/offset/坏卷。支持 `--status <s>` / `--reason <r>` 过滤（走 `idx_file_extractions_status`）。
- `history show <task_id>`：`task_events` 时间线 + 该 task 的所有 file_extractions 行。

**test / list / detect**：本次**不接线**。仅确认新表字段容纳其未来数据（status 取值 `detected/unreadable/intact/corrupt`、`damaged_volumes_json`、`password_required` 均已在 schema/reason 枚举中预留）。命令行为改动留后续任务。

**验证**

- `cargo build` + `cargo test`（全 workspace）绿。
- 手动跑一次 extract（含分卷 + 密码 + 嵌套 fixture）→ `history files` 校验 per-file 行；重复跑一次校验去重跳过 + `--force`。
- 清理临时产物。

---

## 明确不在本次范围

- test / list / detect 命令的交互实现（编码切换 UI、坏卷定位逻辑、内嵌进入）。
- 配置文件通配符密码匹配（求密码流程留 TODO 层）。
- hashcat `crack_jobs` 表与深度爆破。
- compress 写库。
- 编码确认的 GUI 最佳体验。
