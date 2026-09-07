# 实施与证据

基线：`d8724d6`。开始时已有 AGENTS、CONTEXT、docs/agents 和本地技能文件修改，本任务未编辑这些文件。未提交或推送。

## 清理依据

- `access_archive_with_password` 只有 list 工作流一个调用者，`load_listing` 恒为 true；删除另一模式。最终通过 match 返回必有的 `ArchiveListing`，删除调用方不可能触发的 UnsupportedFormat 兜底和完整 listing 克隆。
- 删除始终为空的内部事件字段和始终 None 的加密字段；事件继续由同一个 EventSink 产生，公开 `encrypted` 结果保持 None。移除只在一次 list 调用中创建、写入后立即丢弃的 batch 密码缓存；解压批次复用保持不变。`has_password` 从已使用的密码推导。
- 当前密码来自正在遍历的去重序列，改用 enumerate 计数，删除再次 position 查找及不可能的 0 兜底。删除只包装 Some(clone) 的 password_value。
- 删除扫描配置转发函数、扫描策略未使用的三个私有参数，以及原始 ZIP 文件名评估未使用的密码参数。保留公开请求字段。
- 完整性归约中 Coverage::Complete 当且仅当 Integrity::Intact，而 Intact 已先返回 NotApplicable；删除后续不可达的 Exact 分支，不删除公开枚举值或改变报告语义。
- 采样指纹已排除长度 0，采样位置均在范围内；删除重复位置夹取、buffer truncate/resize，改用固定数组，保留短读行为与采样字节顺序。
- CLI JSON 退出码直接由任务状态生成，删除重复参数。

## 测试取舍

删除 4 个测试：重复的扩展名测试（完整保留相邻参数用例）、只检查构造赋值的测试、只复述编码常量的测试、未真正执行 history 默认分派的解析测试。将原有 Completed + exit_code=1 的不一致 JSON 用例替换为非空 Partial 结果合同测试。

新增 6 个行为用例：

- 2 个真实 CLI 子进程测试，使用独立临时数据库和禁用后端发现的显式配置：密码清理 preview 不写库、apply 保留置顶、导入去重/恢复、导入失败不改库、导出/删除；history 实际默认分派、别名、状态与原因交集过滤、show 和未知任务错误退出。
- 4 个 list 工作流用例：存储候选重试成功、交互成功、交互取消、交互密码错误。检查实际请求顺序、停止重试、事件编号、事件不包含测试密码、返回结果与 listener 时间线一致。使用 mock 后端，不将其当作真实归档验收。

## LLVM coverage / CRAP

基线命令：`cargo llvm-cov --workspace --lcov --output-path target/cleanup-baseline/workspace.lcov`，再以 LCOV 运行 `cargo crap --workspace --exclude 'tests/**' --format json`。

最终命令：`scripts/crap-scan.sh --out-dir target/cleanup-after --top 30`；另导出完整 JSON 与 `cargo llvm-cov report --summary-only`。脚本现覆盖完整 workspace、排除独立测试文件，不再 `--ignore-run-fail`。quick 模式仍只是复杂度假定零覆盖，不能用作实际 CRAP 基线。

| 函数 | 行覆盖率 前 → 后 | CC 前 → 后 | CRAP 前 → 后 |
| --- | --- | --- | --- |
| CLI password | 0 → 54.21% | 31 → 31 | 992.00 → 123.29 |
| CLI history | 0 → 51.81% | 31 → 31 | 992.00 → 138.56 |
| access_archive_with_password | 55.44 → 87.72% | 31 → 28 | 116.03 → 29.45 |
| test_reduce::reduce | 78.74 → 80.49% | 44 → 41 | 62.60 → 53.49 |

Workspace 汇总行覆盖率 48.16% → 49.01%。总比例包括 GUI 等本轮未改变区域；不是验收门槛。采样指纹行覆盖率 93.75% → 93.10% 是删除已覆盖行导致的分母变化，不代表增加了未覆盖分支。

原始产物在 `target/cleanup-baseline/{workspace.lcov,crap.json,summary.txt,coverage.log}` 和 `target/cleanup-after/{smartzip.lcov,crap.json,summary.txt}`；构建产物目录未纳入版本管理。

## 验证

- 基线 LLVM workspace 测试 438 passed；最终同范围 440 passed（删除 4、新增 6），零失败、零忽略。
- `cargo build -p smartzip-cli --locked` 通过；`python3 scripts/verify_cli_beta.py target/debug/smartzip`：真实 7-Zip CLI 验收 23 组全部通过，包含直接密码解压、路径/链接攻击、冲突回滚、嵌套分卷、资源预算、进程取消回收、终端密码与历史一致性。日志：`target/cleanup-baseline/acceptance.log`。
- `cargo check --workspace --all-targets --locked`、`cargo fmt --all --check`、`scripts/check_routing_guards.sh`、`bash -n scripts/crap-scan.sh`、`git diff --check` 通过。
- all-targets 检查仅有现有依赖 proc-macro-error2 2.0.1 的 future-incompatibility 提示。

## 范围边界

未把 fallback、panic 转换或 I/O 错误处理按外观批量删除。高复杂度解压编排与分卷解析仍有热点，覆盖率工具不能证明它们的保护无用；本轮只删除能从调用点和不变量证明冗余的代码。保留公开 API、路径安全、资源、密码、取消与输出回滚测试。此次没有性能测量或 GUI 交互验收，不能据此宣称解压提速或所有行为绝对无回归。
