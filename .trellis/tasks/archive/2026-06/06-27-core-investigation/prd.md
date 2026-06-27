# Core Extraction Investigation

## Goal

围绕当前已确认的 SmartZip 核心问题，按既定优先级开展实际排查，结合代码、测试、样本和运行行为，形成可执行的根因判断与修复输入。

## Confirmed Facts

- 排查顺序已经确认，不再需要重新做优先级讨论。
- 当前最优先的是恢复“核心解压能力可用 + CLI 可用”。
- 现有规划任务已经完成：
  - 需求基线迁移
  - 问题清单整理
  - 修复顺序与阶段计划整理
- 这一步不是纯规划，而是正式进入调查阶段，必须读取代码并结合测试、fixture 和真实命令行为。
- 调查输出不是终点；其结论必须直接回灌到 `06-27-fix-plan`，用于冻结后续修复顺序。
- 当前关于 ZIP 路径的新结论已经确认：
  - 现有原生 ZIP 库不足以覆盖当前需要的复杂 ZIP 场景。
  - 近期先放弃“Native ZIP 作为复杂 ZIP 主解压后端”的目标。
  - ZIP 解压主路径暂时切到 `7z` / `7zz`。
  - 自动编码检测延后，先提供“多编码文件名预览”辅助入口。

## Investigation Scope

- `P0` 核心主流程问题
  - 分卷压缩包解压失败
  - 带密码压缩包正确密码仍失败或流程不稳定
  - 批量解压与嵌套解压流程不稳
  - 智能解压目标路径、覆盖、删除行为与设计不符
- `P1` 契约与边界问题
  - CLI 多输入交互和退出码契约不清晰
  - ZIP 编码处理当前不可用或边界不清
  - 原生 ZIP 后端与 7z fallback 的职责失配
  - 外部依赖与环境假设不一致

## Requirements

- 排查必须按已确认顺序推进：
  1. 分卷、密码、批量/嵌套
  2. 目标路径、覆盖、删除
  3. CLI 交互、错误提示、退出码
  4. ZIP 编码、原生 ZIP 后端、fallback、环境依赖
- 每类问题都必须输出：
  - 复现条件
  - 涉及代码路径
  - 当前实际行为
  - 预期行为
  - 初步根因判断
  - 建议修复方向
- 调查结果必须明确标出：
  - 哪些问题足以改变既有修复阶段顺序
  - 哪些问题只影响某一阶段内部的实施顺序
- 排查过程中要明确区分：
  - 已稳定复现的问题
  - 尚未复现但高风险的问题
  - 文档目标成立但实现偏离的问题
  - 文档本身边界不清的问题
- 排查输出必须能直接供后续修复任务引用，而不是停留在口头判断。

## Constraints

- 不先扩散到 GUI、打包、系统集成、压缩、预览等后置范围。
- 不把编码问题泛化成“所有格式统一编码检测”，当前重点只看 ZIP 路径。
- 不把“命令存在”当成“能力可用”的证据。

## Acceptance Criteria

- [ ] 至少完成 `P0` 问题的逐项排查入口，包含复现条件、代码路径和初步根因。
- [ ] 明确分卷、密码、批量/嵌套、目标路径四类问题的排查结果或当前阻塞点。
- [ ] 明确 CLI 契约与退出码属于实现缺陷、设计缺陷还是文档边界缺陷。
- [ ] 明确 ZIP 编码与原生 ZIP/fallback 的当前职责边界和失效场景。
- [ ] 形成一份可直接转入修复实施的调查结论，而不是只保留过程记录。
- [ ] 调查结论已明确回灌到 `06-27-fix-plan` 所需的排序依据和修复建议。

## Out Of Scope

- GUI 工作台
- `compress`
- `open` / 预览
- 系统集成
- 打包发布

## Investigation Findings

### Finding 1: 错误密码在 CLI 中会静默退化为 `skipped`

- 复现条件
  - `cargo run -p smartzip-cli -- --db /tmp/smartzip-empty.db extract tests/fixtures/pass_cn.zip --password wrong-password --output /tmp/smartzip-cli-wrong-emptydb`
- 涉及代码路径
  - `crates/smartzip-cli/src/main.rs`
  - `crates/smartzip-engine/src/lib.rs`
