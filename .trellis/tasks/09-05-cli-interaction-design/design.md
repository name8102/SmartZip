# SmartZip CLI：密码与编码交互设计

> 2026-09-05 设计草案。以本任务 PRD 为范围；命令命名与短别名已实现，以下交互“目标”及新增参数尚未实现。当前实际能力见 [现状核对](research/current-state.md)。

## 1. 命令围绕用户动作组织

| 命令 | 用户的问题 | 交互边界 |
| --- | --- | --- |
| `detect <path>` | 这是什么文件，可能需要密码或编码处理吗？ | 始终只报告，不求密码、不确认编码 |
| `list <path>` | 里面有什么，文件名读得对吗？ | 按需解开文件名加密；默认列一次，主动选择用 `--pick-encoding` |
| `extract <paths...>` | 把这些文件解出来 | 密码、可疑编码、内嵌选择、最终路径冲突分别按需询问 |
| `enc <path>` | 哪种编码能读懂这些名字？ | 显示对照，不记忆编码、不提交解压产物；按需获取访问密码 |
| `password …` | 管理我存过的密码 | 查看/导入/导出沿用；`add` 可省略位置参数进入隐藏输入 |
| `history [tasks\|files\|show]` | 刚才处理了什么，为什么没解出来？ | 只读查询，不要求归档后端可用 |
| `test` / `compress` | 校验 / 压缩 | 帮助明确标为尚未实现，保留失败语义 |

不改变 extract 默认递归深度 3、layout=conservative、embedded=auto。`--deep` 是增加扫描范围；递归层数仍由 `--recursion-limit` 控制，两者在帮助里分开说明。

本轮保留多输入未指定 output 时共用首个输入父目录的行为，开始时明确显示输出根。逐输入输出根是单独的兼容性变更，不悄悄混入交互调整。

### 1.1 命名与短别名（已实现）

| 主命令 | 短别名 / 兼容名称 |
| --- | --- |
| `enc` | 旧 `encoding-preview` 为隐藏兼容别名 |
| `extract` | `x` |
| `list` | `l` |
| `detect` | `d` |
| `test`（尚未实现） | `t` |
| `compress`（尚未实现） | `c` |
| `password` | `pw` |
| `history` | `hist` |

显式短别名出现在 --help，旧的长编码预览名称不再占主命令栏。别名直接解析为同一个 command variant，沿用参数、执行路径、输出与退出码。保留 `-h/--help` 的含义，不启用任意前缀缩写。

## 2. 一次日常操作应当怎样发生

以下为目标输出示意，路径和文件名均为示例；第一阶段沿用现有英文 CLI 文案，完整中文本地化单独处理。

```text
$ smartzip extract downloads/materials.zip
[1/1] materials.zip · ZIP
Output root: downloads/
Filename encoding needs a choice. Preview of the same entries:
  1  GB18030   资料/说明.txt       资料/封面.jpg   [suggested]
  2  Big5      …                  …
  3  UTF-8     …                  …              [decoding errors]
Choose 1-3, m for another encoding, a to use the suggestion once, s to skip:
> 1
Preview with GB18030:
  资料/说明.txt
  资料/封面.jpg
Enter to confirm, b to compare again, s to skip:
>
Encoding selected: GB18030.
Password: trying saved candidates (3/12)
No matching password. Enter a password (hidden), or Enter to skip this archive:
Password accepted.
Extracted → downloads/materials/
Password saved locally; available to the rest of this batch.
Encoding remembered for this file: GB18030.
Completed: 1 archive; 0 skipped; 0 failed.
```

这是需要密码且编码有歧义时的合并终端输出示例，假定存储提示此前已经展示。正常未加密且名称可读的归档直接执行，只显示阶段和完成结果，不问多余问题。普通加密 ZIP 可以先看名称再在解内容阶段求密码；文件名加密归档必须先取得访问密码。选择后先显示“已选用”，实际写库成功后才显示“已记忆”。

交互页面一次只问一个问题。提示包含当前归档、问题、合法输入和默认行为；无效选择留在当前提示。切换编码产生的是候选预览，最终确认后才生效。

