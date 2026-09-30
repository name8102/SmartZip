# SmartZip 全面代码审查

日期：2026-09-30。源码基线：`724933d`。采用 ponytail 与 code-review 技能。审查当前项目的现有行为，不以某个提交差异为范围；未实施生产修复、提交或推送。

后续状态：用户已要求落实修复并提交；修复、改进与验收见 [implement.md](implement.md)。以下保留审查时的基线观察，位置行号也对应该基线。

## 结论

本轮确认 **9 项问题：3 项 P1、6 项 P2**。其中 7 项有隔离运行证据，2 项由源码及依赖实现确认，尚未做专项动态复现。另有 3 项有当前成本依据的简化机会。已有 APFS 名称碰撞问题和旧 p7zip 兼容问题仍需处理，单独列出，避免将历史证据当成本轮验收。

优先修复顺序：嵌套清理误删新输出 → 数据库执行所有权 → 密码导出权限 → GUI 恢复服务遗漏。其余修复可以按模块独立进行，不需要先引入通用工作流框架。

## 范围、依据与覆盖

阅读了 AGENTS.md、产品范围、相关核心术语、任务执行与输出所有权 ADR、任务记录规则，以及与恢复、路径策略和近期清理有关的任务资料。判断以当前源码为准，未将旧设计视为已实施功能。

| 范围 | 本轮检查内容 |
| --- | --- |
| core、config、db、passwords、encoding、scanner | 并行审查全部生产模块；配置解析/校验、状态迁移和执行锁、密码来源与持久化、采样身份、编码、文件扫描边界；既有测试与隔离 public API 复现 |
| engine | 任务装配、服务注入、递归候选、分卷解析与别名暂存、布局/提交/回滚、源与嵌套清理、预算、资源调度、根控制、状态恢复及事件流的关键调用链 |
| archive | router/adapter 能力与错误分类、后端命令、进程生命周期、成员读取、ZIP 名称解码、路径校验、分卷探测及有界诊断的关键生产路径和既有测试 |
| GUI | 启动/实例转发、配置与密码库、任务装配/恢复、队列持久化、根快照、浏览/预览与交互控制 |
| CLI、platform、scripts、CI | 引导与状态策略、任务与管理命令、文本/JSON 输出、系统集成、安装/打包路径与失败回滚、脚本测试和构建入口 |

覆盖全部 11 个 crate 和相关辅助脚本。此处“全面”指跨模块、跨入口和数据保护边界的项目审查，不表示每行源码都已人工逐行审计，也不表示全部平台或全部真实后端组合已经验收。压缩创建不在当前产品范围内，不提出压缩产品入口需求。

## 问题

### R1 · P1：嵌套归档清理会删除刚发布的同名成员

位置：[extract_workflow.rs:2410](/Users/charl/Documents/Projects/SmartZip/crates/smartzip-engine/src/extract_workflow.rs:2410)、[nested.rs:102](/Users/charl/Documents/Projects/SmartZip/crates/smartzip-engine/src/nested.rs:102)。分卷成员清理分支也使用同一判定。

触发：外层 ZIP 含 `bundle/payload.zip` 和同级 readme；内层 ZIP 的唯一成员也名为 `payload.zip`。使用 `flat-single`、`overwrite` 与 `cleanup.nested_archives=delete` 时，提交将内层源路径替换为新解出的成员。随后清理只检查该路径当前是托管输出根内的普通文件，无法区分它已被新输出替换，于是删除新成员。

真实 7zz 隔离复现：退出 0，任务 `completed`，`processed_count=2`、`failed_count=0`；最终 readme 存在，`bundle/payload.zip` 不存在。此问题不依赖并发或外部文件替换。配置为 trash 时也会回收错误的文件，但本轮只动态验证了 delete。

建议复用 [source_cleanup.rs:10](/Users/charl/Documents/Projects/SmartZip/crates/smartzip-engine/src/source_cleanup.rs:10) 现有源身份快照思路，在提交前捕获原嵌套输入，回收前确认同一文件仍未变化，并排除实际发布目标。对单文件及分卷成员采用一致的源身份规则；不能仅凭路径和所在目录决定清理。回归用例应覆盖同名输出以及 Keep/Delete/Trash 的语义。

