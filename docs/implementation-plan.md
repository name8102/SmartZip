# SmartZip 实施计划 v2

> 基于 `docs/requirements.md` v2、`docs/design.md` v2 和当前源码。
> 原则：优先完成复杂网络归档解压，不进行一次性整体重写。

## 当前判断

保留现有 workspace crate 边界。近期不建设通用 `Archive Workflow Runtime`，也不一次性拆分新的 graph、scheduler、events、policy crate。

迁移集中在：

```text
smartzip-engine
smartzip-archive
smartzip-passwords
smartzip-db
smartzip-scanner
smartzip-platform
smartzip-cli
```

GUI、压缩、预览和系统集成不阻塞核心迁移。

## Phase 0 - 立即修复

目标：修复当前 CLI 解压路径中的真实行为错误，不改变整体拓扑。

| # | 项目 | 主要文件 | 验收 |
|---|---|---|---|
| P0-1 | 保留显式 `EncodingMode::Override`，不要回退为 `Auto` | `smartzip-engine/src/lib.rs` | `--encoding gbk` 进入 backend request |
| P0-2 | 为 `SevenZipBackend` 增加经过实测的 code page 参数映射 | `smartzip-archive/src/sevenzz.rs` | 集成测试覆盖显式编码参数 |
| P0-3 | 仅在明确 `WrongPassword` 时记录失败 | `smartzip-engine/src/lib.rs` | IO、损坏、格式不支持不降权 |
| P0-4 | 交互密码统一执行 `test -> extract`，并复用 carve 后的临时归档路径 | `smartzip-engine/src/lib.rs` | 加密内嵌归档可交互解压 |
| P0-5 | scanner 改为有界读取，不允许 `fs::read()` 后截断 | `smartzip-scanner/src/lib.rs` | 大文件扫描内存有上界 |
| P0-6 | CLI 在全部失败或部分失败时返回明确退出码 | `smartzip-cli/src/main.rs` | shell 可区分成功、部分成功和失败 |
| P0-7 | 手动密码仅在成功后自动保存 | `smartzip-cli/src/main.rs` | 错误 `-p` 不污染数据库 |
| P0-8 | 修正文档漂移：移除未实现的单文件 collapse 和实时事件流声明 | `docs/implementation-progress.md` | 进度文档与源码一致 |

已完成且无需重复实施：

- `7z` 子进程 stdin 已设置为 null。
- `.part01.rar` 已识别为首卷。
- offset 内嵌归档已支持 carve 临时文件。
- backend future panic 已隔离。

## Phase 1 - 安全预算与事务式输出

目标：让不可信网络归档具备可控安全边界。

| # | 项目 | 说明 |
|---|---|---|
| P1-1 | `ExtractionLimits` | 默认 `depth <= 5`、内层归档最多 100、文件最多 500000 |
| P1-2 | 动态磁盘预算 | 保留 `max(10 GB, 可用空间 10%)`，解压期间持续检查 |
| P1-3 | 同盘临时目录 | 默认 `extract -> verify -> normalize -> commit` |
| P1-4 | 超大归档模式 | 预估输出 >= 50 GB、归档 >= 20 GB 或余量不足时提示标准 / 快速 / 取消 |
| P1-5 | `OutputMaterializer` | 保守整理、碰撞追加 `_1`、`_2`，默认不覆盖 |
| P1-6 | 内层归档删除 | 根归档保留；内层成功提交后默认移入回收站 |
| P1-7 | 普通文件清理 | 默认删除空目录；其他规则预览后确认 |

## Phase 2 - 原生 ZIP 后端

目标：解决 ZIP 编码、安全和密码表遍历性能的核心问题。

| # | 项目 | 说明 |
|---|---|---|
| P2-1 | `RawArchiveEntry` | backend listing 表达原始文件名字节，不再用 `PathBuf` 假装原始元数据 |
| P2-2 | `NativeZipBackend` | 支持 ZIP ZipCrypto / AES、listing、test、extract |
| P2-3 | 两阶段密码验证 | 快速筛选候选，命中后 CRC / test 确认 |
| P2-4 | 编码解析 | UTF-8、GBK / GB18030、Big5、Shift_JIS、EUC-KR；低置信度请求确认 |
| P2-5 | 路径安全 | 默认拒绝 `../`、绝对路径、盘符路径和输出目录外部符号链接 |
| P2-6 | `smartzip list` | 展示目录树、编码候选、加密状态、危险路径和预估大小 |
| P2-7 | fallback 路由 | ZIP 优先原生后端；7z AES、RAR 和复杂格式继续使用 PATH 中的 `7zz` / `7z` |

