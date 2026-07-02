# SmartZip — Domain Glossary

> 本文件是术语表，不含实现细节。由 grill-with-docs 会话生成。

## Core Concepts

| Term | Definition |
|------|------------|
| **ArchiveBackend** | 压缩后端抽象 trait。定义 `probe` / `list` / `test` / `extract` / `compress`。所有后端实现（Rust 原生 lib 或 7zz CLI）统一实现此 trait。 |
| **NativeBackend** | 原生后端门面。长期目标是让 `NativeZipBackend` 承担 ZIP 编码恢复、路径安全和高性能密码验证；当前实现中 ZIP 的主提取路径仍优先走 `SevenZipBackend`，`NativeZipBackend` 主要保留为少量特例能力、测试能力和后备路径。 |
| **SmartZipEngine** | 解压/检测/压缩工作流编排器。自身不持有后端、密码服务等依赖——由调用方（CLI/GUI）注入。 |
| **ExtractionCandidate** | 待解压候选条目。包含路径、深度、来源类型、检测格式、内嵌偏移等。 |
| **CandidateSource** | 候选来源枚举：`RootInput`（用户直接输入）、`ExtractedFile`（解压产物中找到的）、`EmbeddedFinding`（扫描器在二进制偏移处发现的）。 |
| **Recursive extraction** | BFS 队列驱动的递归解压。队列中每个候选经过同一管线：格式检测 → 编码检测 → 密码尝试 → 解压 → 输出扫描 → 嵌套候选入队。 |
| **Collapse single output** | 解压产出唯一条目时的优化：将该条目提到父目录，去掉中间层空目录。现在由 `LayoutPlanKind` 的各种 `Commit*` 变体实现，包括内容上移（`CommitSingleDirContentsAsArchiveName`）和直接重命名（`CommitSingleDirAsInnerName`/`CommitSingleFileAsInnerName`）。 |
| **ArchiveNode** | 下一阶段动态节点模型。记录父节点、来源、深度、状态、指纹和成功密码。节点在父归档解压后增量产生，不要求预先构造完整 DAG。 |
| **VolumeSet** | 分卷归档集合。识别首卷、成员、缺卷和重复卷，仅将首卷交给后端。 |
| **ExtractionLimits** | 不可信归档的资源预算：递归深度、内层候选数、文件数、磁盘安全余量和内嵌 finding 数量。 |
| **OutputMaterializer** | 事务式输出策略：规划目标路径、同盘临时目录解压、校验、智能整理、碰撞处理、提交或回滚。失败时默认清理临时目录；开发模式可保留临时目录用于诊断。碰撞处理在布局规划之后执行，通过 `CollisionResolver` 回调解交互。 |
| **LayoutPlan** | 智能整理规划结果。包含 `source`（待移动项）、`kind`（整理策略）、`target`（最终目标路径）、`reason`（决策原因）。由 `plan_layout()` 在解压到临时目录后生成。 |
| **PlanSource** | 待移动项来源：`WholeTempDir`（整个临时目录）、`SingleDir`（单目录）、`SingleDirContents`（单目录内容）、`SingleFile`（单文件）。 |
| **LayoutPlanKind** | 整理策略枚举：`CommitWholeTempAsArchiveDir`（归档名容器）、`CommitSingleDirContentsAsArchiveName`（泛名目录内容上移到归档名）、`CommitSingleDirAsInnerName`（内层目录名）、`CommitSingleFileAsArchiveName`（文件用归档名）、`CommitSingleFileAsInnerName`（文件用内层名）、`PreserveBothSingleDir`（保留双层目录）、`PreserveBothSingleFile`（保留双层文件）、`RawArchiveDir`（原样输出）、`Empty`（空解压）。 |
| **OutputLayoutPolicy** | 输出布局策略：`Conservative`（默认，保留归档名上下文）、`Smart`（更激进折叠）、`Raw`（原样输出）、`FlatSingle`（单项目直接放到输出根）。 |
| **SingleRootNamePolicy** | 单根项命名策略：`Auto`（启发式）、`PreferArchiveName`（强制用归档名）、`PreferInnerName`（强制用内层名）、`PreserveBoth`（保留两层）、`AskWhenAmbiguous`（低置信度时询问）。 |
| **NameScore** | 名称质量评分。基于语义 token 数量、版本号、括号信息、泛名惩罚、hash 惩罚计算总分。用于决定归档名和内层名哪个更有信息量。 |
| **CollisionResolver** | 异步回调，在布局规划后、提交前检测目标路径冲突。接收 `(archive_path, target_path, layout_plan)`，返回 `CollisionAction`（Skip/Overwrite/Rename）。 |
| **MaterializeFailureKind** | 材质化失败类型：`ExtractFailed`（后端解压失败）、`CommitFailed`（提交失败）、`CollisionSkipped`（用户选择跳过碰撞）。 |

## Event Model

