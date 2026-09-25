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
| **Root scan** | 用户直接输入的文件应尽可能解压。空搜索窗口后继续扫描直到文件末尾；命中归档头后完整解析其范围，再从归档末尾继续搜索。窗口不限制前缀、归档间隔或已命中归档的长度。过小载荷、业务容器和嵌套扫描大小等效率门槛仅用于嵌套发现。解压资源预算独立生效。 |
| **Recursive extraction** | BFS 队列驱动的递归解压。有状态批次最多同时推进两个根输入；当前根扫描发现的归档优先于后续根输入，普通嵌套发现仍按 BFS。队列中每个候选经过同一管线：格式检测 → 编码检测 → 有界密码候选直接解压到 `OutputMaterializer` → 输出扫描 → 嵌套候选入队。 |
| **Collapse single output** | 解压产出唯一条目时的优化：将该条目提到父目录，去掉中间层空目录。现在由 `LayoutPlanKind` 的各种 `Commit*` 变体实现，包括内容上移（`CommitSingleDirContentsAsArchiveName`）和直接重命名（`CommitSingleDirAsInnerName`/`CommitSingleFileAsInnerName`）。 |
| **ArchiveNode** | 动态节点语义由持久 NodeId/generation 和现有文件历史行实现，记录父子、阶段、输入身份与提交事实。节点在父归档解压后增量产生，不预先构造完整 DAG；秘密不进入持久执行快照。 |
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
