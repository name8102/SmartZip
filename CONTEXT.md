# SmartZip — Domain Glossary

> 本文件是术语表，不含实现细节。由 grill-with-docs 会话生成。

## Core Concepts

| Term | Definition |
|------|------------|
| **ArchiveBackend** | 压缩后端抽象 trait。定义 `probe` / `list` / `test` / `extract` / `compress`。所有后端实现（Rust 原生 lib 或 7zz CLI）统一实现此 trait。 |
| **NativeBackend** | 统一后端门面。内部按格式分派到对应 Rust 原生库（zip/tar/flate2/sevenz-rust 等），不支持格式回退 7zz CLI。对 engine 透明。 |
| **SmartZipEngine** | 解压/检测/压缩工作流编排器。自身不持有后端、密码服务等依赖——由调用方（CLI/GUI）注入。 |
| **ExtractionCandidate** | 待解压候选条目。包含路径、深度、来源类型、检测格式、内嵌偏移等。 |
| **CandidateSource** | 候选来源枚举：`RootInput`（用户直接输入）、`ExtractedFile`（解压产物中找到的）、`EmbeddedFinding`（扫描器在二进制偏移处发现的）。 |
| **Recursive extraction** | BFS 队列驱动的递归解压。队列中每个候选经过同一管线：格式检测 → 编码检测 → 密码尝试 → 解压 → 输出扫描 → 嵌套候选入队。 |
| **Collapse single output** | 解压产出唯一条目时的优化：将该条目提到父目录，去掉中间层空目录。 |

## Event Model

| Term | Definition |
|------|------------|
| **TaskEvent** | 工作流中产生的事件：Started / Progress / PasswordTried / EncodingDetected / EmbeddedArchiveFound / OutputCreated / Warning / Failed / Completed。 |
| **Event channel** | `tokio::sync::mpsc::UnboundedSender<TaskEvent>`。engine 在解压过程中实时推送事件，CLI/GUI 异步消费显示进度。与函数返回值（ExtractWorkflowResult）并存。 |
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
**Decision**: `extract_recursive` 接受 `Option<UnboundedSender<TaskEvent>>`，事件即产即推。同时保留返回值。
**Rationale**: 消除 CLI 的"卡死感"——用户实时看到进度。返回值保留供测试断言和最终统计。
**Trade-off**: engine 代码中新增 channel send 调用点。

### ADR-003: Mixed backend — Rust libs + 7zz fallback
**Decision**: 常用格式（zip/tar/gz/bz2/xz）用 Rust 原生库；rar/cab/iso/dmg 及其他复杂格式回退 7zz CLI。通过统一 NativeBackend 门面隐藏多后端。
**Rationale**: 原生库消除命令行参数解析 bug、提供精确进度；7zz 覆盖 Rust 生态未支持的格式。
**Trade-off**: 多后端增加代码量；NativeBackend 门面提供统一抽象。

### ADR-004: Two-phase incremental implementation
**Decision**: Phase 1 先加 channel 事件流（不动 backend）；Phase 2 建 NativeBackend 逐步替换 7zz。
**Rationale**: 每步可独立测试、独立回滚。Phase 1 立即改善用户体验。
