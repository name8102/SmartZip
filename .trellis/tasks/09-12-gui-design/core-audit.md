# GUI 核心能力审计与不退化矩阵

审计日期：2026-09-12。依据当前源码静态审查；没有把历史文档、类型声明或 UI 样例当作端到端验收证据。本次不修改运行代码、不运行后端验收。

## 结论与边界

- 当前 `crates/smartzip-gui/src/main.rs` 仅接收 `ExternalPaths` 并调用同步 `SmartZipEngine::detect` 展示内嵌发现；没有解压、配置、持久化、任务调度、密码、预览。其 `cx.spawn` 内同步扫描不能作为已移出 UI 执行线程的证明；实现时需明确工作线程边界。
- 核心真实入口是 `crates/smartzip-engine/src/lib.rs`：`inspect_file_with_listener`、`list_archive_with_listener_interactive`、`test_archives`、`extract_recursive_with_listener_interactive`，加 `with_run_policy`、`with_cancellation_token`。GUI 应复用这些入口与 `BackendRouter`，不能直接调用单一后端替代完整解压工作流。
- 多输入及递归是 `extract_workflow.rs` 中的 `VecDeque` 顺序处理；尚无应用级多任务优先级、最大并发、排队持久化、运行暂停、断点恢复。须实现共享调度服务；暂停队列仅阻止启动后续任务。
- `TaskProgress` 只有 `percent: Option<f32>` 与 `message`。`sevenzz.rs` 的上下文解压转发后端百分比，扫描阶段可不定量；不存在通用字节计数、速度、ETA、递归全局总量。当前阶段百分比不能冒充整个任务百分比。
- 预览已能列目录与条目大小，不能直接读取条目内容、缩略图或选择性解压。`ArchiveEntry` 没有修改时间、CRC 等字段，UI 不应虚构元数据。
- 底层已有 `ArchiveExecutor::compress` 与路由/7z 实现，但没有 CLI 命令和 engine 压缩工作流；7z 当前 `compress` 没有使用 request 的 `level`、`format` 参数。应在设计能力账本中保留“创建压缩包”入口/补齐项，不能宣称完整压缩能力已接通。

## 不退化覆盖矩阵

“快速”指小窗口；“详细”指大窗口。高级能力可从小窗口进入详细页，无需挤入同一个表单。

