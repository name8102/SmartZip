# Embedded and ZIP Contract Follow-up Design

## Objective

把后来追加到 `fix-plan` 的新增需求，收敛为一个边界清晰的 follow-up task，避免旧任务“已完成”的语义被扩大，也避免 embedded / ZIP / CLI 契约问题继续混在历史计划任务里。

## Design Boundary

- 本任务关注的是“新增契约问题的整理与后续修复入口”。
- 本任务不重新定义旧 `fix-plan` 的阶段路线。
- 本任务允许直接引用当前代码和测试状态，因为这些需求本身就是后来的代码审查结论。

## Workstreams

1. Embedded orchestration
   - `AskUser` / `ReportOnly` / `SkipByDefault` 的真实编排行为
   - nested 普通文件扫描模式、预算和 business container 过滤
2. Detection and routing
   - 文件头优先
   - extension 仅作 hint/fallback
   - `probe` 在主流程中的职责
3. ZIP encoding flow
   - ZIP 限定
   - 密码命中后检测
   - Native ZIP `raw_name` 输入
   - CLI 确认闭环
4. CLI contract
   - wrong password
   - encoding confirmation
   - password source priority
   - partial success / visible behavior

## Risks

- 如果继续把这些新增需求留在 `fix-plan`，旧任务会同时承担“已完成”和“未实现新增范围”两种互相冲突的状态。
- 如果不把 ZIP 编码确认闭环单独列出来，后续很容易再次把“已有事件输出”误判成“流程可用”。
- 如果不把 `probe` 慢路径单独标成显式工作项，性能/错误路由问题会继续隐藏在“后端可用”口径之下。
