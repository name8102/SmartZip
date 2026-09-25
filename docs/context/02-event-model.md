## Event Model

| Term | Definition |
|------|------------|
| **TaskEvent** | 工作流唯一任务可观测时间线：生命周期、进度、密码、编码、内嵌归档、输出，以及经 `TaskEventKind::Route` 承载的路由观测。默认输出摘要，verbose 展示排除原因与 fallback 链；密码及敏感参数不得进入事件。 |
| **RouteEvent** | 路由域载荷（RoutePlanned / BackendAttempt* / BackendSelected / RouteExhausted 等），**不是**独立收集通道。只作为 `TaskEvent` 的一部分出现在任务时间线中。 |
| **Task-scoped execution context** | 单次工作流任务内的可变作用域：持有有序 `TaskEvent` 列表、任务级负面能力缓存（原 `TaskRouteContext` 语义），并可被 engine 与 `ArchiveExecutor` 共享写入。实现类型名未冻结；最终形态可扩展 `ArchiveExecutor` 各 operation 的参数，或经 `begin_task` 绑定的 sink 分阶段落地。 |
| **Event channel** | ADR-002：有界 `tokio::sync::mpsc` 在统一 `TaskEvent` 时间线**之后**接入；实时推送与最终 `ExtractWorkflowResult` 事件集合并存。背压策略不得拖慢解压。 |
| **ExtractWorkflowResult** | `extract` 及兼容入口 `extract_recursive*` 的返回值。包含完整的 processed/skipped/enqueued 列表与保留的任务事件（含路由事件）。当前每个任务最多保留前 4096 条 Progress；其余事件不受此进度上限影响，listener 仍接收全部事件。CLI/GUI 与测试以该集合为权威观测面，不从 `BackendRouter` 再取旁路事件。 |
