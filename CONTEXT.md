# SmartZip — Domain Glossary

> 本文件是术语表，不含实现细节。由 grill-with-docs 会话生成。

## Core Concepts

| Term | Definition |
|------|------------|
| **ArchiveBackend** | 当前压缩后端抽象 trait。定义 `probe` / `list` / `test` / `extract` / `compress`。所有后端实现（Rust 原生 lib 或 7zz CLI）统一实现此 trait；在路由迁移完成前保留。 |
| **ArchiveExecutor** | 目标中面向 `SmartZipEngine` 的归档执行 seam，定义 `probe` / `list` / `test` / `extract` / `compress`。`BackendRouter` 实现此 interface；engine 不接触 adapter 发现、能力 profile、排序或 fallback。 |
| **ArchiveAdapter** | 面向 `BackendRouter` 的后端 adapter seam。每个 Rust 原生实现或外部程序实例（包括不同路径或版本的 `7z`、`7zz`）都是独立 adapter，并保留自己的身份和能力，不在进入路由前合并。 |
| **AdapterCapabilities** | 路由实际消费的 adapter 元数据：支持的 operation、读取容器、压缩容器，以及密码和字符集覆盖能力。运行时 UnsupportedCodec/UnsupportedContainer 仅进入任务级负面缓存，不持久化假设性的 profile 规则。 |
| **ArchiveFacts** | 路由需要的归档事实：容器和可选 codec 字符串。事实不包含假设性的后端策略。 |
| **ArchiveRequirements** | 当前调用方的具体路由要求：是否提供密码、是否覆盖文件名字符集，以及已观察的 codec 字符串。 |
| **RoutePlan** | 针对单个归档和 operation 生成的可解释 adapter 顺序，记录容器、候选、排除原因与 fallback 规则。list/test/extract/compress 分别规划；同一任务复用 facts 和 extract 顺序。 |
| **NativeBackend** | 原生 adapter。`NativeZipBackend` 负责 ZIP ZipCrypto / AES、原始文件名字节、编码信息和路径安全；它作为需要这些特殊能力的显式路径使用，不是普通 ZIP 密码路径的默认后端。密码候选通过直接解压验证，`test` 仅用于显式完整性检查。复杂格式由 `BackendRouter` 交给其他 adapter。 |
| **SmartZipEngine** | 解压/检测/压缩工作流编排器。自身不持有后端、密码服务等依赖——由调用方（CLI/GUI）注入。 |
| **ExtractionCandidate** | 待解压候选条目。包含路径、深度、来源类型、检测格式、内嵌偏移等。 |
| **CandidateAttempt** | 对单个 `ExtractionCandidate` 的核心处理尝试。负责检测决策、内嵌归档材质化、编码检测、密码尝试、后端解压、输出材质化和结果事件；BFS 队列仍由 `SmartZipEngine` 管理。 |
| **CandidateSource** | 候选来源枚举：`RootInput`（用户直接输入）、`ExtractedFile`（解压产物中找到的）、`EmbeddedFinding`（扫描器在二进制偏移处发现的）。 |
| **Root scan** | 用户直接输入的文件应尽可能解压。命中归档头后完整解析其范围，再从归档末尾继续搜索，直到窗口无发现；窗口不限制已命中归档的长度。过小载荷、业务容器和嵌套扫描大小等效率门槛仅用于嵌套发现。解压资源预算独立生效。 |
| **Recursive extraction** | BFS 队列驱动的递归解压。队列中每个候选经过同一管线：格式检测 → 编码检测 → 有界密码候选直接解压到 `OutputMaterializer` → 输出扫描 → 嵌套候选入队。 |
| **Collapse single output** | 解压产出唯一条目时的优化：将该条目提到父目录，去掉中间层空目录。现在由 `LayoutPlanKind` 的各种 `Commit*` 变体实现，包括内容上移（`CommitSingleDirContentsAsArchiveName`）和直接重命名（`CommitSingleDirAsInnerName`/`CommitSingleFileAsInnerName`）。 |
| **ArchiveNode** | 下一阶段动态节点模型。记录父节点、来源、深度、状态、指纹和成功密码。节点在父归档解压后增量产生，不要求预先构造完整 DAG。 |
| **VolumeSet** | 分卷归档集合。识别首卷、成员、缺卷和重复卷，仅将首卷交给后端。 |
| **ExtractionLimits** | 不可信归档的资源预算：递归深度、内层候选数、文件数、磁盘安全余量和内嵌 finding 数量。 |
| **OutputMaterializer** | 事务式输出策略与 **extraction staging** 的唯一所有者：为 adapter 尝试提供隔离目录、在失败后验证清理、对成功树做布局规划与碰撞处理，再 `CommitCommand` 提交或回滚。失败时默认清理临时目录；开发模式可保留**已选中**成功树用于诊断（失败 adapter 树必须删除）。碰撞在布局规划之后，经 `CollisionResolver` 交互。 |
| **LayoutPlan** | 智能整理规划结果。包含 `source`（待移动项）、`kind`（整理策略）、`target`（最终目标路径）、`reason`（决策原因）。由 `plan_layout()` 在解压到临时目录后生成。 |
| **PlanSource** | 待移动项来源：`WholeTempDir`（整个临时目录）、`SingleDir`（单目录）、`SingleDirContents`（单目录内容）、`SingleFile`（单文件）。 |
| **LayoutPlanKind** | 整理策略枚举：`CommitWholeTempAsArchiveDir`（归档名容器）、`CommitSingleDirContentsAsArchiveName`（泛名目录内容上移到归档名）、`CommitSingleDirAsInnerName`（内层目录名）、`CommitSingleFileAsArchiveName`（文件用归档名）、`CommitSingleFileAsInnerName`（文件用内层名）、`PreserveBothSingleDir`（保留双层目录）、`PreserveBothSingleFile`（保留双层文件）、`RawArchiveDir`（原样输出）、`Empty`（空解压）。 |
| **OutputLayoutPolicy** | 输出布局策略：`Conservative`（默认，保留归档名上下文）、`Smart`（更激进折叠）、`Raw`（原样输出）、`FlatSingle`（单项目直接放到输出根）。 |
| **SingleRootNamePolicy** | 单根项命名策略：`Auto`（启发式）、`PreferArchiveName`（强制用归档名）、`PreferInnerName`（强制用内层名）、`PreserveBoth`（保留两层）、`AskWhenAmbiguous`（低置信度时询问）。 |
| **NameScore** | 名称质量评分。基于语义 token 数量、版本号、括号信息、泛名惩罚、hash 惩罚计算总分。用于决定归档名和内层名哪个更有信息量。 |
| **CollisionResolver** | 异步回调，在布局规划后、提交前检测目标路径冲突。接收 `(archive_path, target_path, layout_plan)`，返回 `CollisionAction`（Skip/Overwrite/Rename）。 |
| **MaterializeFailureKind** | 材质化失败类型：`ExtractFailed`（后端解压失败）、`CommitFailed`（提交失败）、`CollisionSkipped`（用户选择跳过碰撞）。 |
| **Extraction staging** | 单次材质化周期内的隔离写盘能力：为每个 `ArchiveAdapter` 尝试提供独立目录；成功则选中该树交给布局/提交，失败则在进入下一 adapter 前删除并确认不存在。由 `OutputMaterializer` 实现，供 `BackendRouter` 在 extract 路径上使用；engine 与 CLI 不直接操作 staging。实现类型名未冻结。 |
| **Attempt output** | staging 借出的一次 adapter 写目录句柄：提供路径、成功时交还选中、失败时丢弃并验证清理。不得嵌套在另一层「材质化临时根」之下。实现类型名未冻结。 |

