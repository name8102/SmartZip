# 当前平台 CLI + GUI 安装入口

- `just install [release|debug] [GUI目标目录]` 默认同时安装 CLI 和 GUI；`install-gui` 使用相同平台分派，仅安装 GUI。
- 新增 `scripts/install_desktop.py`：从 Cargo JSON 产物读取实际二进制路径，不硬编码架构、target triple 或 target 目录。CLI 沿用 cargo install，保留 Cargo 安装配置与跟踪。
- Linux 生成 `bundle/SmartZip`，复用已有 Linux 安装器安装到 `~/.local/opt/SmartZip`，在 XDG 应用目录创建启动器；macOS 复用现有签名打包与安装器，安装到 `~/Applications/SmartZip.app`。不更改文件默认关联。
- Python 安装测试 10 项通过，包括平台分派、自定义 Cargo 产物路径、GUI-only、Linux 升级和启动器、已有 Linux/macOS 回滚测试。
- 真实执行 `just install debug <临时目录含空格路径>`，使用临时 `CARGO_INSTALL_ROOT` 和 `XDG_DATA_HOME` 验证 CLI 版本、GUI 可执行文件、打包产物及启动器。临时安装已清理；未替换用户已安装版本。
- `git diff --check`、Python 编译检查通过。macOS 流程仅通过平台分派 mock 与既有安装单元测试，未在 macOS 实机执行。本次真实安装验证使用 debug，默认 release 构建命令已接入。