- 当前实际行为
  - CLI 只输出 `skipped 1 candidate(s)`，没有任何“密码错误”级别的失败说明。
  - 退出码为 `1`，但用户无法区分是密码错误、后端失败还是普通跳过。
- 预期行为
  - 错误密码应形成明确的失败结果，并和普通跳过分开。
- 初步根因判断
  - `crates/smartzip-engine/src/lib.rs` 中，密码循环遇到 `WrongPassword` 时只调用 `record_failure(password)`，不会把错误写入 `last_error`。
  - 后续只有 `last_error` 存在时才发出 `TaskEvent::failed`，因此密码错误被吞掉。
- 建议修复方向
  - 为“所有候选均因 WrongPassword 失败”的场景发出专门失败事件。
  - CLI 输出层区分“密码错误”“碰撞跳过”“非首卷跳过”“无格式跳过”。
- 排序影响
  - 该问题直接影响“核心解压是否成功”的可见性，建议前移到 Phase 1。

### Finding 2: 数据库候选密码会掩盖用户手工输错密码

- 复现条件
  1. `cargo run -p smartzip-cli -- --db /tmp/smartzip-cli.db extract tests/fixtures/pass_cn.zip --password 中文密码123 --output /tmp/smartzip-cli-probe-ok`
  2. `cargo run -p smartzip-cli -- --db /tmp/smartzip-cli.db extract tests/fixtures/pass_cn.zip --password wrong-password --output /tmp/smartzip-cli-wrong-one`
- 涉及代码路径
  - `crates/smartzip-passwords/src/lib.rs`
  - `crates/smartzip-db/src/password.rs`
  - `crates/smartzip-engine/src/lib.rs`
- 当前实际行为
  - 第二次命令虽然传入错误手工密码，仍会成功解压并输出 `文档.txt`。
- 预期行为
  - CLI 需要明确“手工密码是唯一密码”还是“只是优先候选，数据库密码仍会继续尝试”。
- 初步根因判断
  - `PasswordService::ranked_candidates` 会把 empty/manual/clipboard 候选后再追加数据库候选。
  - engine 会把所有候选顺序跑完直到成功，因此错误手工密码会被数据库中的正确密码掩盖。
- 建议修复方向
  - 明确 CLI 密码契约。
  - 如果保留当前行为，至少输出命中的候选来源。
  - 如果希望手工密码具备覆盖语义，则应新增显式模式或修改默认策略。
- 排序影响
  - 更偏 Phase 2 的 CLI/密码策略问题，但必须写入修复计划，否则会持续制造误判。

### Finding 3: 多层嵌套解压在真实 CLI 路径上会发生输出目标冲突

- 复现条件
  - `cargo run -p smartzip-cli -- --db /tmp/smartzip-cli.db extract tests/fixtures/nested_multi_level.zip --output /tmp/smartzip-cli-nested-one`
- 涉及代码路径
  - `crates/smartzip-engine/src/lib.rs`
  - `crates/smartzip-engine/src/layout.rs`
  - `crates/smartzip-engine/src/materialize.rs`
- 当前实际行为
  - `nested_multi_level.zip` 提取后，`L2.zip` 被布局到 `.../nested_multi_level/L3.zip`。
  - 继续处理 depth 2 的 `L3.zip` 时触发 `FAILED: I/O error ... File exists (os error 17)`。
  - 最终 `processed 2 archive(s)`、`skipped 1 candidate(s)`、退出码 `2`。
- 预期行为
  - 多层嵌套 zip 应能继续递归到最深层，不应把内层归档文件名和输出目标撞在一起。
- 初步根因判断
  - `discover_nested_candidates` 用 `archive_stem(path)` 生成嵌套候选的相对输出名。
  - `materialize` 对单文件根又可能直接采用内层文件名作为输出目标，例如 `L3.zip`。
  - 结果是“待提取的归档文件路径”和“提交目标路径”进入同一命名空间，造成冲突。
- 建议修复方向
  - 为嵌套归档文件与其解压输出目录建立稳定隔离规则。
  - 补真实递归布局回归测试，而不是只看 `processed/skipped` 计数。