## Event Model

| Term | Definition |
|------|------------|
| **TaskEvent** | 工作流唯一任务可观测时间线：生命周期、进度、密码、编码、内嵌归档、输出，以及经 `TaskEventKind::Route` 承载的路由观测。默认输出摘要，verbose 展示排除原因与 fallback 链；密码及敏感参数不得进入事件。 |
| **RouteEvent** | 路由域载荷（RoutePlanned / BackendAttempt* / BackendSelected / RouteExhausted 等），**不是**独立收集通道。只作为 `TaskEvent` 的一部分出现在任务时间线中。 |
| **Task-scoped execution context** | 单次工作流任务内的可变作用域：持有有序 `TaskEvent` 列表、任务级负面能力缓存（原 `TaskRouteContext` 语义），并可被 engine 与 `ArchiveExecutor` 共享写入。实现类型名未冻结；最终形态可扩展 `ArchiveExecutor` 各 operation 的参数，或经 `begin_task` 绑定的 sink 分阶段落地。 |
| **Event channel** | ADR-002：有界 `tokio::sync::mpsc` 在统一 `TaskEvent` 时间线**之后**接入；实时推送与最终 `ExtractWorkflowResult` 事件集合并存。背压策略不得拖慢解压。 |
| **ExtractWorkflowResult** | `extract_recursive` 的返回值。包含 processed/skipped/enqueued 列表 + **完整**任务事件集合（含路由事件）。CLI/GUI 与测试以该集合为权威观测面，不从 `BackendRouter` 再取旁路事件。 |

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

### 完整性校验（2026-09-05）

- **VolumeSet**：只读收集同组卷、数值排序、入口、缺失/不可读与 identity/size/mtime 快照。RAR/字节切分从首卷开始，原生 split ZIP 从末段 `.zip` 开始。
- **TestArchiveReport**：一组一个报告，分开 integrity、coverage、password_status、confirmed_volumes、suspect_groups、missing/unreadable/unchecked 和带物理范围的依据。局部通过不代表整卷健康，候选组不能求交集得出确认坏卷。
- **Diagnostic pass**：engine 在失败主测试后发起的独立只读阶段，ArchiveExecutor 至多选择一个不同实现家族的后端；仍尊重强制 `--backend`，普通 corruption fallback 规则保持不变。本地格式校验不经过外部后端路由。
- DB **v4** 给 file_extractions 增加 nullable test_report_json，旧数据保留；damaged_volumes_json 只投影 confirmed 路径。test 不更新 known_files / last_extract_at，也不用首片 hash 表示整组。
- 外部 test 非零退出可返回 `TestResult { ok: false, diagnostics }` 保留证据；调用者必须检查 ok。旧解压流程在既有 test-before-extract 分支把失败报告转换回错误状态，密码/损坏歧义不记密码失败统计。


