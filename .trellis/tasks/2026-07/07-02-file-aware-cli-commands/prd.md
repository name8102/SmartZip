# File-Aware CLI Commands (detect / list)

## Goal

承接 `07-02-file-grain-history`（数据库文件级模型 + extract 闭合），把 `detect` / `list` 两个命令接线到文件级历史与 `known_files` 索引，并把与命令共用的**求密码**与**按编码列条目**抽成引擎层共享子流程，为后续 `test` 接入预留能力。

本任务是命令行为层，不改数据库结构（表与字段已由上一任务预留：`file_extractions.status` 取值 `detected/unreadable/intact/corrupt`、`damaged_volumes_json`、`reason=password_required` 均已就位）。

使用场景仍锚定**解压来自网络的压缩包**：老式非 UTF-8 ZIP 文件名乱码、内嵌/嵌套档、分卷缺失、文件名加密。

## Dependencies

- **必须先完成 `07-02-file-grain-history`**：v3 schema、`file_extractions` / `known_files` repo、`sample_hash` / `sample_hash_segment`、recorder trait 的 per-file 方法、extract 的完整闭合。
- 本任务在其之上只加 detect/list/test 的写入路径与共享子流程。

## Confirmed Facts

grilling 阶段已冻结的命令层裁决：

### 命令语义与管线关系
- **detect ⊂ list 前缀子集**：一条实现管线，两个出口。detect 探到格式+编码即停（不列条目）；list 继续走到列条目 + 编码确认循环 + 内嵌定位 + 求密码。
- **detect**（轻活，纯报告，非交互）：输出格式 + 猜测编码 + 内嵌计数 + “文件名是否加密”。**不列条目、不交互确认编码、不写 confirmed_encoding**。写 `tasks` + `file_extractions`（status=`detected`，认不出格式=`unreadable`）。
- **list**（重活，要求最终必须看到内容）：走完整管线——乱码 → 交互切换编码直到不乱码；内嵌 → 定位；文件名加密 → 求密码解出清单。用户确认/选定的编码 → 写 `known_files.confirmed_encoding`（人工确认，**覆盖**旧值）；求到的密码 → 写 `known_files.password_id`。**不写 `last_extract_at`**（list 不解压落盘）。
- **test**（独立命令，全量校验）由于后端复杂度高于预期，**从当前任务拆分**：本任务只保留 CLI 顶层接口与共享子流程预留，不实现 `t` 全量读校验、损坏卷定位或 `damaged_volumes_json` 写入。

### 共享子流程（抽到引擎层，三命令共用，避免各写一份）
1. **求密码流程**（list / extract 共用；为 test 后续接入保留）：候选顺序 = 命令行 `--password` → `known_files.password_id`（hash+size 命中，置顶不独占）→ 配置文件通配符（**将来 TODO**）→ `passwords` 库排序。候选试穿 → 交互询问用户输入。成功后：bump `passwords` 计数 + 写 `known_files.password_id`。用户取消/放弃 → status=`skipped` + reason=`password_required`（统一，不为加密单设终态）。
2. **按编码列条目** `list_entries(archive, encoding) -> Vec<Entry>`：detect 走一次即停；list 在编码确认循环里反复调。

### 编码确认交互（list）
- CLI 只做**基本功能**：默认用检测器最佳猜测编码列一次；乱码时用 `--encoding <name>` 重列，或 `--pick-encoding` 一次性打印 top-N 候选编码各自解出的前几个条目名供用户选一个（纯 stdout + stdin，**不做 TUI**）。
- **最佳编码交互体验放 GUI 实现**，CLI 不追求原地重刷/方向键切换那种体验（TUI 成本高，已放弃）。
- 只有用户**显式选定**编码的那次（`--encoding` / `--pick-encoding`）才算人工确认 → 写 confirmed_encoding；检测器默认猜的那次不写。

### 加密档边界
- **文件名加密档**（header-encrypted 7z/RAR，不给密码连条目名都看不到）：list 必须求密码才能列条目（走共享求密码流程）；detect 免疫，只报告“文件名已加密”不求密码。
- 普通加密 ZIP：条目名在未加密的 central directory，能 list 条目名（可确认编码），解内容才要密码。
- `test` 的密码处理与损坏校验后续在拆分任务中实现；本任务仅保证共享求密码流程可被后续复用。

### known_files 写入分工（本任务新增的写入路径）
- **detect**：不写 known_files（它不确认编码、不解压）。
- **list**：人工确认编码后 `upsert_confirmed_encoding`（写 confirmed_encoding，追加 name+offset，不写 last_extract_at）；求到密码写 password_id。
- `test`：已从当前任务拆分；其 password_id / damaged_volumes_json 写入在后续任务定义。
- UPSERT 逐字段合并语义沿用上一任务：人工确认编码覆盖、猜测不覆盖；last_extract_at 仅 extract 写。

## Out of Scope

- 数据库结构改动（已由 `07-02-file-grain-history` 落地）。
- `test` 后端实现、`t` 全量校验接线、损坏卷定位与 `damaged_volumes_json` 写入（已拆分为后续独立任务）。
- hashcat `crack_jobs` 深度爆破表与接线。
- `compress` 写库。
- 配置文件通配符/正则密码匹配层（求密码流程留 TODO 占位）。
- GUI 编码确认交互 / TUI。
- 去重跳过逻辑（属 extract，已在上一任务闭合；detect/list 只读不跳过）。

## Acceptance

- `detect <inputs>`：输出格式 + 猜测编码 + 内嵌计数 + 文件名加密标记；写 `file_extractions`（detected/unreadable）；不写 known_files。
- `list <inputs>`：能列出条目；乱码时 `--encoding` / `--pick-encoding` 可选定正确编码并复列；选定编码写入 `known_files.confirmed_encoding`；文件名加密档能经求密码流程列出条目。
- 共享求密码流程被 list / extract 两处调用，并为后续 test 复用保留稳定接口；无重复实现；求密码候选顺序正确，用户取消统一落 `skipped`+`password_required`。
- `list_entries(archive, encoding)` 被 detect（一次）与 list（循环）共用。
- `smartzip test` 在当前任务中仅保留顶层接口与明确未实现错误语义，不伪造校验结果。
- 全 workspace `cargo test` 通过。
