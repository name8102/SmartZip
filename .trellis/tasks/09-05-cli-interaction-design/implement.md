# 实施切片

> 用户追加的命名与别名 S0 已实现；S1–S6 仍待实施，不需要等待 test/compress。

## S0 · 缩短命令与常用别名（已实现）

- 编码预览主命令改为 enc，保留 encoding-preview 作为隐藏兼容别名。
- 顶层命令增加 x/l/d/t/c/pw/hist，在帮助中显示；相同 command variant 直接复用执行逻辑。
- test 帮助明确尚未实现，t/c 不改变占位命令的失败语义。
- 同步 README 和设计文档；验证主命令/短别名帮助及输出一致、旧名称兼容、CLI 测试与格式检查。
- 验证结果：CLI 6 项测试、build、fmt、routing guard 均通过；隔离 DB 与小 ZIP 验证全部别名执行等价及 enc/encoding-preview JSON 相同。Clippy 完成，仍有既有 detect/list_archive/extract 的 3 处 too_many_arguments 警告。

## S1 · 密码值和验证结果可信（P0）

改动范围：CLI 密码列表/导入、PasswordService 候选规范化、engine access/extract 的成功证据。

- 去掉密码内容 trim，仅在行输入边界去 LF/CRLF；保留 NUL 拒绝规则。
- Unicode 安全裁剪密码列表，消除字节边界 panic。
- 访问结果分开表达无需密码、已验证、拒绝和无法判断；普通 ZIP list 不保存任意 `-p`。
- 测试必须包含带首尾空格真实密码、中文与 emoji、普通未加密 ZIP、仅内容加密 ZIP、文件名加密归档、损坏与密码无法区分的情况。
- 验收：password list 不崩溃；列未加密 ZIP 后库内没有新增未验证密码；已验证密码仍能复用。

## S2 · 可连续重试的隐藏密码输入（P1）

改动范围：CLI terminal/password handlers、engine interactive/access/extract、PasswordService 保存策略。

- 建立统一终端能力判断与显式 reply；隐藏输入、输错重试、空行跳过、EOF 结束当前问题。
- `password add` 支持省略密码，新增 `--password-stdin`；接线 `--use-clipboard`，平台不可用时明确失败。
- 实现 `--no-save-password` 与默认保存通知；通知失败不阻塞工作。
- 保留已验证密码的任务缓存和既有密码顺序，重试不重跑整库。
- 验收：PTY 中输入不会回显且所有结束路径恢复回显；错→错→对成功；stdin 管道不等待额外交互；检查 DB 的密码表、计数和 known_files 关联无禁写泄漏。
- 提示/凭据日志用合成密码测试，不读取开发者真实密码库或真实剪贴板。

## S3 · 看着文件名选择编码（P1）

改动范围：engine encoding_flow/access/workflow/extract、CLI list/extract/enc。

- 共享 ArchiveAccess 上下文和编码预览；不重复定位、求密码或把扩展名当唯一格式来源。
- 同一组名称候选对照、别名合并、成功样本复列与返回重选；list/extract --pick-encoding 共用。
- 最终 EncodingResolution 保留来源与人工确认标记；持久化使用最终选择，不检查原始 request 代替。
- 默认 list 给结果与建议；默认 extract 只在可疑时问；无原始名称字节时诚实报告能力边界。
- 验收矩阵：GBK/GB18030、Big5、Shift_JIS、UTF-8、ASCII、相同候选结果、无效编码、预览失败、内嵌 offset、文件名加密。
- 持久化断言：显式确认写库并下次命中；auto/接受一次/取消/失败不覆盖；list 不改 last_extract_at；不同子归档不继承提示选择。

## S4 · 任务级取消与统一展示（P1）

改动范围：CLI renderer/terminal、engine 任务执行与 executor/adapter 取消链路。

- 进度、交互预览写 stderr，结果写 stdout；提示期间暂停进度刷新，密码只显示来源和计数。
- Ctrl-C 作为取消信号穿透后端；等待子进程退出和 staging 清理、历史收尾后统一 exit 130。
- 注入式取消测试：等待输入、密码重试、解压写盘三个阶段；不重启已取消任务，不删除已提交文件。
- PTY 与管道验收：无交错提示、无密码回显、无 ANSI 文件名注入；JSON 无提示污染。

## S5 · 批次冲突和摘要（P1）

改动范围：CLI output prompter 与 renderer、engine collision callback；沿用 materializer 所有权。

- 实现 `--on-conflict`，提示真实布局目标、Enter 跳过、限定问题类型的本批次策略。
- 输出根与每个根输入的实际结果清晰可见，嵌套统计单列；失败不吞掉后续输入。
- 验收：布局后目标才触发冲突；`--force` 不隐含覆盖；rename/overwrite 失败仍保留既有文件；无终端默认 skip。

## S6 · 命令入口与文档一致（P2）

改动范围：CLI args/context/dispatch、README 和 CLI 设计入口。

- 根选项成为真正 global，参数校验早于 DB/router 初始化；CLI 按职责拆分。
- password/history 不依赖坏的 routing 配置；test/compress 帮助显式标为未实现。
- dry-run 遍历全部输入，输出内容未知的候选计划；JSON 正确，无 DB、后端发现或输出目录副作用。
- 冻结本轮沿用的退出码与 JSON 兼容边界；不引入新错误码表。
- 验收：binary 级全局参数前后等价、缺输入/非法比率/冲突选项、无后端密码管理、JSON dry-run、旧常用命令回归。

## 统一验证门槛

- 每个切片先补能暴露旧缺陷的行为测试；函数调用形状或源码字符串断言不能替代行为证据。
- unit 测试负责排序、内容保真、来源、保存策略；DB 集成负责成功/不写入；PTY 负责回显、EOF 与 Ctrl-C；真实小归档负责密码验证与名称对照。
- 每步运行受影响 crate 的测试与路由 guard；结束时运行 workspace check/test。平台剪贴板与终端恢复需 Linux/macOS 实测，不能只用 mock 宣称完成。
- 同步当前任务验收项与旧 file-aware CLI 的未闭合项；本任务未全部验收前保持 planning/in_progress，不能仅因文档完成标 completed。
