# 实施记录

## 当前基线差异

06efceb 已实现配置单次加载、显式覆盖、按需 DB/backend 初始化、clipboard 显式 unsupported、encoding 原始名字节迭代、部分私有参数和别名清理。因此不恢复旧 eager bootstrap，不重复实现已完成项。公开 layout 字段、VolumeResolution variant 和 engine 旧 facade 保留源码兼容。

## 行为锁定

既有 23 项真实 CLI beta 包含直接 x 命令序列、跨 64 MiB 根扫描、原输入 hash、输出/冲突/预算回滚、取消子进程、分卷和历史状态。既有 engine/volume/layout/diagnostic 测试作为 characterization 基线，新增 CLI help 和工作流关键事件合同补齐本轮入口移动风险。

### Commit 0：characterization

新增 3 项 CLI 合同测试：5 个命令完整 help 快照/输出通道，参数错误退出 2 且不建状态目录，真实 UTF-8 ZIP 的关键事件顺序/结果状态/根输入字节保持。完整门槛通过，日志 target/ponytail-validation/00-characterization/；真实 release beta 23 项通过。没有修改生产代码。

### Commit 1：死结构与兼容入口

删除 layout 子分支中不可达的 Raw 分支、私有 MaterializeResult 的无意义 Option、extract 第二次未消费的 candidate key、DB recorder 与 trait 相同的 start override，以及 inspect 私有 workflow 的 PasswordService 参数。外部 inspect 签名保留。VolumeResolution 增加统一借用/归约入口，归约用 Result 保留未解析诊断，既有公开 variant 不删除。

完整门槛通过，日志 target/ponytail-validation/01-dead-structure/；release beta 23 项通过。随后清理一个不再使用的 import 并再次通过 all-targets check、fmt、diff check。未触碰输出事务算法。

### Commit 2：准备、分类与历史行

PreparedArchive 统一 list/inspect/extract 的 carved/canonical 输入 guard、采样、known-file、确认编码、ZIP assessment 和 recorder name。preparation 不发事件；调用者在原位置发事件。FileExtractionRow 使用语义构造器固定 detected/extracted/skipped/failed/unreadable/tested 的默认字段；test 证据归约不变。

nested 使用同一 classify_nested_file；单文件输出与目录枚举的相对路径规则保留。审查未指出的真实差异也保留：单文件输出不进入无后缀内容扫描；目录候选可以。新增 matrix 在重构前后均通过，覆盖头部优先、关闭头部扫描后的后缀回退、业务容器排除、offset/size 和目录 symlink 不跟随。

完整门槛通过，日志 target/ponytail-validation/02-preparation/；release beta 23 项通过。清理移动代码后遗留的 unused import，再次通过 all-targets check/fmt/diff check。

### Commit 3：CLI orchestration 与 facade

Detect/List/Test/Extract 使用各自 clap Args 请求，保留字段顺序、帮助文本和参数名字。bootstrap.rs 集中配置/依赖创建，commands.rs 接收命令请求并执行，render.rs 负责结果与事件输出；main 保留分派与退出码。不创建通用 service container。Command::json_output 是唯一对应命令集合的 JSON 判断。删除已被 bootstrap explain/dry-run 提前返回覆盖的旧私有 dry-run 分支。

engine 新增 extract(request, ExtractInteraction, ExtractObserver) 和不依赖 PasswordService 的 inspect 入口；旧公开重载转发保留，CLI 使用新入口，GUI 未修改。现有管理命令先于 backend、DB 按需创建的行为保持。

完整门槛通过，日志 target/ponytail-validation/03-cli/；help/事件 characterization 与 release beta 23 项通过。

### Commit 4：backend 单实现与兼容 API

SevenZip/Unrar 的 list/extract（及 SevenZip compress）无 context 方法转为 detached context 薄包装，参数构造、协议解析、错误归约和 observer 在 context 实现中只执行一次。probe 统一准备/结果组装，但显式保留旧入口与 context 入口的诊断差异：7z 原有两种 test 协议、Unrar password diagnostics 的 supported 判定不被静默改写。

ArchiveExecutor/ArchiveAdapter 仍保留旧 required 方法和 context 默认转发方向，避免破坏外部 trait 实现者或形成互递归。Router 的 canonical facts/context 与 fallback 未修改。新增 3 项 fake-process 合同测试，锁定命令参数、返回值、observer 单次交付与 probe 差异；这类注入测试不是实际 unrar 验收。

完整门槛通过，日志 target/ponytail-validation/04-adapters/；release beta 23 项通过。
