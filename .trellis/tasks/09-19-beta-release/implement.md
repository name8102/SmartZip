# Beta 发布收尾记录

日期：2026-09-19。基线 `b4e5513` 加本轮工作区修改；未提交、推送或发布。

## 实现

- 修复真实 CLI 验收发现的并行根任务去重回归：各根独立的候选/分卷集合改为任务内共享。同一归档重复传入或同组多卷只执行一次，跳过保持成功终态；保留每个输入的持久历史行。`verify_cli_beta.py` 增加 extracted/skipped/duplicate 历史断言。
- GUI worker 后端测试接受实际可用的 `7zz` 或 `7z`，不再因可执行名称不同而无法在 Linux 验收。
- 增加 `verify_recovery.py`：实际 CLI 升级 v5 数据库；真实 7-Zip 暂存完成但未交还成功时 SIGKILL；Linux 通过测试专用 renameat2 动态库分别在备份后、发布前、发布后 SIGKILL，再以不同输入触发恢复。核对输出字节、源 hash、唯一成功历史、累计预算以及暂存/备份清理。产品代码没有测试故障开关。另验证持有执行期间 list 可用，第二执行所有者被拒绝。
- 增加资源检查：192 MiB 空前缀后的 ZIP；4000 小文件超过 2000 条预算；3 个根共享 3 MiB 字节预算。低空闲空间、动态膨胀与取消清理继续由既有 CLI beta 脚本覆盖。仅作为有界安全/资源证据，不宣称性能提升或全面压力覆盖。
- 桌面包包含 CLI、GUI、说明、版本/构建系统/动态依赖清单和 SHA-256。Linux 安装器保护未知或用户修改目录，失败恢复旧安装，卸载不删除外部状态。macOS 安装器支持直接安装签名后的现成 `.app`；GUI/CLI 版本统一为 `0.1.0-beta.1`。
- 发布 CI 扩为 Linux/macOS workspace、显式 GUI worker、安装器、解包后 CLI/交互/恢复/资源验收与安装升级卸载；CLI-only 和桌面包的 CLI 字节一致性检查。测试报告作为独立 artifact 保存。保留 beta 标签触发 prerelease，未在本轮触发。
- 同步 README、当前实现进度、需求范围、领域上下文、CLI 指南、桌面安装/升级/恢复说明和发行说明，消除旧“恢复未实现”、扫描提前结束及历史去重描述。

## 已执行验证

- 本机 CachyOS x86_64、真实 `/usr/bin/7z`。release 产物使用本机默认 Rust 1.100 nightly 构建；CI 固定 Rust 1.97.1 的额外核对结果见下方追加记录。未改变用户默认工具链。
- `cargo test --locked --workspace --no-fail-fast`：577 passed、0 failed、6 ignored（其中 5 项 GUI 真实后端随后单独运行；1 项是父进程死亡测试 helper）。这是 Cargo 各目标执行计数，包含 GUI 集成目标重复编译的模块测试，不等于独立场景数。
- `cargo test --locked -p smartzip-gui --test runtime_worker -- --ignored`：5/5 真实后端通过；没有启动 GUI 窗口。
- `cargo build --release --locked -p smartzip-cli -p smartzip-gui`：通过。
- 解包后的最终 release CLI：23 组常规、5 组交互回归、6 组恢复/迁移/所有权、3 组资源检查通过。结果见 `research/cli.json`、`regressions.json`、`recovery.json`、`resources.json`。
- `python3 -m unittest discover -s scripts -p 'test_*.py'`：7/7 安装/打包测试通过，包括 Linux 用户修改保护、替换失败回滚与未知目录/链接保护；macOS 合成安装测试不代表原生签名验收。
- Linux 桌面包 SHA-256、解包、独立目录安装、升级、卸载及数据库保留通过，见 `research/package.json`。CLI-only 与桌面包 CLI 字节一致。
- fmt、routing guards、diff 空白检查通过；发布 YAML 解析通过。既存 platform 等编译 warning 保留。

## 交付与仍需平台验证的边界

- 本机候选包在 `target/beta-dist/`，仅供同环境测试。GUI 引用了 GLIBC_2.44，**不是 Ubuntu 24.04 通用发行包**；正式 Linux 包由 Ubuntu 24.04 CI 生成。
- 尝试本机容器验证 Ubuntu 基线时，Podman 因 `newuidmap: Could not set caps` 无法启动。没有修改系统权限或以宿主安装代替干净系统验收。
- 本轮未运行远端 CI，macOS 最新版本的构建、真实安装/签名及提交中断现场验证尚未完成。工作流存在不能视为两平台已经通过。
- 原生 GUI 操作与系统集成验收按用户要求保留给用户；无窗口 worker 通过不证明视觉、拖放、输入法或系统文件关联。
- ad-hoc 签名不等于 Apple 公证。资源检查不证明全格式/NAS/GPU 性能；检查点预算不是硬配额，恢复不承诺断电持久性。

## 固定工具链核对

另外安装并显式使用 CI 相同的 Rust 1.97.1，`cargo +1.97.1 check --locked --workspace` 和 `cargo +1.97.1 fmt --all --check` 均通过；没有修改默认 nightly 工具链。此项是固定工具链编译/格式证据，运行测试和本机 release 包仍对应前述 nightly 构建。CI 显式使用 bash 的 pipefail，防止验收脚本经 tee 保存报告时掩盖失败退出码。

## 未使用代码告警清理

用户要求按真实调用关系删除废代码或做最小标注。复核发现原 13 条 dead_code 均为 macOS 实现或其跨平台纯逻辑测试使用的辅助定义，没有删除有效功能：Finder 辅助定义/导入使用 `cfg(any(target_os = "macos", test))`，仅原生使用的 `info_plist`、`required_bundle` 使用 `cfg(target_os = "macos")`；关联白名单校验保留测试条件。`tempfile` 从通用运行依赖移到 macOS 运行依赖，保留测试依赖。没有添加 `allow(dead_code)`。

本次 Linux 验证：platform 5 项测试通过；`cargo rustc --locked -p smartzip-platform --lib -- -D warnings` 通过；`cargo check --locked --workspace --all-targets` 和 CLI/GUI release 构建通过，四份日志均无 warning/error；Rust 1.97.1 fmt 与 diff 检查通过。未做 macOS 原生构建；此前打包产物未重新生成，此项证据对应当前源码及新构建的 `target/release` 二进制。
