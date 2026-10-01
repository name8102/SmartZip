# NixOS 运行与 Nix 开发环境

SmartZip 的 flake 提供 Linux x86_64 / aarch64 的 CLI、GUI 和开发 shell。`flake.lock` 固定 Nixpkgs 26.05 的快照及内容哈希；Rust 1.98.1 通过同样锁定的 rust-overlay 提供，工具链下载也有固定哈希。Cargo 依赖仍由 `Cargo.lock` 固定。首次使用需要启用 Nix 的 `nix-command` 和 `flakes` 实验功能。

## 运行、安装与升级

在仓库目录执行：

```sh
nix run . -- --help
nix run . -- doctor
nix run .#smartzip-gui
nix build .#smartzip
./result/bin/smartzip doctor
```

包内提供 `smartzip` 和 `smartzip-gui`，并为两个入口补充 7-Zip 的 PATH。GUI 入口还提供 X11/Wayland、字体、Vulkan/OpenGL 动态库和桌面集成命令；桌面文件声明支持的归档 MIME 类型与快速解压操作，不设置默认文件关联。配置、数据库和任务历史继续使用原来的 XDG 路径。

安装到当前用户的 Nix profile：

```sh
nix profile add .#smartzip
smartzip doctor
smartzip-gui
```

确认 `~/.nix-profile/bin` 在 PATH，`~/.nix-profile/share` 在桌面会话的 `XDG_DATA_DIRS` 中。NixOS 通常已配置这些路径；必要时重新登录桌面会话。运行 GUI 仍需主机提供桌面会话、字体和可用 GPU 驱动，NixOS 驱动通过 `/run/opengl-driver/lib` 使用。

源码更新后，查看 `nix profile list` 中 SmartZip 的名称，再执行 `nix profile upgrade <名称>`。回退最近一次 profile 变更使用 `nix profile rollback`；移除使用 `nix profile remove <名称>`。这些操作只管理程序，不删除配置、数据库或解压结果；旧程序是否支持当前数据库 schema 仍遵循[桌面 beta 指南](desktop-beta.md)的恢复边界。

## 从旧 Linux 安装迁移

传统发行版构建的 ELF 使用 `/lib64/ld-linux-x86-64.so.2` 等加载器路径，并依赖系统动态库。NixOS 的库位于 `/nix/store`，直接复制旧程序可能在进入应用前报加载器或动态库错误。Nix 包重新编译并保留运行依赖引用，不需要修改全局 `nix-ld` 配置。`nix develop` 提供新构建的开发环境，不负责修补旧二进制。

旧 `just install` 写入的 `~/.local/share/applications/org.smartzip.SmartZip.desktop` 会优先于 Nix profile 中的桌面文件。安装 Nix 包后，如该文件仍指向旧安装，将它移到应用目录以外备份，再重新登录。若设置了 `XDG_DATA_HOME`，检查对应目录下的 `applications`。不要覆盖用户自定义启动器或删除配置数据库。

使用 `command -v smartzip` 检查 CLI 是否仍被 `~/.cargo/bin/smartzip` 或 `~/.local/bin/smartzip` 遮蔽；可调整 PATH 优先级或备份旧入口。旧受管理的 GUI 目录可先保留，确认新版本后按原安装器卸载。

也可在 NixOS 或 Home Manager 配置中把本仓库作为 flake input，并加入 `inputs.smartzip.packages.${pkgs.stdenv.hostPlatform.system}.smartzip`，分别放在 `environment.systemPackages` 或 `home.packages` 中。使用配置的 lock 管理源码版本。

## 开发与验证

```sh
nix develop
rustc --version
cargo build --workspace --locked
cargo test --workspace --locked
cargo run -p smartzip-gui
python3 -m unittest discover -s scripts -p 'test_*.py'
```

开发 shell 提供 Rust/Cargo、rustfmt、Clippy、rust-analyzer、Clang、pkg-config、bindgen 所需 libclang、压缩与 GUI 原生库、7-Zip、Python、just 和常用验证工具。PATH 使用 Nix 工具链，不需要在 NixOS 上通过 rustup 下载通用 Linux 编译器。仓库的 `rust-toolchain.toml` 继续供非 Nix 环境使用。

默认构建目录为 `target/nix`，可通过 `CARGO_TARGET_DIR` 覆盖。`nix develop -c cargo build --locked` 可以直接运行一次命令。仓库附带 `.envrc`；安装 direnv / nix-direnv 并启用 direnv 的 shell hook 后，`direnv allow` 可自动进入环境。

```sh
nix fmt flake.nix nix/*.nix
nix flake check
```

flake 的 smoke check 在隔离环境中运行已安装的 CLI，使用真实 7-Zip 完成后端诊断、列出、完整性校验和解压，并比对内容。它不启动原生 GUI；GUI 的拖放、输入法和文件关联仍需桌面验收。Nix 包构建不重复运行全 workspace 测试，需要时使用上面的开发 shell 命令。

## 更新固定版本

```sh
nix flake update nixpkgs rust-overlay
nix develop -c rustc --version
nix flake check
```

更新 Rust 时同时修改 `flake.nix` 中明确的版本号，检查工具链符合 `Cargo.toml` 的最低版本；完成受影响验证后提交 Nix 配置及 `flake.lock`。升级 Nixpkgs 稳定分支时，同时修改 `flake.nix` 的 input URL。不能只依赖设备的全局 channel 或 registry，否则不同机器的环境无法由仓库锁定。

打包方法采用 [Nixpkgs 的 Rust 构建支持](https://nixos.org/manual/nixpkgs/stable/#rust)；旧二进制的加载路径问题见 [NixOS 二进制打包说明](https://wiki.nixos.org/wiki/Packaging/Binaries)。
