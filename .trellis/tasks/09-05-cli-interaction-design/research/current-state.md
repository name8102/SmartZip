# CLI 状态核对与复现证据

核对日期：2026-09-05。本地 `feat/db-history-persistence`，HEAD `de0ea48`（提交日期 2026-07-31）。未 fetch；以下不声称远端没有新提交。

## 已有实现与任务状态

- routing 整合父任务及 6 个子任务均 completed；能力 profile、task-scoped context、统一 TaskEvent 与单一 staging 已落地。
- CLI 可执行命令：detect/list/extract/encoding-preview/password/history。test/compress 都返回明确未实现错误和 exit 1。
- 文件级历史与 known_files 已存在；detect/list 有接线。旧 file-aware CLI 任务仍 planning，候选编码对照和其他验收未全部实现。
- CLI main.rs 1777 行，6 个单元测试；没有 CLI 级密码终端交互回归测试。
- `.trellis/scripts`、`.trellis/spec`、`.trellis/workflow.md` 在当前 checkout 不存在；未尝试重装 Trellis。

## 源码证据

| 观察 | 实现位置 | 设计影响 |
| --- | --- | --- |
| pick-encoding 只打印固定编码名；0 经 saturating_sub 也会选到第一个；非终端退回 auto | CLI `prompt_pick_encoding` | 真实名称对照、严格输入与无终端校验 |
| 密码输入使用 read_line 并 trim；引擎和服务继续 trim/normalize | CLI `prompt_password_stdin`；engine access/extract；PasswordService `normalize_password` | 所有层统一内容保真，不能只改 UI |
| `use_clipboard` 被绑定为 `_use_clipboard`，request.clipboard 固定 None | CLI `run` / `extract` / `list_archive` | 参数实际生效或明确失败 |
| list 成功后直接 record_success | engine `access::access_archive_with_password` | 成功列目录不等于密码有效 |
| 交互改编码返回 Override，但已确认编码写库仍看原始 request | engine `encoding_flow`、workflow 的 list 收尾、extract_workflow 收尾 | 携带最终选择来源，避免漏记交互确认 |
| 各提示只单独看 stdin；JSON 不阻止 prompter 注入 | CLI 各 `prompt_*`、`routing_listener` | 统一终端模式与输出协调 |
| 密码列表按 `&p.value[..27]` 截断 | CLI `password` | Unicode panic，P0 修复 |
| 全部命令 dispatch 前 build_backend | CLI `run` | 密码/历史操作与归档配置不应耦合 |
| dry-run 只打印首个路径且已开 DB | CLI `extract` / dispatch | 多输入候选预览和无副作用契约 |
| root args 没有 global，布局/扫描配置未消费 | CLI `Cli` / `build_backend` | 帮助与实际配置范围必须说清 |

## 实际 CLI 复现

使用本轮 `cargo build -p smartzip-cli` 生成的二进制；临时目录中独立 SQLite 与 XDG 目录，命令由 subprocess 调用，结束删除。全部密码为合成值，没有读取真实密码库或剪贴板。

| 场景 | 实际观察 |
| --- | --- |
| 顶层和 extract `--help` | exit 0；test 帮助未明确未实现，extract 输入显示可选 |
| `history tasks --db <temporary-db>` | exit 2，unexpected argument；`--db` 必须放在子命令前 |
| `--db <temporary-db> extract a.zip b.zip --dry-run --json` | exit 0，只显示 a.zip 的文本；不是 JSON，也没有检查不存在的输入 |
| `test <fixture> --json` | exit 1，stderr 未实现，stdout 空 |
| `compress missing-file` | exit 1，stderr 未实现 |
| `--config <missing.toml> password list` | exit 1，在访问密码库前因无关 routing 配置失败 |
| 添加合成值 `a` + 12 个 `密`，然后 password list | exit 101，UTF-8 字节截断 panic |
| 用标准库创建未加密 plain.zip，list 时传入任意合成 `-p` | exit 0，JSON used_password=true；SQLite 中出现该未验证密码 |

最后一项是验证语义缺口，不是待优化文案：用户可能被误导为一个密码已经验证成功。应在新交互之前修正。

## 本轮验证

- `cargo build -p smartzip-cli`：成功。
- `cargo test -p smartzip-cli`：6 passed。
- `cargo test -p smartzip-engine -p smartzip-passwords -p smartzip-encoding --lib`：engine 177、passwords 3、encoding 9 passed；合计连同 CLI 为 195 项。
- `bash scripts/check_routing_guards.sh`：`routing guards: clean`。

这是初始核对时相关测试的结果，并不覆盖全部 workspace、跨平台终端、clipboard 或新设计验收。现有测试通过与上述实际缺口同时成立。初始核对阶段没有修改 Rust 行为，也没有把未实现项标为完成。

## 追加命名与别名实施（2026-09-05）

用户要求缩短 encoding-preview 并增加常用短别名；已将主命令改为 enc，保留隐藏兼容名称 encoding-preview；新增 x/l/d/t/c/pw/hist 可见别名。改动限 clap 命令元数据与 test 帮助文案，未改动归档操作流程。

- `cargo test -p smartzip-cli`：6 passed；`cargo build -p smartzip-cli` 成功。
- `cargo fmt -p smartzip-cli --check`、`git diff --check`、routing guard 均通过。
- `cargo clippy -p smartzip-cli --all-targets --no-deps` exit 0；3 处既有 too_many_arguments warning 位于 detect/list_archive/extract，未引入新警告抑制或修改无关函数。
- 二进制帮助显示 enc 和全部 7 个短别名；完整命令与别名 --help 相同。
- 隔离临时 DB/XDG 与合成 ZIP：enc/encoding-preview 的 JSON 相同；x dry-run、l/d JSON、pw list、hist files、hist 默认查询与完整命令的对应结果相同；t/c 保持未实现 exit 1。

因此仅 S0 命名/别名已完成，密码/编码交互问题与 S1–S6 仍未修复或实现。