| Term | Definition |
|------|------------|
| **TaskEvent** | 工作流中产生的事件：Started / Progress / PasswordTried / EncodingDetected / EmbeddedArchiveFound / OutputCreated / Warning / Failed / Completed。 |
| **Event channel** | 目标设计是有界 `tokio::sync::mpsc` 实时事件流；当前实现仍以 `ExtractWorkflowResult` 中汇总的 `Vec<TaskEvent>` 为主，并支持可选 listener 回调。 |
| **ExtractWorkflowResult** | `extract_recursive` 的返回值。包含 processed/skipped/enqueued 列表 + 完整事件集合。供调用方做最终统计和断言。 |

## Password Model

| Term | Definition |
|------|------------|
| **PasswordService** | 密码候选生成 + 排序 + 成功/失败记录。不持有数据库连接，通过注入的 PasswordRepository 操作。extract 的有效顺序为命令行 > known-files 精确命中 > 当前批次刚成功密码 > 其余数据库候选；交互密码成功后立即写库并加入任务内缓存。 |
| **PasswordCandidate** | 单个密码候选，含 value、source（Empty/Manual/Clipboard/Database）、可选 id。 |
| **PasswordCandidateRequest** | 控制候选生成的参数：是否含空密码、手动密码列表、剪贴板、数量上限。 |
| **InteractivePasswordPrompter** | 异步 trait。当所有存储密码失败时调用，让用户手动输入。实现方须用 spawn_blocking 隔离阻塞 I/O。 |

## History Model

| Term | Definition |
|------|------------|
| **TaskHistoryRecorder** | 引擎注入式历史记录 trait。`extract_recursive*` 在有 recorder 时把任务（父级 `tasks` 行）、事件时间线、per-file 解压动作（`file_extractions`）落库，并 UPSERT `known_files` 索引。best-effort 语义：写库失败降级为 `Warning` 事件，不中断解压。与 `PasswordService` 一样由调用方注入（ADR-001）。**注：v3（`07-02-file-grain-history`）后历史模型改为文件级；旧的 `record_encoding_detection` / `record_embedded_findings` / `record_password_match*` 方法随对应表删除而移除，改为 per-file 记录方法。** |
| **DbTaskHistoryRecorder** | `TaskHistoryRecorder` 的 SQLite 实现，借用 `&rusqlite::Connection`。`Connection` 是 `!Sync`，因此 trait 不加 `Send + Sync` 约束——与已有的 `&PasswordService` 一样，解压 future 本就是 non-Send，仅 `.await` 不 spawn。 |
| **TaskOutcome** | 任务结束时交给 `finish()` 的聚合值。v3 精简为终态（completed/partial/failed/cancelled）+ 输出根路径；`encoding_selected` / `embedded_found` 等明细下沉到 `file_extractions`，不再作为 task 级聚合累积。 |
| **sample_hash** | 采样内容哈希，用于对“重复下载的同一压缩包”去重。`BLAKE3(前 64KB ‖ 后 64KB)` + `file_size` 联合判等；文件 < 128KB 全量哈希；carve/内嵌档对 `[offset, offset+size)` 段采样，size 未知时不算、不参与去重。取代 v2 的 `path_hash`（后者随 `encoding_detections` / `embedded_archive_detections` 删除而废弃——路径不再作 join key）。 |
| **known_files** | `UNIQUE(sample_hash, size)` 去重/复用索引。存 `password_id`（精确密码记忆）、`confirmed_encoding`（人工确认编码，自动猜测不覆盖）、`last_extract_at`（非空即成功解压过，去重判据）、`names_offsets_json`（name+offset 配对列表）。取代 v2 的 `password_matches` backfill 与 `encoding_detections` 的复用职责。 |

## Architecture Decisions (ADR-worthy)

### ADR-001: Thin engine with caller injection
**Decision**: SmartZipEngine 不持有 backend/passwords/db 等依赖；由 CLI/GUI 注入到 `extract_recursive()` 参数中。
**Rationale**: CLI 和 GUI 的密码策略、交互方式不同。薄 engine 让调用方控制这些策略。
**Trade-off**: 调用方代码略多；engine 可测试性不变（注入 mock 即可）。

### ADR-002: Real-time event streaming via mpsc
**Decision**: 目标是在后续阶段为 engine 增加有界 `mpsc` 事件流，事件即产即推。同时保留最终汇总结果。当前尚未实现该通道，仍以事件汇总 + listener 为主。
**Rationale**: 消除 CLI 的"卡死感"——用户实时看到进度。返回值保留供测试断言和最终统计。
**Trade-off**: 有界通道需要定义背压策略，不能让高频进度事件拖慢解压。