## 3. 终端、输出和取消规则

### 3.1 什么时候可以询问

统一计算 `can_prompt = stdin 是终端 && stderr 是终端 && !json && !non_interactive && !password_stdin`。所有密码、编码、冲突和内嵌提示使用同一规则。

| 场景 | 行为 |
| --- | --- |
| 正常终端 | 按需交互 |
| stdout 重定向，stdin/stderr 仍为终端 | 可以交互；提示和预览写 stderr，stdout 保留结果 |
| stdin 或 stderr 非终端 | 不询问；不从管道偷读答案 |
| `--json` 或新增 `--non-interactive` | 不询问；按明确策略执行或报告缺少决策 |
| 新增 `--password-stdin` | stdin 只消费一行密码；该命令不再交互 |
| 显式 `--pick-encoding` 但不能交互 | 执行前报参数组合错误，提示改用 `--encoding`，不回退 auto |

`--non-interactive` 下，密码试尽记 `password_required`；需要选择的内嵌 finding 跳过；输出冲突遵守显式策略、默认 skip。list 对猜测编码给出结果和 warning；extract 对已被 engine 判为需要人工确认的编码跳过并给出 `encoding_confirmation_required`。明确 `--encoding` 或已确认文件记忆可消除这个歧义。

以上更严格的非交互 extract 编码行为属于可见变更，需要发布说明和对应 JSON reason；不把自动跳过伪装成成功。`--embedded ask` 无终端则报告缺少选择，保留其他输入继续处理的能力。

### 3.2 输出分工

- stdout：list 的条目、detect/preview/history/password 的结果、extract 的最终摘要，或既有 JSON 文档。
- stderr：阶段进度、提示、交互预览、warning、可选路由诊断；`--verbose-routing` 不改变是否可交互。
- 提示显示期间冻结进度重绘，决策完成再恢复；同一命令只有一个终端输入协调器。
- 默认显示阶段切换，不逐条刷屏打印所有密码候选；需要时显示来源和尝试计数，绝不显示候选值。
- 文件名中的控制字符、ANSI 转义、换行在终端预览中转义显示；真实路径与 JSON 数据不因此改写。
- 没有总字节数就显示阶段/已处理数量，不制造百分比。第一阶段用现有 listener，不等待 mpsc 改造。

### 3.3 三种中断不能混为一谈

| 输入 | 含义 | 后续 |
| --- | --- | --- |
| 密码提示空行 / 编码或冲突选择 skip | 跳过当前归档 | 继续其他输入；记录具体 reason |
| EOF / 输入读取错误 | 当前问题无法取得答案 | 当前归档按缺少决策处理，不循环重试、不默认确认 |
| Ctrl-C | 取消整个任务 | 停止新尝试，取消运行中的后端，清理 staging，收尾历史，目标 exit 130 |

取消需要从 CLI 信号传到 engine 与 executor，并等待资源释放；不是在提示函数里直接 `process::exit`。隐藏输入使用可恢复的终端状态守卫，成功、错误、EOF 和取消都恢复回显。实现未贯通前不能宣称支持清理式取消。文件已成功提交不回滚；摘要区分已完成、跳过、失败与取消。

## 4. 密码：输入、重试、验证、记忆

### 4.1 入口和内容保真

| 入口 | 目标契约 |
| --- | --- |
| `-p/--password <value>`，可重复 | 保持调用方传入的内容与顺序；引号由 shell 处理 |
| 交互隐藏输入 / `password add` 无位置参数 | 禁用回显；仅移除输入行的一个 LF 或 CRLF；保留首尾空格、tab、全角空格 |
| 新增 `--password-stdin` | 只接受重定向 stdin，消费一行 UTF-8 文本并只去行结束符；与 `-p`、`--use-clipboard` 互斥；EOF 无数据报输入错误，stdin 为终端则提示改用隐藏输入 |
| `--use-clipboard` | 显式请求后读取一次；保留现有拼写；平台不支持或读取失败时明确报错，不能忽略 |
| `password import` | 一行一个密码，只去行结束符；空行忽略，纯空白但非空的行保留；不改写已有 DB 内容 |

