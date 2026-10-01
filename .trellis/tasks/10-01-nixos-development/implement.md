# NixOS 适配与开发环境（2026-10-01）

旧 GUI 在本机启动时报 `libxcb.so.1` 缺失；`ldd` 同时发现 liblzma、libbz2 与 XKB 库缺失。旧 ELF 使用 `/lib64/ld-linux-x86-64.so.2`，现有 NixOS 兼容加载器只能让部分旧程序启动，不能替代项目的依赖声明。

实现与日常命令统一维护在 [Nix 指南](../../../docs/nix.md)：

- Nixpkgs 26.05 使用固定发布快照和内容哈希；rust-overlay 固定提交与哈希，明确选择 Rust 1.98.1。开发 shell 使用 Python 3.14.7，默认输出到 `target/nix`。
- `buildRustPackage` 按 `Cargo.lock` 构建 CLI 与 GUI。启动包装器保留 7-Zip 和 GUI 运行依赖，桌面文件包含归档 MIME 类型和快速解压操作。
- Linux 应用内注册使用包装器注入的 `SMARTZIP_GUI_LAUNCHER`，避免生成绕过依赖设置的桌面入口；普通未包装启动继续使用 `current_exe`。
- README 与 beta 指南原先仍称 Linux 系统集成未实现，与当前源码不符；已修正为实际支持的注册、默认关联及桌面动作范围。

本机验证（NixOS x86_64、niri Wayland）：

- `cargo test -p smartzip-platform --locked`：7 项通过，包含包装器路径和无效路径的注册回归检查。
- Python 安装器测试：12 项通过。
- `nix flake check --no-build --all-systems`：x86_64 / aarch64 输出求值通过。
- `nix build .#smartzip --cores 4`：CLI 与 GUI release 构建通过；ELF 使用 Nix store 加载器，动态链接依赖全部解析。
- `nix develop -c cargo check -p smartzip-gui --locked -j 2`：通过；另确认 Rust/Cargo、rust-analyzer、Python、7-Zip 和 pkg-config 使用 shell 的固定版本。
- `nix flake check --cores 4`：安装后 CLI 通过真实 7-Zip 的 doctor、列出、完整性校验、解压及内容比对。
- GUI 用临时 XDG 配置和数据目录启动，niri 确认创建标题为 SmartZip 的真实窗口；检查后退出。未验收拖放、输入法、关联操作或 aarch64 实际构建。
- 格式检查及 `git diff --check` 通过。
- `.envrc` 已在本机授权，`direnv exec . rustc --version` 自动加载固定的 Rust 1.98.1。

已安装到本机用户 Nix profile。旧 Cargo CLI 与生成的用户桌面文件备份到 `~/.local/state/SmartZip/migration-2026-10-01/`，原 GUI 安装目录保留；配置、数据库及用户输出未迁移或删除。安装后的默认 CLI/GUI 均指向 Nix profile，CLI 的 stateless doctor 通过。

首次官方源下载缓慢；本机从镜像获取工具链并逐项校验 rust-overlay 的固定哈希，同时用 `Cargo.lock` 校验已有 crate 缓存后预热 Nix store。该步骤只加速本机缓存，仓库仍使用固定的官方来源和校验值。
