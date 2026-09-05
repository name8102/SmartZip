# SmartZip

SmartZip 是一个用 Rust 重写的跨平台压缩包辅助工具，目标是把**检测、递归解压、密码管理、编码处理**整合到统一工作流中。

> 说明：仓库已经从旧版 AHK 实现迁移到 Rust 工作区，旧脚本、旧截图和相关遗留资源已清理。

## 当前能力

- **检测**：识别嵌入式压缩包和伪装成普通文件的压缩数据
- **列出内容**：`list` 共享密码与编码处理；`enc` 可查看不同编码下的文件名
- **校验**：`test` / `t` 完整校验归档，失败后自动诊断分卷，区分确认损坏、疑似组、缺失与未检查范围
- **解压**：支持递归/嵌套解压，并可按需控制最大递归深度
- **密码管理**：支持密码列表查看、添加、删除、导入、导出与清理
- **编码处理**：支持自动识别与手动指定文件名编码
- **工作区**：CLI、GUI、核心库、扫描、密码、平台适配等模块分层组织

## 当前状态与下一步

- 能力路由整合已落地：后端按能力、配置与归档要求选择，CLI 与 engine 使用统一执行入口；文件级历史与密码/编码记忆保留。
- `test` 已接通后端、自动诊断、JSON 和历史报告；[分卷定位说明](.trellis/tasks/2026-07/07-03-test-command-backend-split/design.md) 记录证据规则与格式边界。压缩命令与 GUI 不在 CLI beta 范围。
- CLI beta 已加入可恢复覆盖提交、扫描与产出预算、Ctrl+C、非交互策略和 `doctor`。安装、平台范围、退出码、JSON 与限制见 [CLI beta 指南](docs/cli-beta.md)。设计草案中超出本轮的交互能力仍待实现。
- 当前核对结果与已知缺口见 [实现进度](docs/implementation-progress.md)。

## 快速开始

全局安装 CLI（安装到 Cargo 的用户级 bin 目录）：

```bash
just install
smartzip --help
smartzip doctor
```

查看帮助：

```bash
cargo run -p smartzip-cli -- --help
```

常用命令示例：

```bash
cargo run -p smartzip-cli -- detect <path>
cargo run -p smartzip-cli -- list <path>
cargo run -p smartzip-cli -- list <path> --encoding gb18030
cargo run -p smartzip-cli -- enc <path>
cargo run -p smartzip-cli -- extract <path>
cargo run -p smartzip-cli -- test <任意一卷>
cargo run -p smartzip-cli -- t movie.part03.rar other.zip --json
cargo run -p smartzip-cli -- password list
cargo run -p smartzip-cli -- password add <password>
```

常用短别名：

| 命令 | 短别名 |
| --- | --- |
| `extract` | `x` |
| `list` | `l` |
| `detect` | `d` |
| `test` | `t` |
| `password` | `pw` |
| `history` | `hist` |

编码预览直接使用 `smartzip enc <path>`；旧名称 `encoding-preview` 保留兼容。短别名与完整命令使用相同参数，例如 `smartzip x archive.zip`、`smartzip pw list`、`smartzip hist files`。

`extract` 支持常见参数，例如：

- `--output <dir>`：指定输出目录
- `--deep`：启用深度扫描
- `--encoding <name>`：指定文件名编码
- `-p/--password <value>`：预置密码

`test` 接受多个归档或任意分卷，同组输入只校验一次。默认 `--diagnose auto`，失败后追加只读校验和至多一次不同后端复核；`--diagnose off` 关闭追加诊断，`--diagnostic-timeout 30` 限制追加阶段为 30 秒，`--no-history` 不保存任务记录。JSON 模式不提示输入，可用 `-p` 提供密码。

RAR5 的独立局部校验可以确认具体坏卷；跨卷 ZIP 数据、7z solid 依赖或无法解密的元数据通常只能给候选组或未知范围。疑似组不代表组内每卷都坏。退出码为 `0` 全部完整、`1` 无组完整、`2` 部分组完整、`130` 取消；参数错误仍使用 `2`。

当前 `--db`、`--config`、`--backend`、`--verbose-routing` 是根层参数，必须放在子命令前。例如指定数据库：

```bash
cargo run -p smartzip-cli -- --db ./smartzip.db extract <path>
```

当前 `--use-clipboard` 尚未接线；`list --pick-encoding` 只显示编码名称。需要文件名对照时先使用 `enc`，再通过 `--encoding` 指定。

## 工作区结构

- `crates/smartzip-cli`：命令行入口
- `crates/smartzip-engine`：解压与扫描编排
- `crates/smartzip-archive`：压缩包后端抽象
- `crates/smartzip-scanner`：嵌入式压缩包/伪装数据扫描
- `crates/smartzip-passwords`：密码候选与排序逻辑
- `crates/smartzip-db`：密码数据库
- `crates/smartzip-config`：配置加载
- `crates/smartzip-core`：共享类型、错误与进度事件
- `crates/smartzip-platform`：平台相关能力
- `crates/smartzip-gui`：图形界面
- `docs/`：需求、设计、实现进展等文档

## 开发

构建：

```bash
cargo build
```

测试：

```bash
cargo test
```

复杂度/覆盖风险扫描：

```bash
scripts/crap-scan.sh
```

说明：

- 默认只针对 `smartzip-engine` 和 `smartzip-cli` 收集覆盖率并运行 `cargo-crap`
- 使用临时 XDG 目录，避免平台路径测试把扫描流程直接打断
- `scripts/crap-scan.sh --quick` 可跳过覆盖率采集，只看复杂度热点
- 当前建议把它作为调查/重构前的辅助检查，不作为全 workspace CI 阻塞门禁

## 文档

- `docs/requirements.md`
- `docs/design.md`
- `docs/implementation-plan.md`
- `docs/implementation-progress.md`
- [CLI 交互设计草案](.trellis/tasks/09-05-cli-interaction-design/design.md)
- `docs/agents/`
- `docs/compose/plans/`
- `docs/research/`
- `CONTEXT.md`

## 许可证

MIT
