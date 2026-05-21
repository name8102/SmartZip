# SmartZip 实施计划

> 基于 `CONTEXT.md`、`docs/improvement.md`、Code Review 及架构建议。
> 25 项需求按优先级分 5 个 Phase。

---

## Phase 0 — 立即修复：安全/数据 + 行为错误

| # | 需求 | 文件 | 改动 |
|---|------|------|------|
| C5 | `collapse_single_output` 盲目 `remove_dir_all` | `smartzip-engine/src/lib.rs` | 用追加 `_collided_N` 替换 `remove_dir_all`，见 B7 |
| C1 | `parse_entries` 含压缩包自身为条目 | `smartzip-archive/src/sevenzz.rs` | 识别 `Type = ` 行过滤压缩包元数据块 |
| C2 | `map_failure` 不返回细分错误 | `smartzip-archive/src/sevenzz.rs` | `BackendFailed` → 正确返回 `WrongPassword` / `UnsupportedFormat` |
| C3 | 清理时间戳格式不匹配 | `smartzip-cli/src/main.rs` | `cutoff.format("%Y-%m-%d %H:%M:%S")` 匹配 SQLite CURRENT_TIMESTAMP |

## Phase 1 — 核心管线修正

| # | 需求 | 文件 | 改动 |
|---|------|------|------|
| B1 | 幻数优先 | `smartzip-engine/src/lib.rs` | 调整 `extract_recursive`：先 scanner 后 extension |
| B2 | 加密归档延迟编码 | `smartzip-engine/src/lib.rs` | 加密时跳过 pre-list；密码匹配后补做 list + encoding detect |
| B3 | 测试先行 | `smartzip-engine/src/lib.rs` | 密码循环改调 `backend.test()`，命中后单次 `extract` |
| C6 | 缓存密码候选 | `smartzip-engine/src/lib.rs` | 进入 `extract_recursive` 时一次性拉取 ranked_candidates 到 Vec 缓存 |
| C4 | GUI 拖拽不阻塞 | `smartzip-gui/src/main.rs` | `cx.spawn` 异步执行 `engine.detect` |

## Phase 2 — 架构升级

| # | 需求 | 文件 | 改动 |
|---|------|------|------|
| A1 | 实时事件流 mpsc | `smartzip-engine/src/lib.rs` + CLI | `extract_recursive` 接受 `UnboundedSender<TaskEvent>`；CLI spawn 消费打印 |
| D1 | 双向 oneshot 通道交互 | `smartzip-engine/src/lib.rs` + CLI | 消去 `InteractivePasswordPrompter` trait；用 `PasswordRequired` 事件 + `oneshot::Sender` |
| D2 | 领域错误收敛 | `smartzip-core/src/error.rs` + 各 crate | 增加 `Database(DbError)` / `ScannerFailed` 等子变体 |
| B6 | 并行爆破 + kill-on-drop | `smartzip-engine/src/lib.rs` | `futures::stream` 并发 `test` + `kill_on_drop(true)` |
| B5 | 内嵌压缩包切片 | `smartzip-engine/src/lib.rs` | 含 offset 的候选先 carve 到临时文件再交后端 |

## Phase 3 — 混合后端 + 体验

| # | 需求 | 文件 | 改动 |
|---|------|------|------|
| A2 | 混合后端 NativeBackend | `smartzip-archive/` | 新建 `NativeBackend`；Rust 原生 zip/tar/gz/bz2/xz + 7zz fallback |
| B7 | 命名模板 + 安全碰撞 | `smartzip-engine/src/lib.rs` + config | `{stem}_{depth}_{format}` 模板渲染；追加 `_collided_N` |
| B4 | 先过滤再坍塌 | `smartzip-engine/src/lib.rs` | Glob 规则清洗 → 再判断坍塌 |
| B8 | Zip Slip 防御 | `smartzip-engine/src/lib.rs` | 拦截 `../`、绝对路径、外部符号链接 |
| B9 | 回收站删除 | `smartzip-platform/src/lib.rs` + engine | 平台 Trash API |

## Phase 4 — 持久化

| # | 需求 | 文件 | 改动 |
|---|------|------|------|
| B10 | 任务历史表 | `smartzip-db/src/schema.rs` | 新建 `task_histories` 表 |
| D3 | 断点日志与作业持久化 | `smartzip-engine/` + `smartzip-db/` | Task journal + 崩溃恢复/续传 |

---

## 依赖关系

```
Phase 0 (C5,C1,C2,C3)  ← 互相独立，可并行
    ↓
Phase 1 (B1,B2,B3,C6,C4)  ← 依赖 C1,C2 修完
    ↓
Phase 2 (A1,D1,D2,B6,B5)  ← 依赖 Phase 1 管线稳定
    ↓
Phase 3 (A2,B7,B4,B8,B9)  ← 依赖 Phase 2 架构就绪
    ↓
Phase 4 (B10,D3)  ← 独立，但建议最后
```
