# 代码、数据流与依赖精简

## 目标与边界

梳理整个 Rust 工作区，减少重复实现、重复状态和无用依赖特性，改善维护与编译成本。保留当前产品功能、密码证据规则、分卷能力、取消、资源预算、隔离输出及提交回滚；不升级依赖或重做 UI。

基线为 `40732f9`，开始时 main 干净、已与 origin/main 同步。

## 调查与决策

| 范围 / 候选 | 证据与处理 |
| --- | --- |
| engine 后端泛型贯穿异步工作流 | `ArchiveExecutor` 已是 object-safe async-trait；生产主要传 BackendRouter，测试有多种实现。18 个入口/工作流统一用 `&dyn ArchiveExecutor`，避免为每种后端重新生成整条工作流；保留注入边界。 |
| CLI 服务拆装 | bootstrap 的三个包装拆开 `policy.services()`，各命令再手动拼 RunStores。删除包装，detect/list/test/enc 直接使用现有 RunServices；enc 仍关闭历史与 known 写入。 |
| 编码评估重复事实 | 私有 ZipEncodingAssessment 同存原始检测、core 检测、确认上下文及可推导布尔值。直接使用 EncodingConfirmationContext，确认条件由 suspicious_reasons 判断；预览、事件、inspect 与 extract 共享同一结果。 |
| 编码准备的伪异步层 | assess_zip_encoding 与 prepare_resolved_archive 只有同步操作，移除无实际挂起点的 async/await。 |
| list 密码成功分支重复收尾 | 候选密码与交互密码成功后执行相同编码补取条件。合并到成功后的一处，保留准备阶段失败后再次读取的时机。 |
| list 密码证据的空操作 | record_listing_access 恒返回 None、私有 outcome 密码字段恒为 None/false。删除空方法及常量状态传递；公共结果和历史仍明确保持未验证密码语义。 |
| NativeZip 的旧 adapter 状态 | 现在只是原始名字/加密标志 helper，没有调用 with_id/id。改为无状态类型，删除 String 分配、身份重映射及重复错误映射；保留 raw entry 元数据与错误类别、路径。 |
| 固定字段 UPDATE 动态构造 | TaskRepository::finish 为三个字段分配 SQL 片段、Box<dyn ToSql>、字符串和参数 Vec。改为参数化固定 UPDATE/COALESCE；全 None 仍直接返回。扩展现有测试验证只更新 output_path 和空更新。 |
| 重复密码错误映射与路径 clone 包装 | extract 复用已有 load_password_candidates；删除只克隆 relative_path 的函数，各调用方借用字段。 |
| ZIP Deflate 特性 | zip 8.6.0 的 deflate 同时启用 zopfli 和 flate2/zlib-rs。改用 deflate-flate2-zlib-rs；项目不创建产品归档，也没有使用 Zopfli 的测试需求。保留 Deflate 读写与其余实际使用的解码器。 |
| ZIP time 特性 | 没有使用 ZIP/time 转换 API，移除特性；其他依赖仍使用 time，不声称移除了整个 time 包。 |
| engine 的直接 zip 依赖 | **保留**：container.rs 的 classify_zip_path 是生产使用，不能因大部分 zip 引用在测试中就移到 dev-dependencies。生产构建已验证。 |
| Binwalk 与 scanner 自定义解析 | Binwalk 覆盖多种格式签名/解析，没有可裁剪 Cargo 特性；scanner 的 ZIP/LZMA/RAR 自定义代码补足行为。替换 Binwalk 需重写解析与全面格式验收，本轮保留。 |
| legacy / context-aware 双后端接口 | **保留**：SevenZip legacy probe 与 context probe 的诊断协议不同，adapter_compatibility 有明确断言；不是可机械删除的同义入口。 |
| Auto ZIP 的第二次 list | 首次 Auto、检测确认后 Override 导致重新 list。预先改用检测编码会改变首轮路由的能力要求和交互顺序，本轮不改。 |
| decoded_zip 重复打开归档 | 可进一步共用打开句柄，但需要重划“不是 ZIP / 不支持 / 已损坏”的错误边界，本轮不移动该边界。 |
| CLI 配置反写 Clap 参数 | SafetyOptions 与 CompiledRunPolicy 有重复状态；彻底分离解析层/执行上下文会影响参数优先级、`--set` 与终端交互。服务装配已先复用现有接口，本轮不重做参数模型。 |
| GUI 恢复准备与 CLI 不同 | GUI 可复用 PreparedExtractTask，但该入口会额外装配 history/known 服务；直接替换会改变恢复任务的复用行为。保留先完成可失败预读、再消耗 recovery claim 的既有顺序。 |
| CLI/GUI 密码清理重复 | 两处有共同业务，但极大 stale_days 的错误处理不同；直接迁移还涉及 chrono 的归属。chrono 仍由 Binwalk/trash/GPUI 间接引入，不能以删一条声明宣称消除编译。 |
| GUI/config/platform 依赖与状态 | GPUI component/assets、url、image、plist、tempfile 均有实际使用；QuickOverrides 表达继承/显式覆盖，数据库浏览与 worker 的读写语义不同。配置编辑保留注释、权限、并发检查与原子发布，未强行合并。 |
| 数据库文件 hash 与分卷 fingerprint | 取样位置与身份用途不同，不能合并算法改变已有历史识别。rusqlite bundled 与迁移保留。 |