证据：[cli-probes.json](/Users/charl/Documents/Projects/SmartZip/target/ponytail-review-2026-09-30/cli-probes.json)，可复现脚本 [cli-probes.py](/Users/charl/Documents/Projects/SmartZip/target/ponytail-review-2026-09-30/cli-probes.py)。

### R2 · P1：数据库路径别名绕过执行者锁

位置：[db/lib.rs:39](/Users/charl/Documents/Projects/SmartZip/crates/smartzip-db/src/lib.rs:39)、[db/lib.rs:152](/Users/charl/Documents/Projects/SmartZip/crates/smartzip-db/src/lib.rs:152)；消费者见 [state_store.rs:614](/Users/charl/Documents/Projects/SmartZip/crates/smartzip-engine/src/state_store.rs:614)。

锁路径由传入数据库路径直接追加 `.owner.lock` 得到。`state.db` 与指向它的 `alias.db` 使用不同锁文件，即使首个 owner 仍存活，第二个 owner 仍可取得相同物理数据库的执行权。

隔离 public DB API 复现：两个 owner 的 epoch 为 1 和 2；第二 owner 的 `claim_recoverable` 将首个 owner 活跃节点的 generation 从 0 改为 1，首个 worker 下一次 transition 返回 false。StateStore 启动随即执行恢复、暂存和提交协调，因此可能干扰仍在运行的解压。**本轮未启动双 GUI，也未动态演示暂存删除或数据库损坏**，这些后果不能当作已观察结果。

建议先统一实际数据库身份再取得排他执行锁，并让读取/写入/恢复都使用统一路径。明确处理符号链接与其他物理别名；单纯 canonicalize 对符号链接有效，但对硬链接不充分。新增同一数据库经别名打开时拒绝第二 owner 的验证，保留现有 epoch/generation 防迟到写入机制。

证据：[data-evidence/results.json](/Users/charl/Documents/Projects/SmartZip/target/ponytail-review-2026-09-30/data-evidence/results.json) 与同目录 Rust harness；仅符号链接别名做了动态验证。

### R3 · P1：CLI 密码导出在常见权限设置下对其他用户可读

位置：[cli/main.rs:923](/Users/charl/Documents/Projects/SmartZip/crates/smartzip-cli/src/main.rs:923)。

`password export` 通过 `std::fs::write` 新建明文文件，权限随进程 umask。隔离数据库中加入合成密码，使用 `umask 022` 导出：命令成功，新文件权限为 **0644**；当所在目录允许其他用户访问时，他们可以读取密码。

GUI 已在 [library.rs:253](/Users/charl/Documents/Projects/SmartZip/crates/smartzip-gui/src/library.rs:253) 使用私有临时文件、校验和发布流程。建议在合适的共享层复用该导出规则，Unix 上新文件仅 owner 可读写，发布完整文件；处理已有目标时也要明确权限和覆盖策略。保留命令输出的计数与路径，密码内容不进入日志。Windows ACL 未在本轮验证。

证据：`cli-probes.json` 的 `password_export.mode=0o644`。这是明文导出文件的访问权限问题，导出动作本身是用户要求的功能。

### R4 · P2：GUI 恢复遗漏保存的已知密码和确认编码提示

位置：[runtime/recovery.rs:45](/Users/charl/Documents/Projects/SmartZip/crates/smartzip-gui/src/runtime/recovery.rs:45)、[:104](/Users/charl/Documents/Projects/SmartZip/crates/smartzip-gui/src/runtime/recovery.rs:104)、[:117](/Users/charl/Documents/Projects/SmartZip/crates/smartzip-gui/src/runtime/recovery.rs:117)。

GUI 恢复自行 decode policy、构建 PasswordService，并以 `history: None` 调用 `extract_task`。`access::prepare_resolved_archive` 只有收到相应提示/记录服务才计算采样身份并查询 known_files，因此恢复缺少已知密码与确认编码提示，也不会更新该文件提示。新 GUI 解压和 CLI 恢复通过 `PreparedExtractTask::run` 注入 `RunServices::stores`。

