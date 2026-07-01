# Embedded and ZIP Contract Follow-up Implementation Plan

## Ordered Steps

1. 审核当前代码覆盖情况
   - 对照新增需求逐项判断：已完成 / 部分完成 / 未完成
   - 记录对应文件、测试和缺口
2. 收敛 embedded 编排缺口
   - `AskUser` 必须真正暂停自动提取
   - `ReportOnly` / `SkipByDefault` 不得继续自动走普通解压路径
   - nested 普通文件扫描必须按 `embedded_scan_mode`、比例阈值和预算执行
3. 收敛 detection/router 缺口
   - archive detection 改成文件头优先
   - extension 仅作 hint/fallback
   - 移除或替代 `SevenZipBackend::probe()` 的完整扫描慢路径
4. 收敛 ZIP 编码闭环
   - 编码检测只对 ZIP 生效
   - 带密码 ZIP 在密码命中后再做 `raw_name` 检测
   - Native ZIP 提供 listing/raw-name 输入
   - CLI 增加编码预览与确认闭环
5. 收敛 CLI 契约
   - 明确手工密码、数据库密码、交互密码和编码确认的顺序
   - 明确 wrong password / partial success 的可见输出
6. 端到端验证
   - embedded fixtures
   - nested fixtures
   - password fixtures
   - encoding fixtures

## Validation

- `cargo test -p smartzip-engine --test embedded_integration`
- `cargo test -p smartzip-engine --test smartzip_integration`
- `cargo test -p smartzip-archive --lib`
- `cargo test -p smartzip-cli`

## Non-Goals

- 不重写 `06-27-fix-plan`
- 不恢复 GUI、工作台、打包发布等长期方向
- 不把所有格式都纳入统一编码检测模型

## Exit Condition

- 新增需求已经与旧 `fix-plan` 明确拆分
- 当前代码覆盖情况已完成审计
- 后续实现可以直接按本任务推进而不再回写旧 task
