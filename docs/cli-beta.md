# SmartZip CLI beta

Beta 交付范围是 Linux x86_64（Ubuntu 24.04 基线）和 macOS arm64（macOS 14 基线）。独立 CLI 包不包含 GUI；桌面包同时提供 GUI 与 CLI，见 [桌面 beta 指南](desktop-beta.md)。只有通过两平台工作流的 beta tag 才会生成 GitHub prerelease；本地验证不能替代 macOS 验证，也不代表已经发布。

## 安装与后端

从 GitHub Releases 下载对应的 `smartzip-<version>-<target>.tar.gz` 和 `.sha256`。在下载目录校验：Linux 使用 `sha256sum -c <文件>.sha256`，macOS 使用 `shasum -a 256 -c <文件>.sha256`。解包后把 `smartzip` 放到 PATH，例如 `~/.local/bin`；程序不是静态全依赖包。

Linux 安装 `sudo apt-get install 7zip liblzma5 libbz2-1.0`；macOS 安装 `brew install sevenzip xz`。必须能通过 PATH 找到 `7z` 或 `7zz`，或在 TOML 中声明绝对路径。RAR 的额外诊断可选使用 `unrar`。程序不捆绑这些后端，不自动下载或升级它们。安装后运行：

```sh
smartzip --version
smartzip doctor
smartzip doctor --json
```

`doctor` 显示后端路径、版本、能力、数据库路径和资源默认值；没有后端返回 1。若加载器在启动前报告缺少动态库，先安装对应依赖；Linux 可用 `ldd smartzip`，macOS 可用 `otool -L smartzip` 排查。

从源码构建：`cargo build --release --locked -p smartzip-cli`。Ubuntu 构建还需 `pkg-config liblzma-dev libbz2-dev libfontconfig1-dev libfreetype6-dev`（扫描库的间接编译依赖）。CI 固定 Rust 1.97.1，运行 workspace、真实后端、恢复与安装验收；原生 GUI 由用户另行验收。

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

JSON、非终端 stdin 或 `--non-interactive` 均禁用输入提示。未指定密码时先尝试允许的候选，仍需密码则失败；冲突默认跳过，可用 `--on-conflict skip|overwrite|rename|ask`。可疑编码默认跳过，可用 `--suspicious-encoding skip|accept|ask` 明确选择。用户直接输入的文件自动尝试扫描发现的归档，不按载荷大小或占比跳过；嵌套发现中的歧义仍按策略询问或跳过。显式 `--embedded ask` 保留确认，`ignore` 关闭内嵌扫描。普通终端中的密码输入不回显，保留首尾空白。Ctrl+C 终止并等待外部后端，清理当前暂存输出；之前成功提交的输出保留。

多输入解压采用有界流水线：有状态任务最多同时推进两个根输入，完成一个再补入下一个，避免整个批次的扫描排在解压前。完成结果仍按输入顺序汇总。扫描当前输入发现的归档优先解压，不排到后续输入末尾；完整根扫描策略本身不变。

### 扫描与解压预算

用户直接输入的文件按 64 MiB 窗口寻找归档头；命中后完整解析归档范围，再从归档末尾继续寻找下一个归档，遇到空窗口仍继续，直到到达文件末尾。文件开头命中格式也继续扫描。搜索窗口不是归档长度上限：大归档可跨越窗口，无法确定真实末尾时保留未知长度，不按窗口末尾截断。多个归档分别入队并保留独立输出名。

嵌套发现才采用效率限制：默认只读取前 64 MiB、过滤过小载荷及业务容器；`--max-scan-bytes` 和 `--deep` 调整嵌套扫描。根输入不受这些大小、占比及 finding 数量门槛影响。文件扫描采用固定块读取和有界格式解析，不再把整个载体读入内存；可取消。某些格式无法在解析预算内确定边界时保持长度未知，不伪造完整覆盖。

解压产出预算独立生效，不以截断归档扫描的方式实现：

| 参数 | 默认值 | 含义 |
| --- | --- | --- |
| `--max-files` | 0（不限） | 本次工作流累计生成条目，含目录 |
| `--max-output-bytes` | 0（不限） | 累计展开文件逻辑字节数 |
| `--min-free-bytes` | 0（关闭） | 可用磁盘余量下限 |
| `--max-nested-candidates` | 10000 | 新发现的嵌套候选数量 |
| `--recursion-limit` | 3 | 默认递归深度 |
| `--password-limit` | 128 | 从密码库取出的候选上限 |

默认不轮询解压中的输出树，也不查询磁盘余量；解压结束后只做一次产出统计，供历史和恢复使用。显式设置文件数或字节上限时，才启用至少间隔 1 秒的检查，慢目录按扫描耗时继续退避，并在提交前终检；只设置余量下限时仅轮询容量，不反复遍历输出树。任一上限设为 0 即关闭该项，关闭容量限制后不调用容量查询。已存在的 TOML 显式限额仍生效。累计统计不会因内层归档被回收而退还，轮询不等同于硬磁盘配额。