已用当前 GUI runtime 与真实 7zz 隔离复现：默认独立配置、临时数据库，复制仓库加密 fixture 并保存其已知密码；设置 `passwords.sources=[known]`、`interaction.mode=never`，关闭递归、固定后端。新 GUI 任务 `completed` 且存在输出；持久化相同计划后走 GUI Recover 为 `failed` 且无输出。确认编码遗漏由同一服务调用链确认，尚未单独动态复现。

建议复用 `PreparedExtractTask::recover` / `run`，保留 GUI 的输入存在检查、根历史展示、显式 claim、取消，以及恢复不追补源归档回收的语义。这样同时消除重复装配和入口间服务漂移。补充 known-only 恢复及确认编码复用验证。

证据：[recovery-probe.log](/Users/charl/Documents/Projects/SmartZip/target/ponytail-review-2026-09-30/recovery-probe.log)，harness：[probe/src/main.rs](/Users/charl/Documents/Projects/SmartZip/target/ponytail-review-2026-09-30/probe/src/main.rs)。

### R5 · P2：接受 `AUTO` 配置，却跳过自动编码检测

位置：[config/model.rs:417](/Users/charl/Documents/Projects/SmartZip/crates/smartzip-config/src/model.rs:417)、[access.rs:167](/Users/charl/Documents/Projects/SmartZip/crates/smartzip-engine/src/access.rs:167)、[run_services.rs:58](/Users/charl/Documents/Projects/SmartZip/crates/smartzip-engine/src/run_services.rs:58)。

校验将 encoding mode 转为小写判断有效性，但不规范化保存值；运行时多处使用大小写敏感的 `== "auto"`。同一 GBK ZIP、显式现代 7zz：`auto` 与 `AUTO` 都通过 config check；前者列出正确中文名、结果 encoding 为 GBK、存在 EncodingDetected，后者列出乱码、结果 encoding 为 auto、没有检测事件。GUI 的设置映射也存在精确字符串比较。

建议在共同配置/策略入口统一规范化，或复用统一的编码模式解析，使 CLI、GUI 和恢复快照遵守同一规则；无需为两个特殊字符串建立另一套设置系统。

证据：[encoding-results.json](/Users/charl/Documents/Projects/SmartZip/target/ponytail-review-2026-09-30/data-evidence/encoding-results.json)；同目录 `reproduce_encoding.py` 已固定 7zz，并由主审复验。

### R6 · P2：CLI 原样输出归档名称中的终端控制字符

位置：[render.rs:334](/Users/charl/Documents/Projects/SmartZip/crates/smartzip-cli/src/render.rs:334)。

`list` 非 JSON 输出直接显示 entry path。隔离 ZIP 成员名包含 ESC 的清屏序列；真实 7zz 下命令退出 0，stdout 保留原始控制序列。显示归档内容可以清屏或伪装后续终端文本，本轮未验证、更不宣称命令执行。

同文件已存在 `safe_text`，且测试报告渲染已使用它。建议复用到所有非 JSON 的不可信路径、成员名及后端消息，保持 JSON 序列化的转义契约。源文件名/事件消息的其他直出入口应一并检查，避免只补 list。

证据：`cli-probes.json` 的 `terminal_control.stdout_contains_raw_escape=true`；脚本只报告布尔结果，不将控制序列输出到用户终端。

### R7 · P2：普通与流式后端的管道收尾不再响应取消

位置：[process.rs:218](/Users/charl/Documents/Projects/SmartZip/crates/smartzip-archive/src/process.rs:218)、[:233](/Users/charl/Documents/Projects/SmartZip/crates/smartzip-archive/src/process.rs:233)。**源码确认，未做本轮专项动态复现。**

Ordinary/Streaming 的 `select!` 只覆盖 `child.wait()`；父进程返回后，stdout/stderr 的 `collect` 位于取消分支之外。若配置的后端是包装程序、父进程退出而后代仍持有管道，后续取消不会选中，调用继续等待 EOF。