## Architecture Decisions (ADR-worthy)

### ADR-001: Thin engine with caller injection
**Decision**: SmartZipEngine 不持有 backend/passwords/db 等依赖；由 CLI/GUI 注入到 `extract_recursive()` 参数中。
**Rationale**: CLI 和 GUI 的密码策略、交互方式不同。薄 engine 让调用方控制这些策略。
**Trade-off**: 调用方代码略多；engine 可测试性不变（注入 mock 即可）。

### ADR-002: Real-time event streaming via mpsc
**Decision**: 目标是在后续阶段为 engine 增加有界 `mpsc` 事件流，事件即产即推。同时保留最终汇总结果。当前尚未实现该通道，仍以事件汇总 + listener 为主。
**Rationale**: 消除 CLI 的"卡死感"——用户实时看到进度。返回值保留供测试断言和最终统计。
**Trade-off**: 有界通道需要定义背压策略，不能让高频进度事件拖慢解压。

### ADR-003: Capability-aware mixed backend routing

**Decision**: 目标是将现有后端抽象拆为面向 engine 的 `ArchiveExecutor` seam 和面向 router 的 `ArchiveAdapter` seam。`BackendRouter` 保留所有 adapter 的身份和具体 `AdapterCapabilities`，按 operation、读取/压缩容器、password、charset-override 过滤，再按优先级稳定排序，最后执行显式错误 fallback；`7z` 与 `7zz` 等不同程序或版本是独立 adapter。显式配置优先，自动发现补充；版本不一致只警告。明确的 UnsupportedContainer / UnsupportedCodec 会加入任务级负面缓存。

每个 `ExtractionCandidate` 独立管理临时输出。adapter 失败后立即清理当前候选的临时产物，确认清理成功后才交给下一个 adapter；清理失败终止当前候选，但不影响任务中的其他候选。只有 UnsupportedContainer、UnsupportedCodec、BackendUnavailable 和 BackendProtocolError 允许 fallback；密码错误、损坏、安全、资源、权限、磁盘和取消错误均终止当前 route。router 产生可解释 `RoutePlan` 和结构化 attempt/cleanup/selection 事件。迁移完成前，现有 `ArchiveBackend` stack 保持不变。

**Rationale**: 容器格式不足以表达真实兼容性；codec/filter、分卷、加密方式、原始文件名字节等能力同时决定 adapter 是否适用。持久化 profile 让选择可重复，执行 fallback 处理配置与现实偏差，独立候选清理避免失败 adapter 的部分输出污染后续 adapter。

**Trade-off**: 配置 schema、adapter profile 和路由测试矩阵显著增加；外部程序能力配置可能陈旧，因此需要版本警告、可解释排除原因、任务级负面缓存和严格的失败清理。

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

### ADR-008: Single TaskEvent timeline (route events included)

**Decision**: 任务可观测性只有一条时间线：`TaskEvent`。`RouteEvent` 仅作为 `TaskEventKind::Route` 载荷。删除 `BackendRouter` 旁路事件缓冲作为生产观测面；CLI 只读 `ExtractWorkflowResult.events`。直接 `router.extract` 测试仍可在不经 engine 的情况下注入 task-scoped context / sink 断言路由事件。端态：将 task-scoped execution context 贯穿 `ArchiveExecutor` 各 operation（任务级负面缓存与事件同属该作用域）。分阶段允许先用 `begin_task` 绑定 sink 减少 churn。ADR-002 的 mpsc 在统一时间线之后接入，不另开 `RouteEvent` 通道。
**Rationale**: 双收集器迫使 CLI 依赖具体 `BackendRouter`，破坏 `ArchiveExecutor` 深度；已存在未使用的 `TaskEventKind::Route`。
**Trade-off**: executor 签名最终会变；过渡期可能同时存在 sink 与参数两种绑定方式，需尽快收敛。

### ADR-009: Single extraction staging owner

**Decision**: 取消 materialize 临时根下再嵌套 router attempt 临时目录的双重隔离。`OutputMaterializer` 拥有 **extraction staging**：每 adapter 独立 attempt 目录；失败删除并**确认不存在**后才允许下一 adapter；成功选中该树再 `plan_layout` / 碰撞 / `CommitCommand`。密码重试仍完整重新 `materialize`（新 staging）。`BackendSelected` 之后的布局/碰撞/提交失败不得再 fallback 到其他 adapter。实现类型名未冻结；允许先落地 staging 语义再定名。
**Rationale**: 嵌套 temp + `move_attempt_output` 把清理规则拆到两个 module，违反 ADR-003 的单一清理语义。
**Trade-off**: extract 请求不再把「最终输出路径」伪装成 adapter 写路径；adapter 与 executor 签名需调整；严格 cleanup 确认在部分文件系统上可能更慢，但不放宽。
