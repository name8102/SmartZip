# SmartZip 实现缺口复核

日期：2026-09-19。以下保留修复前的审查证据，范围为用户粘贴的十项 review、现有设计，以及刚推送的 GUI 实现。后续已按用户要求完成本地合并并修复；当前结果见 [implement.md](implement.md)。

## 版本基线

- 远端最新：`origin/main = 24fccc9`，`feat(gui): add native archive workflows and macOS integration`。
- 本机：`main = cd95c12`，相对远端 ahead 6、behind 1；另有四个原有未提交文件。远端 GUI 提交和本机六个提交都基于 `06efceb`。
- 在 `/home/charl/.codex/worktrees/smartzip-review-gui/SmartZip` 隔离审查远端；原工作区及未提交修复保留。
- 以下源码位置默认指向远端 worktree；涉及本机未同步修复和实测时单独注明。不能把两个分支各自的结果宣称为已合并版本的结果。

## 结论

原 review **有真实问题，但 GUI、预览、系统集成方面明显过时，优先级和部分设计前提需要重写**。现在已有实际桌面解压实现，不能再称为“只有检测原型”或“产品层尚未落地”。另一方面，实际复现的文件名编码、密码证据、扫描覆盖问题，比增加命令占位符或完整并行调度更紧迫。

## 优先处理的具体问题

### 1. P1：远端遗漏本机的长前缀扫描修复

触发：有效 ZIP 前面有超过 64 MiB 的无签名前缀，或归档之间存在一个完整的空白搜索窗口。

远端 `scan_path` 在首个窗口没有签名时直接返回；`scan_windows` 在没有有效命中的窗口直接结束。构造 72 MiB 前缀再附加有效 ZIP，远端 CLI `detect` 返回 `embedded_count=0`、`not_found`、退出码 1。

证据：[首窗口提前返回](/home/charl/.codex/worktrees/smartzip-review-gui/SmartZip/crates/smartzip-scanner/src/lib.rs:150)、[空窗口结束循环](/home/charl/.codex/worktrees/smartzip-review-gui/SmartZip/crates/smartzip-scanner/src/windows.rs:79)。

这不是 GUI 新增的回归；本机已有未提交修复且 scanner 的 15 项测试通过，但尚未进入远端。先整合已有修复，不应另写一套。

### 2. P1：显式编码没有真正修正历史 ZIP 文件名

触发：合法 ZIP 的成员名使用 GBK 原始字节，未设置 UTF-8 标记。

构造内容有效的 `中文文件名测试.txt` ZIP，直接 `7z t` 成功。远端 CLI 的 `list --encoding gbk` 仍显示乱码；`extract --encoding gbk` 退出 0、内容字节正确，但落盘文件名错误；`enc` 的 8 个成功候选只有 1 组不同名称，实际上全部相同。

证据：[encoding_arg](/home/charl/.codex/worktrees/smartzip-review-gui/SmartZip/crates/smartzip-archive/src/sevenzz.rs:351)、[enc 逐编码调用后端](/home/charl/.codex/worktrees/smartzip-review-gui/SmartZip/crates/smartzip-cli/src/main.rs:1965)。当前 `-scs...` 参数映射并未实现所承诺的 ZIP 名称重解释。

因此问题比“还没有并排选择界面”更深。已有四项编码集成测试只验证条目数和路径非空，不能证明名称正确。验收应比较真实 Unicode 名称、落盘名称和内容；不要只检查返回的 encoding 标签。可以优先补原生 ZIP 名称处理及必要的安全解压路径，但不必把所有 ZIP 编解码、加密算法和 Native 7z 一起重做。

### 3. P1：list 会把未经验证的旧密码绑定到文件，并写入解压时间

触发：内容加密、目录不加密的 ZIP；密码库只有一个错误候选，使用 `list --no-empty`。

远端复现：列目录成功，返回 `used_password=true`、`password_id=1`；数据库 `known_files` 绑定错误密码，同时出现 `last_extract_at`。同一错误密码运行 `test` 失败。普通未加密 ZIP 配合任意手动密码，也会报告 used_password=true。

