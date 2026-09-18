# gpui-component 适用性调查

调查日期：2026-09-12。用户已明确允许升级依赖，因此**首选评估 gpui-kit / gpui-component 0.6.1 + gpui-pre 0.3.1 整体升级**，0.5.1 + gpui 0.2.2 仅作回退。结论是有条件适用：最新版具备双窗口归档工具所需的通用控件，文件拖放、多窗口、异步仍来自 GPUI 平台层，调度、历史、密码服务与归档预览业务仍由 SmartZip 提供。尚未运行原生 PoC，不能把源码能力等同于产品验收通过。

## 1. 版本、包名与升级入口

| 对象 | 核实结果 | 决策 |
| --- | --- | --- |
| 当前 SmartZip | 根 Cargo.toml 声明 gpui 0.2 + macos-blade，Cargo.lock 锁定 gpui 0.2.2 | 仅作迁移起点，不限制新 GUI 选型 |
| 最新正式发布 | crates.io 与 GitHub API 均为 **0.6.1**，2026-09-09 发布；旧 longbridge/gpui-component 仓库已重定向 longbridge/gpui-kit | 固定该正式 tag 评估，不直接跟踪 main |
| gpui-kit 0.6.1 | 聚合 crate，默认 features 为 component、assets；重导出 GPUI、platform、base、component、assets；提供 application() 与 init() | **首选工程入口**，用一套官方匹配依赖，减少误混 GPUI 包类型 |
| gpui-component 0.6.1 | 仍是正式发行的 styled component crate，并未被删除或只剩旧兼容壳；依赖 gpui-base | 若需要精细控制可直接依赖，但必须同步配置 gpui-pre / platform / assets |
| gpui-pre 0.3.1 | 0.6.1 workspace 将依赖名 gpui 指向此 package，平台入口单独来自 gpui-pre-platform 0.3.1 | 现有 gpui 0.2.2 的 Entity / Window / App 与其不能混用 |
| 回退 0.5.1 | 官方 tag 明确依赖 gpui 0.2.2，兼容当前依赖版本；旧表格叫 Table<D> | 仅当新版原生构建或必需行为未过门槛时，用具体失败证据决定回退 |

