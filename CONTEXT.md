# SmartZip 领域上下文

归档执行由 engine 编排，后端能力与差异由 router/adapter 保留。解压输出必须支持隔离、清理与提交回滚；密码不进入日志或事件。普通测试通过不代表真实后端可用或性能提升。

按当前问题检索（`rg -n "关键词" CONTEXT.md docs/context`），只读相关专题。代码中的路径默认相对仓库根目录；无需按顺序或全文阅读。

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
