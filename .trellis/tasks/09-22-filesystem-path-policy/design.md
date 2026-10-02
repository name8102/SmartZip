# 目标文件系统路径策略与解压映射

设计日期：2026-09-22。2026-10-02 已实施，结果与支持边界见 [implement.md](implement.md)；本文保留原设计及当时的源码核对基线。本方案基于用户提供的兼容性建议，并纳入 7-Zip 选择/排除成员的实际能力。

## 1. 核心决定

引入 **目标卷策略 → 全归档路径计划 → 实际名称验证 → 批量/受控写入 → 最终布局再校验 → 提交**。保留 `OutputMaterializer` 对暂存和发布的所有权，后端只在计划允许的范围内创建原名文件；任何被映射的成员由 SmartZip 控制落盘。

采用三种执行方式，而不是一出现长名称就对整个包逐文件启动进程：

| 方式 | 使用条件 | 执行 |
| --- | --- | --- |
| `BulkOriginal` | 全部名称保持原样，列表可靠，后端路径/链接语义满足安全要求 | 现有整包解压 |
| `HybridMapped` | 只有部分成员需映射，普通集合能精确排除映射集合，所有成员可唯一定位 | 7-Zip 批量解压普通集合；逐成员内容流写入映射集合 |
| `ManagedAll` | 广泛映射、批量路径不可靠，或必须按索引区分条目 | SmartZip 创建全部目标；后端/库提供内容和元数据 |

无法证明唯一选择或安全落盘时，返回明确能力错误；不能改成“先解压到其他临时目录再重命名”。目标分量本身过长时，其他普通临时目录同样可能无法创建。

## 2. 与当前源码的衔接

源码位置均相对仓库根目录；这里描述当前行为，不把旧文档当成已经实现的能力。

| 当前位置 | 核对结果 | 设计影响 |
| --- | --- | --- |
| `smartzip-archive/src/backend.rs` | 两层接口是 `ArchiveExecutor` / `ArchiveAdapter` | 扩展现有 seam，不新建平行的 `ArchiveBackend` 体系 |
| `native_zip.rs`、`decoded_zip.rs` | `NativeZipBackend` 只提供名称检测元数据；后者在特定旧编码 ZIP 上按索引解压 | 复用 ZIP 按索引读取代码，分离内容读取与落盘；不能写成“现有完整 NativeZip 后端已支持” |
| `sevenzz.rs` | 先运行技术列表，再 `7z x`；特殊编码转入 `decoded_zip` | 同一 attempt 的目录清单应复用，路径计划必须实际控制后续命令 |
| `member.rs` | 已有 `x -so -spd -ssc`，返回有界 `Vec<u8>`；限制 32 MiB/30 秒、分卷及链接 | 复用选择器和进程回收经验，新增流式提取契约；不放大预览缓存来承载解压 |
| `types.rs::ArchiveEntry` | 只有路径、可空的 `raw_name`、大小及目录标记 | 不能可靠表达重复成员、链接或原始名来源；需要专用提取清单 |
| `router.rs::extract_isolated_planned` | materializer 提供一个空 staging，失败后清空并验证，再依次重用 | 延续当前单所有者契约；`CONTEXT.md` 中“每次 adapter 独立目录”的措辞与当前代码有差异 |
| `materialize.rs`、`layout.rs` | 完成后布局，现有冲突后缀直接追加 `_collided_n` | 顶层布局名称、冲突后缀也必须进入策略，保留既有冲突交互时点 |
| `budget.rs`、扫描/恢复相关代码 | 仍有完整 `PathBuf`、`WalkDir`、`std::fs` 调用 | 单独把 writer 改成 `openat` 不等于已支持超长深目录 |
| `smartzip-core/src/error.rs`、`process.rs` | I/O 能保留 OS 错误，但无路径业务分类；外部输出最终主要是字符串 | 增加结构化路径失败和有界、脱敏的后端诊断 |
| `smartzip-engine/src/events.rs` | 保留事件集合上限 4096 | 完整映射不能只放在逐条事件里 |

## 3. 目标策略

### 3.1 配置和探测

新增统一配置 `output.path_mode = "native" | "portable"`，默认 `native`，随任务策略快照冻结。CLI/GUI 复用配置解析；策略是命名约束，不承诺输出可被所有旧应用打开。