已核对本地 process-wrap 9.1.0 的 Unix `ProcessGroupChild::wait`：先等待父进程，再对组执行 waitpid；非本进程直接子进程且未被收养的后代不能由该 waitpid 等待，ECHILD 会结束等待。因此进程组包装不能保证消除此收尾窗口。router 又明确等待后端完成后才清理，所以此窗口影响取消返回和暂存清理。

Diagnostic 已把 wait/read 三项纳入同一个取消范围，成员读取也有并发 join。建议让普通/流式收尾复用相同生命周期范围，同时保留各 mode 的错误分类和输出截断规则。既有 parent-exit 管道持有测试只覆盖 Linux Diagnostic；应为其他模式补对应取消契约验证。

### R8 · P2：RAR5 快速探测对不可信长度使用未检查加法

位置：[volume_probe/rar.rs:97](/Users/charl/Documents/Projects/SmartZip/crates/smartzip-archive/src/volume_probe/rar.rs:97)。**源码确认，未做本轮专项动态复现。**

`extra_size` 来自头内 u64 变长整数，执行 `pos + extra_size as usize` 前没有范围检查。64 位 debug 构建会因足够大的值加法溢出而 panic；关闭溢出检查的构建会回绕，令长度检验可能错误通过。输入只有少量头字节即可进入此探测，不能依赖后续后端验包弥补前面的解析边界。

建议使用 checked conversion / checked_add，对不完整或溢出字段保守返回无法确定结构；同时验证变长整数末字节范围和卷编号转换。诊断模块的 Bytes parser 已有相应有界判定模式，可复用规则，但不要因此合并快速分卷探测与完整性诊断的职责。

### R9 · P2：成功覆盖后仍报告不存在的备份

位置：[materialize.rs:445](/Users/charl/Documents/Projects/SmartZip/crates/smartzip-engine/src/materialize.rs:445)、[:500](/Users/charl/Documents/Projects/SmartZip/crates/smartzip-engine/src/materialize.rs:500)。

发布成功时将 `old output backup retained at ...` 加入警告；随后 finalize 成功删除备份，却保留先前警告。真实 7zz 的 R1 同名覆盖复现中出现一次该警告，任务结束后相应路径不存在。这会给用户错误的恢复位置。

建议在最终清理结果确定后才生成“保留备份”警告；成功清理不产生该消息，删除失败才保留路径和原因。保留提交阶段备份及回滚保护。

证据：`cli-probes.json` 中 `backup_warning_count=1`，`backup_warning_paths_exist=[false]`。

## Ponytail 简化机会

### S1：减少 GUI 等待队列的重复序列化和完整字符串副本

位置：[queue_store.rs:118](/Users/charl/Documents/Projects/SmartZip/crates/smartzip-gui/src/queue_store.rs:118)、[:136](/Users/charl/Documents/Projects/SmartZip/crates/smartzip-gui/src/queue_store.rs:136)、[:150](/Users/charl/Documents/Projects/SmartZip/crates/smartzip-gui/src/queue_store.rs:150)；调用见 `ui/mod.rs:122/128/288`。

100 ms UI tick 内调用两次 sync。每次在判断变化前，对非活动解压任务（含仍展示的已结束任务）克隆并序列化完整 JobRequest。签名又保存完整快照字符串，sent/saved 重复持有副本，移除任务的 tracked 项没有回收路径。即使不写盘，仍约每秒 20 次重新准备这些状态。

建议按排队、编辑、顺序或状态变化推进修订号，只为变化项构造一次脱敏快照，用 `(id, revision)` 确认保存，并在移除任务最终写入确认后释放 tracker。保持最新快照保存成功才能启动、迟到 GUI 保存不得覆盖 engine 执行状态的保障。证据为调用频率与分配路径，未测原生 GUI FPS/RSS，不宣称已观察到卡顿。

### S2：节点快照复用已有 revision，避免不变状态的全量克隆

位置：[model.rs:528](/Users/charl/Documents/Projects/SmartZip/crates/smartzip-gui/src/model.rs:528)、[root_management.rs:64](/Users/charl/Documents/Projects/SmartZip/crates/smartzip-engine/src/root_management.rs:64)、[:102](/Users/charl/Documents/Projects/SmartZip/crates/smartzip-engine/src/root_management.rs:102)。