- 排序影响
  - 这是直接影响嵌套主流程成功率的 P0 问题，应保留在 Phase 1。

### Finding 4: 默认数据库行为与注释/环境假设不一致

- 复现条件
  - `cargo run -p smartzip-cli -- extract tests/fixtures/pass_cn.zip --password wrong-password --output /tmp/smartzip-cli-probe`
- 涉及代码路径
  - `crates/smartzip-cli/src/main.rs`
  - `crates/smartzip-platform/src/lib.rs`
- 当前实际行为
  - 不传 `--db` 时，CLI 会创建平台数据目录并打开磁盘数据库。
  - 在当前受限环境中直接失败为 `Read-only file system (os error 30)`。
- 预期行为
  - 注释、帮助文本和实际行为应一致。
  - 如果默认走磁盘库，环境失败时也应给出明确指引或 fallback 策略。
- 初步根因判断
  - `Cli` 注释写的是“Defaults to in-memory if not set”，但 `open_db(None)` 实际总是创建平台路径并打开磁盘库。
- 建议修复方向
  - 统一帮助文案与实现。
  - 评估数据库路径不可写时是否回退到内存库。
- 排序影响
  - 属于 Phase 2 的环境/CLI 契约问题。

### Finding 5: 分卷仍是高风险项，但当前缺少真实样本和端到端回归

- 复现条件
  - 当前仓库没有 multipart fixture 可直接做真实提取复现。
- 涉及代码路径
  - `crates/smartzip-engine/src/lib.rs`
  - `crates/smartzip-engine/tests/smartzip_integration.rs`
- 当前实际行为
  - 仓库只验证了 `is_first_volume(...)` 命名规则，以及“非首卷候选会被跳过”。
  - 没有覆盖真实 `.part1.rar` / `.001` / 缺卷 / 首卷提取 的端到端回归。
- 预期行为
  - 至少应有真实首卷样本、缺卷样本和对应 CLI/engine 回归。
- 初步根因判断
  - 当前分卷能力更多停留在候选筛选层，尚不足以证明真实提取流程可用。
- 建议修复方向
  - 把“补 multipart fixture + 端到端测试”列为 Phase 0 先决条件。
  - 在没有样本前，不应把分卷能力视为当前已确认可用。
- 排序影响
  - 不改变“分卷属于 Phase 1”的结论，但会把“先补样本和测试”提升为进入修复前的阻塞前置条件。

### Finding 9: Native ZIP 后端当前不适合作为复杂 ZIP 主路径

- 复现条件
  - 使用带密码的分卷 ZIP 主文件 `*.zip`，旁边存在 `*.z01` / `*.z02`。
  - 现有原生 ZIP 路径无法稳定处理这类样本；而 `7z` 能识别 multivolume ZIP 并进入密码流程。
- 涉及代码路径
  - `crates/smartzip-archive/src/native_zip.rs`
  - `crates/smartzip-archive/src/router.rs`
  - `crates/smartzip-cli/src/main.rs`
- 当前实际行为
  - 原生 ZIP 路径无法满足分卷、密码、编码和行为一致性这组组合要求。
  - 如果继续强行保留 “ZIP 优先 Native” 的路由，主流程可用性会持续受损。
- 预期行为
  - 当前阶段优先恢复 ZIP 解压可靠性，而不是继续维持不成立的架构承诺。
- 初步根因判断
  - 现有原生 ZIP 方案的能力边界比历史设计假设更窄。
  - 这不是单点 bug，而是“实现能力与文档承诺失配”。
- 建议修复方向
  - 短期：ZIP 的 `probe/list/test/extract` 切到 `7z` / `7zz` 主路径。
  - 中期：自动编码检测暂时降级，增加“多编码文件名预览”命令辅助用户选编码。
  - 长期：重新评估是否还有值得投入的 Native ZIP 方案，而不是沿用当前实现继续补洞。
- 排序影响
  - 该结论会直接改写 Phase 3 的后端边界定义，但不改变“Phase 1 先修核心可用性”的大顺序。

### Finding 6: `smartzip extract --json` 参数当前无效

- 复现条件
  - `cargo run -p smartzip-cli -- --db /tmp/smartzip-cli.db extract tests/fixtures/enc_utf8.zip --output /tmp/smartzip-json-probe --json`