对实际输出父目录打开句柄后探测，而不是按当前操作系统猜文件系统。不存在的输出父目录先由可信的用户输出根逐级创建。允许用户选择的输出根本身经系统解析到真实目录，此后固定根身份；归档提供的分量不得重新选择根。

记录目标卷/根身份、文件系统类型、大小写/规范化规则、限值来源和置信度。staging 建在最终父目录所在卷；提交前复核根身份。目录级大小写规则按实际父目录求值，新建子目录也需要确认，不能只缓存一个卷级布尔值。

| 目标 | 分量预算 | 名称比较与访问 |
| --- | --- | --- |
| Linux/Btrfs | 255 **字节**，探测实际目录作为证据 | 精确输出字节；受控路径采用目录 FD 相对访问。[Linux 定义](https://github.com/torvalds/linux/blob/master/include/uapi/linux/btrfs_tree.h) |
| macOS/APFS | v1 采用 255 UTF-16 单元作为**应用保守预算** | 有效 UTF-8；分别考虑大小写与规范化等价；创建结果是最终依据。[Apple DTS](https://developer.apple.com/forums/thread/726970) |
| Windows/NTFS | `GetVolumeInformationW` 的最大分量长度，按 W API 的 UTF-16 单元计 | 目录大小写规则；不主动把 NFC/NFD 合并。内部使用绝对 verbatim 路径。[卷信息](https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-getvolumeinformationw)、[目录大小写](https://learn.microsoft.com/en-us/windows/wsl/case-sensitivity) |
| 未知 POSIX/网络卷 | `fpathconf` 的已知字节限值，未知则保守预算并保留探测状态 | 不假定大小写敏感；受控排他创建验证。无法提供所需语义时明确失败 |

`fpathconf` 前清零 errno：返回 -1 且 errno 为 0 表示“不确定”，不能转成巨大无符号上限；探测失败与未知分开记录。[接口说明](https://man7.org/linux/man-pages/man3/pathconf.3.html)

APFS 的 UTF-16 预算是本应用选择，不是 Apple 的正式底层计量规则。普通 Unicode 库的比较键也只是预筛选，不能代替文件系统比较；Apple 的 APFS 指南是历史资料，应结合支持版本上的实测确认。[APFS 文件名说明](https://developer.apple.com/library/archive/documentation/FileManagement/Conceptual/APFS_Guide/FAQ/FAQ.html)

`portable` 在实际卷限制上叠加：有效 UTF-8、每分量不超过 255 UTF-8 字节、Windows 安全名称、保守大小写和规范化等价检查。实际卷有更小限制时取交集。它降低中文/emoji 可用长度，且不保证完整路径或未知网络卷通用。

### 3.2 最小模型

以下为类型草案，不是可直接编译的接口；最终字段随实现测试收敛。

```rust
struct TargetPathPolicy {
    version: u32,
    mode: PathMode,
    target: TargetIdentity,
    fs_kind: FsKind,
    component: ComponentBudget, // 值、计量单位、来源、置信度
    access: PathAccessStrategy,
    comparison: NameComparison, // 可按父目录求值
    invalid_names: InvalidNameRules,
}

enum LengthMetric { Utf8Bytes, Utf16Units }
enum PathAccessStrategy { PosixDirRelative, WindowsVerbatim }
enum PolicyConfidence { Known, Conservative, Unknown }
```

文件系统约束和后端/API 能力分开建模。不能因为 POSIX writer 可逐级访问，就声称外部 `7z`、扫描器或 GUI 打开动作也能接受任意完整路径。

Windows 名称清洗包括非法字符、控制字符、尾部空格/句点、带扩展名的设备名；覆盖官方列出的 `COM¹`、`COM²`、`COM³`、`LPT¹` 等形式。`:` 不能在成员中变成 ADS。即使 verbatim API 接受某些 Shell 不友好的名称，v1 也应用上述应用层规则。[Windows 名称规则](https://learn.microsoft.com/en-us/windows/win32/fileio/naming-a-file)

## 4. 提取清单与成员身份

新增用于提取的 `ExtractionManifest`，不强迫现有浏览列表承载全部执行状态：

```rust
struct ExtractionEntry {
    id: EntryId,                  // 清单内唯一；不是显示路径
    source_name: SourceName,       // 原始值及来源；不伪造 raw bytes
    logical_components: Vec<LogicalComponent>,
    kind: EntryKind,              // file / directory / symlink / hardlink
    selector: EntrySelector,      // index 或验证过的精确后端选择器
    metadata: EntryMetadata,      // 大小、校验值、时间、权限等可用字段
}
```

- ZIP 优先使用中央目录序号和原始 name 字段；Unicode path extra、选定解码结果分别保存。当前 `decoded_zip` 注释说明 `zip::name_raw()` 可能已经采用 Unicode extra，不能自动当成归档原始字节。
- 7-Zip/Unrar 的文本列表可能只有解码名称，显式标记 `BackendText`；不能把 UTF-8 重编码冒充原始字段，也不能声称换后端后哈希必然一致。原始字节不可得时，以版本化的后端文本身份生成映射并记录来源。
- 清单与 adapter 身份、归档 identity、编码绑定。持有可复用输入句柄时优先使用；外部程序重开路径前后检查输入/分卷身份。输入发生变化则废弃计划，不能套旧索引。现有首尾采样 hash 不是内容未变的严格证明。
- 按归档编码和格式识别组件，再做安全验证；不能在 GBK/Shift-JIS 等多字节序列中盲切 `0x5c`。安全路径、原始后端选择器和显示名称是不同字段。
- 拒绝绝对路径、驱动器/UNC 前缀、越界 `..`、NUL、设备节点等危险条目。兼容层只清洗合法相对路径中的名称；安全拒绝不能被重命名回退绕开。
- `./` 和目录分隔符按格式契约规范化；去掉这些语法后相同的逻辑目录共用一个节点。保留所有原始别名；节点身份使用稳定排序后的代表来源，而非哪个成员先出现。
- 文本列表若遇到换行/字段注入等无法无歧义解析的名称，不属于“完整清单”。必须转用结构化/原生解析或报告能力不足。

清单的条目数、原始名称总字节数、路径深度都受有界解析预算约束，进入现有资源限制配置/诊断体系。深度限制必须检查隐式目录；不能只限制显式目录条目。

## 5. 确定性映射

### 5.1 路径树与冲突

先构建整包逻辑 trie，包括隐式目录，父节点只映射一次。目标比较只作用于输出名称，不能先按目标大小写把两个不同的归档目录合并。

处理顺序为安全解析、结构冲突、名称清洗/长度、目标等价冲突、完整路径/API 检查。普通同名文件/目录类型矛盾（如 `a` 是文件，同时存在 `a/b`）返回 `name_collision`，不悄悄改变层次或丢文件。

v1 的重复**文件**路径一律拒绝为 `name_collision/duplicate_entry`，包括安全规范化后的重复；重复目录声明仅在类型及元数据兼容时合并。即使 ZIP 能按索引读取，也暂不引入 first-wins / last-wins 策略。两个不同名称在目标上等价时，则分别加确定性后缀保留两份。

碰撞组中所有相互等价的原始节点都改名，避免“枚举第一个保留原名”的顺序依赖。未改名的兄弟名称先保留；生成候选若撞到它们，继续扩展自己的哈希。每轮均重新检测兄弟节点等价性。

### 5.2 名称算法 v1

输出形式：`<stem>~<hash><extension>`。

1. BLAKE3，域 `smartzip-path-v1`，输入采用长度前缀编码的来源类型、组件序列和节点类型；真实 raw name 可得时用原始组件前缀。不包含 staging 名、枚举序号、密码或随机数。
2. 哈希先取 80 bit，小写 Base32 16 字符；遇到冲突依次扩展到 96、128、256 bit。每次扩展重新计算 stem 预算。
3. 清洗发生变化、超预算或目标语义冲突均添加哈希。所有后缀、点和扩展名计入限制；不能先截到 255 再追加后缀。
4. 按扩展字素簇边界保留最长 stem；绝不使用 Rust `chars().take(n)` 代表字素簇，也不切断 UTF-8。一个字素簇都放不下时使用纯哈希名称。
5. 目录无扩展名特权。文件优先保留最后一个扩展名；`.gitignore` 视作完整 stem。v1 不猜测复合扩展名。扩展名自身过长时先在其字素边界缩短，再必要时丢弃；记录变更原因。
6. 非法字符替换、尾部清洗及保留名处理完成后再次验证，确保不产生空名、`.`、`..` 或新的设备名。
7. 连最小可用哈希名也放不下，返回 `name_too_long`。满长哈希仍发生碰撞则返回 `name_collision`；不退化为随机后缀或覆盖。

纯规划器可注入哈希实现，方便验证人工构造碰撞。正常输出 Unicode 不强制改写成 NFC/NFD；规范化主要用于比较。v1 输出使用可靠解码后的 Unicode，POSIX 原始非 UTF-8 字节保真属于另一个显式能力，不能通过 lossy 解码默默合并。

### 5.3 创建时确认和回退

对于保守/未知比较语义，在隔离 staging 中先按稳定节点顺序创建目录和零长度普通文件，验证实际命名空间；文件内容尚未解压。记录节点对应的真实文件身份，FD 数量有界，重新打开时不跟随链接并检查身份。

预筛选未发现的 `EEXIST` 必须按原计划追踪的对象确认：如果是同一已知目录可复用；若是两个节点的实际等价冲突，重新规划该碰撞组；来源未知的文件/链接属于环境变化，不能直接信任或覆盖。重规划命名空间时尚无数据，可关闭句柄、清空并重建，最多 4 轮；超限明确失败。

单分量创建得到 `ENAMETOOLONG` 时，以稳定预算阶梯重试：原预算的 3/4、1/2、1/4，再到 ASCII 哈希名（扩展名能放下才保留）。失败原因必须确认为名称限制；权限、磁盘、取消、普通 `EINVAL` 等不得误判为“再截短”。每次变化都重新校验兄弟碰撞。错误码表按平台/操作建立，不通过一条本地化 stderr 猜测。

实际创建接受的映射冻结后才生成批量选择/排除集合。运行中出现新环境冲突时丢弃 attempt 并有限重试，不在已写好的树中随意修改祖先。事件记录的是实际最终映射，预检计划需要标明 tentative。

## 6. 7-Zip 选择、排除与流式补写

### 6.1 已验证的能力

本机 Linux、7-Zip 26.03、tmpfs 上的合成 ZIP 可行性探针结果见 [selection-results.json](research/selection-results.json)。这不是跨平台或性能验收。

```sh
# 按成员路径选择；-spd 关闭通配符，-ssc 按大小写匹配
7z x -spd -ssc -oout -- archive.7z 'dir/file.txt'

# 排除一个成员；大集合使用 UTF-8 列表文件
7z x -spd -ssc '-x!dir/file.txt' -oout -- archive.7z
7z x -spd -ssc -scsUTF-8 '-x@exclude.txt' -oout -- archive.7z

# 内容由应用消费并写到映射后的路径
7z x -so -spd -ssc -- archive.7z 'dir/file.txt'
```

实测包含精确选择、排除单成员、排除长分量、长文件/目录成员 stdout、字面 `*`、大小写选择。特别是同名重复成员输出 `firstsecond`，多成员输出 `AB`：**`-so` 没有天然的成员边界，退出 0 也不能证明仅输出了一个成员。** 不采用“全包 stdout 再随意切分”的实现。

生产代码用 `Command` 参数数组，不经 shell；使用 `--` 并限制列表文件/选择器语法。`-scsUTF-8` 指定列表文件编码，不等于归档文件名编码。`@`、换行、控制字符、选项样式名称、引号和反斜杠必须有明确协议覆盖；无法表达时拒绝该路线，不静默跳过。当前 `member.rs` 比后端实际能力更保守，例如拒绝 `*`，不能仅因为单例实测成功就直接放宽。

### 6.2 集合划分

在完整、冻结的映射树上计算：

- `M`：自身或任一祖先变更、必须由受控 writer 处理、或因链接依赖加入的节点。
- `B`：剩余可原名批量解压的节点。
- `M ∩ B = ∅`，`M ∪ B` 覆盖全部实际条目；隐式目录独立记录。

若目录改名，其目录条目及全部后代属于 `M`。只排除目录条目而漏掉子文件，7-Zip 仍可能创建原目录。优先从清单生成显式成员排除表，并以同一后端执行选择预检，确认选中集合等于 `B`；不能用未经验证的通配符近似。

流程：验证命名空间 → 关闭/移除零长度普通文件占位 → 批量解压 `B` → 校验实际树与 `B` 对应 → 排他创建并流式写入 `M` → 验证完整树。目录可保留，临时权限保证后续写入，最终目录元数据最后恢复。

对于未知/APFS/NTFS 比较语义，混合路径只有在实际命名空间验证及后端行为均通过平台测试后启用；否则使用 `ManagedAll`。批量与流式部分在同一 attempt 中串行执行，普通提取不能访问映射输出或跟随链接。v1 的混合路线排除含链接、元数据不能可靠恢复或存在选择歧义的归档；转入支持这些能力的全受控路线，或明确报不支持。

批量成功但补写失败，整个 attempt 失败并清理，不能发布“只完成普通文件”的树。失败后不能把原 `7z x` 全包命令当兼容回退。后端特有自动清洗、碰撞重命名或遗漏必须在输出核对中识别，不当作成功。

### 6.3 性能及格式边界

7z/RAR solid 归档逐成员提取可能重复解压前面的数据；混合方式也可能重复解码压缩块。探针只证明选择能力，不证明速度优势。以 solid/non-solid、映射比例和外部进程数量测量后再确定自动选择阈值；首版不编造一个“10% 最优”规则。

大量映射成员优先使用按索引/回调提供数据的库方案。7-Zip SDK 可作为后续适配候选，接入前核对许可证、构建与 codec/加密/分卷覆盖；不作为本阶段已具备的能力。Unrar 的 print/选择功能同样需要独立验证，不能套用 7-Zip 的参数和结论。

## 7. 接口与责任分配

不增加新 crate，沿用现有依赖方向：

| 模块 | 新职责 |
| --- | --- |
| `smartzip-core` | 纯数据策略、路径诊断、事件摘要；无平台调用、无密码 |
| `smartzip-platform` | 目标目录探测，受控目录句柄，排他创建/无覆盖改名/安全遍历与删除；返回平台错误 |
| `smartzip-archive` | 提取清单、精确成员身份、选择/排除、流式内容生产、元数据解析、后端能力和进程诊断 |
| `smartzip-engine` | trie/映射规划，执行方式选择，与 `OutputMaterializer` 集成的 writer、布局及提交复核 |
| `smartzip-config` / CLI / GUI | 统一模式配置；展示改名数量、原名/目标名、失败原因 |
| `smartzip-db` | 扩展现有任务模型保存映射报告和诊断引用，配合提交对账 |

对现有 executor 增加“准备提取 session”和“执行已准备 session”的窄接口，旧 extract 方法保留适配。session 保存固定 adapter、清单、输入身份和内存内凭据；`Debug`/序列化不得输出凭据。

```text
prepare_extract(request, context) -> PreparedExtraction
PreparedExtraction.manifest() -> &ExtractionManifest
PreparedExtraction.capabilities() -> ExtractionCapabilities
extract_prepared(session, SelectionPlan, ExtractionDestination, context)

ExtractionDestination = BulkDirectory | ManagedSink | HybridDestination
```

`ManagedSink` 由 materializer 借出，后端用 `EntryId` 获取写入端而不是自己拼路径；借用只覆盖单次 attempt，后端不能持有并跨清理继续写。同步 ZIP reader 在受控 blocking worker 中执行；异步外部 stdout 通过有界缓冲与背压送入 writer。接口应让“成员读到 EOF且校验成功”“子进程退出成功”“输出已关闭”成为可观察完成条件，不仅返回一个裸 `Read` 对象。

能力仅增加路由实际需要的项目：完整清单、`Index`/`UniqueSelector` 定位、成员流、精确排除、可恢复元数据、路径访问能力。不要恢复旧的通用 profile/rule 系统。准备清单失败遵守密码/取消等既有分类；无重映射能力在内容写入前排除该候选。

ZIP 复用 `decoded_zip` 的库读取，但从 SevenZip 内部隐式特殊分支中提取共享逻辑，使执行事件区分外部 7-Zip 和 ZIP 库写入。普通 ZIP 仍可走原批量路线；强制 `--backend` 时不得悄悄换实现，必要时报告该 adapter 不支持本次映射。

同一次 attempt 复用清单；密码改变且目录加密、编码改变、输入改变或更换 adapter 时重新准备。已有 router 的终止错误规则保留：路径/磁盘/权限/安全/取消不能仅为“多试几个后端”而 fallback；路由能力不足只影响当前请求，不能把整个容器永久标为不支持。

## 8. 受控文件系统操作、链接和元数据

### 8.1 路径访问

POSIX 从固定根 FD 开始逐分量调用 `mkdirat/openat/renameat/unlinkat`，目录打开使用不跟随链接语义。Linux 可用 `openat2` 的 `RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS`；不把 `RESOLVE_BENEATH` 与 `RESOLVE_IN_ROOT` 随意组合。无 `openat2` 时逐级 `O_NOFOLLOW` 并验证目录身份。[openat2](https://man7.org/linux/man-pages/man2/openat2.2.html)

目录 FD 降低前缀替换风险，但不是对同权限攻击者的完整隔离。staging 使用私有权限，禁止写入中外部插入链接；提交/恢复继续核对父目录与对象身份。

Windows 只对安全解析过的路径构造绝对 `\\?\` / `\\?\UNC\`，采用 W API、排他创建并检查 reparse point，不跟随归档创建的 junction。manifest 中声明 `longPathAware`，但正确性不依赖用户注册表开关。约 32767 的完整路径限制不是精确通用常量，实际调用仍可能失败。[Windows 长路径](https://learn.microsoft.com/en-us/windows/win32/fileio/maximum-file-path-limitation)

临时文件直接使用隔离 staging 内的最终映射名；未完成的树不可发布。其他 scratch、备份和 marker 使用固定短前缀加 token，不把 `.partial` 追加到已经用满预算的名称。

### 8.2 链接与元数据

- 普通文件先写，硬链接随后、符号链接最后；数据写入不跟随任何归档链接。
- 符号链接目标按归档格式与链接父目录解析到逻辑节点，再计算最终布局中映射后的相对目标。安全的 `../sibling` 可以解析，但绝不能逃出发布根；链接目标的规则与成员路径的规则不同。
- 硬链接必须指向清单内唯一普通文件身份；前向引用允许，环、类型矛盾或不明确的引用拒绝。
- v1 只重写能唯一解析的内部链接。缺失目标的安全 dangling link，在无需改写其语义时可保留；涉及映射后不能确定对应关系时失败，不猜测。绝对/越界链接保留现有安全拒绝边界，不因兼容修复放宽。
- 布局折叠可能改变链接关系，最终布局选择后必须复核并必要时重建链接；只有相对关系确实不变时原样保留。
- 文件内容全部成功后恢复安全的时间戳和普通权限，目录元数据最后恢复；不自动恢复 setuid/setgid、设备节点或越权 ACL。xattr/ACL/ADS 等要列为 adapter 实际能力，缺失时给明确警告/错误，不能宣称 `-so` 自动保留它们。

流式写入复用任务取消和预算：固定大小缓冲；显式输出字节限额在写入前检查；磁盘余量与累计产出仍接现有 monitor。不能把单成员大小相信为实际写入上限。取消或错误后先关闭/终止并等待全部生产者，再回收 writer、清理并验证 staging；清理失败终止重试并保留诊断。

## 9. 布局、完整路径和恢复

成员映射与最终 `LayoutPlan` 分两步：前者解决成员内部名称，后者使用同一个分量算法处理归档容器名、折叠后的顶层名和用户选择的冲突改名。不得映射或截短用户指定输出根之外的祖先目录。

冲突交互仍在布局后，对**已经符合目标策略的候选路径**进行。`Rename` 保留递增序号语义，但每次为序号预留预算、保留扩展名并重新验证；取消当前 PID 随机式兜底。已有用户文件的冲突不等于归档内部碰撞：覆盖仍需现有策略授权和目标身份检查。

`PathMappingReport` 表示逻辑成员 → staging 相对路径 → 最终相对路径的组合，不能仅记录解压时的名称。布局若影响目录大小写继承规则或 Windows 总长度，要重新校验最终父目录语义。

Windows 对完整 staging 路径和最终路径分别测量，包含根、分隔符、前缀和终止符/API 余量。必要时对**本次输出拥有的**祖先目录做第二轮确定性缩短：按稳定路径顺序处理超限叶，依次缩短可贡献预算最多的祖先（并列按稳定节点键），更新全部后代并重检碰撞/链接。达到纯哈希最小树仍失败则 `path_too_long`，不扁平化目录。

POSIX 的逐分量 writer 不是整链路深路径支持。预算遍历、顶层扫描、递归发现、嵌套输入打开、提取前列表、提交/备份清理、崩溃恢复都必须改为句柄/分量访问。外部后端无法打开长输入路径时，需要经验证的短路径输入视图或明确能力失败；不能将整个目录路径再次传给 `std::fs::open`。完整迁移前设 route 的路径访问上限并在预检报告 `path_too_long`，不得先生成无法扫描或删除的输出。

持久任务在 `CommitIntent` 中增加 `mapping_version`、`mapping_digest` 和报告引用；布局及回退已冻结后才写 Prepared。继续复用现有 Prepared/Published 对账，不另建完成文件库。目标卷、编码、策略或清单身份变更时，未提交节点清理后重新规划；已经 Published 的结果按原记录对账，不运行新算法再改名。

历史 best-effort 与提交恢复必需状态分开：普通历史写失败可警告，必需的提交记录/映射依据写失败则不得发布。若本次不生成可恢复执行记录，仍提供内存报告供当前调用者查看，不把报告混进用户文件树。

## 10. 错误、事件和持久化

新增路径诊断值并嵌入 `SmartZipError`，保留原始 `io::Error` 作为 source，不把原有密码/损坏/安全错误压成统一字符串。

| 稳定 reason | 含义 |
| --- | --- |
| `name_too_long` | 已定位单分量超限且回退耗尽 |
| `path_too_long` | 分量可接受，完整路径或当前调用链/API 超限 |
| `invalid_name` | 目标名称拒绝，无法可靠清洗/创建 |
| `name_collision` | 无法消歧、重复条目或结构冲突 |
| `path_remap_unsupported` | 当前可选后端不能可靠实现必要映射 |
| `path_constraint_unknown` | 有路径错误证据但无法确定分量/全路径原因，避免伪精确分类 |

诊断至少包括 `stage`（preflight/create/layout/commit/cleanup）、`scope`、成员身份/组件索引、显示原名、raw 来源及可用字节、目标策略快照、计量值/限值/单位、候选与实际映射。失败记录和恢复记录不依赖保留事件数组是否截断。

外部诊断记录 adapter ID/版本、退出码、受限 stdout/stderr 及截断标记。子进程退出码不是 OS errno；只有实际捕获到的 OS 调用错误才填 `os_error_code`，否则为 null。列表截断必须拒绝规划，不能对残缺清单解压。

保留字节也要先做秘密过滤：密码可能被后端回显，完整命令行不入库；无法证明安全时舍弃 raw 片段并记录 `redacted=true`。不要承诺“完全原始 stderr”同时又承诺不泄密。解析输出和诊断留存分离；建议诊断每流保留首尾合计 64 KiB，并继续排空管道避免死锁，沿用现有解析输出预算。

成功变更不是失败。统一 `TaskEvent` 发送 `PathMappingPlanned`、`PathMappingApplied`、`PathMappingFallback` 摘要，携带 reason/count/report ID；GUI 默认显示“已调整 N 个名称”，详情分页展示原名与目标名。事件不逐成员复制整份映射。

DB 在现有 `file_extractions` 增加 nullable 路径报告/诊断引用与稳定 reason；按实际迁移版本追加（当前为 v7，不提前把版本号写死到调用方）。完整报告可采用关联表分批保存，键包含任务节点、generation、attempt 和 manifest digest；父目录映射按节点保存，避免为每个后代重复整条长前缀。通过条目/名称总量预算控制报告大小，不能静默截断恢复必需映射。

`known_files` 历史去重契约保持：不以输出位置或映射存在与否重新定义过去是否成功解压。

## 11. 实施切片与验收

| 切片 | 交付和退出条件 |
| --- | --- |
| P1：策略及计划 | 实际目录探测、配置、清单身份、纯 trie/映射、错误类型；规则与碰撞测试通过；不宣称可修复落盘 |
| P2：7-Zip 可用子集 | 流式生命周期、唯一选择、精确排除、Hybrid/Managed writer、最终布局命名；非歧义普通文件归档能完成端到端修复，失败不部分提交 |
| P3：ZIP 索引和元数据 | 拆出 `decoded_zip` 读取，补真实原始名来源和字符集；处理 P2 无法精确选择的 ZIP；补链接、权限和恢复记录 |
| P4：三平台与深路径 | APFS/NTFS 实际语义、POSIX 全调用链分量访问、Windows 长路径及总预算缩短、预算/递归/清理/恢复原生验收 |
| P5：格式覆盖和成本 | 真实 7z solid、RAR、加密头/数据、分卷、Unrar；核对元数据并测映射比例成本，决定是否引入回调库后端 |

P2 首个验收面采用 Linux 本地字节语义文件系统；APFS/Windows 只有相应端到端测试完成后才启用对应混合路线。每个切片的支持矩阵须写清楚“能检测”“能映射提取”“完整深路径支持”，不能把后续目标当成已有能力。

### 规则测试

- UTF-8 字节与 UTF-16 单元、字素簇、组合字符/ZWJ emoji、超长扩展名、纯哈希退化。
- Windows 设备名/扩展名/上标数字、尾部空格/句点、ADS；portable 与目标更小限值交集。
- 共享长目录、隐式目录、不同 raw 名解码同名、规范化及大小写等价、生成名与原名相撞、人工哈希碰撞。
- 重排清单后结果稳定；重复文件/目录与文件作为祖先；GBK/Shift-JIS 的分隔字节边界。
- 实际创建反馈导致重规划，次数有上限；磁盘/权限失败不触发缩短。

### 真实文件系统与后端测试

| 环境/行为 | 必须验证 |
| --- | --- |
| Btrfs | 255/256 ASCII 字节；85/86 个三字节字符；63/64 个四字节字符，以上均计入扩展名；实际检测卷类型 |
| APFS | 长中文、代理对预算、NFC/NFD、大小写敏感/不敏感卷、实际创建回退；不以 Linux 模拟替代 |
| NTFS | 255/256 UTF-16 单元、emoji、保留名、目录大小写标志、UNC、超过 260 的路径；真实 W API |
| 深路径 | Linux/macOS 超单次路径参数限制后的写入、遍历、递归发现、输入提取、提交和清理；Windows 接近扩展路径边界 |
| 7-Zip | ZIP/7z solid/non-solid、唯一选择/排除、空目录、整棵长目录、特殊选择器、重复名称、多选 stdout、实际大小/CRC、输入变更 |
| 事务 | 内容中途失败、错误密码、取消、磁盘写满、目录碰撞、外部替换、清理失败；失败无部分发布，旧输出可恢复 |
| 布局及链接 | 每个 `LayoutPlanKind`、名称原本已满预算再 Rename、安全相对链接/前向硬链接/逃逸；折叠后目标正确 |
| 持久化 | mapping digest 与 Prepared/Published 一致、事件截断后报告完整、迁移旧记录、含密码 stderr 的脱敏、各提交步骤崩溃注入 |

性能单列：固定归档/后端版本/文件系统，比较普通包与少量/大量映射包的耗时、峰值内存、进程数、解码量及磁盘占用。完成测试后才制定 Hybrid/Managed 选择阈值。

## 12. 2026-09-22 设计阶段完成与未验证项

完成源码核对、平台一手资料核对、方案和任务记录，以及 9 个小型 7-Zip/ZIP 可行性探针。探针输出证明选择/排除/流式读取的基础能力，并揭示重复成员拼接行为。

本次没有修改生产代码或运行 Rust 回归套件；没有完成 Btrfs、APFS、NTFS 原生验收，没有验证 RAR/solid/加密/分卷的混合路径，也没有性能证据。附件中的“第一阶段 NativeZip 完整提取”按当前源码调整为先复用 7-Zip 选择能力、再抽取 ZIP 索引 reader 的实施顺序。
