# 原生执行落地

2026-11-10。替换产品中的 7-Zip CLI 调用；研究方案保留为历史设计，当前状态以本文和源码为准。

## 执行与数据边界

- `smartzip-sevenzip` 直接加载固定官方 26.06 库，封装 ABI、COM/variant 所有权、可 seek 输入、密码/分卷/输出回调及逐项结果。Session 绑定创建线程；Rust 回调封住 panic。
- `smartzip-archive-worker` 每次执行持有一个 session，目录与解码共用打开状态。私有分帧管道传递元数据、进度、成员字节与原生结果；秘密只走 stdin，OS 路径按原生字节/宽字符传递。
- 与研究草图不同，文件 writer 统一放在父进程：字节块先由全局预算准入，再落盘。这样不需要额外额度 RPC，也没有被终止 worker 留下的写盘句柄。取消会终止并等待 worker；普通输出流结束即关闭文件，不为整包保留全部文件描述符。
- `ArchiveInput` 传递范围和逻辑卷映射。原生输入不 carve、不复制、不建立卷别名树；只有 Unrar 在自己的 adapter 边界按需物化路径。
- `EntryRef` 为来源快照、库路径与原生索引；快照使用文件身份、尺寸、mtime 和卷映射，不全量哈希载荷。编码变更只生成名称视图，不改变成员身份。重开会检查来源，失效引用直接报错。
- 现有 executor/任务系统保留，提取请求增加精确索引集合；一次批量 Extract 为各成员接独立流。GUI 预览按索引读取，重复、换行、通配符和 `@` 名称不再是选择器问题。Prefix 与完整读取分别返回部分/完整状态。
- `sevenzip/output.rs` 单独负责落盘名称、安全路径、写前准入、链接、权限/时间和 manifest。同名文件确定性改名保留全部内容。普通文件完成后才创建安全链接；不写特殊设备或穿过归档控制的链接。
- engine 直接消费 manifest 作为 inventory；原生路径不再轮询/重扫输出树。Unrar 保留有效的外部后端监控；失败目录清理确认后才释放/重置额度，最终提交仍由 OutputMaterializer 负责。

## 删除与结果模型

删除 `sevenzz.rs`、CLI 名称选择器、技术列表/错误/进度解析、`decoded_zip.rs` 的第二套载荷解码、engine 输入 carve/卷别名模块及相应历史协议测试。ZIP central-directory helper 只读取名称、尺寸和编码元数据；直接解析目录记录，保留同名条目和有效 Unicode extra，载荷统一由库解码。

完整性子报告 schema 2 区分 Native、Process、Host 证据。原生结果保留 HRESULT、archive flags、项索引结果、额外结果与流错误；显式拒绝密码与加密数据不确定失败分别处理。列表重试遇到 PasswordIndeterminate 会继续候选，且不将其记录为已确认错误密码。RAR 和 RAR5 是同一领域容器，格式提示只排序候选，不排除签名识别出的实际 handler；能力路由读取库的格式/decoder 表。

配置改用 `family = "seven-zip-library"` 与 `path`，不兼容旧 CLI 配置字段。Nix、桌面/CLI 打包、安装器及 CI 携带 worker、固定库、许可证和完整源码；构建方式见 `docs/native-sevenzip.md`。

## 已执行验证

Linux x86-64：

- 官方源码 Format7zF 真实构建；Nix 开发库同样实际构建。固定版本与哈希在 `nix/sevenzip-source.json`。
- 底层原生验收 2 项：ZIP/TAR/GZip/XZ、同名及特殊名称、非 BMP 文本/密码、加密头、CRC、I/O、panic 封边、取消、SFX、库/session 生命周期、RAR5 八卷与缺卷；RAR 回归显式使用 `format="rar"` 提示。
- archive 库单测 45 项通过（另有 1 个供父进程测试调用的忽略 helper）；物理分卷诊断 10 项通过。
- 真实 worker 验收 6 项：独立索引读取、同名输出、Prefix、子集提取、范围输入、来源变化、写前预算、原生测试证据、缺密码/错误密码、GZip/XZ 默认输出名、GBK 明文/AES ZIP 与路径逃逸。
- engine `--lib native_` 9 项通过；配置集成 8 项、CLI 配置集成 5 项通过。
- CLI 实际执行 doctor/list/test/extract、Unicode 密码，以及从非首卷进入的 7z、RAR5、标准跨磁盘 ZIP；7z 与 ZIP 解压产物逐字节比对；RAR5 完整性校验与解压成功。
- CLI tar 实际打包和解包，移除库/worker 环境变量后成功自动加载随包 runtime 并执行 test。
- `cargo check -p smartzip-gui --tests` 通过；未打开原生窗口做视觉验收。
- 安装/打包 Python 测试 16 项通过；迁移的验收脚本通过 py_compile，未运行完整历史验收套件。Nix x86-64/aarch64 的库与应用 derivation 求值通过；未执行完整 GUI Nix 构建。

原生命令使用 `nix develop`；裸 Cargo 的 engine 检查遇到本机 fontconfig.pc 缺失，使用项目开发环境后通过。Windows/macOS ABI 与打包路径已实现，对应主机编译、签名和运行验收尚未执行；Linux aarch64 未实际构建。

能力设计中的混合密码成员级续写、嵌套 direct-stream/spool 浏览和 GUI 多选提取尚未开放。当前已接通全量工作流、索引预览与后端子集提取 API，不新增另一套任务或兼容后端。