证据：[record_listing_access 原样返回候选 id](/home/charl/.codex/worktrees/smartzip-review-gui/SmartZip/crates/smartzip-passwords/src/lib.rs:236)、[listing 成功直接接受密码](/home/charl/.codex/worktrees/smartzip-review-gui/SmartZip/crates/smartzip-engine/src/access.rs:227)、[list 调用解压记录接口](/home/charl/.codex/worktrees/smartzip-review-gui/SmartZip/crates/smartzip-engine/src/workflow.rs:447)。

需要分离“成功读取目录”“验证内容密码”“成功提交解压结果”的证据。`PasswordStatus` 和 test 路径的分类已存在，应统一复用。不能再泛称“所有手动 list 密码都会自动入库”：带配置的当前 CLI 已阻止新手动密码因此入库，剩余缺陷是旧密码绑定、状态标记和记录语义；无 policy 的兼容 API 仍保留 record_success 路径。

### 4. P1：根扫描仍可能整文件驻留内存，缺少扫描阶段取消检查

触发：大文件早期出现可识别签名。远端随后 `read_to_end` 读取全部输入；本机修复空窗口后的代码也保留相同策略。扫描接口同步执行且没有 cancellation token，解压输出的预算不能限制这部分输入缓冲。

证据：[根扫描整文件读取](/home/charl/.codex/worktrees/smartzip-review-gui/SmartZip/crates/smartzip-scanner/src/lib.rs:161)、[根扫描取消大小上限](/home/charl/.codex/worktrees/smartzip-review-gui/SmartZip/crates/smartzip-engine/src/policy.rs:38)。本机合成 192 MiB 载体的 detect 进程峰值 RSS 为 214004 KiB，约 209 MiB；这是单例资源证据，不是性能基准或 OOM 复现。

修复方向是有界读取/解析和取消检查，同时保持根输入扫描到 EOF 的覆盖承诺。不能用“只扫前 64 MiB”解决内存问题，否则重新引入第 1 项。

### 5. P2：新增成员预览拒绝浏览器已经接受的 `./` 路径

触发：TAR 或 ZIP 内成员为 `./folder/note.txt`，这是常见的合法归档路径。

归档浏览器明确支持前导 `./`，并保留原始成员名供读取；`read_member` 却先去掉 `./`，再要求规范化前后字节完全相等，因而返回 UnsafeArchivePath。使用真实 ZIP、TAR 分别调用新公开读取接口，两者均复现。后端 listing 也确认保留该路径。

证据：[浏览器保留文件路径](/home/charl/.codex/worktrees/smartzip-review-gui/SmartZip/crates/smartzip-gui/src/archive_browser.rs:43)、[读取接口严格比较](/home/charl/.codex/worktrees/smartzip-review-gui/SmartZip/crates/smartzip-archive/src/member.rs:42)。

应统一“用于安全检查的路径”和“精确定位成员的原始名称”的契约；保留对穿越路径和选择器注入的拒绝，不能简单删掉安全校验。

### 6. P2：密码输错后没有当前任务内重试；CLI list 的提示取消状态不一致

加密 7z 的交互探针显示，list/extract 输错一次即退出，均只提示一次。extract 的 `prompted` 标志和 list 的一次性分支使 GUI 接入同一核心后也只能结束再重新运行整个任务，而非在当前等待点继续输入。

本机 PTY 实测 list 提示时 Ctrl+C 退出 1，extract 退出 130。远端保留相同 list 分支：提示返回 None 被折叠为 PasswordRequired，没有在此处区分取消。GUI worker 的取消包装能缓解 GUI 终态显示，但不能修复 CLI 退出码。

证据：[extract 单次提示](/home/charl/.codex/worktrees/smartzip-review-gui/SmartZip/crates/smartzip-engine/src/extract_workflow.rs:875)、[list 提示处理](/home/charl/.codex/worktrees/smartzip-review-gui/SmartZip/crates/smartzip-engine/src/access.rs:257)、[None 转 PasswordRequired](/home/charl/.codex/worktrees/smartzip-review-gui/SmartZip/crates/smartzip-engine/src/access.rs:317)。建议区分 Submit / Skip / Cancel，并允许验证失败后再次请求输入。