允许后端解压符号链接和硬链接，统计与递归发现不跟随符号链接。保留条目绝对路径和 `../` 路径逃逸检查。普通外部后端负责其支持的链接类型；旧编码 ZIP 路径将符号链接推迟到普通文件完成后创建。

TOML 可配置资源默认值，CLI 显式参数优先：

```toml
[limits]
max_files = 0
max_output_bytes = 0
min_free_bytes = 0
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

指定 `--output` 后，解压输出暂存和内嵌归档切片放在目标目录一侧。分卷规范命名只创建符号链接，不复制或 reflink 卷数据：优先在源目录的临时子目录中建立相对链接，源目录不可写或链接不可访问时尝试系统临时目录中的链接；仍不可用则明确报错，不回退复制。创建/清理链接有少量元数据 I/O，后端解压仍需正常读取原卷。未指定输出目录时，仍使用默认输出位置。

先在同盘临时目录完整解压并整理布局。覆盖时先将旧目标移到 `.smartzip-backup-*`，新输出提交成功后才清理旧备份。提交失败会尝试恢复；若其他进程占用了恢复目标，则保留该目标和旧备份并报告恢复路径。启用可写状态和历史时，下一次解压入口会对未完成任务的暂存、备份和提交状态对账；已经发布的输出补记完成，未发布的节点清理后重做。无状态运行不创建或同步恢复标记。身份不一致时明确失败；不承诺断电后的文件系统持久性。恢复范围与升级备份见 [桌面指南](desktop-beta.md#数据与恢复)。不要在解压期间并发修改同一输出位置。

Linux 上优先使用原子防覆盖重命名。NFS 等文件系统不支持该操作（返回 `EINVAL`、`EOPNOTSUPP`，或系统返回 `ENOSYS`）时，提交、回滚和恢复自动采用“检查目标不存在，再普通重命名”的兼容路径；已有目标（包括悬空符号链接）仍会被拒绝，但检查与重命名之间存在竞态，不能保证不覆盖其他程序在此间新建的目标。部分 NFS 服务端重命名会改变 inode，身份核对因此允许使用同一设备上的类型、大小和修改时间匹配，无法区分这些属性完全相同的替换对象。请避免同时修改同一输出路径。

CLI 根输入源文件保留；GUI 可通过共享引擎的源归档回收选项在全部成功后回收原包及实际使用的整组分卷。成功解压的受管理输出树内的嵌套归档可能移入回收站，回收失败提示并保留；未成功的内层归档保留。历史用于复用密码、人工确认编码和诊断。`extraction.reuse.skip_completed` 默认关闭；开启后按既有成功解压历史跳过同一内容，不要求旧输出仍存在或位于原处。`--force` 可重解。身份使用头尾采样哈希与大小，大文件只改中段可能无法区分。列目录不会验证内容密码或写成功解压事实。

密码数据库与导出内容为明文。Unix 数据库文件权限强制为 0600；请使用仅自己可访问的父目录。命令行 `-p` 可能进入 shell 历史和进程参数，交互输入避免这两种暴露。存储候选串行尝试，默认 128 个，可使用 `--password-limit 0` 禁用库候选，手动 `-p` 不占用库上限。密码错误、缺少密码或无法区分密码错误与加密数据损坏时，继续尝试有界候选；歧义结果不计密码失败统计，候选耗尽仍保留歧义。后端、权限、明确损坏、资源限制等错误不继续遍历密码表。

提取路径用每个候选密码直接 `extract`，失败暂存目录清理后再尝试下一个；不再先完整 `test`。路径和链接检查仍使用后端目录清单，独立 `test` 命令保留完整校验。不承诺全库深度密码搜索、剪贴板接入、命名密码表、祖先链优先级、并行密码池或 Hashcat。

## 脚本接口

`extract`、`test` 的终态为 `completed`、`partial`、`failed`、`cancelled`，任务历史与退出码使用相同结果。正常去重、递归限制和用户选择跳过不计为错误。`detect` 的 unreadable 和全部失败的编码预览返回 1。

| 退出码 | 含义 |
| --- | --- |
| 0 | 成功，包括无错误的主动跳过 |
| 1 | 失败，没有成功完成的输入 |
| 2 | 部分成功；命令行参数解析错误也使用 2 |
| 130 | 用户取消 |

JSON 输出只写 stdout；提示和进度写 stderr。提取对象包含 `task_id`、`status`、`failed_count`、`processed_count`、`skipped_count`、`enqueued_count`、对应数组、`events`、`exit_code`。`test` 保留其分组报告模式；`encoding-preview` 保留候选数组，每项含 `encoding`、`ok`、`names`、`error`。运行时的早期错误返回 `{schema_version:1,status,exit_code,error}`，doctor 使用版本 1 对象。参数解析错误由 clap 输出 stderr。各命令的既有 JSON 形状尚未合并成统一封套，beta 中新增字段时消费者应忽略未知字段。

不承诺压缩创建、完整格式预览、归档内部字节级续传或稳定版兼容性。`--dry-run` 当前仅解释配置，不是实际输入/嵌套结果预演；剪贴板密码尚未接入，显式启用会报错。
