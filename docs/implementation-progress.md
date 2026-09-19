# SmartZip 实现进度

最后更新：2026-09-19。当前产品范围以 [需求第 0 节](requirements.md#0-当前产品范围2026-09-19) 为准；实施存在不等于原生 GUI、跨平台或性能验收通过。

| 主题 | 当前实现与边界 |
|---|---|
| 共享核心 | 能力路由、允许错误的 fallback、统一事件时间线、独立 adapter staging、智能布局和提交回滚已实现 |
| 解压与校验 | 递归/内嵌/分卷、test/t 诊断、密码候选与重试、真实 ZIP 编码处理已实现；复杂格式继续依赖外部后端 |
| 扫描 | 固定块、有界格式解析和取消；根输入空窗口继续到 EOF，嵌套输入保留效率限制 |
| 历史与状态 | schema v7；同一任务/文件历史承载持久执行信息。历史去重可配置，独立于输出位置与恢复判定 |
| 调度与恢复 | 资源准入、等待释放、节点身份/代际、配置快照、提交对账已实现；保守并发，不含内存/临时空间预约、自适应调优或扫描内部续点 |
| GUI | 真实解压、四类交互、归档浏览、有限成员预览、密码/历史/设置、macOS 系统集成已有实现；原生操作验收由用户完成 |
| 分发 | CLI 与桌面 tar.gz、SHA-256、Linux 专用目录安装器、macOS .app 安装器和双平台 CI；本轮机器只有 Linux，macOS 新版结果待对应 runner |
| 不在首发范围 | 压缩创建、Windows 发布、完整原生 7z、任意格式预览、归档内部字节级续传 |

最新发布收尾结果统一见 [beta 实施记录](../.trellis/tasks/09-19-beta-release/implement.md)。发布使用说明见 [桌面 beta](desktop-beta.md) 与 [CLI beta](cli-beta.md)。

相关历史证据按版本保留：

- [2026-09-19 任务系统](../.trellis/tasks/09-19-task-system-implementation/implement.md)
- [2026-09-19 合并与缺口修复](../.trellis/tasks/09-19-review-gap-audit/implement.md)
- [GUI 实施](../.trellis/tasks/09-12-gui-design/implement.md)
- [此前 CLI 加固](../.trellis/tasks/archive/2026-09/09-05-cli-beta-hardening/implement.md)

旧任务中的测试数量和“待实现”描述属于当时版本，不覆盖以上当前状态；不要求把历史方案全部兑现才发布 beta。