## 验证结果

- 第一轮 `cargo test --locked -p smartzip-engine -p smartzip-archive -p smartzip-cli -p smartzip-db -p smartzip-passwords -p smartzip-scanner --no-fail-fast`：549 通过，2 项既有忽略项（子进程 helper、需 NFS 挂载）。
- 随后移除同步准备函数的 async 和私有密码常量字段，最终 `cargo test --locked -p smartzip-engine`：354 通过，1 项 NFS 忽略。
- `cargo check --locked -p smartzip-gui --all-targets` 通过；最终 GUI worker 测试也完成编译。
- `cargo test --locked -p smartzip-gui runtime::worker_tests:: -- --include-ignored`：7 通过，2 项回收源文件测试因 Nix 默认 `/tmp` 所在挂载的 `/.Trash-1000` 无写权限失败。仅对这两项设置工作区内 TMPDIR 与隔离 XDG_DATA_HOME 后重跑：2 通过。9 项真实后端 worker 行为全部得到通过结果，未操作用户回收站。
- `cargo build --locked -p smartzip-cli` 通过；`scripts/verify_review_fixes.py target/nix/debug/smartzip`：5 项真实后端验收通过，覆盖历史复用、密码证据、编码预览及 PTY 密码重试/取消。
- `cargo fmt --all -- --check`、`scripts/check_routing_guards.sh`、`git diff --check` 通过。
- 本机 Linux，外部后端来自 Nix 开发环境。未做 macOS/Windows 执行验收或原生 GUI 人工视觉验收。

## 依赖与编译证据

Linux CLI 的 `cargo tree -p smartzip-cli --prefix none --format '{p}'` 去除重复标记并去重后，从 221 个包降至 219 个：不再编译 Zopfli 与该链上的 bumpalo。Cargo.lock 删除 Zopfli；bumpalo 仍可能出现在其他平台的锁定依赖中。

编译测量使用同一临时源码路径，基线来自 git archive，之后仅覆盖本轮修改文件。禁用 sccache 与 incremental；每个阶段预热前清理全部 11 个工作区包，避免共享 target 复用另一个源码快照的本地依赖。外部依赖保留预热产物，每个计时样本再清理对应的 engine/CLI/GUI 包。每组排除 1 次预热，保留 2 个原始样本：

| 构建范围 | 修改前（秒） | 修改后（秒） | 中位数变化 |
| --- | --- | --- | --- |
| `cargo build -p smartzip-cli -p smartzip-gui --locked` | 6.943 / 7.188 | 7.185 / 7.453 | 7.066 → 7.319，约 +3.6% |
| `cargo test -p smartzip-engine --no-run --locked` | 5.070 / 4.939 | 4.826 / 4.524 | 5.005 → 4.675，约 -6.6% |

测试构建小幅下降，桌面开发构建小幅上升；当前没有取得生产开发构建提速。已确认的收益是依赖减少、重复状态消除和工作流代码生成集中。两样本比较只覆盖上述预热依赖的构建范围，未测全工作区冷构建或运行时吞吐。

原始样本与工具链见本目录 `build-desktop-measurements.json`、`build-engine-tests-measurements.json`；测量脚本和完整构建/测试日志保留于 `.work/simplification/`。

Rust 源码净减少 135 行（包含扩展的现有测试；不含锁文件和本任务记录）。
