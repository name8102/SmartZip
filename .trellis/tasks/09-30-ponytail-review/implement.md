# 审查修复与改进

日期：2026-09-30。审查基线 `724933d`；实施分支 `codex/ponytail-review-fixes`。用户授权落实全面审查结论并本地提交。沿用 ponytail、rust-patterns、rust-testing；三个代理按 engine、archive/scanner、data 边界并行修改，主代理处理 GUI/CLI 并复核集成，额外只读复核 GUI 状态转换。

## 实施结果

| 审查项 | 修复与验收依据 |
| --- | --- |
| R1 嵌套清理误删新输出 | 提交前捕获现有 SourceSnapshot，回收线程核对源身份与托管根，排除实际发布目标及子树。`nested_cleanup.rs` 的真实 7zz 单包/分卷、Keep/Delete/Trash、同名输出/正常清理/提交后源变化共 18 场景通过；原 CLI 复现现保留产物。Trash 用注入删除回收器验证路径选择。 |
| R2 数据库别名绕过 owner | 所有读写/执行入口使用 canonical 路径；硬链接明确拒绝，保留 epoch fencing 和单独 owner 锁。写入口创建缺失父目录，读入口不创建；拒绝 dangling 最终链接。Unix 不额外打开/关闭 DB 文件描述符，避免释放 SQLite POSIX 锁。符号链接、父目录别名、硬链接/WAL、独立进程事务锁回归通过。 |
| R3 密码导出权限 | CLI/GUI 复用 passwords 层私有临时文件、同步后完整发布，已有文件/链接一律拒绝覆盖。Unix 0600；含换行密码拒绝导出且错误不含值。真实 CLI umask022 复验为0600；重复目标和 dangling symlink 保留。 |
| R4 GUI 恢复服务遗漏 | 复用 PreparedExtractTask::recover/run，保留输入预检、根历史、显式 claim 与取消。真实加密 ZIP 的 Known-only/Auto/Never 恢复完成且无提示，记录正确 password_id、sample_hash、encoding_corrected=true。临时密码仍不保存，恢复不追补源回收。 |
| R5 AUTO 大小写 | 配置反序列化及策略编译共同规范化 encoding mode，覆盖手工构造与恢复快照。配置/策略回归通过；真实 GBK ZIP 的 auto/AUTO 均正确中文名并产生检测事件。 |
| R6 CLI 控制字符 | 复用已有 safe_text 到成员、路径、消息、历史、提示与错误的文本显示。JSON 继续交给序列化器，保存原字段语义。历史控制字符回归及真实 ZIP ESC 列表复验通过。 |
| R7 管道收尾取消 | Ordinary/Streaming 的 wait 和 pipe drain 均处于取消 select 范围，保留各模式的截断/错误分类。Unix 保留初始进程组 ID，父进程已退出时仍终止后代；三个 mode 的后代持管道回归通过。 |
| R8 RAR5 不可信整数 | header/extra 范围采用 checked conversion/add/sub，限制 vint 最后字节和 u32 卷号，畸形输入保守降级。溢出、截断、越界和有效结构回归通过。 |
| R9 假备份警告 | 仅 finalize 删除失败后报告保留路径及原因；正常覆盖不报告已删除备份。成功清理与注入清理失败回归通过。 |
| S1 队列反复 JSON | Job 设置修订号与 Write 修订号替代全量字符串签名；仅编辑时序列化，位置/终态独立保存，确认只传 ID/修订号。旧确认不能解除新版本启动屏障；删除记录等待最终确认后释放，已确认终态释放 JSON。旧确认、重排/移除、失败重试、100 次空闲无重复发送测试通过；SQL 接管保护保留。 |
| S2 根快照成本 | 使用带修订号的 snapshot_if_changed，未变化时不加锁复制节点/事件，终态强制获取最终快照。根活动改一次遍历聚合；修订与状态同锁发布，优先级及并发一致性回归通过。 |
| S3 ZIP 重复尾部搜索 | Input 共享 EOCD 位置和已搜范围，上限512 KiB，超限回退原检查路径；不缓存针对某起点的失败关联。无 EOCD 多头、内层、ZIP64、拼接、完整 EOF 和取消测试通过。 |