剪贴板按单条密码处理，最多去一个末尾行结束符；包含中间换行的文本明确拒绝，提示改为密码导入。所有入口遇到 NUL 明确拒绝；本阶段文本输入不承诺任意二进制密码。

`-p ''`/stdin 空行保留空密码候选语义，空密码不保存；交互空行始终是“跳过”，提示中说明此区别。`--no-empty` 与显式空密码冲突，报参数错误。

当前 CLI、engine、PasswordService 都有 trim/normalize 路径；保真规则必须贯穿所有层。旧测试把空白裁剪视为预期，应以真实含空格密码 fixture 替换这个契约，不能只换终端输入库。

### 4.2 自动候选与手动重试

自动候选顺序：显式 `-p` 或 stdin → 显式剪贴板 → known_files 精确命中 → 本批次已验证成功密码 → 默认空密码尝试 → 其余数据库候选。相同内容精确去重；`--no-empty` 只移除默认空候选。延续当前有界候选策略，暂不增加新性能参数。

自动候选用尽后进入隐藏输入。明确 WrongPassword 时提示“密码未匹配，请重试”，再输入不重新遍历数据库；直到验证成功、空行跳过或取消。当前归档本任务已试过的相同密码不重复调用后端，提示重新输入。

权限、磁盘、安全、缺卷、后端不可用、损坏与无法区分密码/损坏的错误不能变成无限求密码。只有明确密码错误才记失败统计；错误来源和可执行下一步应一并展示。

### 4.3 什么才算密码成功

必须显式区分 `NotNeeded`、`Verified`、`Rejected`、`Indeterminate`，由归档访问流程携带验证证据；“调用返回 Ok”本身不是密码证据。

- 普通 ZIP 的未加密目录可以在没有密码时列出。list/enc 成功只证明名称可读，不能自动保存任意传入密码或增加命中次数。
- 文件名加密归档：后端可靠确认使用该密码解开头部，才可用于密码记忆。
- extract：可靠确认解密成功才记密码命中；不插入一次额外的全量 test。
- 若后端不能证明密码被使用，标为 Indeterminate，不自动保存、不污染 known_files。

默认在 Verified 后立即保存非空密码并加入批次缓存；失败的候选不入库。保存失败只给 warning，操作继续，本批次内仍可复用。

### 4.4 保存规则与用户反馈

保持“默认自动保存成功密码”的已有需求。首次使用明确提示本地明文 SQLite 存储；用一次性状态记录提示是否已展示，不要求用户确认才能继续。状态不可写时退化为每次调用最多显示一次。提示应在可能保存前出现，JSON 模式写 stderr；关闭保存时不提示“将自动保存”。

新增 `--no-save-password` 表示本次不写密码相关状态：不插入密码、不改成功/失败统计、不写新的 known_files.password_id；仍可读取库和在内存中复用本批次密码，已有关联不清空。该参数属于归档访问命令，不改变用户显式执行 password add/import 的持久化含义。

| 参数组合 | task/event/file 历史 | 密码相关写入 |
| --- | --- | --- |
| 默认 | 保持现有命令行为 | Verified 后保存/关联，明确失败可记统计 |
| `--no-history`（extract 现有） | 不注入历史 recorder | 现有独立密码统计/保存仍可发生 |
| `--no-save-password`（新增） | 照常，敏感值不进入事件 | 不写密码、统计或关联 |
| 两者同时使用 | 不写操作历史 | 不写密码相关状态 |

`--no-history` 当前还会影响经 recorder 完成的 known_files 行为；本任务不借机把它改成“无任何 DB 写入”，也不承诺本轮补齐无历史时的指纹复用。

成功后仅输出“已保存并可供本批次复用”或“仅本批次使用”。`password list/export` 是用户显式查看密码的命令，继续提供密码值；列表宽度裁剪按字符安全处理，不能按 UTF-8 字节切片。所有普通任务日志、错误包装、route/history 事件不得含密码。

## 5. 编码：先看名字，再明确选择

### 5.1 统一预览来源

