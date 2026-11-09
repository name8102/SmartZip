## Architecture Decisions

保留决策编号方便历史引用；具体接口和迁移过程查阅源码、相关任务与 `docs/design.md`。

### ADR-001: Thin engine with caller injection

engine 负责工作流编排，由 CLI/GUI 注入后端、密码、交互和历史依赖，便于替换策略和测试。

`CompiledRunPolicy` 保持只读快照；`PreparedExtractTask` 在执行前解析一次请求，并按任务自身策略装配密码、历史和已知文件服务。新建和恢复均使用同一准备入口，旧解压重载和兼容别名已删除。

### ADR-002: Real-time event streaming via mpsc

有界事件流是后续方案，当前以事件汇总与 listener 为主。若引入通道，应复用统一时间线、保留最终汇总，并明确背压和取消行为。

### ADR-003: Capability-aware mixed backend routing

executor 隔离 engine 与 adapter 细节；router 按 operation、容器、密码和字符集能力选择候选，保留不同原生库/外部程序的身份与可解释的路由依据。显式配置优先，运行时不支持信息只进入任务级负面缓存。

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

`OutputMaterializer` 管理每次提取尝试的隔离目录和终止时的清理。只有 router 要在同一尝试内切换 adapter 时，才先清空目录、确认无残留并重置尝试额度。终止失败由材质化层一次移除目录，不再逐项清理后重复删除。

成功树交给布局、碰撞处理和提交；这些阶段的失败不再触发后端 fallback。密码重试开启新的材质化周期，避免双层临时目录与清理职责分散。

### ADR-010: Native sessions with parent-owned output

7-Zip 使用官方固定版本原生库，不再解析 CLI 文本。短生命周期 worker 保持可终止的解码边界；私有管道传递请求、目录、字节和结构化结果，密码不进入 argv。父进程持有受控输出 writer，按实际写入准入预算；逐流关闭并累积 manifest，保留 OutputMaterializer 提交/清理职责。内嵌和字节分卷直接使用范围/拼接流，不预先 carve 或建立别名树。

探测只返回格式和加密信息，完整性校验只返回原生结果和失败成员，按索引读取只传所选条目。ZIP 名称读取器只提供编码判断与覆盖所需的数据；成员身份与名称视图分离。提取产出清单转移给预算、提交和嵌套发现，GUI 浏览目录保留类型化结果，诊断 JSON 在导出时生成。
