# SmartZip CLI beta

Beta 交付范围是 Linux x86_64（Ubuntu 24.04 基线）和 macOS arm64（macOS 14 基线）。构建不包含 GUI。只有通过两平台工作流的 beta tag 才会生成 GitHub prerelease；本地验证不能替代 macOS 验证，也不代表已经发布。

## 安装与后端

从 GitHub Releases 下载对应的 `smartzip-<version>-<target>.tar.gz` 和 `.sha256`。在下载目录校验：Linux 使用 `sha256sum -c <文件>.sha256`，macOS 使用 `shasum -a 256 -c <文件>.sha256`。解包后把 `smartzip` 放到 PATH，例如 `~/.local/bin`；程序不是静态全依赖包。

Linux 安装 `sudo apt-get install 7zip liblzma5 libbz2-1.0`；macOS 安装 `brew install sevenzip xz`。必须能通过 PATH 找到 `7z` 或 `7zz`，或在 TOML 中声明绝对路径。RAR 的额外诊断可选使用 `unrar`。程序不捆绑这些后端，不自动下载或升级它们。安装后运行：

```sh
smartzip --version
smartzip doctor
smartzip doctor --json
```

`doctor` 显示后端路径、版本、能力、数据库路径和资源默认值；没有后端返回 1。若加载器在启动前报告缺少动态库，先安装对应依赖；Linux 可用 `ldd smartzip`，macOS 可用 `otool -L smartzip` 排查。

从源码构建：`cargo build --release --locked -p smartzip-cli`。CI 固定 Rust 1.97.1，运行 CLI 和库测试，以及解包后二进制的真实后端验收。GUI 不属于这条发布链。

## 日常使用

```sh
smartzip detect archive.zip --json
smartzip list archive.zip --encoding gb18030
smartzip enc archive.zip
smartzip extract archive.zip --output ./result
smartzip extract archive.zip --output ./batch --non-interactive --on-conflict rename --suspicious-encoding skip --json
smartzip test archive.7z.001 --json
```

`--db`、`--config`、`--backend` 放在子命令前。`doctor` 可确认实际数据库位置；`--db ./private.db` 可覆盖默认路径。`list --pick-encoding` 只列编码名称；真实名称对照使用 `enc`。自动编码判定不保证正确，允许 `--encoding` 明确指定。

### 非交互与取消

JSON、非终端 stdin 或 `--non-interactive` 均禁用输入提示。未指定密码时先尝试允许的候选，仍需密码则失败；冲突默认跳过，可用 `--on-conflict skip|overwrite|rename|ask`。可疑编码默认跳过，可用 `--suspicious-encoding skip|accept|ask` 明确选择。内嵌歧义默认跳过；显式 `--embedded all` 选择全部扫描候选。普通终端中的密码输入不回显，保留首尾空白。Ctrl+C 终止并等待外部后端，清理当前暂存输出；之前成功提交的输出保留。

### 扫描与解压预算

明确的常规归档优先直接交给后端。默认内嵌扫描上限 64 MiB，`--max-scan-bytes` 可调整但硬上限为 256 MiB。`--deep` 也遵守上限，不保证发现扫描窗口之外的内嵌归档；窗口截断会提示。扫描大小与解压产出预算是不同限制。

| 参数 | 默认值 | 含义 |
| --- | --- | --- |
| `--max-files` | 100000 | 本次工作流累计生成条目，含目录 |
| `--max-output-bytes` | 21474836480 | 累计展开文件逻辑字节数（20 GiB） |
| `--min-free-bytes` | 536870912 | 可用磁盘安全余量（512 MiB） |
| `--max-nested-candidates` | 10000 | 新发现的嵌套候选数量 |
| `--recursion-limit` | 3 | 默认递归深度 |
| `--password-limit` | 128 | 从密码库取出的候选上限 |

产出计入累计预算后，即使内层归档被回收也不退还预算。动态产出检查约每 50 ms 一次，并在提交前再次检查；超限停止当前后端并回滚暂存输出。检查间隔内可能超量，文件系统遍历也需要时间；这不是操作系统沙箱或磁盘配额，不承诺针对任意恶意后端的严格瞬时上限。后端路径与链接检查覆盖常见逃逸输入，仍应使用可信、更新的外部后端。

TOML 可配置资源默认值，CLI 显式参数优先：

```toml
[extraction]
max_files = 100000
max_output_bytes = 21474836480
min_free_bytes = 536870912
max_nested_candidates = 10000
```

后端配置示例：

```toml
[backends]
auto_discover = false
[[backends.installations]]
id = "system-7zip"
family = "seven-zip-cli"
executable = "/usr/bin/7z"
```

### 输出、密码和历史

先在同盘临时目录完整解压并整理布局。覆盖时先将旧目标移到 `.smartzip-backup-*`，新输出提交成功后才清理旧备份。提交失败会尝试恢复；若其他进程占用了恢复目标，则保留该目标和旧备份并报告恢复路径。不能保证断电/崩溃后的自动恢复，残留备份需要人工核对。不要在解压期间并发修改同一输出位置。

根输入源文件保留。成功解压的受管理输出树内的嵌套归档可能移入回收站，回收失败提示并保留；未成功的内层归档保留。历史用于复用密码、编码和诊断，同一归档换输出目录不会被历史成功记录阻止。

密码数据库与导出内容为明文。Unix 数据库文件权限强制为 0600；请使用仅自己可访问的父目录。命令行 `-p` 可能进入 shell 历史和进程参数，交互输入避免这两种暴露。存储候选串行尝试，默认 128 个，可使用 `--password-limit 0` 禁用库候选，手动 `-p` 不占用库上限。只在密码错误时继续候选；后端、权限、损坏、资源限制等错误不继续遍历密码表。

提取路径仍先完整 `test` 再 `extract`。不承诺全库深度密码搜索、剪贴板接入、命名密码表、祖先链优先级、并行密码池或 Hashcat。

## 脚本接口

`extract`、`test` 的终态为 `completed`、`partial`、`failed`、`cancelled`，任务历史与退出码使用相同结果。正常去重、递归限制和用户选择跳过不计为错误。`detect` 的 unreadable 和全部失败的编码预览返回 1。

| 退出码 | 含义 |
| --- | --- |
| 0 | 成功，包括无错误的主动跳过 |
| 1 | 失败，没有成功完成的输入 |
| 2 | 部分成功；命令行参数解析错误也使用 2 |
| 130 | 用户取消 |

JSON 输出只写 stdout；提示和进度写 stderr。提取对象包含 `task_id`、`status`、`failed_count`、`processed_count`、`skipped_count`、`enqueued_count`、对应数组、`events`、`exit_code`。`test` 保留其分组报告模式；`encoding-preview` 保留候选数组，每项含 `encoding`、`ok`、`names`、`error`。运行时的早期错误返回 `{schema_version:1,status,exit_code,error}`，doctor 使用版本 1 对象。参数解析错误由 clap 输出 stderr。各命令的既有 JSON 形状尚未合并成统一封套，beta 中新增字段时消费者应忽略未知字段。

不承诺 GUI、压缩、完整预览、系统集成、崩溃后 resume 或稳定版兼容性。
