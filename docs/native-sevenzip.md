# 原生 7-Zip 构建与分发

SmartZip 的归档 worker 使用官方 7-Zip **26.06** 的 `Format7zF` 全格式动态库，包含 RAR 解码器；运行时不调用 `7z`、`7zz` 或 p7zip。版本、官方源码 URL 与 SHA-256 统一固定在 [`nix/sevenzip-source.json`](../nix/sevenzip-source.json)。版本依据：[7-Zip 官方下载页](https://www.7-zip.org/download.html)（26.06，2026-11-06）。

## Linux / macOS 本地开发

需要 Python 3.12+、GNU make、C/C++ 编译器；macOS 使用 Xcode Command Line Tools。示例使用 release；自定义 `CARGO_TARGET_DIR` 时把输出路径换成 Cargo 实际报告的目录。

```sh
cargo build --locked --release -p smartzip-cli -p smartzip-gui -p smartzip-archive
python3 scripts/package_sevenzip.py --output target/release/lib
./target/release/smartzip doctor
```

构建脚本下载固定版本源码、先验证 SHA-256，再在临时目录构建，保留所有格式与 RAR decoder。使用可移植 C/C++ 实现，不依赖外部汇编器；保留编译警告，不将上游源码在新编译器中的警告视为错误。离线构建显式传入已下载的原始源码包，同样验证哈希：

```sh
python3 scripts/package_sevenzip.py --source-archive /path/to/7z2606-src.tar.xz \
  --output target/release/lib --jobs 4
```

`just install` / `just install-gui` 会构建 CLI、GUI、worker 和原生库，随后安装。设置 `SMARTZIP_7Z_LIBRARY` 时复用该库及配套分发材料，不另行下载构建。Nix 开发 shell 已提供固定库；先 `cargo build -p smartzip-archive` 生成 worker，再执行 CLI/GUI。

## 加载与包布局

库查找顺序：`SMARTZIP_7Z_LIBRARY`、可执行文件旁 `lib/7z.so`（macOS 为 `7z.dylib`，Windows 为 `7z.dll`）、`../lib/smartzip/7z.so`。worker 使用 `SMARTZIP_ARCHIVE_WORKER` 或应用同目录的 `smartzip-archive-worker`（Windows 带 `.exe`）。

Linux 桌面 tar 包的原生文件在 `bin/`；CLI tar 包在包根目录；macOS 应用在 `SmartZip.app/Contents/MacOS/`。每个可执行文件目录包含：

```text
smartzip-archive-worker
lib/
  7z.so                         # macOS: 7z.dylib; Windows: 7z.dll
  7zip-source.tar.xz             # 构建使用的完整、未修改官方源码
  licenses/7zip/
    License.txt
    copying.txt
    unRarLicense.txt
    source.json
```

打包脚本只复制现有产物，缺 worker、库、源码包或许可证时直接报错，不自动联网。指定外部库时，其同目录需有上述 `7zip-source.tar.xz` 和 `licenses/7zip/`；Nix 固定库的 `share/` 布局也可直接使用。macOS 将动态库、worker 和应用依次做本地 ad-hoc 签名；发行签名、公证由发布环境负责。

```sh
python3 scripts/package_cli_beta.py target/release/smartzip x86_64-unknown-linux-gnu dist
python3 scripts/package_desktop_beta.py target/release/smartzip target/release/smartzip-gui \
  x86_64-unknown-linux-gnu dist
# 在对应 macOS 主机上：
python3 scripts/package-macos.py --profile release
```

Nix 的 `.#sevenzip` 单独输出 `lib/smartzip/7z.so`，许可证放 `share/licenses/7zip/`，完整源码包放 `share/smartzip/`。`.#smartzip` 包含库和 worker；CLI/GUI wrapper 设置两个绝对路径，不修改 PATH 来寻找 7-Zip CLI。开发 shell 中的 `7zz` 仅用于生成测试 fixture。

## Windows 构建

在对应架构的 MSVC Native Tools 命令行中，解开相同固定源码并验证上述 SHA-256，然后执行：

```bat
cd CPP\7zip\Bundles\Format7zF
nmake /f makefile
```

使用生成的 `7z.dll` 与本机 Rust worker，按上述布局准备源码和许可证。当前自动安装脚本面向 Linux/macOS；Windows 的 MSVC 构建与分发、macOS 两架构的编译和签名需在对应主机验收。

## 许可证与替换

SmartZip 自身保持 MIT 许可；7-Zip 的 LGPL、BSD 与 unRAR 限制见包内原文。RAR 解码代码不得用于开发 RAR 兼容压缩器。库通过运行时动态加载，可通过 `SMARTZIP_7Z_LIBRARY` 换成 ABI 兼容的重建库；同包提供含上游 makefile 的固定源码和完整许可证，重建不需要 SmartZip 私有材料。