## 原 review 十项逐项判定

| 原意见 | 复核判断 | 应如何调整 |
|---|---|---|
| 1. GUI 只有检测原型 | **已过时** | 已有双窗口、真实解压队列、四种决定请求、取消、队列暂停、重新运行、历史、密码库和统一设置。剩余缺口是节点级任务树/控制、持久队列等，不能继续列“GUI 未接 engine”P0。 |
| 2. CLI 契约漂移 | **功能缺失属实，冻结契约依据不足** | compress、CLI open/preview、completion、裸路径解压确实没有；本次未在当前仓库资料中找到“裸路径契约已冻结”的充分依据。显式子命令是合理设计，不必添加只报未实现的 compress 占位。GUI 文件参数已用于打开预览，与 CLI 入口应分别说明。 |
| 3. 密码能力缺口 | **部分属实，证据问题优先** | 剪贴板、password-stdin、password add 无参数隐藏输入仍缺。单次禁写并非只能永久改配置：CLI 支持 --set，GUI 已有临时密码。仍需明确“不保存新密码”“不更新统计”“不更新文件密码绑定”三个边界。 |
| 4. 编码交互缺口 | **属实且低估了底层错误** | 不同候选实际名称相同；真实落盘名称错误。最终选择的 provenance 和记忆语义仍需补齐，不能只做 UI。 |
| 5. 顺序 BFS/缺少调度 | **现状属实，当前设计已收缩** | 新 GUI 设计明确单并发且等待输入占执行槽。这是可接受的首版范围，不应按旧“等待不阻塞任何其他根任务”直接判不合格。节点级调度仍是后续能力。 |
| 6. 事件与决策接口缺失 | **大半已过时，容量边界仍缺** | 已有 JobHandle、worker、oneshot 决策回复、取消生命周期、相邻进度合并。Mailbox 仍是无界 VecDeque，engine 仅限制进度记录数量，其他事件无总量限制。应要求容量和终态可靠性，不必硬性规定某一种 mpsc。非 JSON CLI 进度仍写 stdout；JSON 模式未受此污染。 |
| 7. Native ZIP/7z 和后端分发 | **基本属实，但不是同等优先级** | NativeZip 目前是元数据辅助工具，没有注册为完整归档适配器；Native7z 缺失。先解决 ZIP 名称正确性，再按收益决定完整原生路径。macOS bundle 脚本没有捆绑 7zz，仍需系统安装/配置。 |
| 8. dry-run 与复用 | **属实** | dry-run 仅配置解释，缺失输入仍退出 0；skip_completed 被禁用；用户已明确它用于避免重复解压，应按成功历史启用，不要求输出存在。应另做真实输入的只读计划，允许未知项；不能承诺不展开嵌套数据却精确预演所有内层结果。 |
| 9. 通用删除/重命名规则 | **缺失属实，可后置** | 属于扩展能力，不是现有解压错误。需要先有规则预览、目标约束和回滚，尤其不能让默认规则误删用户文件。 |
| 10. 压缩、预览、集成、分发、恢复 | **混合，必须拆开** | GUI 目录浏览和有界文本/图片预览已有；macOS .app 打包/安装、Finder Quick Action、文件关联已有代码。压缩产品工作流、选择性解压、Linux/Windows 桌面集成、跨崩溃恢复/resume、完整发布分发仍缺。macOS 代码存在不等于已完成现场系统集成验收。 |

CLI 单任务禁止新密码保存、统计和文件绑定写入的现有组合为：

```sh
--set passwords.save_success=false \
--set passwords.record_statistics=false \
--set 'state.known_files="read-only"'
```

本机实测该组合解压成功、passwords/known_files 均无新记录、仍有任务历史。它也禁止编码缓存写入，不能完全替代将来更精细的 `--no-save-password`；`--stateless` 范围更广。

