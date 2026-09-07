# Ponytail：保留行为的重复路径清理

用户于 2026-09-07 提供基于 6fd52bd 的审查，并明确要求“实施并提交”。本轮执行基线为 06efceb：配置任务已改变 bootstrap、状态和选项语义，必须冻结当前行为，不能回退到审查旧基线。

范围：先补行为锁定，再按独立提交清理死结构、共享归档准备与历史行、nested 分类、CLI 命令请求、后端 canonical context 实现、外部进程生命周期。保留公开 Rust API 的薄兼容入口。排除可选 Commit 6、依赖删除、默认值变化、GUI 重构和 crate 级架构调整。

必须保持 CLI 参数/help/stdout/stderr/JSON/退出码，任务事件顺序、数据库状态与提示更新条件、源文件与输出树、直接 x 无预测试、fallback、分卷判定、完整性证据分类、取消/预算/提交回滚。两套分卷系统继续独立；OutputMaterializer 事务流程不改写。

每个提交通过 fmt、routing guards、workspace all-target check、非 GUI workspace tests、release CLI build、真实 beta acceptance、diff check。覆盖率与 CRAP 仅用于定位热点，不作为无回归或性能证明。现有测试覆盖的验收不重复造同义用例，新增证据记录在 implement.md。
