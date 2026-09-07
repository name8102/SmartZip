# 统一配置

CLI 使用 `smartzip-config` 解析配置，由 engine 编译为本次任务的固定策略。运行期间修改配置只影响后续任务。GUI 暂未接入；原有库调用接口保留兼容路径。

## 选择与覆盖

生效顺序为内置 defaults-v1、选中的一个 TOML 文件、CLI 显式参数。文件按 `--config PATH`、`SMARTZIP_CONFIG`、平台默认位置的顺序选择，不叠加多个文件，也不读取工作目录、归档旁边或解压产物里的配置。

| 平台 | 默认配置 |
| --- | --- |
| Linux | `$XDG_CONFIG_HOME/smartzip/config.toml`，环境变量缺失或不是绝对路径时使用 `~/.config/smartzip/config.toml` |
| macOS | `~/Library/Application Support/SmartZip/config.toml` |
| Windows | `%APPDATA%/SmartZip/config.toml` |

默认文件不存在时直接使用默认值，不创建文件；显式指定的文件不存在、语法错误、未知字段或版本不支持时，任务启动前报错。缺失字段继承，`false` 关闭，空数组清空，数组整体替换。文件内相对路径相对配置文件，CLI 相对路径相对调用目录。

标准路径修正后会检测旧的重复应用目录：只有旧配置时继续读取并提示；新旧配置同时存在时选新配置并提示。数据库只有旧文件时继续使用旧库；新旧数据库同时存在时报错，需要用 `--db PATH` 明确选择。不会自动合并、移动或删除数据库。

```bash
smartzip config path
smartzip config init             # 创建最小版本头，不覆盖现有文件
smartzip config init --full      # 在尚不存在的文件中写出全部默认字段
smartzip config show --defaults
smartzip config show --effective --sources
smartzip config get extraction.recursion.enabled --sources
smartzip config set extraction.recursion.enabled false
smartzip config unset extraction.recursion.enabled
smartzip config check
```

`show` 默认展示合并后的 TOML；`--sources` 展示 JSON，包括字段来源、被上层策略抑制的配置和诊断。查询指定任务的 CLI 覆盖使用该任务的 `--explain`。

`set` 的值使用 TOML 语法，字符串也要带 TOML 引号，例如：

```bash
smartzip config set extraction.cleanup.nested_archives '"keep"'
smartzip extract archive.zip --set 'passwords.sources=["manual","batch","empty"]'
smartzip extract archive.zip --no-recursive --stateless
```

`--set KEY=VALUE` 可重复，使用点分字段名。同一字段被多个显式参数覆盖会报错，避免依赖参数排列顺序。后端和版本字段不支持任务级 `--set`；后端安装列表写在配置文件中。

## 常用策略

以下是可直接使用的配置示例：保留普通后缀递归，关闭产物内容扫描，保留内层归档，只使用手动、本批次和空密码，不访问持久化运行状态。

```toml
schema_version = 1
defaults_version = 1

[extraction.recursion]
enabled = true
max_depth = 3

[extraction.embedded]
root = "auto"
nested = "off"

[extraction.cleanup]
nested_archives = "keep"

[passwords]
sources = ["manual", "batch", "empty"]
save_success = false
record_statistics = false

[state]
mode = "off"
```

