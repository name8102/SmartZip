# Database History Persistence

## Goal

进一步完善 SmartZip 的 SQLite 数据层：把 `docs/design.md § 4` 中已经描述但尚未
落库的 `tasks`、`task_events`、`encoding_detections`、`embedded_archive_detections`
四张表实际实现，并让 `smartzip-engine` 的解压与检测流程通过注入的历史记录器
（recorder）写入这些表，供 CLI/GUI 后续查询。

## Motivation

- `docs/implementation-progress.md` 明确标注这四张表“尚未实现”。
- `smartzip-engine` 现在已经把丰富的 `TaskEvent` 全部汇总在返回值里，只差一层
  持久化，就能让任务历史、编码修正历史、内嵌归档发现历史都可回溯。
- `password_matches` 已建表但没有任何写入路径；这次一并补上最基本的
  `filename_pattern` 命中记录，让 Phase P3 的密码策略可以基于已有数据继续。

## Scope

### In scope

- SQLite schema：新增 v2 migration，创建四张目标表并保持 v1 迁移向前兼容。
- 仓储 API：`TaskRepository`、`TaskEventRepository`、`EncodingDetectionRepository`、
  `EmbeddedArchiveDetectionRepository`；`PasswordRepository` 增加 match 记录。
- Engine：`TaskHistoryRecorder` trait + `DbTaskHistoryRecorder` 实现；
  extract / detect 流程按 `recorder: Option<&dyn TaskHistoryRecorder>` 注入。
- CLI：默认使用 DB recorder，新增 `--no-history` 全局开关；新增
  `smartzip history list` 和 `smartzip history show <task-id>` 两个只读命令。
- 文档：`docs/design.md § 4` 与 `docs/implementation-progress.md` 的实现状态描述。
- 测试：单元测试覆盖迁移、每个仓储、engine 端到端历史写入、CLI history 输出。

### Out of scope

- 命名密码表 / 批量密码集（`password_sets` / `password_set_memberships`）—— 归 P3。
- 密码相似度打分（`directory_hash`、路径相似度）—— 归 P3。
- 任务取消 / resume / 崩溃恢复 —— 归 Phase 5+。
- 历史记录 GC / 保留期策略 —— 后续任务。
- GUI 集成 —— 保持原型状态不变。

## Requirements

1. `SmartZipDb::open` 在第一次遇到 v1 数据库时必须无损升级到 v2，之后不再重复
   执行 v2 步骤。`schema_migrations` 用 `version` 主键顺序标注。
2. Recorder 采用“best-effort”语义：任何写入失败必须转为 `TaskEventKind::Warning`
   或 `eprintln!` 提示，不能阻断解压流程，也不能改变 `ExtractWorkflowResult` 的
   最终成功/失败判定。
3. 每次 `extract_recursive_*` / `detect` 调用都必须落一条 `tasks` 行，并写入若干
   `task_events`；根输入的编码检测和内嵌归档发现必须分别落到
   `encoding_detections` / `embedded_archive_detections`。
4. `path` 类字段一律以 SHA-256 十六进制字符串存储；输入优先使用
   `fs::canonicalize`，失败时回退到 raw bytes 哈希。原始路径 **不** 落库，避免
   在多用户机器上泄漏敏感目录名。
5. CLI `history list --limit N` 默认 20，最大 200；`history show <task-id>` 按
   `task_events.created_at` 顺序显示；两个命令都返回 exit 0（未找到即空列表）。
6. `--no-history` 全局开关仅关闭 recorder；`password` 库的写入仍然按现有逻辑。

## Acceptance Criteria

- [ ] 打开 v1 数据库文件可无缝升级并写入四张新表；`schema_migrations`
      有 `version=1` 与 `version=2` 两条记录。
- [ ] `cargo test -p smartzip-db` 通过；覆盖迁移与每个仓储的插入/查询路径。
- [ ] `cargo test -p smartzip-engine` 通过；新增的历史测试断言：
      - 每个 extract 调用写入 1 条 `tasks` + N 条 `task_events`
      - 检测到的编码写入 `encoding_detections`
      - `smartzip-fixtures` 中的内嵌样本写入 `embedded_archive_detections`
- [ ] `cargo test -p smartzip-cli` 通过；`history` 子命令在临时 DB 上的两个
      snapshot 断言成立。
- [ ] `docs/design.md § 4` 的“尚未实现”预警注释被删除；
      `docs/implementation-progress.md` 的相关行改为 ✅。
- [ ] `smartzip extract` 在 `--no-history` 下不再写入任何历史行；无该开关时会
      写入 `tasks` 行；这两种情况均在 CLI 测试里覆盖。

## Non-Goals

- 不重写 password 领域模型。
- 不改变现有 CLI exit code 语义。
- 不引入 async SQLite（保持 `rusqlite` blocking）。
