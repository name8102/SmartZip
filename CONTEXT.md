# SmartZip — Domain Glossary

> 本文件是术语表，不含实现细节。由 grill-with-docs 会话生成。

## Core Concepts

| Term | Definition |
|------|------------|
| **ArchiveBackend** | 压缩后端抽象 trait。定义 `probe` / `list` / `test` / `extract` / `compress`。所有后端实现（Rust 原生 lib 或 7zz CLI）统一实现此 trait。 |
| **NativeBackend** | 原生后端门面。第一阶段优先实现 `NativeZipBackend`，负责 ZIP ZipCrypto / AES、原始文件名字节、路径安全和高速密码验证。复杂格式通过独立 fallback 路由交给 `SevenZipBackend`。 |
| **SmartZipEngine** | 解压/检测/压缩工作流编排器。自身不持有后端、密码服务等依赖——由调用方（CLI/GUI）注入。 |
| **ExtractionCandidate** | 待解压候选条目。包含路径、深度、来源类型、检测格式、内嵌偏移等。 |
| **CandidateSource** | 候选来源枚举：`RootInput`（用户直接输入）、`ExtractedFile`（解压产物中找到的）、`EmbeddedFinding`（扫描器在二进制偏移处发现的）。 |
| **Recursive extraction** | BFS 队列驱动的递归解压。队列中每个候选经过同一管线：格式检测 → 编码检测 → 密码尝试 → 解压 → 输出扫描 → 嵌套候选入队。 |
| **Collapse single output** | 解压产出唯一条目时的优化：将该条目提到父目录，去掉中间层空目录。 |
| **ArchiveNode** | 下一阶段动态节点模型。记录父节点、来源、深度、状态、指纹和成功密码。节点在父归档解压后增量产生，不要求预先构造完整 DAG。 |
| **VolumeSet** | 分卷归档集合。识别首卷、成员、缺卷和重复卷，仅将首卷交给后端。 |
| **ExtractionLimits** | 不可信归档的资源预算：递归深度、内层候选数、文件数、磁盘安全余量和内嵌 finding 数量。 |
| **OutputMaterializer** | 事务式输出策略：规划目标路径、同盘临时目录解压、校验、保守整理、碰撞处理、提交或回滚。失败时默认清理临时目录；开发模式可保留临时目录用于诊断。 |

## Event Model

| Term | Definition |
|------|------------|
| **TaskEvent** | 工作流中产生的事件：Started / Progress / PasswordTried / EncodingDetected / EmbeddedArchiveFound / OutputCreated / Warning / Failed / Completed。 |
| **Event channel** | 计划使用有界 `tokio::sync::mpsc`。engine 在解压过程中实时推送事件，CLI/GUI 异步消费显示进度，并支持取消和等待用户决策。与最终汇总结果并存。 |
| **ExtractWorkflowResult** | `extract_recursive` 的返回值。包含 processed/skipped/enqueued 列表 + 完整事件集合。供调用方做最终统计和断言。 |

## Password Model

| Term | Definition |
|------|------------|
| **PasswordService** | 密码候选生成 + 排序 + 成功/失败记录。不持有数据库连接，通过注入的 PasswordRepository 操作。 |
| **PasswordCandidate** | 单个密码候选，含 value、source（Empty/Manual/Clipboard/Database）、可选 id。 |
| **PasswordCandidateRequest** | 控制候选生成的参数：是否含空密码、手动密码列表、剪贴板、数量上限。 |
| **InteractivePasswordPrompter** | 异步 trait。当所有存储密码失败时调用，让用户手动输入。实现方须用 spawn_blocking 隔离阻塞 I/O。 |

## Architecture Decisions (ADR-worthy)

### ADR-001: Thin engine with caller injection
**Decision**: SmartZipEngine 不持有 backend/passwords/db 等依赖；由 CLI/GUI 注入到 `extract_recursive()` 参数中。
**Rationale**: CLI 和 GUI 的密码策略、交互方式不同。薄 engine 让调用方控制这些策略。
**Trade-off**: 调用方代码略多；engine 可测试性不变（注入 mock 即可）。

### ADR-002: Real-time event streaming via mpsc
**Decision**: 下一阶段为 engine 增加有界 `mpsc` 事件流，事件即产即推。同时保留最终汇总结果。
**Rationale**: 消除 CLI 的"卡死感"——用户实时看到进度。返回值保留供测试断言和最终统计。
**Trade-off**: 有界通道需要定义背压策略，不能让高频进度事件拖慢解压。

### ADR-003: Mixed backend — Rust libs + format-specific fallback
**Decision**: 第一阶段实现 `NativeZipBackend`；RAR 增加 `UnrarBackend`，优先评估 `unrar` crate，必要时使用 `unrar` CLI；7z 增加 `NativeSevenZipBackend` 评估，先看 `sevenz-rust2`，备选 `zesven`；复杂格式和库级能力缺口继续通过 `SevenZipCliBackend` fallback。路由对 engine 透明。
**Rationale**: ZIP 是编码恢复、路径安全和高性能密码表遍历的关键格式；RAR 在 7zz 解压中已出现部分失败，需要格式专用后端提高成功率和诊断质量；7zz 仍覆盖 Rust 生态未支持的复杂格式。
**Trade-off**: 多后端增加测试矩阵，同一功能在不同后端上的能力必须明确标记，并记录实际后端、失败原因和 fallback 链路。

### ADR-004: Two-phase incremental implementation
**Decision**: 不整体迁移，不提前拆分新的 graph/scheduler/events crate。先修复现有行为错误和安全预算，再实现 Native ZIP、密码 worker pool、动态 ArchiveNode，最后接入事件流和 GUI。
**Rationale**: 每步可独立测试和回滚，并优先解决复杂网络归档的真实风险。
