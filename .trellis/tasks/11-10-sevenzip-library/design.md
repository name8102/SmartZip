# 7-Zip 原生库替换 CLI：研究与迁移设计

2026-11-10；SmartZip 调研基线 `44e52a0`。本文保留研究阶段方案；原生执行现已落地，当前实现、方案差异与验证见 [implement.md](implement.md)。

## 1. 结论

**首选直接 Rust FFI 调用官方 7-Zip 原生库，不增加 C++ 桥接层。**

上层不按旧 CLI 命令形状保留：以来源视图、成员身份、执行计划和结果证据重设计，具体能力与组件契约统一记录在[能力驱动的组件设计](capability-design.md)。

7-Zip 导出 C 函数，创建 COM 风格对象；Rust 可以声明导出函数、vtable 和回调，直接实现输入/输出流。Linux 原型已通过动态加载、创建 handler、结构化列举、校验、索引读取及取消检查，见[复现记录](research/README.md)。

推荐的产品组合：

- 官方最新版全格式库 + 私有 Rust ABI 模块 + 安全的 `Library` / `ArchiveSession` 封装。
- 用一个 SmartZip 自有 worker 承载原生调用，保持当前后端可终止的生命周期；与 CLI/GUI 之间使用私有结构化协议。**调用语言与进程隔离是两个独立选择**：worker 内仍是 Rust 直接 FFI，不是调用 7-Zip CLI。
- 复用 engine/router、密码服务、分卷解析和 `OutputMaterializer` 的职责；调整它们的输入输出，以使用范围来源、索引选择、写前预算和逐项证据，不重建一套任务系统。
- 以条目索引取代名字选择器；以结构化状态取代 `-slt`、stderr 关键词、进度行和退出码推断。
- 保留 ZIP 原始名称元数据读取，移除因 CLI 名称限制产生的第二套 ZIP 载荷解压路径。

本轮不实施替换。压缩创建仍不在当前产品范围。

## 2. 版本、接口与参考实现

### 2.1 采用哪个 7-Zip