- 涉及代码路径
  - `crates/smartzip-cli/src/main.rs`
- 当前实际行为
  - 命令接受 `--json` 参数，但输出仍是普通文本进度和 summary，不会产生 JSON 结果。
- 预期行为
  - 如果公开暴露了 `--json`，应有明确 JSON 输出契约；否则不应暴露该参数。
- 初步根因判断
  - `run()` 中 `Command::Extract` 分支把 `json` 解构为 `json: _json`，随后完全丢弃，没有传入 `extract(...)`。
- 建议修复方向
  - 二选一：
    - 实现 `extract --json` 输出
    - 或删除/隐藏该参数，避免制造伪能力
- 排序影响
  - 属于 Phase 2 的 CLI 契约问题。

### Finding 7: `password export` 默认输出路径绕过平台路径约定

- 复现条件
  - `cargo run -p smartzip-cli -- --db /tmp/smartzip-cli.db password export`
- 涉及代码路径
  - `crates/smartzip-cli/src/main.rs`
  - `crates/smartzip-platform/src/lib.rs`
- 当前实际行为
  - 不传 `--path` 时，CLI 会导出到当前工作目录下的 `smartzip-passwords.txt`。
- 预期行为
  - 若项目已经提供 `PlatformPaths::password_export_path()`，默认导出路径应与该平台约定一致，或至少在帮助文案中说明为何不用它。
- 初步根因判断
  - `PasswordCmd::Export` 分支直接使用 `PathBuf::from("smartzip-passwords.txt")`，没有接入 `PlatformPaths`。
- 建议修复方向
  - 统一导出默认路径与平台层约定，或显式确认“当前目录导出”才是预期。
- 排序影响
  - 属于 Phase 2 的 CLI/平台契约问题。

### Finding 8: 高 CRAP 项的主要风险来自“浅断言 + CLI 无测试”，不是纯复杂度噪声

- 复现条件
  - 运行 `./scripts/crap-scan.sh --top 5`
- 涉及代码路径
  - `crates/smartzip-engine/src/materialize.rs`
  - `crates/smartzip-engine/src/lib.rs`
  - `crates/smartzip-engine/tests/smartzip_integration.rs`
  - `crates/smartzip-cli/src/main.rs`
- 当前实际行为
  - `smartzip-engine` 的高风险函数虽然已有部分集成测试，但断言主要停留在“有处理成功/失败计数”，没有覆盖最关键的失败契约。
  - `test_engine_wrong_manual_password_is_not_saved_to_database` 只断言“未保存错误密码 + 进入 skipped”，没有断言应产生 `Failed` 事件，因此静默吞错可以长期存在。
  - `test_engine_respects_recursion_limit` 只验证 depth 限制，不验证 `nested_multi_level.zip` 在允许继续递归时的最终落盘路径，因此真实 `File exists` 冲突未被测试挡住。
  - `smartzip-cli` 只有 `src/main.rs`，没有任何独立测试文件；`extract`、`open_db`、`password` 这些 CRAP 最高函数当前基本处于 0% 覆盖状态。
- 预期行为
  - Phase 1 修复前，至少应先补：
    - 错误密码必须产生明确失败事件/输出的回归测试
    - 多层嵌套归档成功递归到最深层且路径不冲突的回归测试
    - CLI 的 `--json`、默认 DB、`password export` 默认路径等契约测试
- 初步根因判断
  - 当前测试分布偏向 backend happy path 和基础 fixture 可用性，未覆盖 CLI 契约层，也没有对高分支复杂度函数的错误事件进行细粒度断言。
  - 因此 `cargo-crap` 在这里指出的是“真实高风险缺口”，而不是单纯对复杂实现的噪声报警。
- 建议修复方向
  - 把“为高 CRAP 项补最小回归护栏”纳入 Phase 0/1 前置条件。
  - 后续是否把 `crap-scan` 接入工作流，应以“先有稳定、低噪声的最小回归集”为前提，否则只会持续重复暴露已知空洞。
- 排序影响
  - 不改变业务缺陷本身的优先级，但改变实施顺序：部分最小测试护栏应先于或伴随 Phase 1 修复提交。
