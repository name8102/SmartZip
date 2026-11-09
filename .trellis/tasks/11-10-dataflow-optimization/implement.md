# 验证与测量

## 验证

使用 Nix 开发环境，Rust 1.98.1；本机 Linux。未执行 macOS/Windows 或 GUI 人工视觉验收。

- archive：98 项通过，1 个既有子进程 helper 忽略；覆盖真实 7z 探测、提取、密码、旧编码 ZIP、成员读取、取消和回滚。
- DB/passwords：36 + 6 项通过；config：7 项通过，包括新的不限深度默认值、512 层配置设置/恢复继承和系统文件锁。三 crate 首轮合计 48 项，随后 config 改动由整合检查及新增定向用例覆盖。
- core：12 项通过。
- engine：356 项通过，1 个既有 NFS 用例忽略；覆盖根管理、深度限制、BFS、密码、分卷、产出 inventory、历史与恢复。候选数 0 不限的新增断言定向通过。
- CLI：13 项通过，覆盖现行配置、命令、非交互和可观察事件顺序。
- scanner：最终 28 项通过；文件与内存共用解析，格式、跨块、截断、内嵌、取消及 I/O 计数回归均通过。首次 I/O 断言暴露了候选统一读取 64 KiB 前缀的额外成本，改为按格式读取所需前缀后通过。
- GUI：60 项现行单测得到通过结果，9 个真实后端用例保持原 ignored 标记。递归字段变为可选后，整合中修复了深度显示的类型转换和表单字段覆盖测试；最终分别定向通过可选字段及空数值继承用例。
- GUI 真实后端 Detect/List/Test/Extract + 复用 List 路由器读取成员（0 不限）：1 项通过；队列增量保存的两个用例通过。
- `verify_review_fixes.py`：5 项真实后端验收通过，覆盖完成事实、密码证据、多编码预览、PTY 重试与 Ctrl+C。
- `verify_recovery.py`：5 项真实后端验收通过，覆盖执行所有权、解压 SIGKILL、覆盖提交三个故障点重启，检查输出、历史、预算和 staging/backup 清理。C 故障注入 helper 存在既有 write 返回值未使用警告。
- 包级格式化、最终全仓格式静态检查、routing guards 和 diff 空白检查通过。

上述为受影响 crate 和定向验证的汇总，不是一次 `cargo test --workspace`。没有重复运行已通过且未再变化的检查。日志保存在 `.work/flow-optimization/`。

## 性能证据

### 截断 ZIP 扫描

同一 8 MiB 文件，包含 256 个有效本地 ZIP 头但没有 EOCD；Deep、只包含 ZIP、无扫描/数量上限。使用同一 Nix/debug 构建配置及同一示例代码；每阶段 4 次，首个作为预热，比较其余 3 次中位数。

| 阶段 | 原始秒数（含首个预热） | 去预热中位数 |
| --- | --- | --- |
| 修改前 | 2.272709 / 2.260854 / 2.244172 / 2.256747 | 2.256747 s |
| 修改后 | 0.035687 / 0.034785 / 0.034224 / 0.034370 | 0.034370 s |

该工作负载约 **65.7 倍**加速，所有样本均返回 256 个 finding。回归测试另外通过计数 reader 验证 2 MiB/256 头输入的读取量小于输入的 4 倍，避免只用耗时阈值判断。

原始样本：`scanner-before.jsonl`、`scanner-after.jsonl`。复现时把 `scanner-bench.rs` 临时复制到 `crates/smartzip-scanner/examples/flow_bench.rs`，运行 `nix develop -c cargo run --locked -q -p smartzip-scanner --example flow_bench`；测量后删除临时 example。

### 阶段事件 SQLite 微基准

内存 SQLite 3.53.3/Python 3.14.8，同一事务插入 5000 行，各 3 次；比较旧 MAX(sequence) 热路径与使用插入 ID 的现行路径。

| 路径 | 原始秒数 | 中位数 |
| --- | --- | --- |
| 旧 MAX 子查询 | 1.220999 / 1.304424 / 1.278835 | 1.278835 s |
| 单一 ID | 0.011500 / 0.011259 / 0.013120 | 0.011500 s |

这是 SQL 热路径约 111 倍加速的微基准，不是归档整体吞吐。脚本及三种候选方案的原始样本为 `event_sequence_benchmark.py`、`event_sequence_samples.json`。其他运行时优化有调用次数或复杂度依据，本轮未测 GUI 帧率、所有格式吞吐或冷构建速度。
