# SmartZip desktop beta

首发目标为 Linux x86_64（Ubuntu 24.04+）与 macOS arm64（macOS 14+），提供 GUI 与 CLI。产品聚焦智能解压、归档浏览/有限内容预览、任务调度与恢复；不提供压缩创建。Windows 不在本次发布承诺内。

工作流通过表示自动验证完成，不代表原生 GUI 已验收。发布候选的拖放、窗口、输入法、显示和系统集成由用户手工验收；创建 beta 标签前须完成。当前结果与未验证边界见仓库中的 `.trellis/tasks/09-19-beta-release/implement.md`。

## 下载与依赖

桌面包名为 `smartzip-desktop-<version>-<target>.tar.gz`，包含 CLI、GUI、安装脚本、说明、版本清单和动态依赖清单。只需要 CLI 时可选择 `smartzip-<version>-<target>.tar.gz`。下载后先验证 `.sha256`：

```sh
# Linux
sha256sum -c smartzip-desktop-*.tar.gz.sha256
# macOS
shasum -a 256 -c smartzip-desktop-*.tar.gz.sha256
```

不捆绑 7-Zip，也不自动下载后端。Linux Ubuntu 24.04 运行依赖：

```sh
sudo apt-get install 7zip liblzma5 libbz2-1.0 libxcb1 libxkbcommon0 libxkbcommon-x11-0 libwayland-client0 libvulkan1 mesa-vulkan-drivers fontconfig
```

需要可用的桌面会话、GPU 驱动和字体。`release.json` 记录实际构建系统与 libc；只有 Ubuntu 24.04 runner 构建并验证的包才具有该基线承诺，本地其他发行版生成的包仅供对应环境测试。实际链接库记录在 `dynamic-dependencies.txt`；`ldd bin/smartzip-gui` 可检查缺失库。这里不承诺任意发行版、无显示服务器环境或所有 GPU 上可运行。

macOS 先执行 `brew install sevenzip xz`。应用包仅做 ad-hoc 签名，未做 Developer ID 签名或 Apple 公证；下载的包可能被 Gatekeeper 拦截。确认来源后，通过系统设置中的“隐私与安全性”处理明确的单应用许可，不关闭全局保护。`otool -L SmartZip.app/Contents/MacOS/smartzip-gui` 可检查链接路径。Finder 启动的精简 PATH 由后端发现补充常见安装目录，也可在设置中配置绝对路径。

安装脚本需要 Python 3.11+；支持安全解包过滤器的版本用于自动包验收。

## Linux 安装、升级与卸载

解包后在包目录执行：

```sh
python3 install-linux.py
~/.local/opt/SmartZip/bin/smartzip doctor
~/.local/opt/SmartZip/bin/smartzip-gui
```

可用 `--destination /完整路径/SmartZip` 指定专用安装目录。安装器先复制再替换；更新时校验旧目录归属及文件内容，发现用户修改或额外文件就拒绝覆盖。替换失败恢复旧版本。它不修改 PATH、文件关联或桌面默认程序。

升级前退出应用，再从新版解包目录执行同样命令。卸载：

```sh
python3 install-linux.py --uninstall
```

卸载只删除安装目录，不删除配置、数据库、用户输入或解压输出。不要把用户资料存进安装目录。

## macOS 安装、升级与卸载

从解包目录运行，默认安装到 `~/Applications/SmartZip.app`：

```sh
python3 install-macos.py --source ./SmartZip.app
./bin/smartzip doctor
open ~/Applications/SmartZip.app
```

安装器验证签名，仅更新相同标识应用，失败回滚；不自动设置默认打开方式。CLI 可单独复制到用户 PATH。升级前退出应用，从新包重复安装。卸载时删除该 `.app`；配置和数据库保留。若曾在应用中安装 Finder 服务，先在系统集成页面移除，避免留下指向旧路径的服务。

macOS 文件关联和 Finder 快速操作已有实现，需在真实系统由用户验收；Linux 默认关联/右键菜单管理暂未实现。

## 数据与恢复

配置、数据库位置以 `smartzip doctor` 和配置页面为准。升级前关闭 CLI/GUI，备份配置及数据库；如存在数据库 `-wal`、`-shm` 文件，将其与数据库一起备份。数据库当前 schema 为 v7，会从旧版本向前迁移；不支持降级写入，回退旧二进制时应同时恢复升级前备份。

启用可写状态与任务历史时，下一次 CLI/GUI 解压入口会对未完成任务对账并恢复。仅打开应用、列目录或 `doctor` 不会启动恢复。恢复沿用旧任务的非秘密配置快照；临时密码不写入快照，需要时重新输入。已暂停任务不会自动调度。扫描和未完成归档节点从阶段头重做，不是归档内部字节级续传；旧版本历史不被当作待恢复任务。

覆盖提交会记录旧目标、暂存和提交身份；提交已经完成则补记事实，尚未发布则恢复/清理后重试。身份不一致时失败并保留可诊断证据，不能保证断电后的文件系统持久性。遇到恢复错误不要手工删除 `.smartzip-*`，先保存报错与相关路径。当前自动进程中断验收包含 Linux 提交前后的真实 SIGKILL；macOS 提交路径还需其平台验收，不能由 Linux 结果外推。

同一个数据库只允许一个执行系统所有者；CLI 与 GUI 同时解压使用相同库时，另一进程会明确报错，不会抢占恢复任务。运行时目前采用保守的一个外部后端进程及同设备 I/O 准入；没有用户可配置的自适应并发、进程级内存或临时空间预约。文件/字节/空闲空间预算是检查点限制，详见 `cli-beta.md`。

GUI 支持 UTF-8 文本（1 MiB）和 PNG/JPEG（8 MiB、最多 1600 万像素）的有限内容预览；其他格式、部分分卷/内嵌读取路径需解压后查看。不承诺完整格式预览、全量虚拟化目录树或性能基准。

## 发布验证

`Beta release` 工作流在两平台运行 workspace 测试、显式 GUI worker 后端测试、安装器测试，并对解包后的 CLI 运行常规回归、交互回归、恢复及资源检查。Linux 还进行测试专用 rename 故障注入。桌面包做临时目录安装、升级与卸载检查；自动验证不会打开原生窗口。

创建匹配版本号的 `v*-beta.*` 标签会在两平台自动门禁均通过后发布 prerelease。版本标签应在用户完成 GUI 验收后创建；构建脚本和 CI 定义本身不证明该版本已在两平台成功运行。
