# 单一现行契约

## 目标与授权

用户明确要求：只维护一套约定、数据库和配置，不需要旧版本兼容；大改动按收益推进，不因影响面大而排除。此约束取代上一轮审计中“保留旧接口/迁移”的取舍。

不操作用户数据库、配置或安装；所有验证使用临时数据。源文件的旧协议和历史测试直接删除，不为旧断言补回兼容路径。

## 实施

- **后端合同**：executor/adapter 只有显式 TaskExecutionContext 的执行方法；executor 解压只接受 facts + context。删除无上下文入口、默认兼容转发、Router 重复解压路径以及 SevenZip/Unrar 旧 probe 分支。
- **SevenZip 执行链**：删除独立公开的 report/observer API、SevenZipResult/Report/ExitStatus 等包装及测试专用报告字段；直接使用 BackendCommandOutput，产品进度只通过任务上下文发出。结构化完整性诊断、输出预算、错误分类、kill + wait、失败清理与路径检查保留。
- **引擎入口**：删除四种 extract_recursive 重载、带无效 PasswordService 参数的 inspect 包装、ExecutionStateRecorder 兼容别名。调用方使用 extract + ExtractInteraction/ExtractObserver 与 ExecutionControl。保留参数不同的显式任务身份入口及准备任务入口。
- **配置数据流**：SafetyOptions 只解析 CLI 输入；编译得到的 CompiledRunPolicy 是 CLI 执行层唯一配置来源，不再反写 Clap 字段或重新拼默认值。提示控件直接使用 config 领域枚举，恢复任务沿用自己的策略和同一 stdin 锁。
- **配置文件与路径**：删除 v0 转换、config migrate、GUI 迁移入口与旧应用目录探测。只解析现行 SmartZipConfig；缺失字段继承默认值，未知字段/不支持的版本报错。默认配置和数据库只使用标准路径，显式路径仍可指定。
- **数据库**：删除 v1–v7 迁移链与 rusqlite_migration 依赖，使用一次事务创建当前完整 **v8** schema。删除仅为兼容保留的 known_files.last_extract_at 字段；完成事实只存 file_extractions。只读和可写打开都只接受 v8；旧版本或非空未知库报错、保留原文件，不自动重建或清空。旧库使用者需通过 --db/state.database 选新路径。
- **恢复装配**：GUI 与 CLI 共用 PreparedExtractTask::recover/run，不再单独解码、编译配置或创建密码服务。所有可能失败的预读仍在取得恢复执行权之前完成，任务身份和预算保持不变。
- **密码策略**：PasswordService 始终持有当前 Passwords 策略，删除无策略排序分支。数据库候选上限只取策略，不再在每个请求重复传递。删除未接通的 clipboard 候选模型和 --use-clipboard 占位参数；手动密码、本批次、已知文件、空密码与数据库来源按同一策略排序。
- **历史测试**：删除旧数据库迁移 fixture/验收、旧接口等价测试、公开报告格式测试，以及冻结旧帮助文本的五份快照。保留当前命令错误输出、归档行为、编码、密码、回滚、资源与恢复测试。Slow 测试 adapter 按新合同显式观察 context 取消，而非依赖已删除的默认转发。

## 验证与测量

- `cargo check --workspace --all-targets` 通过。
- 最终 `cargo test --workspace --locked --no-fail-fast`：**620 通过、0 失败、11 忽略**。其中 9 项 GUI 后端验收在下一项显式执行；剩余为子进程 helper 和需要 NFS 挂载的既有忽略项。
- 使用工作区内 TMPDIR 和隔离 XDG_DATA_HOME 执行 `cargo test --locked -p smartzip-gui runtime::worker_tests:: -- --include-ignored`：**9 通过**。覆盖任务恢复、源文件变化/回收、临时密码、取消及根控制，不使用用户回收站。
- `cargo build --locked -p smartzip-cli` 通过；`verify_review_fixes.py` **5 项通过**，覆盖历史唯一完成事实、密码证据、编码预览、PTY 重试和取消。
- `verify_recovery.py` **5 项通过**：真实后端执行所有权、解压中及 Linux 提交三阶段 SIGKILL/重启，校验输出、历史、预算与暂存/备份清理。只删除了旧 v5 迁移验收段。
- 新增现行密码策略的数据库候选上限验证；数据库初始化/重复打开、只读版本检查、拒绝旧版/未知库并保留数据均通过。
- `cargo fmt --all -- --check`、`scripts/check_routing_guards.sh`、`git diff --check` 通过。
- 恢复故障注入 C helper 有既有的 write 返回值未使用编译警告，验收成功；未扩展修复该 helper。

本轮 Rust 源码净减少 **1326 行**（包含测试变化，不含删除的帮助快照、SQL fixture、锁文件和文档）。Linux CLI 依赖树去重包数从 **219 降至 218**，移除 rusqlite_migration；未升级其他依赖。

编译比较基线为 `6c62a9b`，在同一临时源码路径分别使用旧源码和本轮源码。Rust 1.98.1、20 逻辑 CPU，关闭 sccache/incremental；每阶段先清理全部本地工作区包并预热，然后每个计时样本只清理 engine/CLI/GUI，运行 `cargo build -p smartzip-cli -p smartzip-gui --locked --timings`。

| 范围 | 修改前（秒） | 修改后（秒） | 中位数 |
| --- | --- | --- | --- |
| 预热依赖后的 engine/CLI/GUI 重编译，各 2 样本 | 7.281 / 6.815 | 6.781 / 6.232 | **7.048 → 6.507（约 -7.7%）** |

该工作负载不含 archive/DB 及第三方依赖的冷编译耗时。原始样本见 `build-measurements.json`，测量脚本和完整日志保存在 `.work/one-contract/`。本机为 Linux；未执行 macOS/Windows 运行验收。
