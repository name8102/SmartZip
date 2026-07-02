# File-Grain History & Known-Files Index

## Goal

把历史记录从**操作级**重构为**文件级**：每个触发解压流程的文件都要有独立记录（状态、原因、编码、落点、offset、坏卷列表），并额外建立一张 `known_files` 索引，用 `sample_hash + size` 对“网络下载的重复压缩包”去重、复用用户确认过的编码、复用开包成功的密码。

本任务承接已完成的 `07-02-database-history-persistence`（操作级四表模型），将其推倒为文件级模型。范围严格锁定在**数据库层 + 闭合已实现的 extract / history 命令**；未实现的 `detect / test / list` 只由新表结构预留字段，命令本身不接线、留后续任务。

使用场景锚定：**解压来自网络的压缩包**。压缩包的密码明文和路径都不敏感（路径可明文存、密码只存 `password_id` 不存明文）。核心痛点是同一个包被重复下载、老式非 UTF-8 ZIP 文件名乱码、分卷缺失、内嵌/嵌套档。

## Confirmed Facts

这些是 grilling 阶段已冻结的裁决，实现时不再重开讨论：

### 数据模型
- 旧表**无有效数据**，v3 迁移直接 DROP + 建新，**不回填**。
- **DROP**：`encoding_detections`、`embedded_archive_detections`、`password_matches`。
- `tasks` **瘦身**为纯操作级父表（方案 A）：只留 `id / kind / status / output_path / started_at / finished_at`。删掉 `input_summary / error_code / error_message / password_attempts / encoding_selected / embedded_found`。`status` 保留为反规范化缓存（供 history 列表首屏，不必每行现算聚合）。`output_path` 语义是“用户请求的输出根目录”（操作级）。
- 新建 `file_extractions`（**append-only 日志**，一行 = 一次解压动作）。
- 新建 `known_files`（**去重 / 复用索引**，`UNIQUE(sample_hash, size)`，UPSERT）。
- **日志与索引必须是两张表**：日志要保留重复项（同包解压多次 = 多行，历史要能回答“上次为什么失败”），索引每个 `(sample_hash,size)` 恰好一行（匹配热路径要小而快）。单表方案会逼出 `GROUP BY` + 行数无限增长，或牺牲日志完整性——都不接受。

### `file_extractions` 一行的语义
- 一行 = **一次解压动作**，不是“一个文件”。
- root 输入不是压缩档时也是一行（`status` 反映“未找到压缩文件”）。
- 普通文件：`sample_hash` = 整文件采样，`offset` = NULL，`input_path` = 自身。
- carve/内嵌档：`sample_hash` = 对 `[offset, offset+size)` 那段字节采样，`offset` = Some，`input_path` = **宿主路径**（仅供展示“来自 host.bin”）。
- 分卷：全部输入时，首卷记 `extracted`，其余记 `skipped`（reason=`not_first_volume`）；只输入首卷时只有首卷一行。
- 内嵌：匹配并解压多个内嵌档时**每个都记一行**（靠 offset 区分）；没找到或只找到一个时记外层文件。
- 嵌套档每层都记录。总原则：**输入的文件都要有记录，每个触发了解压流程的文件都要记录**。

### sample_hash（采样哈希）
- 形状：`BLAKE3(前 64KB ‖ 后 64KB)`，配合 `file_size` 一起判等（size 是判等的另一半）。
- 文件 < 128KB：全量哈希。
- carve 档：对 `[offset, offset+size)` 段做同样的头尾采样。
- **size 未知的 carve 档**（scanner 的 `size_hint` 可为 NULL）：`sample_hash` 与 `file_size` 都写 NULL，**不参与去重**（每次都解，可能重复解 —— 已接受，见 11a）。

### 去重
- 只对 **extract** 生效（detect/list/test 只读，不跳过）。
- 命中条件：`known_files` 里 `last_extract_at` **非空**且落在**时间窗内**（默认 1 个月，为将来配置文件预留接口）。`last_extract_at` 非空即“成功解压过”，是唯一判据（known_files 无 status 列）。
- 命中即跳过 + **显式提示**；`--force` 忽略去重强制重解。
- 去重比较对象是**历史索引**，不追踪输出是否还在（“解压后文件必然重新整理，无法追踪输出位置”）——因此靠时间窗兜底，而非查输出存在性。