engine 提供共享预览能力供 list、extract 和 enc 使用。归档定位、内嵌 offset、已获访问密码与 route context 复用同一次访问上下文；CLI 不另按扩展名猜格式或为每个候选重新求密码。

能取得原始名称字节时读取一次，再对同一组条目解码。默认最多展示 4 个候选、每个 6 个相同索引的名字，优先含非 ASCII 的条目；更多名字按需展示。大归档预览有采样与缓存预算，不能为预览展开全部文件内容。

候选优先考虑文件记忆、检测建议和常用编码；别名归一化，重复解码结果合并标签。显示“建议 / 解码失败 / 后端不支持”，检测分数不表述为正确概率。ASCII-only 不触发自动询问。

后端只能返回 Unicode 名称而无法重新解码时明确说明“不支持原始名称编码预览”，不展示多份相同结果冒充候选。已有 ZIP Unicode 元数据规则继续由 engine/adapter 决定，CLI 不自行重解可信名称。

### 5.2 选择与生效规则

- `list --pick-encoding`：预览候选 → 数字或编码名 → 复列样本 → Enter 确认 / 返回重选 / 跳过 → 输出完整条目。没有确认就不写记忆。
- `extract --pick-encoding`（新增）：同一流程在内容解压前执行；只增加查看名称的工作，不创建最终输出；密码需求由真实归档决定。
- 默认 list：最佳猜测列一次，遇到可疑编码给 warning 和 `--pick-encoding` 的具体建议，不强迫所有 list 用户选择。
- 默认 extract：仅在 engine 判定可疑时询问。输入 `a` 表示“这次使用建议”，不是人工确认；空输入在尚未选中时不直接确认。
- `--encoding <name>`：验证并规范化编码名后视为显式选择；与 `--pick-encoding` 互斥。`auto` 大小写不敏感且永远不是显式确认。
- 选中不支持的编码或复列失败：显示错误并回到选择；不把失败候选写库。最终列表使用已确认的成功 listing，避免再走一次候选/密码流程。

第一阶段保留编码名手动输入和数字选择，不加入方向键、全屏刷新或外部 pager。

### 5.3 把“实际采用什么”和“是谁确认的”分开

不能用 `EncodingMode::Override` 同时表达 CLI 指定、自动猜测、文件记忆和交互确认。目标内部结果包含：规范编码名、来源、是否明确确认、可用预览/条目结果。来源至少区分 `AutoDetected`、`Remembered`、`CliExplicit`、`PromptExplicit`、`AcceptedOnce`。

| 来源 | 用于本次操作 | 覆盖 confirmed_encoding |
| --- | --- | --- |
| 自动猜测 / 无终端自动使用 / `a` 仅本次 | 是，遵守疑似乱码策略 | 否 |
| 精确文件指纹记忆 | 是 | 不重复确认为新选择 |
| `--encoding` 显式非 auto / 预览后确认 | 是 | 实际成功列名或成功解压后写入 |
| 只运行 enc / 选择取消或失败 | 仅预览或不采用 | 否 |

持久化使用最终 resolution 的编码和来源，不能只检查原始 request；否则交互修改会遗漏，自动转换为 Override 又可能被误记为人工确认。

记忆键沿用 sample_hash + size，内嵌归档使用已定义的区段指纹；无法可靠算指纹时只用于本次并提示未记忆。list 不更新 last_extract_at。extract 仍在成功材质化后的既有历史落点写入。

显式 CLI `--encoding` 继续按本次请求作用；通过提示选出的编码只用于当前候选，不自动施加到兄弟或子归档。不同文件的编码不能靠“本批次不再问”统一，下一份文件重新检测或命中自己的记忆。

## 6. 批次冲突与结果反馈

密码成功可以加入任务级缓存；编码确认绑定当前指纹；输出策略与内嵌策略各有独立批次状态。提示永远说清“当前归档”还是“本批次剩余冲突”，不设置笼统的“全部同意”。

