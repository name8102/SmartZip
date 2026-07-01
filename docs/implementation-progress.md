# SmartZip Implementation Progress

> 本文档只记录仓库级整体进度与当前实现快照。
> 具体任务状态、验收步骤和执行记录统一在 `.trellis/tasks/` 下维护。
> 最后更新：2026-07-01

## 当前快照

### 工作区模块

| 模块 | 状态 | 说明 |
|------|------|------|
| `smartzip-core` | ✅ 已落地 | 共享类型、错误、事件模型 |
| `smartzip-scanner` | ✅ 已落地 | binwalk 封装、内嵌归档扫描基础 |
| `smartzip-archive` | 🟡 部分落地 | `BackendRouter`、`UnrarBackend`、`SevenZipCliBackend`、`NativeZipBackend` 已存在 |
| `smartzip-db` | 🟡 部分落地 | 已实现密码相关 schema；任务/事件相关表未实现 |
| `smartzip-encoding` | ✅ 已落地 | 编码检测与候选输出 |
| `smartzip-passwords` | ✅ 已落地 | 候选生成、排序、统计 |
| `smartzip-config` | ✅ 已落地 | TOML 配置加载 |
| `smartzip-platform` | 🟡 部分落地 | 路径、桌面能力、部分平台封装 |
| `smartzip-engine` | 🟡 部分落地 | BFS 解压、嵌套发现、布局规划、事件汇总 |
| `smartzip-cli` | 🟡 部分落地 | `detect`、`extract`、`encoding-preview`、`password` 子命令 |
| `smartzip-gui` | 🟡 原型阶段 | 窗口与基础交互已存在，未形成完整任务工作台 |
| `packaging/` | ❌ 未开始 | AppImage、dmg、捆绑 7zz 等未完成 |

### 当前实现要点

| 主题 | 当前状态 |
|------|----------|
| ZIP 路由 | `BackendRouter` 对 ZIP 默认优先走 `SevenZipCliBackend`；`NativeZipBackend` 目前主要保留为少量特例能力、路径安全校验、压缩和后备路径 |
| RAR 路由 | `UnrarBackend` 优先，失败或不可用时回退 `SevenZipCliBackend` |
| 7z 路由 | 仍依赖 `SevenZipCliBackend`；`NativeSevenZipBackend` 尚未实现 |
| 事件模型 | 当前为 `Vec<TaskEvent>` 汇总结果 + 可选 listener；设计中的有界 `mpsc` 实时通道尚未实现 |
| 输出布局 | 已实现 plan-execute separation 与 collision-after-layout |
| 数据库 | 已实现 `schema_migrations`、`passwords`、`password_matches`；其余设计表尚未实现 |
| CLI 退出码 | 当前稳定使用 `0` 成功、`1` 全失败/通用错误、`2` 部分成功 |

## 里程碑进度

| 日期 | Stage | 状态 | 结果 |
|------|-------|------|------|
| 2026-05-20 | 1 | ✅ | workspace、core types、scanner、archive、CLI skeleton |
| 2026-05-20 | 2 | ✅ | engine orchestration 与 `detect` 命令接线 |
| 2026-05-20 | 3 | ✅ | SQLite 密码库与 candidate service |
| 2026-05-20 | 4 | ✅ | 递归解压工作流骨架 |
| 2026-05-20 | 5 | ✅ | `extract_recursive` 与 CLI extract 接线 |
| 2026-05-20 | 6 | ✅ | 真实 7z 集成测试 |
| 2026-05-20 | 7 | ✅ | chardetng 编码检测 |
| 2026-05-20 | 8 | ✅ | platform paths 与 config TOML |
| 2026-05-20 | 9 | ✅ | 编码接入 `extract_recursive` |
| 2026-05-20 | 10 | ✅ | password CLI 子命令 |
| 2026-05-20 | 11 | 🟡 | GPUI window prototype |
| 2026-05-20 | 12 | 🟡 | GUI 接入 engine 与拖拽原型 |
| 2026-06-04 | 13 | 🟡 | 后端路由、`UnrarBackend`、`NativeZipBackend` 辅助能力；ZIP 主路径仍为 7z |
| 2026-06-12 | 14 | ✅ | smart output layout（plan-execute separation） |

## 与设计的主要差距

| 主题 | 设计方向 | 当前实现 |
|------|----------|----------|
| Native ZIP 主路径 | ZIP 最终应由增强后的原生后端承担主能力 | 当前为 `SevenZipCliBackend` 优先 |
| Native 7z 后端 | 计划评估并接入 `NativeSevenZipBackend` | 尚未实现 |
| 实时事件通道 | 有界 `mpsc` 实时事件流 | 尚未实现 |
| 任务落库 | `tasks`、`task_events` 等历史记录表 | 尚未实现 |
| 检测历史落库 | `encoding_detections`、`embedded_archive_detections` | 尚未实现 |
| GUI 工作台 | 完整任务树、日志、设置、密码管理 | 仍处原型阶段 |
| 打包分发 | AppImage、dmg、bundled 7zz | 尚未开始 |

## 文档边界

- 设计目标与长期契约以 `docs/design.md` 和 `docs/requirements.md` 为准。
- 研究、验证和技术评估材料位于 `docs/research/`。
- 组合式实施草案位于 `docs/compose/plans/`，需要结合状态标记阅读。
- 具体任务拆分、执行顺序和验收证据以 `.trellis/tasks/` 为准。