## Phase 3 - 密码库与高性能尝试

目标：支持数万级密码表和复杂嵌套归档的密码继承。

| # | 项目 | 说明 |
|---|---|---|
| P3-1 | 命名密码表 | 新增 `password_sets` 和 `password_set_memberships`；导入条目全部写入 SQLite |
| P3-2 | 全局唯一去重 | 保持 `passwords.value UNIQUE`，密码可属于多个集合 |
| P3-3 | `PasswordResolver` | 父密码、祖先密码、批次命中、手动、剪贴板、置顶、成功、未命中、失败 |
| P3-4 | 默认智能模式 | 自动尝试前 1000 条候选 |
| P3-5 | 深度模式 | 分页遍历剩余密码，显示进度、速度和预计时间，支持取消 |
| P3-6 | 全局 worker pool | ZIP 默认使用全部逻辑核心；`7z` / `7zz` 子进程并发硬上限初始为 16 |
| P3-7 | 失败统计 | 仅记录 `WrongPassword`；按归档指纹去重；惩罚有上限并随时间衰减 |
| P3-8 | 密码保存提示 | 默认自动保存非空成功密码，允许关闭，首次使用明确提示本地明文存储 |

## Phase 4 - 动态节点工作流

目标：将单个 BFS 循环收敛为可供 CLI 和 GUI 共同使用的动态节点状态机。

| # | 项目 | 说明 |
|---|---|---|
| P4-1 | `ArchiveNode` | 记录父节点、来源、深度、状态、指纹和成功密码 |
| P4-2 | `CandidateIdentity` | 当前任务使用规范化路径、大小和修改时间；内嵌片段增加 offset 和 size |
| P4-3 | 稳定指纹 | 后台异步计算内容哈希，不阻塞几百 GB 资源包 |
| P4-4 | 批次模型 | 多个独立根任务共享批次成功密码，状态与预算分别维护 |
| P4-5 | 等待节点 | 密码未知、编码低置信度、多内嵌片段和预算触发时暂停当前节点，其他分支继续 |
| P4-6 | `VolumeSet` | 识别 `.partNN.rar`、`.7z.NNN` 和纯 `.NNN`，检查缺卷和重复卷 |
| P4-7 | 根输入扫描 | 用户显式解压的根文件忽略扩展名；单高置信度片段自动处理，多片段请求选择 |
| P4-8 | 内层扫描策略 | 默认仅明确归档后缀；激进模式增加容器排除、大小筛选和 finding 数量上限 |

## Phase 5 - 事件流、取消与 GUI 工作台

目标：将稳定后的动态节点工作流接入用户界面。

| # | 项目 | 说明 |
|---|---|---|
| P5-1 | 实时事件通道 | 使用有界 `mpsc`，避免事件生产失控 |
| P5-2 | 取消 | CLI 和 GUI 可立即取消密码遍历和当前任务 |
| P5-3 | CLI 进度 | 输出节点状态、密码速度、预计时间和预算提示 |
| P5-4 | GUI 批次树 | 展示根任务、内层节点、状态和进度 |
| P5-5 | GUI 用户决策 | 处理未知密码、编码确认、安全预算和多内嵌片段选择 |

## Phase 6 - 后置能力

以下功能不阻塞近期核心迁移：

- 密码库 GUI 管理。
- 压缩工作流。
- 完整归档浏览器和 `open / preview`。
- John the Ripper / Hashcat 可选外部深度恢复后端。
- 任务图持久化、崩溃恢复和 `smartzip resume <task-id>`。
- Linux、macOS 和 Windows 系统集成。
- 正式分发时捆绑经验证的 `7zz`。

## 依赖关系

```text
Phase 0 真实缺陷修复
  ↓
Phase 1 安全预算与事务式输出
  ↓
Phase 2 Native ZIP backend
  ↓
Phase 3 密码库与 worker pool
  ↓
Phase 4 动态 ArchiveNode 工作流
  ↓
Phase 5 事件流、取消与 GUI
  ↓
Phase 6 后置能力
```

Phase 2 和 Phase 3 的数据库 schema 准备工作可以部分并行。Phase 4 不应提前建设复杂资源感知 Scheduler；先使用单一全局 worker pool 和清晰状态机。
