## Architecture Decisions

保留决策编号方便历史引用；具体接口和迁移过程查阅源码、相关任务与 `docs/design.md`。

### ADR-001: Thin engine with caller injection

engine 负责工作流编排，由 CLI/GUI 注入后端、密码、交互和历史依赖，便于替换策略和测试。

`CompiledRunPolicy` 保持只读快照；`PreparedExtractTask` 在执行前解析一次请求，并按任务自身策略装配密码、历史和已知文件服务。旧解压入口继续保留兼容行为。

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

### 源归档回收

`SmartZipEngine::with_source_recycling(true)` 启用任务级成功后回收，默认关闭。共享引擎记录实际成功卷组并在整批结束后清理，GUI 只传递选项；不重新按文件名猜测卷组。具体行为见 [桌面指南](../desktop-beta.md#解压后回收源文件)。

### ADR-009: Single extraction staging owner

`OutputMaterializer` 统一管理隔离输出：每次 adapter 尝试使用独立目录；失败后删除并确认不存在，才允许下一次尝试。清理失败终止当前候选，避免污染后续尝试。

成功树交给布局、碰撞处理和提交；这些阶段的失败不再触发后端 fallback。密码重试开启新的材质化周期，避免双层临时目录与清理职责分散。
