# 实施与验证

## 实现边界

- `PreparedExtractTask` 共用新建/恢复请求准备、脱敏提交与运行装配；`CompiledRunPolicy` 只读，规范化请求只在进入执行前应用一次。外部已有解压 API 保留，并一致规范化相对输入身份。
- `RunServices` 集中密码、历史、已知文件配置，保留只读或关闭历史写入时的完成记录查询。CLI/GUI 持有数据库、后端和交互控件；CLI 恢复交互共享终端锁但读取恢复任务自身策略，GUI 不再由新任务策略屏蔽恢复密码询问。
- 任务层持有一个有界事件缓冲并发出一次 Started/策略快照/Finished，清理警告进入同一时间线；按实际产生顺序保留至多 4096 条，实时 listener 接收全部。历史只开始/结束一次。
- 根输入共享预算、批次密码、去重和后端负面能力缓存；根事件接收与取消独立。准入只保存索引，获准运行后才复制单输入请求，管理/非管理多根使用同一循环。
- `ExecutionControl` 明确主动控制职责，保留 `ExecutionStateRecorder` 兼容别名。集中普通跳过与分卷准备失败的记账；不改发布/提交/回滚边界、锁后去重检查和数据库格式。无控制器的原有单工作流调度语义保留。
- 恢复候选只反序列化一次；阶段计划直接构造；清理误挂或过时注释。

## 验证

环境：macOS arm64、Rust 1.98.0；默认发现 p7zip 17.05 与 7zz 26.02。旧目录的测试结果不计入此处。

- `cargo fmt --all --check`、`bash scripts/check_routing_guards.sh`、`git diff --check` 通过。
- 新主线修改前的 engine lib 248 项通过；修改后 engine lib 254 项通过。新增验证涵盖并发根事件顺序/统一保留预算/一次生命周期、任务路由缓存、异常历史收尾、提交脱敏、恢复身份/代次/预算、只读历史复用与相对输入入口一致性。
- `cargo test --workspace --locked` 首先遇到进程启动错误断言失败。随后使用 `--no-fail-fast -- --skip process_start_errors_preserve_backend_identity_and_io_path` 收集其余结果：613 项通过、3 项失败、7 项忽略。失败中的进程取消时序用例单独重跑通过；其余失败和被过滤的一项见下表。未宣称全工作区测试全绿。
- `cargo test --locked -p smartzip-gui --test runtime_worker -- --ignored`：7 项真实后端验收通过，覆盖密码等待取消、源文件保留/变更/回收、临时密码及根控制。新增真实恢复用例在 GUI runtime 中通过：新任务 interaction=never，恢复任务仍按已保存配置询问密码，两个任务均完成。
- `cargo build --locked -p smartzip-cli` 通过。对本轮 debug 可执行文件运行 `verify_recovery.py`（3 项）、`verify_review_fixes.py`（5 项）、`verify_resource_bounds.py`（3 项）均通过。
- `verify_cli_beta.py` 默认 p7zip 在嵌套 RAR 用例失败；将仅该验收进程的 `7z` 解析到已安装的 7zz 26.02 后，23 项通过。未改项目配置或用户 PATH。
- 资源脚本覆盖 192 MiB 稀疏前缀扫描、4000 文件的 2000 项上限回滚、三个根共享 3 MiB 预算；本机不提供 Linux RSS 采样。这是有界功能证据，不是吞吐性能结论。

### 原有失败与限制

以下三项均在从 `f3b74a0` 导出的未修改源码中、相同工具链与本机环境单独复现；未扩大本次改动去修改这些模块或放宽断言：

| 测试 | 本机结果 |
| --- | --- |
| `smartzip-archive::process_start_errors_preserve_backend_identity_and_io_path` | 将目录用作可执行程序时的错误类型/路径断言不符。 |
| `smartzip_integration::ambiguous_volume_groups_use_first_successful_extraction` | 自动发现路径仍优先选择 p7zip 17.05；错误分组试解以 terminal-backend-error 结束。 |
| `smartzip-platform::automator_runs_generated_workflow_with_file_arguments` | Automator 提示无法与帮助程序通信。 |

`sevenzz::tests::cancellation_kills_process_group_and_stops_writing` 在整套运行中一次因取消前未及时写入而失败，单独重跑通过，保留时序波动记录。

当前平台运行的恢复脚本覆盖 SIGKILL/重启、数据库迁移、执行所有权与产物/预算一致性；Linux renameat2 提交故障注入及原生 GUI 窗口验收未在本机执行。

提交前再次 fetch，`origin/main` 仍为 `f3b74a0`。旧工作目录中的初版重构与用户其他改动保持原样；只提交新 worktree 的实现。
