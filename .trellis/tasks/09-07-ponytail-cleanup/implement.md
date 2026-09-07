# 实施记录

## 当前基线差异

06efceb 已实现配置单次加载、显式覆盖、按需 DB/backend 初始化、clipboard 显式 unsupported、encoding 原始名字节迭代、部分私有参数和别名清理。因此不恢复旧 eager bootstrap，不重复实现已完成项。公开 layout 字段、VolumeResolution variant 和 engine 旧 facade 保留源码兼容。

## 行为锁定

既有 23 项真实 CLI beta 包含直接 x 命令序列、跨 64 MiB 根扫描、原输入 hash、输出/冲突/预算回滚、取消子进程、分卷和历史状态。既有 engine/volume/layout/diagnostic 测试作为 characterization 基线，新增 CLI help 和工作流关键事件合同补齐本轮入口移动风险。

### Commit 0：characterization

新增 3 项 CLI 合同测试：5 个命令完整 help 快照/输出通道，参数错误退出 2 且不建状态目录，真实 UTF-8 ZIP 的关键事件顺序/结果状态/根输入字节保持。完整门槛通过，日志 target/ponytail-validation/00-characterization/；真实 release beta 23 项通过。没有修改生产代码。