| 字段 | 行为 |
| --- | --- |
| `extraction.recursion.enabled` / `max_depth` | 默认启用、深度 3；关闭后不遍历产物发现子归档 |
| `extraction.embedded.root` | `off/auto/ask/all/largest`；`off` 不运行根内容扫描或切片 |
| `extraction.embedded.nested` | `off/auto/ask/aggressive/all/largest`；`off` 仍允许普通后缀递归；`auto/ask` 不扫描无归档特征的普通产物，`aggressive/all` 扩大候选范围 |
| `extraction.volumes.auto_discover` | 关闭解压阶段的兄弟卷枚举、自动准备；显式 `test` 的完整性校验仍有自己的分卷收集与诊断流程 |
| `extraction.encoding.mode` | `auto/backend/utf-8/gb18030/gbk/big5/shift_jis/euc-jp/euc-kr`；`backend` 跳过 SmartZip 编码猜测 |
| `extraction.encoding.on_suspicious` | `ask/skip/accept` |
| `extraction.output.layout` | `conservative/smart/raw/flat-single` |
| `extraction.output.single_root_name` | `auto/archive/inner/preserve-both` |
| `extraction.output.on_conflict` | `ask/skip/rename/overwrite` |
| `extraction.output.destination` | 默认 `first-input-parent`，整个批次使用首个输入的父目录；`directory` 要求同时设置 `directory` 路径 |
| `extraction.on_error` | `continue/stop`；`stop` 完成本次失败清理后停止后续队列 |
| `extraction.cleanup.nested_archives` | `keep/trash/delete`；默认 `trash`，只清理完整成功提交的内层归档 |
| `passwords.mode` | `auto/manual/off`；`manual` 仅显式密码；`off` 不使用凭据，仍可尝试无密码打开 |
| `passwords.sources` | 默认依次为 `manual/known/batch/empty/database`；未列出的来源不读取、不补充 |
| `passwords.database_limit` | 默认 128 |
| `passwords.save_success` / `record_statistics` | 独立控制成功密码新增与已有密码的统计更新 |
| `extraction.reuse.password_hint` / `encoding_hint` | 独立控制已知文件提示 |
| `interaction.mode` | `auto/always/never`；`never` 不等待输入，需要确认的操作报告 `needs_decision` |
| `logging.level` | `off/error/warn/info/debug`，控制任务事件输出；命令结果与启动错误仍会显示 |
| `backends.auto_discover` / 安装项 `enabled` | 沿用后端结构；禁用的安装项不探测、不启动 |

关闭递归时内层扫描显示为不生效。根输入始终保留；`trash` 失败保留文件并报告，不降级为永久删除。失败密码尝试的暂存清理、安全检查、取消、预算和提交回滚始终执行。密码仍直接尝试解压到独立暂存目录，不增加每个候选的完整预测试。

资源限制在 `[limits]`：`max_files=100000`、`max_output_bytes=21474836480`、`min_free_bytes=536870912`、`max_nested_candidates=10000`，延续现有值。扫描搜索窗口不等同于完整解析量或峰值内存限制。

## 持久化状态

`--no-config` 不读外部配置，仍可使用数据库。`--stateless` 等同于本次 `state.mode="off"`，仍读取配置并允许本批次内存密码复用，不删除已有状态。

- `state.mode="off"` 禁止运行状态读写，不打开数据库；`read-only` 以 SQLite 只读模式打开已有库，不创建库或迁移结构；`read-write` 允许按功能写入。
- `state.history=false`（或 `--no-history`）只关闭任务历史，不连带关闭密码保存、统计或已知文件缓存。
- `state.known_files="off/read-only/read-write"` 独立控制缓存，仍受全局状态模式约束。
- `state.database` 可指定数据库路径；数据库仅在当前任务启用的功能需要时打开。只读模式下拒绝修改密码等管理命令。

`extraction.reuse.skip_completed` 默认 `false`。现有缓存缺少目标与策略兼容性的完成证据；即使显式设置 `true`，也会显示被抑制的原因并继续处理归档，不凭历史时间戳跳过任务。

## 解释与安全编辑

```bash
smartzip extract archive.zip --explain
smartzip extract archive.zip --dry-run
smartzip extract archive.zip --json
smartzip --config old.toml config migrate --dry-run
smartzip --config old.toml config migrate --apply
```

当前 `--explain` 和 `--dry-run` 都只生成配置阶段计划：不读归档、不探测后端、不访问数据库，也不准确预演嵌套成员与最终布局；这些动态内容标记为运行后才能确定。`--dry-run` 尚未增加列表预演。

运行事件新增结构化 `Decision`，包含 `stage/action/reason/policy_key/source/detail`，用于策略跳过与需要确认的决定；已有路由、密码、编码、布局事件继续保留。任务记录保存一次配置快照，快照不含手动密码。完整事件可通过 JSON 或启用的任务历史查看。

编辑使用 `toml_edit` 保留未改字段与注释，经校验后在同目录临时写入并原子替换；通过锁文件和原文复核拒绝并发冲突。符号链接和只读文件可读取但拒绝保存。旧无版本配置仅在内存中转换：原 `[extraction]` 资源限制迁至 `[limits]`。只有 `migrate --apply` 才改写，并先创建不覆盖的 `.toml.v0.bak` 备份。

`passwords.sources` 不接受尚未接通的 `clipboard`，`--use-clipboard` 也会报错；`logging.file=true` 暂不支持。逐输入输出目录、额外完整预测试、直接写最终目录均未提供配置开关。
