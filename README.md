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
- `test` 已接通后端、自动诊断、JSON 和历史报告；[分卷定位说明](.trellis/tasks/2026-07/07-03-test-command-backend-split/design.md) 记录证据规则与格式边界。压缩创建不在当前产品范围；桌面 beta 同时提供 GUI 与 CLI。
- CLI beta 已加入可恢复覆盖提交、扫描与产出预算、Ctrl+C、非交互策略和 `doctor`。安装、平台范围、退出码、JSON 与限制见 [CLI beta 指南](docs/cli-beta.md)。设计草案中超出本轮的交互能力仍待实现。
- CLI 与 GUI 已接入[统一配置](docs/configuration.md)；GUI 提供全局与单任务快速配置，以及共享校验的分组设置表单。
- GUI 已有共享单并发解压队列的快速/详细双窗口、拖放解压、进度与后端显示、检测/列出/校验，以及历史和密码管理入口；原生实施与完整能力验收仍在进行，见 [GUI 实施记录](.trellis/tasks/09-12-gui-design/implement.md)。
- 安装、升级、恢复边界和发布验收见 [桌面 beta 指南](docs/desktop-beta.md)。
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

`--config` 可在子命令前后使用；`--db`、`--backend`、`--verbose-routing` 放在子命令前。例如指定数据库：

```bash
cargo run -p smartzip-cli -- --db ./smartzip.db extract <path>
```

当前 `--use-clipboard` 尚未接线，显式使用会报错；`list --pick-encoding` 只显示编码名称。需要文件名对照时先使用 `enc`，再通过 `--encoding` 指定。

启动原生 GUI：

```bash
cargo run -p smartzip-gui
# 打开归档，默认预览内容
cargo run -p smartzip-gui -- /path/to/archive.zip
```

macOS 一键构建、打包并安装（macOS 12+）：

```bash
just install-gui                      # release，安装到 ~/Applications/SmartZip.app
just install-gui debug                # 安装 debug 版
just install-gui release /Applications/SmartZip.app  # 自定义安装位置，需有写入权限
# 不使用 just：
python3 scripts/install-macos.py
```

脚本校验签名后替换同标识的旧应用，替换失败会恢复旧版；不会覆盖其他应用，也不会自动更改默认程序或右键菜单。已有 `just install` 仍用于安装 CLI。

仅打包、不安装：

```bash
cargo build -p smartzip-gui --release
python3 scripts/package-macos.py --profile release
```

把生成的 `target/release/bundle/SmartZip.app` 放到固定位置（例如 `/Applications`），从该应用启动后进入“系统集成”：

- “注册打开方式”将 SmartZip 加入系统候选程序；按格式点击“设为默认”可配置 ZIP、7z、RAR、TAR、GZ、BZ2、XZ、ZST。
- “安装右键菜单”添加 Finder → 快速操作 → SmartZip 快速解压，支持多选；同页可移除。若系统未显示该项，可在系统设置的扩展/快速操作中启用。
- 普通启动进入任务中心；系统打开归档进入预览；右键快速解压进入快速窗口，沿用当前队列暂停状态和快速配置。
- 归档预览按内部目录浏览，支持面包屑和返回上级；点击文件可预览受支持的文本或图片。密码管理用于持久密码库，临时密码在排队任务详情中设置。

打包与启动不会自动更改默认程序。右键菜单绑定应用当前位置，移动应用后需重新安装该菜单。当前系统集成实现针对 macOS；Windows/Linux 尚未接入。

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
- [GUI 双窗口设计与组件调查](.trellis/tasks/09-12-gui-design/prd.md)（设计已完成，原生实施进行中）
- `docs/agents/`
- `docs/compose/plans/`
- `docs/research/`
- `CONTEXT.md`

## 许可证

MIT
