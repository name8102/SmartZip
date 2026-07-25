# Implementation Plan — File-Aware CLI Commands

依赖 `07-02-file-grain-history` 完成。本任务不改 schema，只加命令行为 + 共享子流程。两阶段：engine 共享子流程 → cli detect/list 接线；`test` 从当前任务分离，后续单开任务实现。

---

## 阶段一：engine 共享子流程

**求密码流程**（`smartzip-engine`，抽成共享函数/结构）

- 抽出 `resolve_password(ctx) -> PasswordOutcome`，供 extract / list 共用，并作为后续 test 任务的稳定接入点。
- 候选队列构造（与 extract 已实现的顺序一致，抽出复用）：
  1. 命令行 `--password`
  2. `known_files.password_id`（`lookup_known_file(hash,size)` 命中 → 取 value，置顶不独占）
  3. 配置通配符层 —— **TODO 占位**（留函数钩子，本次返回空）
  4. `passwords.ranked_candidates` 兜底
- 去重后依次试；试穿 → 交互 prompter 询问用户输入（复用现有 `InteractivePasswordPrompter`）。
- 结果：成功 → 返回 password_id（供调用方写 known_files + bump 计数）；用户取消 → 调用方落 `skipped`+`password_required`。

**按编码列条目**（`list_entries`）

- `list_entries(backend, archive, encoding) -> Result<Vec<Entry>>`：用指定编码解条目名。
- detect 调一次（探编码后即停，不必打印）；list 在编码确认循环里反复调。
- 复用现有后端的列条目能力（`smartzip-archive` 的 list/probe 路径），编码参数透传。

**测试（阶段一）**：求密码候选顺序（命令行 > known_files > 库）、置顶不独占的 fallback、用户取消路径；`list_entries` 不同编码解出不同条目名。

---

## 阶段二：cli detect/list 接线

**detect（`smartzip-cli/src/main.rs`，语义扩张）**

- 复合输出：格式 + 猜测编码（`list_entries` 一次） + 内嵌计数 + 文件名加密标记。
- 写 `file_extractions`：status=`detected`（认不出格式=`unreadable`）。**不写 known_files、不交互确认编码。**
- 非交互，可管道。

**list（新命令）**

- 默认：检测器最佳猜测编码 → `list_entries` → 打印条目。
- `--encoding <name>`：指定编码复列。
- `--pick-encoding`：一次性打印 top-N 候选编码各自解出的前几个条目名 + 一行 `read` 选择（纯 stdout/stdin，无 TUI）。
- 文件名加密档：调共享求密码流程解出条目（试不出/取消 → 报错 + `skipped`+`password_required`）。
- 用户显式选定编码（`--encoding`/`--pick-encoding`）→ `upsert_confirmed_encoding`（写 confirmed_encoding，追加 name+offset，不写 last_extract_at）。求到密码 → 写 password_id。
- 写 `file_extractions`：status 复用 detected/unreadable。

**test（接口保留，后端后置）**

- 保留顶层命令与参数占位。
- 当前实现明确返回未实现错误与 exit 1。
- 不伪造 `intact/corrupt`、不写 `damaged_volumes_json`、不接密码校验后端。

**验证**

- `cargo build` + `cargo test` 全 workspace 绿。
- 手动：
  - 乱码 ZIP fixture → `list` 默认乱码 → `list --encoding gb18030` 正常 → 校验 known_files.confirmed_encoding 写入。
  - 加密档 → `list` 经求密码流程。
  - `smartzip test` → 明确未实现错误且 exit 1。
- 清理临时产物。

---

## 明确不在本次范围

- schema 改动（上一任务已落）。
- `test` 后端、损坏定位、`damaged_volumes_json` 写入（拆分到后续任务）。
- hashcat crack_jobs、compress 写库、GUI 编码交互 / TUI。
- 配置通配符密码层（仅 TODO 钩子）。
- extract 的去重跳过（上一任务已闭合）。