官方证据：[0.6.1 workspace](https://github.com/longbridge/gpui-kit/blob/v0.6.1/Cargo.toml)、[Kit Cargo](https://github.com/longbridge/gpui-kit/blob/v0.6.1/crates/kit/Cargo.toml)、[Kit 门面源码](https://github.com/longbridge/gpui-kit/blob/v0.6.1/crates/kit/src/lib.rs)、[Component Cargo](https://github.com/longbridge/gpui-kit/blob/v0.6.1/crates/component/Cargo.toml)、[发布记录](https://github.com/longbridge/gpui-kit/releases/tag/v0.6.1)、[crates.io 元数据](https://crates.io/api/v1/crates/gpui-component)、[回退版 Cargo](https://github.com/longbridge/gpui-kit/blob/v0.5.1/Cargo.toml)。

推荐 PoC 清单为 `gpui-kit = "=0.6.1"`，使用 `gpui_kit::*` 与 `gpui_kit::component::*`；原生入口由 `Application::new()` 改为 `gpui_kit::application()`，启动时调用 `gpui_kit::init(cx)`，每个窗口分别用 Root 包住顶层视图。可直接参考[固定版 hello_world](https://github.com/longbridge/gpui-kit/blob/v0.6.1/examples/hello_world/src/main.rs)。若源码/宏确实要求 `gpui::` 名字，则显式别名 `gpui = { package = "gpui-pre", version = "=0.3.1" }`，确认与 Kit 解析到同一包，而不是保留旧 gpui 0.2.2。这段是设计建议，尚未修改仓库 Cargo 或编译验证。

### macos-blade 不能原样迁移

在线核对 crates.io 0.3.1 的 gpui-pre、gpui-pre-platform features 均无 `macos-blade`。新平台包将 macOS 后端分离为 gpui-pre-macos，其 Cargo 有 metal 依赖，`runtime_shaders` 转发到 gpui_apple/runtime_shaders。Kit 的固定版 workspace 为平台包开启 font-kit、x11、wayland、runtime_shaders。**runtime_shaders 是着色器构建/加载选项，不是 macos-blade 的同义 feature**；升级需要移除旧 feature 并按新平台路径实际验证 GPU/工具链要求、冷启动和分发构建。不能承诺原先 Blade 环境无变化。

证据：[gpui-pre 0.3.1 features](https://crates.io/api/v1/crates/gpui-pre/0.3.1)、[platform 0.3.1 features](https://crates.io/api/v1/crates/gpui-pre-platform/0.3.1)、[macOS 平台发行源码](https://docs.rs/crate/gpui-pre-macos/0.3.1/source/Cargo.toml)、[Kit workspace 的平台配置](https://github.com/longbridge/gpui-kit/blob/v0.6.1/Cargo.toml)。本次也直接读取 crates.io 下载的 macOS 平台源码 Cargo，未安装或构建它。

### tag 与 main 的边界

0.6.1 tag 对应 `36b51819deb52c947a79f8de29e0e9175eda7464`。调查时 main HEAD 为 `84f57fdfcb4910623fb0bb7f795b077e249f9271`（2026-09-11 16:32:39 UTC），比 tag 多 21 个 commit，GitHub compare 返回 173 个变更文件，包括根 Cargo、Kit Cargo、输入状态/绘制与架构文档。虽然 main 仍显示 0.6.1 / gpui-pre 0.3.1，**同一版本字符串不意味着未发布 main 与发行源码相同**。本文新版能力以下均取 v0.6.1；官网滚动文档仅辅助导航。[固定 tag 对比本次 main](https://github.com/longbridge/gpui-kit/compare/36b51819deb52c947a79f8de29e0e9175eda7464...84f57fdfcb4910623fb0bb7f795b077e249f9271)

## 2. 最新版需求—能力—责任矩阵

| 产品需求 | 0.6.1 库 / GPUI 基础 | SmartZip 自建与验证边界 | 官方证据 |
| --- | --- | --- | --- |
| 小窗口拖入文件 | GPUI 外部文件拖放事件和 ExternalPaths；组件可画 drop zone | 自建高亮、路径筛选、多卷归组、批量去重；OS 拖放须真实验证，不能用内部列表拖拽代替 | [gpui-pre 0.3.1](https://docs.rs/crate/gpui-pre/0.3.1/source/)、[GPUI 平台包](https://crates.io/crates/gpui-pre-platform/0.3.1) |
| 文件 / 目录选择、双窗口 | GPUI 平台能力；库提供 Root 和 TitleBar | 小 / 大窗口共享任务状态、各自 UI 状态，关闭窗口不终止业务；文件选择和拖放进入同一提交流程 | [hello_world 窗口入口](https://github.com/longbridge/gpui-kit/blob/v0.6.1/examples/hello_world/src/main.rs)、[Root](https://github.com/longbridge/gpui-kit/blob/v0.6.1/crates/component/src/root.rs) |
| 解压进度 | base Progress 支持 indeterminate，另可用 Spinner；已知百分比和未知量阶段均可表达 | 百分比只绑定真实核心数据；无总量使用不确定态和阶段文字，不伪造速度/ETA | [Progress](https://github.com/longbridge/gpui-kit/blob/v0.6.1/crates/base/src/progress.rs) |
| 快速配置 | Input、Select、Checkbox、Switch、Popover、Tooltip | 表单校验、当前批次与持久默认值区分、配置存储自建 | [组件模块](https://github.com/longbridge/gpui-kit/tree/v0.6.1/crates/component/src) |
| 大窗口导航和详情 | Sidebar、Tabs、Resizable、Sheet、滚动；有 Dock，但首版不必引入可持久化任意布局 | 固定侧栏和可调详情分栏即可，应用持有选择、详情导航与窗口位置 | [组件实现](https://github.com/longbridge/gpui-kit/tree/v0.6.1/crates/component/src) |
| 多任务 / 历史表格 | **DataTable** / TableState / TableDelegate，state 源码使用 uniform_list 和 virtual_list；另有简单 Table | 调度、历史分页、排序/筛选、稳定行 ID 自建；视图排序不改变执行顺序；虚拟绘制不等于数据分页 | [table 模块](https://github.com/longbridge/gpui-kit/blob/v0.6.1/crates/component/src/table/mod.rs)、[虚拟表格状态](https://github.com/longbridge/gpui-kit/blob/v0.6.1/crates/component/src/table/state.rs) |
| 压缩包目录树 / 列表 | base Tree 源码使用 uniform_list，组件层提供外观 | 条目身份、空目录、懒加载、选择、归档读取和预算由应用提供；不能因为有树控件就宣称已有归档预览器 | [Tree](https://github.com/longbridge/gpui-kit/blob/v0.6.1/crates/base/src/tree.rs) |
| 文件内容预览 | 文本/图片展示可用基础能力；TextView 可辅助 | 格式判断、选择性读取、临时文件清理、内容大小限制自建；与目录元数据浏览明确区分 | [组件模块](https://github.com/longbridge/gpui-kit/tree/v0.6.1/crates/component/src) |
| 密码输入 | masked 输入；**0.6.1 已阻止遮罩输入复制/剪切明文**，有专门源码回归测试 | 密钥链、解锁、密码存取、记忆授权、提交后清空与明文传播边界仍由应用负责 | [base 输入状态](https://github.com/longbridge/gpui-kit/blob/v0.6.1/crates/base/src/input/base/state.rs) |
| 冲突、需密码、错误 | Dialog、Sheet、Notification、PopupMenu；Root 管理窗口浮层 | 稳定交互请求 ID 绑定任务，双窗口只能回答一次；阻塞任务的信息不能只有短暂 toast | [Root](https://github.com/longbridge/gpui-kit/blob/v0.6.1/crates/component/src/root.rs) |
| 主题与图标 | 主题机制、组件尺寸；Kit 默认启用 assets | 自定义产品颜色、错误提示、紧凑布局；双窗口主题一致 | [Kit features](https://github.com/longbridge/gpui-kit/blob/v0.6.1/crates/kit/Cargo.toml) |
| 中文 / IME / 键盘 | locales 包含 zh-CN / zh-HK；输入状态与键盘导航由组件基础层提供 | 产品文案、日期/字节格式、中文字体回退、候选窗位置、模态焦点需验收 | [新版翻译](https://github.com/longbridge/gpui-kit/blob/v0.6.1/crates/component/locales/ui.yml)、[输入绘制](https://github.com/longbridge/gpui-kit/blob/v0.6.1/crates/base/src/input/base/element.rs) |
| 无障碍 | 新版 base 有语义状态；例如 Progress 不确定态省略数值，并有相关测试 | 仍不可直接宣称 VoiceOver/NVDA 全面通过；读屏、焦点、键盘替代拖放为 PoC 门槛 | [Progress 语义与测试](https://github.com/longbridge/gpui-kit/blob/v0.6.1/crates/base/src/progress.rs) |
| macOS / Windows / Linux | gpui-pre-platform 选择各 OS 平台包；x11/wayland feature 分离 | 各平台 GPU、拖放、文件选择、标题栏、IME 要独立验收；本次没有跨平台运行证据 | [platform 元数据](https://crates.io/api/v1/crates/gpui-pre-platform/0.3.1) |
| 异步核心接线 | GPUI spawn / Entity 更新；Kit 示例使用异步窗口创建 | 核心运行时留在服务层；不在 UI 阻塞子进程或磁盘 I/O；事件按任务 ID 汇入共享状态，合并进度但保留终态/交互 | [固定版启动示例](https://github.com/longbridge/gpui-kit/blob/v0.6.1/examples/hello_world/src/main.rs) |

## 3. 密码行为与迁移范围

**旧版缺口不能套用到最新版。**0.5.1 的 copy/cut 未判断 masked，会把选区原文写入剪贴板。0.6.1 的 base InputState::is_copyable 明确要求 `!self.masked`，copy/cut 都先调用它；包含 `test_masked_input_keeps_its_value_out_of_the_clipboard` 和 `test_masked_input_disables_the_copy_context_menu_items`。这是新版优先评估的实际收益之一。[旧版源码](https://github.com/longbridge/gpui-kit/blob/v0.5.1/crates/ui/src/input/state.rs#L1429)、[新版源码](https://github.com/longbridge/gpui-kit/blob/v0.6.1/crates/base/src/input/base/state.rs#L596)

上述测试在本次只做源码审阅，没有实际运行。取消遮罩后可复制的语义仍需产品决定；应用依旧要在提交后清空并释放临时输入，不让密码进入通用共享模型、日志、事件、历史和错误文本，验证关闭弹窗、任务结束及撤销历史不会恢复旧输入。库文本存储没有在本次调查中证明内存零化，不能把禁止剪贴板复制误认为安全存储或零化保证。

迁移主要集中在 `crates/smartzip-gui/src/main.rs` 的 GPUI imports / 类型、应用启动、窗口创建、异步闭包和新增 Root/组件初始化，以及根 Cargo / GUI Cargo 的依赖与 feature。现有 core / backend 不应因 UI 包升级改变业务行为。升级时检查所有 GPUI 宏、Context/AsyncApp/WeakEntity 签名是否匹配新版本；涉及代码约 250 行的现有窗口入口虽小，不能因此免除真实渲染与事件验证。Kit 默认资产和平台依赖可能改变二进制大小、构建时长与分发条件，需要记录真实构建结果。若需要回退，保留旧版密码字段封装和未知量 Spinner 方案；新版首选并不意味着允许功能退化。

## 4. 可执行 PoC 与选型门槛

以下是尚待执行的验收计划，数值为设计目标而非本次性能结论。建议在隔离示例工程或专门开发分支验证，**不得为调查直接改动主工程依赖**。

| 门槛 | 操作与样本 | 通过条件 / 失败后的动作 |
| --- | --- | --- |
| P0 依赖统一 | 固定 `gpui-kit = "=0.6.1"`（内部 gpui-pre 0.3.1），移除旧 macos-blade 配置；运行 `cargo check`、`cargo tree -d` 并启动两窗口 | 两者共享同一 gpui-pre 包实例；Root、Input、DataTable、Tree、Dialog 可构建和显示。编译失败必须记录 toolchain/features 原因后决定修复或迁移 |
| P1 外部拖放 | 从 Finder 拖入单包、20 个包、含空格/中文路径、目录、多卷中的多个文件、混合不支持文件；再用文件选择器重复 | 无重复提交；路径原样传入核心；不支持项解释清楚；悬停/离开/落下均反馈正确；关闭大窗口后小窗口仍可接收 |
| P2 双窗口生命周期 | 同一任务运行时来回切换 50 次、关闭并重开详细窗口；同时发生需密码/取消/终态 | 任务与模型唯一；终态一致；每个交互只处理一次；失效窗口句柄不 panic；后台任务不依附临时 View 的生命周期 |
| P3 事件洪峰 | 测试事件源每秒 1,000 条进度、连续 60 秒，混入密码请求、取消回执和完成事件 | 高频进度可合并到约 10–20 次/秒刷新；关键事件无丢失；队列有界；按钮响应目标 <100 ms；记录机器与实际结果 |
| P4 大列表 / 树 | 10 万条历史/归档行、至少 1 万可见树条目；滚动、筛选、排序、打开详情 | 渲染只覆盖可见区；无每帧全量解码/排序；滚动帧时间 p95 目标 <33 ms；保留选择稳定；记录载入时间与增量内存，不能仅以“虚拟化”判通过 |
| P5 密码输入 | 中文输入法、粘贴、全选复制/剪切、撤销、显示/隐藏、关闭重开、任务结束清空 | 明文不进入日志/事件/历史；遮罩时复制策略符合上述约定；完成后不能撤销恢复旧输入；焦点及 IME 正常；不通过则先实现安全字段封装 |
| P6 原生可用性 | 两窗口浅/深色、125%/200% 缩放、键盘走完拖放替代流程、VoiceOver；目标平台另验 NVDA 与 Linux | 无截断关键操作；错误不只用颜色；输入法候选位置正确；读屏支持按实测记录，若为发布必需则不通过即阻塞选型 |
| P7 真实核心贯通 | 接入现有服务跑普通包、加密包、冲突包、多卷包；取消发生在执行和提交边界 | UI 状态来自真实核心；终态、清理、回滚与既有核心行为一致；只能通过该门槛后称“无功能退化”，组件 demo 不构成证据 |

建议决策：优先以 **gpui-kit / gpui-component 0.6.1 + gpui-pre 0.3.1** 完成 P0–P2、P5 的原生 PoC，再接入真实核心推进 P7；性能、跨平台和无障碍按发布范围设硬门槛。只在发现具体构建或必需功能阻塞且无法合理修复时，评估回退到 0.5.1 + gpui 0.2.2，并记录失去的新版能力及补救。没有证据表明最新版本身不适合本需求。

## 5. 本次验证范围

已完成：现有 Cargo / 源码起点、官方实时版本、固定 tag 与 main 差异、Kit / Component 包名、gpui-pre 包族和 macOS feature 变化、新旧密码行为、虚拟表格/树及进度能力的官方源码核查。通过 HTTPS 读取 GitHub API、固定 tag raw 源码、crates.io 元数据和 macOS crate 下载；浏览工具部分 URL 抓取失败时使用 Python HTTPS 访问相同官方来源。未执行：Cargo 编译、原生 PoC、真实拖放、读屏、跨平台、性能与真实解压验收。未修改仓库依赖或锁文件。