### known_files UPSERT 合并语义
- 字段：`sample_hash / size / names_offsets_json / password_id / confirmed_encoding / last_extract_at`。
- `names_offsets_json`：`[{name, offset}]` 配对列表，每次遇到新的 name/offset 组合就**追加**（同一内嵌档嵌在不同宿主 / 不同 offset 时，name 与 offset 必须成对，不能拆成两列各存一个）。
- 编码合并：detect 的**自动猜测永不覆盖**已确认编码；**人工确认**编码覆盖之前的（无论来自 detect 还是 list）。
- `last_extract_at`：**仅 extract 写**；detect 不写（detect 不解压，只确认编码）。
- UPSERT 是**逐字段合并**（填 NULL 的、追加列表、刷新时间），不是整行替换。
- 写入时机：extract success 后（写 last_extract_at + password_id + 追加 names_offsets）；将来 detect/list 人工确认编码后（写 confirmed_encoding，不写 last_extract_at）——本任务只落 extract 的写入路径。

### 求密码候选顺序（extract）
1. 命令行 `--password` 参数
2. `known_files.password_id`（hash+size 精确命中 → 取 `passwords` 里那行的 value，**置顶但不独占**：先试它，不灵仍 fallback 到全库）
3. 当前批次内刚刚交互成功的密码（成功后立即写入密码库并进入任务内缓存，后续文件无需再次询问）
4. 配置文件通配符匹配（**将来**，本次仅留 TODO 占位）
5. `passwords` 库常规排序（现有 `ranked_candidates`）
- 去重后依次尝试。成功后 bump `passwords` 计数 + 写 `known_files.password_id`。
- 密码复用**不经 password_matches**（该表连同其 pattern 一起删除）。精确密码记忆由 known_files 承担，泛化匹配将来走配置文件正则/通配。

### 编码复用（extract）
- 顺序：命令行编码参数 > `known_files.confirmed_encoding`（hash+size 命中且是人工确认过的）> 当场自动检测。
- 编码与密码走**同一次** `SELECT ... WHERE sample_hash=? AND file_size=?` 查询，不查两遍。
- detect 的猜测编码不复用（自动检测成本低，重算比查历史更省心）；只复用人工确认过的（不可再生）。

### status / reason 取值（全局唯一，靠值自证来自哪个命令）
- file 行**不冗余 kind 列**（kind 是 task 的属性）；靠 status 取值区分命令。
- extract：`extracted / skipped / failed / partial`
- detect：`detected / unreadable`（本次仅预留）
- test：`intact / corrupt`（本次仅预留）
- “需密码但最终没拿到”（候选试穿 + 用户取消）：**统一** `status=skipped` + `reason=password_required`，不为加密单设终态。
- `reason` 枚举：`not_found / wrong_password / corrupt / target_exists / not_first_volume / recursion_limit / duplicate / business_container / password_required`。`status=extracted` 时 reason 为 NULL。
- 坏卷定位：`damaged_volumes_json`（列表，报全部坏卷，不只报第一个）——本次由 test 预留，extract 不写。

### 命令归属
- **本次闭合**：`extract`（写 tasks + file_extractions + known_files）、`history`（读）。
- **仅预留字段、不接线**：`detect`（tasks + file_extractions，status=detected/unreadable；将来加编码确认写 known_files）、`test`（独立命令，全量校验 + 坏卷定位 + `damaged_volumes_json`）、`list`（走完整管线看到内容）。
- `compress` 只进 tasks，延后。`password` 命令已存在，不动。
- 命令管线关系（供后续任务参考，本次不实现）：**detect ⊂ list 前缀子集** —— 一条管线两个出口，detect 探到编码即停，list 继续走到列条目 + 编码确认循环 + 内嵌定位 + 求密码。编码确认循环属于 list（要列出条目让用户看乱码才能确认），不属于 detect。CLI 只做基本功能，最佳编码交互体验在 GUI 实现。