| 能力 | 当前源码与接口证据 | GUI 覆盖与验收要求 |
| --- | --- | --- |
| 拖入、多路径解压 | `gui/main.rs::on_drop`；`engine/types.rs::ExtractWorkflowRequest.inputs` | 快速拖放/选择文件即按可见配置创建任务；详细支持批量；分卷组不得因多个成员被拖入而重复创建操作 |
| 完整解压编排 | `engine/lib.rs::extract_recursive_with_listener_interactive`；`extract_workflow.rs` | 复用密码、编码、分卷、嵌套、输出布局、预算、历史全部路径；不能只调用 `ArchiveExecutor::extract` |
| 输出与目录布局 | `engine/layout.rs`；`config/model.rs::Output` | 输出首输入父目录/指定目录；conservative、smart、raw、flat-single；根名 auto、archive、inner、preserve-both；默认 conservative |
| 冲突处理 | `engine/interactive.rs::InteractiveOutputPrompter` | 询问、跳过、覆盖、重命名；显示冲突实际目标；运行期间交互属于对应任务，不阻塞无关任务 |
| 递归与嵌套清理 | `config/model.rs::Recursion/CleanupPolicy`；`extract_workflow.rs` | 开关、最大深度（默认 3）；嵌套归档保留/回收站/删除；原始输入归档保留；明确永久删除语义 |
| 内嵌包/SFX | `core/embedded.rs`；`engine/embedded.rs`、`extract_workflow.rs` | CLI auto/ask/largest/aggressive/all/ignore 保持等价；配置区分别编辑 root 与 nested 支持的枚举；主导比例；展示偏移、大小、格式、置信度；支持提取/跳过/余下全部提取 |
| 大文件扫描确认（既有缺口） | `TaskEventKind::LargeEmbeddedScanConfirmationRequired`；`ExtractWorkflowRequest.confirm_large_scan` | CLI 将 bool 传入 request 后，生产代码没有读取；事件仅有类型声明、历史序列化与 CLI 打印，没有实际发出点。因此不能靠设置 bool 宣称已执行确认。需补扫描前门控和可等待交互入口，并用真扫描调用顺序验证 |
| 业务容器保护 | `engine/container.rs`、`policy.rs`；`BusinessContainerSkipped` | 对文档/应用等容器的保护和跳过原因保持可见；不为“自动解压”绕过领域策略 |
| 分卷 | `engine/volumes.rs` 及子模块；`archive/volumes.rs`、`volume_probe.rs` | 自动发现开关、分卷分组、缺卷/损坏卷列表；显示实际输入与规范首卷；沿用匹配/物化规则 |
| 文件识别 | `InspectRequest`、`FileAwareDetectResult` | 格式、加密（含未知）、编码置信度、内嵌发现、需要密码、已知密码/编码提示、状态/原因；详细“检测”动作 |
| 压缩包目录预览 | `ListArchiveRequest`、`ListArchiveResult`；`archive/types.rs::ArchiveEntry` | 目录树/表格、名称、目录标识、可选压缩大小/展开大小；加密包密码输入；未知大小显示“—”；不把列表结果当作内容读取 |
| 编码自动与手选 | `EncodingMode`；`InteractiveEncodingPrompter`；CLI `enc` 与 `--pick-encoding` | auto/backend 和 UTF-8、GB18030、GBK、Big5、Shift_JIS、EUC-JP、EUC-KR；候选名称对比；可疑编码接受/覆盖/跳过；保留已确认编码复用 |
| 密码候选决策 | `passwords/lib.rs::PasswordService`；`engine/password_order.rs`；`config/model.rs::Passwords` | 多个手动密码、空密码尝试开关、数据库候选上限（默认 128）；auto/manual/off；来源顺序 manual/known/batch/empty/database；成功保存与统计开关；候选耗尽后手动输入/跳过 |
| 密码库管理 | `db/password.rs::PasswordRepository`；CLI `password` | 排名、成功/失败计数与时间、来源、置顶；添加、删除、按行导入、导出；清理保留上限/陈旧天数、先预览再应用；清理实际禁用而非永久删除，置顶受到保护 |
| 密码隐私边界 | `core/progress.rs::PasswordTried`；`db/schema.rs` | 事件只携带 candidate_id，GUI 默认掩码；密码不得进入任务标题、日志、事件和普通导出。数据库当前是明文 TEXT，不可把掩码 UI 称为加密保险库 |
| 完整性测试 | `engine/test_workflow.rs::TestWorkflowRequest/Result`；`archive/integrity.rs` | 多归档/分卷组测试、密码/编码；诊断 auto/off；额外诊断时间预算（不是主测试超时）；报告损坏卷、原因、阶段/轮次，展示混合结果 |
| 进度事件 | `core/progress.rs::TaskEventKind`；`engine/events.rs`；`archive/sevenzz.rs` | 定量/不定量双态；显示阶段和当前条目；任务取消/等待时不继续假进度；事件中包含路由、策略原因、编码、内嵌、输出、警告、终态 |
| 取消与结果 | `SmartZipEngine::with_cancellation_token`；`TaskCompletionStatus`；7z/unrar 进程组取消 | 每任务独立 token；取消中直到清理结束；completed/partial/failed/cancelled 区分；跳过不是失败；部分结果保留显示，取消不是暂停 |
| 安全与资源预算 | `engine/budget.rs`、`materialize.rs`；`archive/safety.rs` | 文件数、总输出字节、最小剩余空间、嵌套候选数；不绕过路径检查、staging、提交回滚；清理失败保留路径和警告；UI 调度新增全局预算不能取代现有每工作流预算 |
| 已处理复用 | `engine/history.rs::KnownFileHit`；`config/model.rs::Reuse`；`config/resolve.rs::explanation` | 密码提示与编码提示可复用。skip_completed 默认 false，设 true 也被明确 suppressed（旧缓存缺少输出及策略完成证据）；当前总是按本次输出碰撞策略处理。force 参数仍可传入，但生产代码不读取 request.force，没有额外执行效果；不能因 CLI/源码旧注释宣称去重或 force 已工作 |
| 历史任务 | `db/task.rs`；CLI `history tasks` | 最近任务及条数；操作类型、状态、输出、起止时间；状态关闭时说明不可用而非伪空列表 |
| 历史逐文件 | `db/file_extractions.rs`；CLI `history files` | 状态/原因筛选；输入输出、偏移、密码 ID、编码、损坏卷、测试报告；保留提取/跳过/失败细粒度结果 |
| 历史详情 | `db/task_event.rs`；CLI `history show` | 按 TaskId 查看事件时间线及该任务全部文件动作；读取失败/未知报告 schema 显示明确状态 |
| 状态控制 | `config/model.rs::State`；`engine/run_policy.rs` | 数据库路径、off/read-only/read-write、history、known_files；no-history 不等同 stateless，前者仍可更新密码统计；只读禁止写管理操作 |
| 错误策略 | `config/model.rs::OnError`；`extract_workflow.rs` | 继续/停止与终态原因可见；单任务 on_error=stop 不暗中变成取消所有任务 |
| 配置管理 | `config/resolve.rs::ResolvedConfig`、`store.rs`、CLI `ConfigCmd` | 路径、初始化简版/完整版、默认/文件/有效值及来源、get/set/unset/check、迁移预览/应用；复用验证/原子编辑，不另设与 TOML 漂移的默认值 |
| 本次覆盖与解释 | CLI `--config/--no-config/--set/--no-recursive/--explain`；`CompiledRunPolicy` | “仅本次”与“保存默认”分开；显示最终值、来源和 inactive 解释；运行任务使用提交时配置快照 |
| Dry run | CLI `run` 中先于归档/DB访问处理 `--dry-run` | 展示配置与阶段计划，dynamic_content=unknown_until_run；不可宣称已遍历归档、输出树完全准确；CLI 后部旧路径预览分支已被提前返回遮蔽 |
| 后端诊断与路由 | CLI `Doctor`；`archive/router.rs`；`config/lib.rs::BackendConfig` | doctor 版本/可用性/能力/数据库位置；自动发现、安装 ID/路径/启用/优先级、指定 backend；详细路由回退与失败原因 |
| 结构化输出等价 | CLI detect/list/test/extract/enc/doctor/history/password 的 `--json` | 对应页面支持复制/导出相同语义的结构化结果；密码导出独立且明确；GUI 不需复制 CLI 别名和进程退出码控件，但必须保留 0/1/2/130 对应结果区别 |
| 底层压缩 | `archive/backend.rs`、`types.rs::CompressArchiveRequest`、`router.rs::compress_with_context`、`sevenzz.rs` | 设计详细“创建压缩包”流程并标明需补齐工作流/格式级别映射/提交清理/历史；不得把已有底层接口遗忘，也不得声称 GUI 已具备安全完整实现 |

