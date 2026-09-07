# 实施与验证

2026-09-07，统一配置已接入 CLI 和 engine；GUI 源码未修改。

## 实施

- smartzip-config 保持无 engine/SQLite 依赖，以 model、resolve、store 划分模型/默认值/校验、单文件选择/覆盖/来源、安全编辑/迁移。拒绝未知字段、版本、重复来源、非法路径与不支持的选项。
- CLI 根据 clap 的 CommandLine value_source 生成显式补丁，避免解析器默认值覆盖文件。config 管理和 explain 在后端/DB 创建前返回。任务只按启用功能打开数据库；平台默认路径修正并保留旧库检测。
- engine CompiledRunPolicy 固定配置快照，控制递归、根/内层扫描、自动分卷、编码、输出、清理、失败停止与交互。新增 Decision 事件，保留原有路由/密码/编码/布局事件。
- PasswordService 可无仓库运行，严格按允许的来源排序；手动模式、批次复用、保存与统计分别受策略约束。已知文件缓存抽出 KnownFileStore/DbKnownFileStore，与任务历史独立选择。RunStores 通过原 TaskHistoryRecorder 接口适配旧工作流，旧公共调用兼容路径继续保留。
- state.read-only 使用 SQLite 只读打开，不迁移；off 不打开数据库。清理 keep 不触发回收器；delete 显式永久删除，trash 不降级。根分卷不得作为内层归档清理。
- docs/configuration.md 为用户配置说明与能力边界的唯一入口，README 添加链接。

## 验证

日志位于本地忽略目录 target/config-validation/：

| 检查 | 结果 |
| --- | --- |
| cargo test --workspace --locked | 450 passed、0 failed、0 ignored，覆盖当前生产源码；workspace-tests.log |
| 后续新增禁用后端测试并重跑 CLI configuration 集成套件 | 5 passed，含上轮已有 4 项及新增 1 项；cli-configuration.log |
| cargo check --workspace --all-targets --locked | 通过；check.log |
| cargo build -p smartzip-cli --locked | 通过；build.log |
| python3 scripts/verify_cli_beta.py target/debug/smartzip | 23 项真实后端验收通过；cli-beta.log |
| cargo fmt --all -- --check、git diff --check、check_routing_guards.sh | 通过 |
| 文档 TOML 示例、config init --full 生成结果 | 使用实际 CLI config check 验证通过 |

关键新增证据：配置优先级/路径/非法值/安全编辑/迁移 5 项；CLI 无状态无 DB、只读库内容不变、历史和缓存独立、来源选择/无工作目录加载、解释无后端/归档副作用；密码测试通过删除密码表证明禁用来源不查询，独立检查保存/统计；引擎假后端证明 cleanup=keep 的回收调用为 0，后续配置变化不影响已编译策略；禁用后端通过可执行标记脚本证明探测和启动均未发生。

新增禁用后端测试初次错误地预期“关闭唯一后端后仍解压成功”，实际正确报告无可用后端。修正测试期望为失败并断言原因，仍检查脚本调用标记不存在；未为此修改生产路由。

真实 beta 包括密码直接解压（两次 x、零完整 test）、加密和伪装归档、分卷、预算/路径/提交回滚、取消及失败清理。以上不是性能证据；跨平台路径代码已编译，未开展 Windows/macOS 真实运行验收。工具链提示 proc-macro-error2 2.0.1 的未来 Rust 兼容警告，本次未升级依赖。

## 兼容与未实现边界

- GUI 按用户要求排除；无配置策略的旧库 API 保留既有行为。
- skip_completed 默认 false，显式 true 因旧缓存缺少目标与策略完成证据而被抑制并解释，不做不可靠跳过。
- explain/dry-run 当前均为配置阶段计划，不预演动态嵌套成员与最终布局；dry-run 尚未增加归档列表阶段。
- clipboard、file logging 直接拒绝；逐输入输出目录、额外完整预测试、直接写最终目录不提供空开关。
- 自动分卷开关是 extraction 范围；显式完整性 test 的分卷收集/诊断保留其独立语义。Decision 覆盖策略门控，已有运行事件保留，尚未把所有历史事件统一转换为 Decision。
- 用户已有 AGENTS/CONTEXT/docs/skills 改动保留在本地；用户随后授权将本轮实现与前序清理提交并推送。
