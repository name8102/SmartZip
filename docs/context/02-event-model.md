## Event Model

| Term | Definition |
|------|------------|
| **TaskEvent** | 工作流唯一任务可观测时间线：生命周期、进度、密码、编码、内嵌归档、输出，以及经 `TaskEventKind::Route` 承载的路由观测。默认输出摘要，verbose 展示排除原因与 fallback 链；密码及敏感参数不得进入事件。 |
| **RouteEvent** | 路由域载荷（RoutePlanned / BackendAttempt* / BackendSelected / RouteExhausted 等），**不是**独立收集通道。只作为 `TaskEvent` 的一部分出现在任务时间线中。 |
| **Task-scoped execution context** | 单次解压任务共享事件保留预算、负面能力缓存、资源预算、去重与批次密码；根输入仍独立接收事件与取消信号。生命周期与历史收尾在任务层执行一次。 |
| **Event channel** | ADR-002：有界 `tokio::sync::mpsc` 在统一 `TaskEvent` 时间线**之后**接入；实时推送与最终 `ExtractWorkflowResult` 事件集合并存。背压策略不得拖慢解压。 |
| **ExtractWorkflowResult** | 解压入口的返回值。包含按输入顺序聚合的 processed/skipped/enqueued 列表，以及按发生顺序保留的任务事件（含路由事件，最多 4096 条，超限有截断说明）。listener 接收实时事件；不从 `BackendRouter` 另取事件。 |