GUI 每 tick 为活动任务生成完整 snapshot，再做全量相等比较。snapshot 在共享 mutex 内克隆所有节点/事件，并为每个未结束根扫描全部节点计算活动，成本为 O(N + R×N)。RootManagement 已有 watch revision。

建议仅在 revision 变化时取快照，任务完成时仍取最终快照；若变化帧仍成为热点，一次遍历聚合各根活动。复用现有状态模型和通知通道即可，不需要再增加一套 GUI 事件模型。当前为源码成本判断，未进行原生 GUI 性能测量。

### S3：ZIP 候选共享尾部搜索结果，减少平方扫描成本

位置：[scanner/zip.rs:9](/Users/charl/Documents/Projects/SmartZip/crates/smartzip-scanner/src/zip.rs:9)、[file_scan.rs:140](/Users/charl/Documents/Projects/SmartZip/crates/smartzip-scanner/src/file_scan.rs:140)、[policy.rs:38](/Users/charl/Documents/Projects/SmartZip/crates/smartzip-engine/src/policy.rs:38)。

每个疑似 local ZIP 头都会向剩余文件搜索 EOCD；未确定范围的 finding 被接受后只前进一个字节。默认根扫描确实走此路径，并允许全部 findings，故多个无 EOCD 的候选会反复搜索同一尾部。

隔离 public scanner、debug 构建、每 64 KiB 放一个合法形状 local header 且无 EOCD：

| 输入 | 头数量 | 首轮时间 | 取消检查次数 |
| --- | ---: | ---: | ---: |
| 4 MiB | 64 | 320 ms | 3,499 |
| 8 MiB | 128 | 1,080 ms | 11,723 |
| 16 MiB | 256 | 3,956 ms | 41,995 |

复验时间为 312/1073/3960 ms，趋势相同。取消仍有效；这是合成 scanner 成本证据，不是端到端解压性能或 release 速度结论。

建议共享已搜索范围或 EOCD 候选位置，减少重复尾部遍历；保留目录有效性、ZIP64、内层 ZIP 关联和取消检查。不能仅将默认根扫描截断或跳过候选来“优化”，因为读到 EOF 是当前要求。

## 已有未解决问题与文档差异

- **APFS 名称等价碰撞，P1：**当前 SevenZip 批量 `x -y` 路径仍按原名写入暂存，尚无目标卷等价名称计划。既有 macOS 任务在相同源码基线复现了大小写、NFC/NFD 两份成员静默覆盖且报告成功。本轮核对源码未实现修复，但没有重跑 APFS 专项；历史证据见 [macOS 检查记录](/Users/charl/Documents/Projects/SmartZip/.trellis/tasks/09-30-macos-platform-check/implement.md:7)。[路径策略任务](/Users/charl/Documents/Projects/SmartZip/.trellis/tasks/09-22-filesystem-path-policy/prd.md:3) 明确仍为设计完成、尚未实施。
- **旧 p7zip 歧义分卷兼容，P2：**本轮默认 workspace 唯一失败为 `ambiguous_volume_groups_use_first_successful_extraction`。旧后端输出没有匹配当前可回退分类，首个坏候选终止尝试。将测试进程的 `7z` 定向到已有现代 7zz 后，该用例单独通过；没有修改用户后端配置或系统命令。不要通过扩大所有 corrupt/password 错误的回退范围来修复这一特定协议差异。
- `docs/context/01-core-concepts.md` 仍描述 CandidateAttempt 类型，当前 crates 未找到该类型；实际候选编排集中于 `extract_recursive_with_listener_interactive`。
- README 的系统集成段仍称 Windows/Linux 未接入，但当前已存在 Linux 系统集成实现；应将平台支持说明与现有入口、实际验收范围对齐。

## 本轮验证与边界