调研时最新稳定版为 **26.06**，官网下载页标注日期 **2026-11-06**；固定源码提交为 `d3296e39e1f1f92c7b41c0c3848e65640428d42d`。实施开始时再次核对最新稳定版，然后将库、绑定定义、打包清单和源码哈希一起固定。不支持 p7zip、旧版 7-Zip 或任意新版本自动混用。[官方下载](https://www.7-zip.org/download.html)、[26.06 发布](https://github.com/ip7z/7zip/releases/tag/26.06)。

| 平台 | 库来源与实施要求 |
| --- | --- |
| Windows | 官方同版安装包/Extra 中的完整 `7z.dll`，匹配进程架构；不用只支持部分格式的 `7za.dll` / `7zxa.dll` |
| Linux | 官方 Linux 二进制包提供 console；从同版源码 `CPP/7zip/Bundles/Format7zF` 构建完整 `7z.so`。本轮已完成 x86-64 构建 |
| macOS | 从同版源码构建 Mach-O 动态库，打包路径和文件名由应用确定；须在 Apple Silicon / Intel 上验收加载、依赖和签名。本轮未编译 |

Linux 构建入口见[官方 makefile](https://github.com/ip7z/7zip/blob/26.06/CPP/7zip/Bundles/Format7zF/makefile.gcc)。当前本机 `_7zz-rar`/PATH 依赖不能作为新方案的库交付方式。

### 2.2 从 ArchiveRclick 借鉴什么

研究固定在提交 `9bad8a86e29fa11c284c250197d8796d3c07e0c8`，项目源码 MIT；其 README 中 bundled 7-Zip 是 26.03，不沿用这个版本。[参考项目](https://github.com/kirinonakar/ArchiveRclick/tree/9bad8a86e29fa11c284c250197d8796d3c07e0c8)。

值得借鉴的是实现机制：

- `src/archive/sevenzip/platform_impl/ffi.rs`：Rust 手写 vtable、COM 所有权和属性读取。
- `callbacks.rs` / `streams.rs`：password / extract / volume 回调，以及可 seek 的流。
- `engine.rs::open_for_read`：一次打开持有 handler、输入流和回调；区分数字切片与 RAR 后续卷。
- `format.rs`：格式 class ID、分卷识别；`source.rs` / `file_ops.rs`：来源与文件操作边界。

不照搬其 Windows 类型布局、Slint 任务模型或输出策略。SmartZip 已有隔离输出与回滚，不采用参考项目“此前成功写出的文件在后续失败时保留”的产品行为。引用其代码时保留 MIT 版权说明；本轮原型按官方接口独立编写。

### 2.3 三种封装路线

| 路线 | 判断 |
| --- | --- |
| **Rust 直接 FFI** | 选用。依赖和构建层次最少；ABI、引用计数及多接口回调集中在一个私有模块，按平台验收 |
| 官方头文件编译 C++ → 窄 C ABI → Rust | 可行但非必需；把 ABI 工作移给 C++，同时新增桥接构建、异常转换及两语言维护成本。仅在具体目标平台出现 Rust 绑定难以解决的问题时重新评估 |
| 泛用第三方 wrapper 或其他归档库代替 | 本轮不选；需额外核实版本、RAR/分卷/密码回调和旧编码能力，现有需求可由官方接口直接覆盖 |

## 3. 现有实现的真正边界

| 当前源码 | 迁移处理 |
| --- | --- |
| `smartzip-archive/src/sevenzz.rs` | 替换 executable 发现、命令参数、`l -slt`、`t`、`x`、文本进度及错误解析；同一次 extract 的预检和提取共享打开的 session |
| `src/member.rs` | 删除对 `executable_path()`、`7z l` / `x -so` 和名字选择器的依赖；转入 adapter 的 `read_member` 能力 |
| `src/router.rs` | 保留能力路由、事件和任务级负面缓存；改为 library 实例注册、结构化版本/能力诊断，不再执行 `7z i` |
| `src/backend.rs` / `src/types.rs` | adapter 身份不再等同于 executable；条目新增索引、类型及完整性字段，成员读取成为明确 operation |
| `src/integrity.rs` / `src/test_output.rs` | 库后端产出结构化诊断；`test_output` 仅保留 Unrar 真正使用的文本路径 |
| `src/native_zip.rs` / `src/decoded_zip.rs` | 前者目前是原始名称元数据 helper，不是完整 adapter；整合名称读取与解码映射，删除后者的独立载荷提取/读取实现 |
| engine `access.rs` / `test_workflow.rs` | 接受结构化密码证据与 coverage；解压密码重试也处理 `PasswordIndeterminate`，不把不确定失败写成“已确认密码错误” |
| engine `volumes/`、archive `volumes.rs` / `diagnostic/` | 保留卷组发现、候选排序和物理证据；为库传显式卷映射，不让库重猜组别 |
| GUI 归档树 / `member_preview.rs` | 条目身份由索引和来源快照组成，不再用显示路径唯一定位成员 |
| config、doctor、脚本、Nix/桌面打包 | 改库路径/版本与 worker 定位；真实验收脚本不再用 shell wrapper 追踪产品对 `7z` 的调用 |

`docs/context/01-core-concepts.md` 对 NativeZip 的历史描述仍比当前源码宽；上述设计按源码中的窄元数据职责处理。实施时同步该术语，当前不改写已实现状态。

## 4. 直接 Rust FFI 的边界

建议新增一个 `smartzip-sevenzip` crate，承载私有 `ffi` 模块和安全 session。worker 二进制由现有 `smartzip-archive` crate 装配来源、writer 和协议；FFI crate 不依赖 engine/GUI 或上层输出事务。不要再拆一层只转发函数的公开 sys crate。依赖方向与预算注入见[组件设计 2.5](capability-design.md#25-依赖方向与注入边界)。

```text
CLI / GUI → engine → BackendRouter → SevenZipLibraryAdapter
                                      │ 私有结构化请求/事件
                                      ▼
                         smartzip-archive-worker (Rust)
                           ArchiveSession / 安全流
                                      │ Rust FFI
                                      ▼
                                 7z.dll / 7z.so
```

私有 ABI 模块仅声明本任务需要的导出、接口、常量和平台类型。完整性、密码策略、路径判断、历史与 UI 类型不进入它。安全封装公开以下概念即可：

```text
Library::load(absolute_path) → RuntimeInfo / formats / codecs
Library::open(input_stream, volume_provider, open_options, control) → ArchiveSession
ArchiveSession::entries() → NativeEntryMetadata[]
ArchiveSession::extract(indices, Test | Extract, stream_factory, control) → NativeRunReport
ArchiveSession::try_item_stream(index) → optional native input stream
```

这是底层安全封装的职责草图；NameView、Full/Prefix、预算和 manifest 由 archive 层的计划/stream factory 组合，不反向进入 FFI crate。executor/adapter 对调用方提供 Catalog 与 Plan，不暴露 COM。一个 session 串行操作；不同凭据尝试建立新 session。元数据、安全预检和提取在同一次执行中共享打开状态，不跨任务持久缓存 handler。已验证成员的定向重试由同一输出 attempt 管理，见[组件设计 3.3](capability-design.md#33-密码处理验证到哪里证据就到哪里)。

### 4.1 ABI 必须显式处理的事实

| 项目 | Windows | Linux/其他 Unix |
| --- | --- | --- |
| 调用约定 | `extern "system"` 对应 Windows API | 平台 C ABI；本轮 Rust 原型使用 `extern "C"` |
| HRESULT / ULONG | 32 位 | 仍为 32 位，不使用 LP64 的 `c_long` 代替 |
| BSTR 元素 | 16 位 OLECHAR | 7-Zip 使用平台 `wchar_t`；本机为 4 字节 |
| PROPVARIANT | 使用对应目标的 Windows SDK/Rust Windows 绑定布局 | 使用官方 `MyWindows.h` 的简化布局；本机 size=16 |
| BSTR/属性释放 | 配对 Windows OLE 分配/清理函数 | 调用所加载库导出的 `SysAllocStringLen` / `SysFreeString` / `VariantClear`，不交给 Rust allocator |
| IUnknown vtable | QI / AddRef / Release 开始 | 26.06 默认相同，无虚析构槽；拒绝旧 p7zip 布局 |

加载后先用 `GetModuleProp` 校验版本和 `kInterfaceType`，再创建 COM 对象。`kClassID` 是 BSTR 包裹的 **16 字节二进制 GUID**，按 byte length 复制，不当文字解码，也不假设其满足 GUID 对齐。

本轮 Rust ABI 定义针对 x86-64 Linux。生产实现须覆盖 Windows SDK 布局、macOS、aarch64、非 BMP 文本及各格式宽字符转换；不能仅凭 `wchar_t` 宽度假定所有文字都是无损 Unicode scalar 序列。[MyWindows.h](https://github.com/ip7z/7zip/blob/26.06/CPP/Common/MyWindows.h)、[MyWindows.cpp](https://github.com/ip7z/7zip/blob/26.06/CPP/Common/MyWindows.cpp)、[模块类型/版本](https://github.com/ip7z/7zip/blob/26.06/CPP/7zip/ICoder.h)。

### 4.2 所有权与回调

- `Library` 活得比所有函数指针、COM 对象、BSTR/variant guard 长；RAII 保证 `Close` / `Release` 与属性释放。
- 输入/回调对象固定堆地址；`QueryInterface` 返回对应接口子对象地址，不能把所有接口都指向一个不匹配的 vtable。COM 引用计数使用原子操作，最后一次 Release 才释放。
- `ArchiveSession` 不实现 Send/Sync，在 worker 原生执行线程内创建和销毁；只跨线程共享取消状态和明确线程安全的统计/事件数据。
- 官方说明 progress 回调可与 extract 回调并发；事件序列化与统计要线程安全，不在回调中等待 GUI、密码输入或持有跨调用锁。
- 每个 Rust FFI 回调阻止 panic 越界，转换为结构化故障与失败 HRESULT；原生库 crash 由 worker 退出处理。
- `PROPVARIANT` 调用前初始化为 `VT_EMPTY`，返回后校验 vt；借出的指针只在其有效期复制使用。

## 5. 执行隔离与取消

**推荐 worker 是为了保留现有可终止后端契约，不是 Rust FFI 的技术要求。**

当前 `process.rs` 与 router 的契约是：取消后停止后端、等待退出，再允许 staging 清理/下一次尝试。`test_workflow.rs` 的追加诊断还有用户配置的 deadline。直接在 GUI/CLI 内 `spawn_blocking` 可以运行原生库，但不能通过取消 async future 强制停止已经开始的 native 调用。

| 方式 | 成本与边界 |
| --- | --- |
| 主进程直接 FFI | 最短调用链，适合原型；协作取消必须等待库实际返回。进程内不能安全强杀一个线程，库 crash 会结束宿主 |
| **worker 内直接 FFI** | 保留可靠停止与故障隔离，增加 worker 打包和私有 IPC；首版选此，不加常驻池/守护服务 |

首版每次 native 执行 lease 一个 worker；同一计划的 open/preflight/批量 extract 同进程同 session。目录浏览和等待人工输入只保留 Catalog/计划/已验证产物值状态，不占用 worker。并发由现有调度器准入，计入现有 backend_processes，不另加固定并发上限。只有测到启动成本后才考虑常驻 worker 复用。

协议用长度分帧的类型化消息：请求、元数据、进度、条目结果、成员字节块、最终结果；与应用一起更新，不维护跨版本协议兼容。请求包含密码时只走私有管道，不放 argv、环境变量或临时文件。OS 路径按 Windows 原生宽字符/Unix 原生字节无损编码；归档显示名称是独立字段。

worker 控制线程接收取消、设置共享标记；输入 Read/Seek、输出 Write、open/progress 回调检查标记并返回 `E_ABORT`。取消先发协作请求，未退出时经有界退出宽限终止并 reap；宽限参数在实现验收时确定，不另设正常解压超时或资源硬限。用户 deadline 到达时按 deadline 终止，不额外延长业务操作。

父进程仅在 worker 退出、所有输出句柄关闭后返回错误或选择成功树。Drop/父进程异常退出也要落实进程所有权；沿用进程组/Job Object 和现有恢复对账，不允许 detached native 写盘。worker 是故障隔离，不设计成权限沙箱。

## 6. 用结构化数据替代 CLI 语义

### 6.1 能力和格式打开

以 `GetNumberOfFormats` / `GetHandlerProperty2` 枚举 handler，以 `GetNumberOfMethods` / `GetMethodProperty` 核实 decoder。只有 RAR handler 而无 RAR decoder 的构建不能宣称可解压 RAR。注册时能力读取失败明确报加载/协议错误，不套回静态的“支持所有格式”表。

`CreateObject` 不自动完成 CLI 的格式选择。结合扫描事实、handler signature/flags 与可用 `GetIsArc` 选择候选，再以 `IInArchive::Open` 确认；后缀只决定优先级，覆盖伪装文件与 SFX。每次候选使用新的 handler/流状态。`S_FALSE` 要结合已识别格式、password 回调、archive flags 和输入错误分类，不能一律映射 UnsupportedContainer。

压缩流层如 GZip/XZ 与内层 TAR 分开表示；沿用 engine 的嵌套发现与递归策略，不在 FFI 层无条件递归解包。参考[handler 导出](https://github.com/ip7z/7zip/blob/26.06/CPP/7zip/Archive/ArchiveExports.cpp)、[官方打开逻辑](https://github.com/ip7z/7zip/blob/26.06/CPP/7zip/UI/Common/OpenArchive.cpp)。

### 6.2 list 与成员读取

`GetNumberOfItems` / `GetProperty` 返回索引、path、类型、size/pack size、encrypted、method、CRC、时间与链接信息。缺失值保留 unknown，不把无属性当 false/0。不会为 unknown size 伪造精确进度。

`ArchiveEntry` 增加 `index` 和明确 `EntryKind`；以 **CatalogId + index** 形成 EntryRef，Catalog 绑定来源快照和 adapter/目录代次，重开时检查来源未变。编码视图单独版本化，切换显示名不改变成员身份；输出计划绑定视图 revision。来源身份复用现有 fingerprint/generation，避免为每次 list 再完整哈希。重复名称可以独立选择、预览与编译提取计划；不同 adapter 索引不直接互换。删除 CLI 专用的 `@`、通配符、换行选择器限制，读内容与写路径的安全规则分开。

单成员读取调用 `Extract(&index, 1, testMode=0, callback)`；多成员请求编译为升序索引一次执行，为各目标返回独立流。完整读取检查实际写出字节和最终 operation result；前缀读取以 PreviewSatisfied 与部分验证范围返回。solid 7z 可能解码同块前面的条目，按块合批而不为每项重复开启解码。分卷交同一来源层处理，移除当前“分卷需先解压”的统一拒绝。详见[浏览与预览设计](capability-design.md#31-浏览与预览)。

### 6.3 test、错误与密码

校验使用 `Extract(..., testMode=1, callback)`，不生成普通输出文件。记录主 HRESULT、逐项 `SetOperationResult`、`IArchiveExtractCallbackMessage2` 的额外结果，以及 `kpidErrorFlags` / `kpidWarningFlags`。只有操作完成、全部应检范围已覆盖且无失败才为成功。[IArchive.h](https://github.com/ip7z/7zip/blob/26.06/CPP/7zip/Archive/IArchive.h)。

| 观测 | 领域结果 |
| --- | --- |
| 密码回调被调用，调用方未提供凭据 | PasswordRequired；返回时携带原因，不把用来中止回调的 E_ABORT 当用户取消 |
| 明确 kWrongPassword | WrongPassword / PasswordRejected |
| 加密内容 DataError/CRC、错误密码或加密头 Open 失败且证据有歧义 | PasswordIndeterminate；允许密码候选继续，不直接确认损坏或密码错误 |
| 未加密内容 CRC/DataError/HeadersError | CorruptedArchive，保留项索引和范围证据 |
| kUnsupportedMethod | UnsupportedCodec，可按当前路由规则 fallback |
| 已请求但缺失的卷 | MissingVolume 诊断，记录逻辑名/物理映射；不归为普通 UnsupportedContainer |
| 输入/输出流 I/O、预算、路径安全故障 | 保留原始领域类别，优先于库转发出来的笼统 HRESULT |
| 用户取消 | Cancelled；其他内部中止原因分别保留 |

**实测：错误数据密码返回 `S_OK` + kDataError(2)，CRC 损坏返回 `S_OK` + kCRCError(3)。** 因此不能只检查 HRESULT，也不能按 DataError 就断言密码错误。

`None` 与 `Some("")` 是不同凭据状态；open/extract 的 `ICryptoGetTextPassword` 都接入同一单次尝试。密码 UI/候选排序仍在 engine：缺密码时结束本次调用、释放执行资源后再询问，不把交互挂在 FFI 回调中。

`BackendTestDiagnostics` 改成公共语义字段 + 来源详情判别联合：`Native { hresult, archive_flags, item_results, stream_failures }` 或 `Process { exit_code, stdout, stderr, truncated }`。保留 coverage、密码状态和物理证据模型，不给库后端伪造 stdout/exit code。面向用户 JSON 的 test report 升版本并同步 CLI/GUI/历史展示；旧记录可原样展示，不参与新结果推断。Unrar 的真实进程详情仍有效。

通用库结果通常不能独立确认具体坏卷；继续用 `diagnostic/{rar,sevenz,zip}.rs` 的物理证据。7-Zip CLI 与库都属于相同 diagnostic family `7z`，不得当作两种独立实现复核。

### 6.4 进度

保留 stage、completed、total、单位/口径及当前 entry index。只有 total 已知且同口径时转换 percent；未知则显示不定进度。`IProgress` 的字节口径依格式而异，不与输出 Write 字节混为同一计数。输出字节预算只累计实际 Write 成功的字节。短暂统计锁和节流合并普通进度，最终结果/错误不丢弃。

## 7. 文件名、输出和分卷

### 7.1 旧编码 ZIP

库的标准 `kpidPath` 是已解码宽字符串；ZIP 的 `GetRawProp` 实现在该版本源码中是注释块，不能据此假定可取得 ZIP 原始名称。[ZipHandler.cpp](https://github.com/ip7z/7zip/blob/26.06/CPP/7zip/Archive/Zip/ZipHandler.cpp)。

Windows ZIP handler 有 `ISetProperties("cp")`，但 Unix 通用转换函数存在强制 UTF-8/locale 路径，不能把数值 codepage 当作跨平台完整转码服务；现有 `-scs` 也不该继续充当文件名编码模型。[ZIP 属性设置](https://github.com/ip7z/7zip/blob/26.06/CPP/7zip/Archive/Zip/ZipHandlerOut.cpp)、[StringConvert.cpp](https://github.com/ip7z/7zip/blob/26.06/CPP/Common/StringConvert.cpp)。

保留窄 ZIP central-directory 读取，向 `smartzip-encoding` 提供原始名称/UTF-8 标志/有效 Unicode extra。明确编码覆盖时，由 Rust 为 **entry index → 输出路径** 建立映射，再让库按索引解码载荷，GetStream 使用这份映射。list、extract、preview 共用映射，不先解压再改名。

映射校验不能只有“条目数量相等”：还要校验索引顺序、大小/CRC 及可得的成员定位信息；不一致明确返回元数据冲突。分卷 ZIP 元数据读取使用卷感知读取器处理 disk/offset；无 central directory 的恢复式归档按库元数据处理，没有原始字节时不宣称完成了原始名称转码。

### 7.2 安全输出与原有行为

所有输出仍只写 `OutputMaterializer` 借出的 attempt 目录。adapter 不创建第二层 staging，不负责最终覆盖、重命名或提交。失败/取消后先停写和关闭句柄，再由唯一 owner 清理。原生 writer 在写入前获得全局预算额度并随写生成 manifest，替代库路径的轮询计数和完成后 inventory 重建；同 attempt 凭据重试可复用已验证成员，提交仍要求整个请求完成。完整契约见[组件设计第 3 节](capability-design.md#3-组件改造)。

GetStream 不直接信任 archive path；预检全部解码名称、父组件和条目类型，写入时核对受控目录句柄，阻止越界、父链接替换与 reparse 跳转。保持现有安全相对符号链接/悬空链接行为：普通项先写，链接延后创建，后续载荷不得沿链接写盘；硬链接目标必须是 attempt 内已验证的成员。设备/FIFO 等特殊项明确报不支持。编码造成的路径冲突、重复输出项和文件/目录冲突在写入前报明确冲突，不照搬 `-y` 静默覆盖。

资源预算沿用用户配置；0 继续不限。只在实际读取、写入、创建数量和磁盘空间等相关位置计费，不新增假设性的压缩比、总大小或固定内存上限。全局额度在 worker 间仲裁，不能各自使用完整剩余额度。

### 7.3 分卷输入契约

| 卷型 | 库输入方式 |
| --- | --- |
| 原始数字切片：`.7z.001` / `.zip.001` 等 | resolver 给出已排序文件，Rust 提供可 seek 的逻辑拼接 IInStream，直接交真实 7z/ZIP handler |
| RAR4/RAR5 分卷 | 从确认首卷打开，IArchiveOpenVolumeCallback 按请求名称返回受控卷流 |
| ZIP spanned：`.z01`…`.zip` | 保留 disk/offset 语义和末卷目录入口，按 ZIP handler 的多卷请求提供映射；不能当原始切片直接拼接 |

新增输入值对象传入 entrypoint、卷类型、顺序、逻辑名→真实文件和来源快照。沿用 engine 当前组别推断/歧义候选策略；回调只从该组映射取卷，不任意扫描目录或接受穿越请求。记录请求的缺失逻辑卷名和实际读取来源，为物理诊断提供证据。

库路径由此可绕开 `engine/volumes/materialize.rs` 的规范卷名链接树；该机制若仍被 Unrar 使用就保留在 Unrar 路径，不全局删除。实现前先用真实分卷 fixture 确认各 handler 对名称和入口的要求。[参考 open_for_read](https://github.com/kirinonakar/ArchiveRclick/blob/9bad8a86e29fa11c284c250197d8796d3c07e0c8/src/archive/sevenzip/platform_impl/engine.rs#L1141)。

## 8. 配置、部署与许可材料

- `SevenZipCli` 替换为 `SevenZipLibrary`；安装项使用 family 判别的 endpoint：库族配置 `library`，Unrar CLI 配置 `executable`。保留 id/enabled/priority 与强制 adapter 选择，不用库路径冒充 executable。
- 自动发现仅定位随应用交付的固定库，以及原有可选 Unrar；显式配置可覆盖库路径，不在 PATH/CWD 搜索任意同名 DLL。关闭发现、禁用安装项仍生效。
- 旧 `family="seven-zip-cli"` 给出明确配置错误和修改指引；不偷偷寻找同目录 DLL，不自动退回 CLI。配置 schema 升版，GUI 设置表单一起更新。
- doctor 输出实际库绝对路径、版本、架构、接口类型、handler/decoder 能力和 worker 可用性；library endpoint 与 process endpoint 分别展示。
- Nix 新增独立的完整 7-Zip library derivation，固定版本、源 rev/hash 和构建参数，显式包含 RAR decoder。由 wrapper/安装布局传递不可歧义的库路径；去掉仅为产品 7z CLI 添加的 PATH 依赖，开发 fixture 生成工具另列。
- Linux 桌面、macOS app、Windows 分发同时带 CLI/GUI 所需的 worker 和库；校验 native 依赖、平台 loader 与 macOS 签名。Windows 使用受控绝对路径及限定依赖搜索目录，Unix 使用本地作用域加载；不依赖 CWD。
- 分发附 7-Zip 许可、unRAR 限制、对应源码和构建信息，并保留动态库替换路径；不把第三方库宣称为 MIT。参考[官方许可](https://www.7-zip.org/license.txt)。

## 9. 删除边界

替换交付时直接移除以下 7-Zip 旧路径，不保留双栈 fallback：

- `SevenZipLocator` 的 7z/7zz executable 搜索和旧版本识别。
- 7-Zip argv、`-slt`/格式表/进度解析、英文错误字符串与退出码推断。
- 成员名字选择器和 `x -so` 专用进程读取器。
- `decoded_zip` 中为绕开 CLI 名称问题而存在的另一套 ZIP 载荷读取/解压。
- 假 7z shell 脚本、参数断言和历史输出 fixture 的专用测试，替换为原生边界故障注入与真实库验收。

保留仍服务 Unrar 的 `process.rs`、定位器及文本诊断。保留路径安全、资源预算、分卷、密码、输出清理和回滚测试的领域断言，按新后端改造其测试入口；不为旧 CLI 断言恢复兼容分支。

## 10. 本轮实测结论

- 完整 Format7zF `7z.so` 构建成功，版本 26.06，接口类型 0，枚举到 60 个 handler；本机 wchar=4 / PROPVARIANT=16。
- Rust 直接 FFI **8/8**：ZIP、7z、TAR、GZip、XZ 的 list/test，按索引读取、CRC 失败和回调取消。
- C++ 官方头文件对照 **13/13**：上述类别外，增加数据密码正确/错误、加密头缺密码/正确/错误；错误数据密码得到 kDataError 而非 kWrongPassword。
- 包含中文、换行和类似 `Path = ...` 的名字、同名成员；结构化 API 中条目仍有独立索引。
- GCC 16.2 默认上游 `-Werror` 在 HandlerCont.cpp 触发 array-bounds 编译失败；研究构建仅将该类告警降为 warning 后成功。产品构建需固定可用编译器/验证补丁，本轮未修改上游源码。

原型是 ABI/能力验证，不接入 SmartZip；具体日志、源码及未覆盖项见[研究记录](research/README.md)。没有本轮吞吐或内存对比数据。

## 11. 实施顺序与验收门槛

| 切片 | 交付和针对性验收 |
| --- | --- |
| 1. 领域契约、ABI 与安全 session | 定义 Source/Catalog/EntryRef/NameView/Plan/Report 并实现跨平台绑定；验证 plain/encrypted、空/Unicode 密码、非 BMP 名、多接口回调、逐项结果、流 I/O 错误；平台布局用官方头文件作对照 |
| 2. 来源与目录 | 实现 Slice、完整库、raw ZIP 名称视图与分卷来源；真实 RAR4/5、7z/raw split、ZIP raw/spanned、缺卷/乱序入口、嵌入式免 carve；旧编码至少 GB18030/Shift-JIS，切换视图保留成员身份 |
| 3. 计划与受控输出 | 批量索引选择、writer、全局写前额度和 manifest，验证多 worker 总预算、solid 合批、重复名称、路径与实际 I/O；库路径不再轮询重建产出 |
| 4. worker、路由与访问 | 结构化协议、按计划能力路由、有范围的密码证据与定向重试；取消/超时/crash/父进程退出后无继续写盘；等待输入释放执行 lease，保留产物仍计费 |
| 5. 消费端与删除旧逻辑 | engine/test JSON/coverage、GUI 选择提取与 Full/Prefix 预览、配置/doctor 全链切换，删除第 9 节旧实现与测试；验证 mixed-password、事务回滚、恢复与嵌套来源策略 |
| 6. 平台交付 | Nix 两架构、Linux bundle、macOS 两架构和 Windows x64 的安装布局/加载/运行冒烟；确认 RAR 解码实际可用、运行不依赖 7z/7zz PATH，许可证及源码材料齐全 |

本轮未做 RAR/分卷、旧编码映射、真实文件安全落盘、Rust 密码回调、worker IPC/强制终止、Windows/macOS 或性能验收；这些是对应切片的验收项。跨平台 smoke 与 ABI 检查通过后再宣告该平台已迁移。

性能验收用相同文件、密码候选和输出盘比较旧 CLI/新库：小包批量、solid 单成员预览、大包、分卷和多密码；记录启动/打开次数、墙钟、CPU 与宿主+worker 峰值 RSS。只有测量结果支持时才提出 worker 池或 session 跨操作缓存。