### ADR-003: Mixed backend — Rust libs + format-specific fallback
**Decision**: 保留混合后端方向：RAR 增加 `UnrarBackend`，优先评估 `unrar` crate，必要时使用 `unrar` CLI；7z 继续评估 `NativeSevenZipBackend`，先看 `sevenz-rust2`，备选 `zesven`；复杂格式和库级能力缺口继续通过 `SevenZipCliBackend` fallback。ZIP 的长期目标仍是增强 `NativeZipBackend`，但当前实现里 ZIP 默认仍优先路由到 `SevenZipBackend`，`NativeZipBackend` 只承担有限特例能力和后备路径。路由对 engine 透明。
**Rationale**: ZIP 是编码恢复、路径安全和高性能密码表遍历的关键格式；RAR 在 7zz 解压中已出现部分失败，需要格式专用后端提高成功率和诊断质量；7zz 仍覆盖 Rust 生态未支持的复杂格式。
**Trade-off**: 多后端增加测试矩阵，同一功能在不同后端上的能力必须明确标记，并记录实际后端、失败原因和 fallback 链路。

### ADR-004: Two-phase incremental implementation
**Decision**: 不整体迁移，不提前拆分新的 graph/scheduler/events crate。先修复现有行为错误和安全预算，再实现 Native ZIP、密码 worker pool、动态 ArchiveNode，最后接入事件流和 GUI。
**Rationale**: 每步可独立测试和回滚，并优先解决复杂网络归档的真实风险。

### ADR-005: Smart output layout with plan-execute separation
**Decision**: 智能整理分为规划（`layout.rs`）和执行（`materialize.rs`）两阶段。规划器扫描临时目录、评分名称、生成 `LayoutPlan`；执行器只按计划移动文件。碰撞处理在规划之后、提交之前执行。
**Rationale**: 规划和执行分离让 CLI dry-run、GUI 预览、碰撞回调成为自然扩展点。`PlanSource` + `LayoutPlanKind` 的显式模型消除了"规划器说一种路径、提交器走另一种路径"的漂移风险。
**Trade-off**: `MaterializeRequest` 需要携带 `archive_path`、`layout_policy`、`single_root_name_policy` 等字段，调用方代码略多。

### ADR-006: Collision handling after layout planning
**Decision**: 碰撞检测从"解压前检查候选输出路径"改为"布局规划后检查实际目标路径"。`CollisionResolver` 回调在 `materialize()` 内部、`plan_layout()` 之后调用。
**Rationale**: 旧流程拿预布局路径判断碰撞，会产生两类错误：(1) 目标存在但最终路径不同→误报；(2) 最终路径冲突但预检查未命中→漏报。新流程保证碰撞检测针对真实目标。
**Trade-off**: `materialize()` 现在是 async 并可能等待用户输入，engine 主循环在碰撞期间阻塞。并发解压多个归档时，碰撞提示会串行化。

### ADR-007: Best-effort task history via injected recorder
**Decision**: 任务历史通过注入的 `TaskHistoryRecorder` trait 落库，与 backend/passwords/prompters 一样由调用方注入（`extract_recursive_with_listener_interactive` 的可选末位参数）。默认实现 `DbTaskHistoryRecorder` 借用 `&rusqlite::Connection`，与 `PasswordService` 共享同一连接。事件在 engine 内先汇总到 `EventSink`，任务结束时统一 replay 进 `task_events`；per-file 动作与 `known_files` 更新在路径、offset、密码和编码上下文完整的位置就地写入。
**Rationale**: 与 ADR-001 的薄 engine 原则一致：engine 不持有 DB。历史写入是可选能力，CLI/GUI 决定是否注入。事件先汇总再 replay 避免把 recorder 引用穿进 `EventSink` 克隆和进度回调，降低耦合。

> **v3 更新（`07-02-file-grain-history`）**：历史模型从操作级改为文件级。变化点：
> - recorder 新增 per-file 方法（`record_file_extraction` / `lookup_known_file` / `upsert_known_file_extract`），删除 `record_encoding_detection` / `record_embedded_findings` / `record_password_match*`（对应表 DROP）。
> - per-file 落点在 engine 主循环 `processed.push(candidate)` 处就地记录（`actual_output_dir` 现成可用，不改 materialize / `ExtractWorkflowResult`）。
> - 跳过分支需拆开各自打 `reason`（`duplicate` / `recursion_limit` / `not_first_volume` / `business_container`），不再合并成一次 `skipped.push`。
> - extract 解压前查 `known_files` 复用 `confirmed_encoding` + 去重跳过；求密码候选顺序注入 known_files.password_id（置顶不独占）。
> - 完整裁决见 `.trellis/tasks/2026-07/07-02-file-grain-history/prd.md`；detect/list/test 接线见 `07-02-file-aware-cli-commands`。
**Trade-off**: recorder 与 `PasswordService` 共享 `!Sync` 的 `Connection`，使 extract future 保持非 Send（只 await、不 spawn，符合当前 CLI 用法）。历史写入为 best-effort——任何 repo 错误降级为 `Warning` 事件，绝不中断解压；因此历史行在极端情况下可能不完整。`extract_recursive_with_listener_interactive` 参数进一步变长，后续可考虑收敛为请求结构体。
