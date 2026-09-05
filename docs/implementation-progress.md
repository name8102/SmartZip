# SmartZip Implementation Progress

> 本文档只记录仓库级整体进度与当前实现快照。
> 具体任务状态、验收步骤和执行记录统一在 `.trellis/tasks/` 下维护。
> 最后更新：2026-09-05
> 当前基线：原工作已合并到 main（fa38539），本轮 CLI beta 加固记录见 [任务](../.trellis/tasks/archive/2026-09/09-05-cli-beta-hardening/implement.md)。此前验收数字属于各自记录的版本。

## 当前快照

### 工作区模块

| 模块 | 状态 | 说明 |
|------|------|------|
| `smartzip-core` | ✅ 已落地 | 共享类型、错误、事件模型 |
| `smartzip-scanner` | ✅ 已落地 | binwalk 封装、内嵌归档扫描基础 |
| `smartzip-archive` | 🟡 部分落地 | 能力路由整合已完成；`ArchiveExecutor` / `ArchiveAdapter`、profile 与受限 fallback 已落地，格式覆盖仍持续完善 |
| `smartzip-db` | ✅ 已落地 | v4 文件级历史：保留 tasks/file_extractions/known_files，新增完整 test_report_json，版本化迁移保留旧记录 |
| `smartzip-encoding` | ✅ 已落地 | 编码检测与候选输出 |
| `smartzip-passwords` | ✅ 已落地 | 候选生成、排序、统计 |
| `smartzip-config` | 🟡 部分落地 | TOML 管理后端路由和解压产出预算；CLI 可覆盖预算 |
| `smartzip-platform` | 🟡 部分落地 | 路径、桌面能力、部分平台封装 |
| `smartzip-engine` | 🟡 部分落地 | 薄 facade 与能力模块已拆分；BFS、布局、文件级历史和统一路由事件已接线 |
| `smartzip-cli` | 🟡 部分落地 | `detect`、`list`、`extract`、`test`、`enc`、`password`、`history` 可执行；`compress` 尚未实现 |
| `smartzip-gui` | 🟡 原型阶段 | 窗口与基础交互已存在，未形成完整任务工作台 |
| `packaging/` | ❌ 未开始 | AppImage、dmg、捆绑 7zz 等未完成 |

### 当前实现要点

| 主题 | 当前状态 |
|------|----------|
| 后端路由 | 根据 operation、facts、requirements、能力 profile 过滤和排序，只有允许的错误才 fallback；不是按扩展名固定选择或遇到任意错误都换后端 |
| ZIP / RAR | 外部 7z/7zz、unrar 参与路由；NativeZipBackend 仅保留原始名称和加密元数据辅助读取 |
| 7z 路由 | 仍依赖外部程序 adapter；NativeSevenZipBackend 尚未实现 |
| 事件模型 | `Vec<TaskEvent>` 汇总结果 + listener，RouteEvent 已纳入同一时间线；有界 mpsc 尚未实现 |
| 输出布局 | 已实现 plan-execute separation 与 collision-after-layout |
| 数据库 | v3 已落库：DROP `password_matches`/`encoding_detections`/`embedded_archive_detections`，瘦身 `tasks`，新增 `file_extractions`（append 日志）+ `known_files`（`sample_hash+size` 去重/复用索引）；v4 新增 nullable test_report_json；schema 支持 v1→v4 迁移 |
| 任务历史 | extract 的文件级历史和密码/编码复用已接线；历史成功不再阻止新目标解压；detect/list 也已接入文件级记录；test 按卷组写完整诊断，history tasks/files/show 可读 |
| CLI 退出码 | `0` 成功、`1` 全失败/通用错误、`2` 实际部分失败，取消 `130`；良性跳过不计错；clap 参数错误也使用 `2` |

### 本轮 beta 加固（2026-09-05）