## CLI 清单核对

完整命令族来自 `crates/smartzip-cli/src/main.rs` 的 `Command`、`ConfigCmd`、`PasswordCmd`、`HistoryCmd`：

- `config path/init/show/get/set/unset/check/migrate`（含 full、defaults/effective/sources、apply/dry-run）。
- `doctor`；`detect/d`；`list/l`；`test/t`；`extract/x`；`enc/encoding-preview`。
- `password/pw list/add/remove/import/export/cleanup`。
- `history/hist` 默认 recent tasks，子命令 `tasks/files/show`。
- 运行共同选项：db、config/no-config、stateless、set、no-recursive、explain、backend、verbose-routing、password-limit、max-files、max-output-bytes、min-free-bytes、max-nested-candidates、non-interactive、on-conflict、suspicious-encoding。
- 扫描选项：deep、max-scan-bytes、min-confidence（detect/list/test）；显式根归档完整扫描与嵌套扫描限额语义不同，0 表示无扫描字节上限。
- 解压专有：output、recursion-limit、重复 password、no-empty、encoding、layout、single-root-name、dry-run、embedded、dominant-min-ratio、confirm-large-scan、no-history、force。
- 测试专有：diagnose、diagnostic-timeout、no-history；`--use-clipboard` 在参数覆盖时返回 `unsupported_option`，不是已工作能力。`logging.file=true` 同样在配置校验中拒绝；GUI 不应提供可保存但无效的开关。

## 接入必须补齐的缝隙

1. **线程与数据库**：`TaskHistoryRecorder` 有意不要求 Send/Sync；默认 recorder 与 `PasswordService` 持有借用的 rusqlite Connection。由专用 worker 在线程内部打开数据库，使用 current-thread Tokio/LocalSet 承载相应 futures，或另行定义串行存储边界；不能把借用 recorder 直接跨线程 spawn。UI 只接收安全 DTO。多 worker 写入须实测 SQLite 锁等待/事务行为。
2. **共享任务调度**：小/大窗口订阅同一服务。外部 JobId 对应开始后引擎发出的 TaskId，等待期间就应有稳定 UI 标识；更改队列顺序不能修改执行中请求。输出根与分卷输入有重叠时做冲突互斥，避免并发拆分破坏工作流内密码批次复用与预算。
3. **交互响应**：四个 async prompter 适合通过 channel + oneshot 连接 UI；请求至少包含 JobId、路径、请求唯一 ID，关闭/取消后清理 pending response，防止回复落到后续任务。大扫描确认不是已工作能力：必须补充扫描前门控或新增专用可等待接口，单独设置 confirm_large_scan 字段没有作用。
4. **进度边界**：`EventSink` 向 listener 转发全部事件，但仅储存前 4096 个 Progress 事件。GUI 需合并高频进度，保留所有终态/交互/警告；持久化事件快照不等于完整高频遥测。
5. **调度与历史不同**：现有历史记录不含可完整恢复的请求快照，也没有持久化优先级/排队状态。历史“重新运行”应创建新任务、校验路径并展示当前或保存的配置，不能宣称断点恢复或原配置完全重放。
6. **密码库 UI 扩展**：现有排名查询只返回启用项；禁用项浏览/恢复、编辑密码/切换置顶的专用交互需补 repository/service 接口或明确事务实现。仅因底层 upsert 存在不可推定所有管理动作已经端到端存在。

## 验收建议

以同一配置/fixture 比较 CLI 与 GUI 请求及结果：普通 ZIP、加密 ZIP、RAR/7z 分卷（从非首卷拖入）、嵌套归档、内嵌包、可疑编码、已存在输出四策略、密码/编码复用、skip_completed 抑制与 force 无额外效果、无后端、超预算、恶意路径、测试损坏报告、取消及部分成功。GUI 验收另外覆盖跨窗口同步、交互归属、排队顺序、关闭窗口仍运行、只读/无状态模式。

这些是后续实现验收项，并非本次已经通过的测试。