| 验证 | 本轮结果 |
| --- | --- |
| `cargo test --workspace --locked --no-fail-fast`，默认后端环境 | **626 passed、1 failed、9 ignored**；唯一失败是上述旧 p7zip 用例 |
| 相同失败用例，测试进程 PATH 增加现代 7zz wrapper，`--exact` | **1 passed**；未重跑整个现代优先 workspace |
| `cargo test --locked -p smartzip-gui --test runtime_worker -- --ignored`，现代后端测试环境 | **9 passed**，含显式恢复、草稿接管、密码等待取消、源变化保护、分卷源回收及临时密码不持久化 |
| `cargo test --locked -p smartzip-gui --bin smartzip-gui` | **56 passed**，包含在 workspace 数字中，不额外累加 |
| `cargo test --locked -p smartzip-engine --lib root_management::tests` | **7 passed**，包含在 workspace 数字中 |
| 六个数据/配置/scanner/core crate 的既有 tests | **100 passed**，同样已包含在 workspace 数字中 |
| 隔离 data public API harness | **3 passed**：owner 别名、scanner 成本、repository 混合时间戳；最后一项不算当前产品缺陷 |
| 隔离 GUI 新任务/恢复 harness，真实 7zz | 新任务 completed/有输出；恢复 failed/无输出 |
| 隔离 CLI 三组探针 | 复现 R1/R3/R6，R1 同时提供 R9 证据 |
| 显式真实 7zz 的 `auto` / `AUTO` | 两种配置均有效，但 AUTO 乱码并缺少检测事件 |
| `python3 -m unittest discover -s scripts -p 'test_*.py'` | **12 passed**；安装行为使用既有隔离/模拟用例，没有安装用户应用 |
| `cargo build --locked -p smartzip-cli` | 成功，CLI 探针使用本轮构建 |
| `cargo fmt --all -- --check`、`bash scripts/check_routing_guards.sh`、`git diff --check` | 通过 |
| `scripts/crap-scan.sh --quick --top 15` | 成功；无覆盖率；extract workflow CC=346，仅用于定位热点 |

默认 workspace 数字是 Cargo 各目标的执行计数，含 GUI 集成目标重复编译模块用例，不等于 626 个独立产品场景。九项 ignored 的真实 GUI worker 测试另行运行，不能把默认 workspace 报告改写为全绿。

日志及复现材料在 [target/ponytail-review-2026-09-30](/Users/charl/Documents/Projects/SmartZip/target/ponytail-review-2026-09-30/workspace-tests.log)，属于忽略目录，可在清理 target 前按需保存。GUI recovery harness 命令：`CARGO_TARGET_DIR="$PWD/target" cargo run --offline --manifest-path target/ponytail-review-2026-09-30/probe/Cargo.toml --quiet`。CLI 复现命令：`python3 target/ponytail-review-2026-09-30/cli-probes.py`。

未完成 Windows/Linux 原生运行验收、macOS 原生 GUI 窗口交互、R7/R8 专项动态测试、确认编码恢复专项复现或 GUI 性能基准。间接依赖 `block 0.1.6` 的 future-incompatibility 提示仍存在。本轮未改用户配置，隔离复验未使用用户数据库。

## 不建议因简化删除的边界

router/adapter 能力、单一 extraction staging 所有权、提交意图与回滚、资源准入、epoch/generation、密码脱敏和根取消都有现行职责，应保留。公开兼容 facade 与两套 volumes 分别服务兼容调用、提取候选归组和物理完整性归因，不能仅因接口多就合并。

extract workflow 的高复杂度值得继续拆解稳定行为，但复杂度数字本身不是删分支或新增泛化框架的证据。本轮建议优先复用已有源身份、服务装配、文本转义和 revision 边界，以实际缺陷/运行成本为验收目标。

repository API 混合 SQLite 空格时间戳与 ISO `T` 时间戳可出现排序错误，但当前 durable 执行关闭 legacy_history，未确认 CLI/GUI 同一任务混用两种写入者，故不列为产品缺陷。采样指纹可能漏判、检查点式预算等已承认的限制，也没有包装成新缺陷。

工作区仅新增/更新本审查记录；原有未跟踪 `.trellis/tasks/09-30-macos-platform-check/` 未改动。生产源码仍无 diff。