其他已确认的交互断点：`enc` 直接调用 backend，没有共享 list 的密码库/交互准备；同一有可用库存密码的加密头归档，list 可成功而 enc 无显式密码失败。`list --pick-encoding --json` 在非交互环境静默忽略选择请求。相关位置：[enc](/home/charl/.codex/worktrees/smartzip-review-gui/SmartZip/crates/smartzip-cli/src/main.rs:1954)、[pick-encoding](/home/charl/.codex/worktrees/smartzip-review-gui/SmartZip/crates/smartzip-cli/src/main.rs:1612)。

## 哪些设计值得对齐

1. **保留共享核心和统一配置。** 双窗口投影同一队列、worker 内拥有数据库服务、四种交互通过请求/回答连接，方向合理，已有实现应继续完善。
2. **先保证数据与证据正确。** 安全路径、暂存提交/回滚、正确文件名、密码验证证据、完整根扫描及资源边界，是目前最值得对齐的要求。不要为了避免 list 错记密码而给所有解压强制增加完整 test 预检；可从真正成功的解压/测试中取得内容验证证据。
3. **接受有清楚边界的串行版本。** 当前等待保留执行槽、队列暂停只阻止启动新任务，设计合理。若要让其他根任务继续，先明确输出路径互斥、暂存预算和数据库写入规则；密码并行更不宜优先，它会放大 CPU、I/O 和错误解压暂存成本。
4. **事件按行为验收。** 进度允许合并；交互、错误和终态不得丢；停止消费不能无限增长；取消必须等待后端清理。把“必须使用某种 channel”换成这些约束。真正做节点级操作时再补稳定 NodeId 和父子关系，当前 JobId 不能代替它。
5. **分开配置解释与输入预演。** explain 输出配置决策；inspect/plan 读取根归档与分卷信息，估计大小、布局、冲突，标注尚待密码/内嵌展开才能知道的部分。只读约束也应明确是否允许临时目录。
6. **历史去重与恢复分开。** 原审查建议绑定目标、策略和现存输出，已被用户纠正：`skip_completed` 用于避免重复解压，直接依据现有成功解压历史，不要求目标存在或保持原位置，不另存一套去重状态。配置开关和 `--force` 提供控制。崩溃恢复的 staging/backup 识别与 resume 是另一项产品能力。
7. **产品范围与发布平台分别承诺。** macOS 集成不能代表三平台桌面交付；CLI 显式命令、GUI 默认预览、快捷窗口默认解压可以并存。历史设计的每个功能条目不自动成为本版必须全部兑现的发布阻塞项。

建议顺序：**整合两端代码与本机扫描修复 → 修正编码/密码证据/扫描资源 → 修复成员预览及交互重试 → 完善事件容量和真实计划 → 根据产品范围补压缩、节点调度、平台分发和恢复。** 原 review 的“先从零建设 GUI bridge”已经不适用。

## 本次证据与边界

- 远端 CLI 构建通过，真实后端 beta 脚本 23/23；另用独立合成归档和临时数据库复现扫描、编码、密码绑定问题。标准脚本通过不覆盖这些反例。
- 在用户要求“不需要 GUI 验收”之前，已完成远端 GUI 编译、45 项 GUI 单元测试及 5 项 worker 真实后端测试；随后不再开展 GUI 验收。未操作原生窗口，未做 macOS Finder/文件关联现场验证。
- archive/config/platform 测试通过；macOS 打包/安装脚本 4 项合成测试通过，不能替代真实 macOS 安装、签名或系统集成证据。
- 本机分支另跑 scanner 15 项和四项编码集成测试；后者通过但断言没有验证期望名称，已在上文说明。
- 临时 probe 源码已移除，远端 worktree 源码干净。原工作区四个既有修改未变；本次仅新增此审查文档。
- 原始合成探针结果位于 `/tmp/smartzip-review-a3w2lfmc/`：`remote-probes.json`、`probes.json`、`pty-probes.json`、`valid-encoding-probes.json`。临时目录不作为长期测试资产。