新增 `--on-conflict ask|skip|rename|overwrite`，默认 ask；不能交互时默认 skip。保持在 plan_layout 得到真实目标之后询问。提示显示归档名、实际目标、现有文件/目录类型；Enter=skip，其他选项包括重命名、覆盖，以及分别针对本批次剩余冲突应用 skip/rename/overwrite。批次设置只影响之后的冲突，不在循环内反复确认。

`--force` 仍仅绕过去重，不能同时覆盖已有输出。一次失败不能吞掉其他输入的结果。完成摘要列出每个根输入的最终路径或跳过/失败原因，并另外汇总嵌套归档数量；没有历史 recorder 时仍有可读结果。

dry-run 是“初始输出候选预览”：遍历全部显式输入、检查输入基本存在性和参数，不初始化 DB、不发现后端、不解压、不创建最终目录、不写记忆。每项列 input、output_root、candidate_output 和 `final_layout_known=false`；JSON 使用独立的 `mode="dry_run"`、`plans[]`、`errors[]` 与 exit_code。任一输入无效时报告全部已发现输入错误并 exit 1；有效输入全部可规划时 exit 0。不能声称已知内容布局、密码可用性、递归发现或最终冲突。

## 7. 参数、初始化与兼容性

| 参数 | 当前 → 目标 |
| --- | --- |
| `--db / --config / --backend / --verbose-routing` | 目前仅根层可用 → 真正 global，子命令前后等价 |
| `--json` | 保持已有命令结果形状；禁止提示；dry-run 必须输出对应 JSON，不能夹普通文本 |
| `--non-interactive` | 新增 global，统一关闭所有提示 |
| `--password-stdin / --no-save-password` | 新增于有实际密码访问的命令；test 落地后再接入 |
| `--pick-encoding` | list 补齐对照流程，extract 新增；enc 保持单纯预览用途 |
| `--use-clipboard` | 已暴露但忽略 → 实际读取或明确不可用 |
| `--on-conflict` | 新增 extract 策略参数 |

先解析并校验，再初始化依赖。extract 至少一个输入；比率必须有限且位于 0..=1；编码是否合法、参数互斥、能否交互在产生副作用前检查。保留 `--` 处理以连字符开头的路径。

password/history 只初始化 DB，不加载归档路由配置。归档操作通过唯一 build_backend 路径创建 router；显式坏配置仍报错，不静默降级。帮助和 dry-run 不初始化 DB/后端。

现有 `--config` 实际仅用于 routing；本轮不宣称布局/扫描配置已经全面接线，也不新增整套配置编辑命令。帮助按“输入输出 / 密码 / 文件名编码 / 递归与扫描 / 诊断”分组展示参数，示例优先展示日常可执行命令。

退出码第一阶段保留 0=成功、1=失败、2=extract 部分处理；当前 clap 参数错误也为 2，这个冲突必须明示。Ctrl-C 的 130 随取消链路落地。旧主设计与 test 草案中更细的退出码定义不一致，完整归一化和版本化 JSON 错误信封在 test 实施前另行裁决，不在本轮暗改脚本契约。

JSON 错误信封未落地前，早期参数/配置/未实现错误允许只写 stderr 并非零退出；不能承诺“所有失败都有 JSON”。正常 extract 的完整 TaskEvent 仍是权威观测面，不建立额外 route 事件缓冲。

## 8. 实现边界与交付顺序

CLI 建议拆为参数声明、按需上下文构造、command handlers、terminal/prompts、render 五部分；main 只解析、dispatch、统一决定 exit。先按真实职责拆分，不引入泛化命令框架。

交互 reply 需要显式区分输入密码/跳过/取消、选定编码/仅本次/跳过/取消。engine 控制重试、验证证据和决策落点，terminal 只呈现问题并返回选择。PasswordService 管候选和保存策略；engine 管最终编码来源和历史写入。此内部变更不要求同时替换公共 JSON。

命名与短别名作为用户追加的小步 S0 已先行实现；后续按 [实施切片](implement.md) 的 S1→S6 顺序推进，每步都提供用户可验证结果。先保证密码内容与“验证成功”含义正确，再接隐藏输入和记忆反馈；编码流程在这套访问语义上复用。