两项已有问题也已处理：旧 p7zip 仅为完整 `Open ERROR: Can not open the file as [7z] archive` 增加窄协议识别，损坏/密码分类仍优先；默认旧 p7zip 的歧义分卷测试由失败变为通过。名称碰撞由 SevenZip、Unrar、decoded ZIP 在实际输出卷临时探测，冲突在成员解压前失败；真实 APFS case/NFC–NFD ZIP/RAR 用例保留源与旧输出且清理探测目录。没有引入完整的自动重命名/名称映射产品。

README 对齐导出目标、DB 别名、成员碰撞边界和现有 Linux 集成实现。核心术语标明 CandidateAttempt 为历史名称，没有新增同名框架。

## 验证

全部使用临时目录、合成密码和隔离数据库；真实后端为当前 macOS 的 7zz 26.02、旧 7z 17.05、Unrar 7.3.1。没有修改用户配置或数据库，没有操作原生窗口或系统关联。

- `cargo test --locked --workspace`：659 passed，0 failed；默认忽略的后端/成本用例另显式验收。计数包含下列 crate 子集，不将它们重复相加。
- `cargo test --locked -p smartzip-gui --bin smartzip-gui`：59 passed，包含最新队列回归。
- GUI `runtime_worker -- --ignored --test-threads=1`，使用现代7zz wrapper：10 passed，包含新恢复回归。
- archive/scanner 专项：124 passed、1 ignored；ignored 成本测试另显式1 passed。engine lib 261 passed，nested_cleanup 2 passed（18场景）。
- CLI 隔离审查复验：completed、产物存在、备份警告0；密码导出0600；原始 ESC=false。固定后端脚本证据保存为 `target/ponytail-review-2026-09-30/cli-probes-fixed.json`，原审查证据保留。
- `verify_review_fixes.py`：5检查通过，包括 PTY 错误后正确密码重试及取消；`verify_resource_bounds.py`：3检查通过，包括192 MiB空前缀、文件数上限、共享输出预算；macOS RSS采样为null，不能据此主张内存峰值。
- `verify_recovery.py`：3检查通过，包括v5迁移重复打开保留数据、第二owner拒绝、SIGKILL后重启恢复无暂存/备份泄漏。macOS未运行Linux renameat2提交故障注入。
- scripts unittest：12 passed。`cargo fmt --all --check`、routing guards、`git diff --check`通过；workspace Clippy通过，保留既有 lint 和 `block 0.1.6` future-incompat提示。GUI新增的无用转换已消除，专项Clippy通过。

可重跑的新增回归随源码提交；运行日志和临时 harness 留在 ignored `target/ponytail-review-2026-09-30/fixes/`，不提交产物。原有 `.trellis/tasks/09-30-macos-platform-check/` 未改动且未纳入提交。

## 性能依据与边界

- 相同 debug 合成扫描 harness 的4/8/16 MiB无EOCD多头输入，修复前312/1073/3960 ms，修复后97/189/378 ms；取消检查次数由3499/11723/41995降至1234/2586/5290。证明该场景的重复读取成本下降，不代表端到端吞吐提升；密集EOCD超过缓存上限会回退并可能重复检查。
- 实际 APFS 的3000个平铺成员，仅名称探测与完整清理，7次426–747 ms，中位610 ms。安全检查每个文件系统组件，最多100000组件及16 MiB路径元数据；普通大量小文件会增加耗时，超预算明确失败。后续快路需要目标卷等价规则的可靠依据，本轮不以猜测casefold/NFC代替检测。
- GUI优化通过状态/确认契约验证；没有原生窗口FPS/RSS或整批GUI性能证据。Windows/Linux原生运行、Windows ACL与进程取消、真实系统垃圾桶回收未验收。

## 提交

本地提交包含本记录、原审查报告、修复源码及回归。没有推送或创建PR。
