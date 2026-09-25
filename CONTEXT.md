# SmartZip 领域上下文

按当前问题检索相关术语与决策。归档执行由 engine 编排，后端能力与差异由 router/adapter 保留。解压输出需要隔离、清理与提交回滚；密码不进入日志或事件。实现状态以源码和相关验证记录为准；历史方案不自动成为当前任务的执行顺序。

当前产品范围统一记录在 [需求第 0 节](docs/requirements.md#0-当前产品范围2026-09-19)；旧设计中的阶段边界不覆盖最新范围决定。

## Core Concepts

[Core Concepts](docs/context/01-core-concepts.md)

## Event Model

[Event Model](docs/context/02-event-model.md)

## Password Model

[Password Model](docs/context/03-password-model.md)

## History Model

[History Model](docs/context/04-history-model.md)

<a id="完整性校验2026-09-05"></a>

## Architecture Decisions

[Architecture Decisions](docs/context/05-architecture-decisions.md)

<a id="adr-001-thin-engine-with-caller-injection"></a>

<a id="adr-002-real-time-event-streaming-via-mpsc"></a>

<a id="adr-003-capability-aware-mixed-backend-routing"></a>

<a id="adr-004-incremental-implementation"></a>

<a id="adr-005-smart-output-layout-with-plan-execute-separation"></a>

<a id="adr-006-collision-handling-after-layout-planning"></a>

<a id="adr-007-best-effort-task-history-via-injected-recorder"></a>

<a id="adr-008-single-taskevent-timeline-route-events-included"></a>

<a id="adr-009-single-extraction-staging-owner"></a>