已实现可恢复覆盖提交、同盘备份、受限扫描、动态产出预算、端到端取消与终端恢复、编码节点跳过、密码错误专属重试、Unicode 安全显示、doctor 及 CLI 两平台发布工作流。完整用户边界见 [beta 指南](cli-beta.md)，最终验证以任务记录为准。两平台完整发布门禁已通过 [CI 33954747834](https://github.com/name8102/SmartZip/actions/runs/33954747834)，代码提交 a21c3eb；未创建公开 Release。

### 此前 CLI 交互核对（加固前的观察）

本轮完成 [交互设计草案](../.trellis/tasks/09-05-cli-interaction-design/design.md) 与实施切片，优先密码、编码和用户反馈。用户追加的命名调整已实现：编码预览改为 `enc`，旧 `encoding-preview` 仍兼容；新增 `x/l/d/t/c/pw/hist` 常用短别名，未改变操作逻辑。

| 缺口 | 当前观察 | 后续方向 |
| --- | --- | --- |
| 密码验证 | 未加密 ZIP 的 list 也会把任意传入密码保存为成功 | 区分无需密码与可靠验证，阻止错误记忆 |
| 密码输入与显示 | 输入回显，多层 trim；特定长中文密码导致列表 panic | 内容保真、隐藏输入、可重试、Unicode 安全展示 |
| 编码选择 | pick-encoding 只有编码名；交互最终选择的持久化来源未统一 | 同一批真实名称对照，显式确认后按指纹记忆 |
| 终端模式 | JSON 仅关闭 listener，提示仍按各自 stdin 判断 | 统一终端判定、stdout/stderr 与取消语义 |
| 参数与预览 | 根参数不能放在子命令后；clipboard 忽略；dry-run 仅首个输入文本 | 参数实际生效、按需初始化、多输入候选预览 |

新任务状态为 in_progress，S0 命名/别名已实现，S1–S6 交互功能仍待实施。旧 file-aware CLI 任务仍标为 planning，虽然 detect/list 已有实现，但编码对照等验收尚未完全闭合；未直接修改旧任务为 completed。

test 分卷定位已实现：[实现契约](../.trellis/tasks/2026-07/07-03-test-command-backend-split/design.md)。任意卷分组与入口、完整后端校验、默认追加只读诊断、密码边界、JSON 与 DB v4 历史已接线。RAR5 的独立 CRC 可确认物理卷；7z solid/多 stream、ZIP 数据及元数据的范围形成候选组，未知范围保持可见。

实现验证：完整 workspace 426 项测试通过；check、fmt、routing guard 通过，clippy 成功但仍有既存 warning。另有 18 个真实分卷样本和 10 个密码/历史/退出码用例通过，源卷 hash 不变。详细版本、种子、边界见 [验收记录](../.trellis/tasks/2026-07/07-03-test-command-backend-split/research/implementation-validation.md)。最初 24 次直接后端实验是设计依据，单独保留，不混作产品验收。

此前 CLI 交互设计核对时的验证：CLI build 成功；CLI 6 + engine 177 + passwords 3 + encoding 9，共 195 项相关测试通过；routing guard clean。另用临时 DB/XDG 目录完成 CLI 缺口复现。该次核对未运行完整 workspace 或跨平台终端验收；后续 test 实现已跑完整 workspace，跨平台验收仍未做。详见 [核对证据](../.trellis/tasks/09-05-cli-interaction-design/research/current-state.md)。

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
| 2026-07-02 | 15 | ✅ | 数据库 v2 全部设计表、版本化迁移与任务/检测历史落库（`smartzip history` 子命令） |
| 2026-07-02 | 16 | ✅ | 历史模型 v3 文件级重构：`file_extractions` + `known_files`、extract 去重/编码/密码复用、history tasks/files/show；detect/list/test 接线留后续任务 |
| 2026-07-31 | 17 | ✅ | 路由整合落地记录：保留 feat 文件级历史与 detect/list 接线，engine 模块化、单一 executor、统一事件与 staging |
| 2026-09-05 | 18 | 🟡 | 核对 CLI 并形成密码/编码交互设计；enc 与常用短别名已实现，其余交互切片待实施 |
| 2026-09-05 | 19 | ✅ | test/t 完整校验、分卷诊断、JSON/历史报告、密码边界与取消；RAR4 等局部算法保守降级 |

## 与设计的主要差距

| 主题 | 设计方向 | 当前实现 |
|------|----------|----------|
| CLI 密码与编码 | 密码可靠验证/重试、可比较编码预览与显式记忆 | 隐藏输入与内容保真已修复；list 不能证明文件内容密码有效、真实名称对照与记忆来源仍需后续完善 |
| Native 7z 后端 | 计划评估并接入 `NativeSevenZipBackend` | 尚未实现 |
| 实时事件通道 | 有界 `mpsc` 实时事件流 | 尚未实现 |
| GUI 工作台 | 完整任务树、日志、设置、密码管理 | 仍处原型阶段 |
| 打包分发 | AppImage、dmg、bundled 7zz | 本轮提供 CLI tar.gz / SHA-256 工作流；复杂包与后端捆绑仍未开始 |

## 文档边界

- 设计目标与长期契约以 `docs/design.md` 和 `docs/requirements.md` 为准。
- 研究、验证和技术评估材料位于 `docs/research/`。
- 组合式实施草案位于 `docs/compose/plans/`，需要结合状态标记阅读。
- 具体任务拆分、执行顺序和验收证据以 `.trellis/tasks/` 为准。
