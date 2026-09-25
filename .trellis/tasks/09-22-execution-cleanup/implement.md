# 实施与验证

完成于 2026-09-22，源码基线 `24fccc9`，macOS / Rust 1.98.0。用户提供的审查基线与当前源码的差异及本轮范围见 [prd.md](prd.md)。

## 最终修改

- `CompiledRunPolicy::services` 集中选择密码、历史、已知文件服务；调用方仍拥有数据库连接并决定连接访问模式。CLI 的 detect/list/test/extract 与 GUI worker 共用权限判断，历史开关不影响已知文件复用能力。
- `CompiledRunPolicy::extract_request` 生成有效请求；CLI/GUI 仅补充扫描器、临时密码等调用参数，再交给正式 `SmartZipEngine::extract`。该入口不重新覆盖请求。旧 `extract_recursive*` 仍在适配边界应用策略，保留调用签名与行为。
- `ExtractPrompts` 和 `ExtractObserver` 取代正式解压入口的连续可选位置参数。内部工作流直接借用引擎，消除 facade 到 workflow 的多参数转发与无用 re-export。
- `CompiledRunPolicy` 的快照只能经 `resolved()` / `values()` 读取；需要修改后续任务策略时克隆配置并重新 compile。外部源码若直接访问旧公开字段 `.resolved`，需改用 `.resolved()`；旧解压方法未删除。
- 私有 `CandidateResults` 统一记录候选终态的文件历史、结果列表、失败数与失败/输出事件，成功时更新已知文件。分支只提供结果与已知元数据。提交、已用资源统计和回收顺序不变。
- 明确保留旧兼容行为：无法打开输入和 carve 失败会增加任务失败数，但文件历史仍为 skipped；嵌套候选预算耗尽是已提交候选之后的任务级失败，不改写成功候选。
- `stage_plan` 直接构造计划列表；运行时事件发送复用该列表。移除 CLI 已在总入口处理的不可达 dry-run 分支、无用候选 key 计算和错位注释。
- 修正事件保留文档：当前仅前 4096 条 Progress 留在最终快照，其他事件和完整候选结果保留；实时 listener 收到全部事件。没有新增事件截断机制。

## 验证结果

| 检查 | 结果 |
| --- | --- |
| 改动前 `cargo test -p smartzip-engine --lib --quiet` | 196 通过 |
| 改动后 `cargo test -p smartzip-engine --lib --quiet` | 207 通过，新增 11 项参数化行为回归用例 |
| `cargo test -p smartzip-engine --test smartzip_integration --test history_integration --test embedded_integration --test test_workflow --quiet` | 90 通过；包含真实 7-Zip 与 fake adapter 验证 |
| `cargo test -p smartzip-cli -p smartzip-gui --quiet` | CLI 10、GUI 51 通过；5 项真实后端验收默认忽略，随后单独运行 |
| `cargo test -p smartzip-gui --test runtime_worker -- --ignored` | 5 通过；真实 7zz 检测/列表/校验/解压、密码等待取消、原包回收及替换保护、临时密码不落库 |
| `cargo fmt -p smartzip-engine -p smartzip-cli -p smartzip-gui -- --check` | 通过 |
| `git diff --check` | 通过 |
| `cargo clippy -p smartzip-engine -p smartzip-cli -p smartzip-gui --all-targets` | 完成；保留既有告警，诊断主位置不在新增行或新 outcome 模块 |
| 同上加 `-- -D warnings` | 未通过：首先被未修改的 platform needless_return/new_without_default 和 config redundant_closure 阻断 |

累计 363 项测试执行通过。新增用例覆盖：新旧入口混合输入的相同结果与事件顺序、每个任务仅一个 Started/Finished、每候选仅一条历史、六种状态权限组合、各任务独立交互策略、密码失败与主动放弃提示的不同终态，以及失败暂存清理和密码不进入事件。

## 限制与交接

- 没有持久恢复、多根准入调度代码可供本轮重构或验收；不声称实现了附带审查中针对另一基线的这些建议。
- 没有运行性能基准，不宣称速度或峰值内存改善。没有人工打开 GUI 窗口做视觉验收，已验证实际 worker 和后端流程。
- 本轮范围内没有新增 Clippy 告警；仓库其他既有告警及依赖 `block 0.1.6` 的未来兼容提示仍在。
- 保留用户原有 AGENTS.md、CONTEXT.md、.trellis/README.md 和 docs/context 拆分；只补充与本轮相关的事件/入口约定。文档拆分与本轮代码分别提交，避免重复维护同一份领域说明。

## 集成基线

原验证基线为 `24fccc9`。本地 `main` 已到 `e01e8fa`，后续合并需要单独验证；上述测试结果仅证明原基线上的行为。

## 与当前主线合并

`main@e01e8fa` 已在独立工作树重新实现同类清理，并新增持久恢复、根准入和执行控制。合并时保留主线的 `PreparedExtractTask`、`RunServices`、`ExtractInteraction` 和执行流程；旧基线的 `CandidateResults` 及相应调用改写未移植，以免覆盖当前主线的提交与恢复边界。纳入本分支的领域文档拆分和历史任务记录，并以主线最新术语更新文档。上述旧基线测试结果不作为合并后验证。

合并后运行 `cargo test --locked -p smartzip-engine --lib --quiet`：254 通过；`cargo test --locked -p smartzip-cli -p smartzip-gui --quiet`：70 通过、7 项默认忽略。没有重跑整仓集成测试或 GUI 手工交互。