### history 两种模式
- `history tasks`（默认）：读瘦身 `tasks`，一行一次操作。
- `history files`：读 `file_extractions`，一行一次解压动作；支持 `--status` / `--reason` **过滤**（file_extractions.status 建索引支撑）。
- `history show <task_id>`：某次操作的 `task_events` 时间线 + 名下所有 file_extractions 行。

## Constraints

- 历史写入保持 **best-effort**：repo 出错只发 `Warning` 事件 + stderr 告警，**绝不中断解压**（沿用现有 `DbTaskHistoryRecorder::warn` 语义）。
- 历史写入归 **engine**（在动作发生点通过注入的 recorder 调用），**CLI 只负责建连接 + 注入 recorder**，不承担写逻辑。原因：reason、offset、segment hash、per-file 落点只有 engine 循环内上下文齐全，CLI 拿不全。
- recorder trait 仍**不要求 `Send + Sync`**（rusqlite `Connection` 是 `!Sync`）。
- per-file 落点**现成可用**：engine 循环里 `actual_output_dir` 就是当前档实际解到的目录（现仅被聚合成 `hist_last_output` 丢了明细）。本次在 `processed.push(candidate)` 处多一次 per-file recorder 调用即可，**不改 materialize、不改 ExtractWorkflowResult 结构**。
- 所有跳过目前合并在一次 `skipped.push`（`lib.rs` `!is_new || depth>limit || !is_first_volume`）＋业务容器分支，需拆开各自打 reason 才能写行——这是 engine 数据模型改动，不只是加表。
- 迁移每步包在事务里、幂等、版本化（沿用现有 `schema.rs` migrate 框架，新增 v3 step，`LATEST_VERSION` → 3）。
- 不引入新的重量级依赖；采样哈希用已有/轻量 BLAKE3。

## Out of Scope

- hashcat 深度密码爆破表（`crack_jobs`）及其接线。设计要点已记录供后续：独立工作表，key 在自己的 `crack_hash`（zip2john/7z2john 抽出的爆破串）+ 关联 `known_files` 的 `hash+size`；破解成功**且试解压成功**后密码才进 `passwords`（source='hashcat'）+ 回填 known_files.password_id；wordlist/mask 跑穿记录作“负缓存”。
- `detect / test / list` 命令的交互、编码确认循环、TUI / 编码预览。接线在后续任务 `.trellis/tasks/2026-07/07-02-file-aware-cli-commands`（本任务的 v3 schema、`file_extractions` / `known_files` repo、`sample_hash`、engine 共享子流程是其前置依赖）。
- `compress` 写库。
- 配置文件通配符/正则密码匹配层（本次仅 TODO 占位）。
- 输出位置追踪（明确放弃）。

## Acceptance

- v3 迁移后：三张旧表消失，`tasks` 为瘦身结构，`file_extractions` / `known_files` 存在且索引齐全；migrate 幂等、可从 v2 升级（虽无数据）。
- extract 一次多输入批处理后：每个触发解压流程的文件（含 root、nested、carve）各有一行 file_extractions，status/reason/encoding/offset/落点正确；分卷非首卷记 `skipped`+`not_first_volume`；跳过分支 reason 正确区分。
- 重复下载同一包再 extract：命中 known_files 时间窗 → 跳过 + 提示；`--force` 可强制重解。
- 人工确认过编码的包（本次经由 extract 命令行编码参数路径可写入 known_files.confirmed_encoding 的部分）再 extract：编码被复用。
- 密码候选顺序：命令行 > known_files 置顶 > 当前批次命中 > 密码库；交互成功立即写库并供同批后续文件复用，且 known_files 命中密码不灵时能 fallback。
- `history tasks` / `history files --status/--reason` / `history show <id>` 三条读路径可用。
- 全 workspace `cargo test` 通过；旧的 encoding_detection / embedded_archive_detection / password_matches 相关测试与代码路径清理干净、无悬挂引用。
