# SmartZip 领域上下文

按需查阅术语与架构原则。实现状态以源码和相关验证记录为准；历史方案不自动成为当前任务的执行顺序。

## Core Concepts

| Term | Definition |
|------|------------|
| **ArchiveBackend** | 旧后端抽象名称，历史任务中仍有引用；当前接口见 `ArchiveExecutor` 与 `ArchiveAdapter`。 |
| **ArchiveExecutor** | 面向 `SmartZipEngine` 的归档执行接口，定义 `probe` / `list` / `test` / `extract` / `compress`。`BackendRouter` 实现此接口，封装 adapter 发现、能力选择和 fallback。 |
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
| **VolumeSet** | 分卷归档集合；入口、成员和诊断语义见下方完整性校验。 |
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
| **TaskHistoryRecorder** | 调用方注入的历史记录接口，保存任务、事件和 per-file 解压动作，并更新 `known_files`。写库失败降级为 Warning，不中断解压。 |
| **DbTaskHistoryRecorder** | `TaskHistoryRecorder` 的 SQLite 实现，借用 `&rusqlite::Connection`。`Connection` 是 `!Sync`，因此 trait 不加 `Send + Sync` 约束——与已有的 `&PasswordService` 一样，解压 future 本就是 non-Send，仅 `.await` 不 spawn。 |
| **TaskOutcome** | 任务结束时交给 `finish()` 的聚合值。v3 精简为终态（completed/partial/failed/cancelled）+ 输出根路径；`encoding_selected` / `embedded_found` 等明细下沉到 `file_extractions`，不再作为 task 级聚合累积。 |
| **sample_hash** | 内容采样指纹：`BLAKE3(前 64KB ‖ 后 64KB)` 与 `file_size` 联合判等；文件小于 128KB 时全量哈希。内嵌归档按其范围采样，范围未知时不参与去重。 |
| **known_files** | `UNIQUE(sample_hash, size)` 复用索引，保存精确密码记忆、人工确认编码、成功解压时间和 name/offset 配对。自动猜测不覆盖人工确认编码。 |

### 完整性校验（2026-09-05）

- **VolumeSet**：只读收集同组卷、数值排序、入口、缺失/不可读与 identity/size/mtime 快照。RAR/字节切分从首卷开始，原生 split ZIP 从末段 `.zip` 开始。
- **TestArchiveReport**：一组一个报告，分开 integrity、coverage、password_status、confirmed_volumes、suspect_groups、missing/unreadable/unchecked 和带物理范围的依据。局部通过不代表整卷健康，候选组不能求交集得出确认坏卷。
- **Diagnostic pass**：engine 在失败主测试后发起的独立只读阶段，ArchiveExecutor 至多选择一个不同实现家族的后端；仍尊重强制 `--backend`，普通 corruption fallback 规则保持不变。本地格式校验不经过外部后端路由。
- DB **v4** 给 file_extractions 增加 nullable test_report_json，旧数据保留；damaged_volumes_json 只投影 confirmed 路径。test 不更新 known_files / last_extract_at，也不用首片 hash 表示整组。
- DB **v5** 仅重建密码排名索引，完整匹配含 COALESCE 的排序。导入和批量禁用使用单事务；导入保留重复行计数、pin 和重新启用规则，输入/SQL 错误回滚整批。
- 解压预算的全树/磁盘检查在阻塞工作线程运行，每个 monitor 仅一个检查在途；取消或后端结束先等待检查与后端回收，成功后做全新终检并返回累计 Usage。轮询是检查点预算，不是逐字节硬配额。
- 外部 test 非零退出可返回 `TestResult { ok: false, diagnostics }` 保留证据；调用者必须检查 ok。旧解压流程在既有 test-before-extract 分支把失败报告转换回错误状态，密码/损坏歧义不记密码失败统计。


## Architecture Decisions

保留决策编号方便历史引用；具体接口和迁移过程查阅源码、相关任务与 `docs/design.md`。

### ADR-001: Thin engine with caller injection

engine 负责工作流编排，由 CLI/GUI 注入后端、密码、交互和历史依赖，便于替换策略和测试。

### ADR-002: Real-time event streaming via mpsc

有界事件流是后续方案，当前以事件汇总与 listener 为主。若引入通道，应复用统一时间线、保留最终汇总，并明确背压和取消行为。

### ADR-003: Capability-aware mixed backend routing

executor 隔离 engine 与 adapter 细节；router 按 operation、容器、密码和字符集能力选择候选，保留不同外部程序的身份与可解释的路由依据。显式配置优先，运行时不支持信息只进入任务级负面缓存。

普通 route 仅对 UnsupportedContainer、UnsupportedCodec、BackendUnavailable 和 BackendProtocolError 允许 fallback；密码、损坏、安全、资源、权限、磁盘和取消错误终止当前 route。独立只读诊断见完整性校验部分。

### ADR-004: Incremental implementation

按可验证、可回滚的切片推进，优先解决真实行为问题；新增模块和抽象由实际需求驱动。旧任务中的阶段顺序是历史计划。

### ADR-005: Smart output layout with plan-execute separation

布局规划生成显式 `LayoutPlan`，执行器按计划提交，方便预览、测试和失败回滚。

### ADR-006: Collision handling after layout planning

碰撞检测针对布局规划后的实际目标，在提交前交给 `CollisionResolver`；预布局路径可能误报或漏报冲突。

### ADR-007: Best-effort task history via injected recorder

历史通过注入的 recorder 保存，失败产生 Warning 而不中断解压。文件级记录承载密码、编码与输出信息；具体 schema 和迁移留在数据库实现及任务记录中。

### ADR-008: Single TaskEvent timeline (route events included)

路由观测作为 `TaskEventKind::Route` 纳入统一任务时间线，供结果、CLI/GUI 与测试使用，避免依赖 router 旁路收集器。任务级缓存与事件共享任务作用域。

### ADR-009: Single extraction staging owner

`OutputMaterializer` 统一管理隔离输出：每次 adapter 尝试使用独立目录；失败后删除并确认不存在，才允许下一次尝试。清理失败终止当前候选，避免污染后续尝试。

成功树交给布局、碰撞处理和提交；这些阶段的失败不再触发后端 fallback。密码重试开启新的材质化周期，避免双层临时目录与清理职责分散。
