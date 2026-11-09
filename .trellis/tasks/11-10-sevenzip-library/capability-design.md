# 从原生能力重设计组件

2026-11-10。基于 7-Zip 26.06 官方接口和当前 SmartZip 源码；这是[迁移方案](design.md)的组件设计，不是已实现功能。

## 1. 更强在哪里

CLI 已能选择文件、测试、解密、处理分卷和输出数据流。原生接口的增量是：**SmartZip 能参与读取、选择、解码结果处理和每一笔输出，而不是只在命令前后工作。**

| 原生控制点 | 比当前 CLI 封装更强的能力 | 对应产品/组件变化 |
| --- | --- | --- |
| 自定义 `IInStream` / Read / Seek | 输入可以是文件片段、内存、逻辑拼接卷，不必先变成命名文件 | 嵌入式归档免 carve 副本；分卷免规范名称链接树；扫描器与后端共用来源视图 |
| item index + 类型化属性 | 同名、含换行/通配符/`@` 的成员可独立定位；属性保留 unknown | 浏览器与预览用成员身份；显示名、编码视图、输出路径分开 |
| 多索引 `Extract` + 每项 `GetStream` | 一次批量选择，多份输出流各自映射到受控目标 | 选中文件/目录提取；同 solid 块的成员合批；不按文件反复启动解码 |
| 自定义 `ISequentialOutStream::Write` | 写出前可拒绝、计费、改输出去向；同时获得实际写出数量 | 精确产出预算、统一安全 writer、直接内存预览、随写生成 inventory |
| 逐项结果 + archive flags + 原始流错误 | 保留哪些成员成功、何处失败、为何中止的证据 | 校验覆盖模型、局部重试、可定位 UI；不再只给整个包一个退出码 |
| open/extract 密码回调 + 加密属性 | 分辨何时需要凭据、验证到了哪一部分 | 密码按头部/成员范围记录；混合密码 ZIP 按成员组尝试，不把列出目录当整包解密成功 |
| caller-owned session | 一次打开内共享元数据、预检和批量动作 | 以执行计划驱动后端，避免 `l → l → x` 式重复打开 |
| volume callback + 自己实现的卷流 | 后续卷从指定映射取得，知道实际请求/读取了哪一卷 | resolver 负责组别，source provider 负责供流，诊断负责物理证据 |
| 结构化进度/handler/decoder 接口 | 知道进度口径、条目类型和可用解码能力 | 路由按请求能力选择；UI 展示阶段与条目，而非解析一行百分比 |

这些能力的接口依据是 [IStream.h](https://github.com/ip7z/7zip/blob/26.06/CPP/7zip/IStream.h)、[IArchive.h](https://github.com/ip7z/7zip/blob/26.06/CPP/7zip/Archive/IArchive.h) 和 [PropID.h](https://github.com/ip7z/7zip/blob/26.06/CPP/7zip/PropID.h)。前轮的 [Rust 原型](research/README.md)已实测索引读取、特殊名称、逐项错误与回调取消，其余组合按第 7 节验收。

## 2. 核心模型：来源、目录、计划、证据

不把新的后端继续设计成“接受 path、返回 stdout 的五个命令”。公开接口隐藏 COM/worker，面向以下领域值；不额外建立一套任务框架。

```text
ArchiveSource → inspect → ArchiveCatalog + NameView
                                  │
                用户/工作流选择 → Selection
                                  │
                策略与路径规划 → ArchivePlan
                                  │
                       execute → OperationReport + OutputManifest
                                  │
                 OutputMaterializer → 布局 / 碰撞 / 提交
```

### 2.1 ArchiveSource：归档不再等于路径

来源描述至少支持：

```text
File(path, source_snapshot)
Slice(parent_source, offset, optional_length)
Concatenated(ordered_parts)
NamedVolumes(entrypoint, logical_name_to_source)
Member(parent_source, entry_ref)
SeekableArtifact(owned_memory_or_spool)
```

`SourceProvider` 将描述打开为可 seek 的流，统一边界、取消、I/O 分类和物理坐标映射；FFI 只把这个流适配成 `IInStream`。复用现有输入身份、fingerprint/generation，观察到来源改变则使目录/计划失效。

- `Slice` 暴露局部坐标，并保留到父来源物理坐标的映射；所有读取不得超出有效范围。不能把归档 `Offset/PhySize` 当任意成员的位置或无条件可信长度。
- resolver 只生成卷清单，不复制卷、改名或打开新组。`NamedVolumes` 回调仅查这份清单。
- scanner 将已有的内部 `scan_reader` 提升为共用来源读取入口；`scan_path` / `scan_bytes` 只是便捷入口，避免另一套扫描算法。
- Unrar 等确实要求文件的 adapter 才请求按需物化。输入物化归 SourceProvider 所有，与 OutputMaterializer 的解压输出 staging 分开。
- Member 由 ArchiveAccess 取得父 session 的直接流或按需 spool，再交来源层；SourceProvider 本身不负责密码和归档解码策略。
- 历史和恢复记录原始来源及 lineage，不把临时 spool 路径当用户输入身份。进程内流句柄和 COM handle 不持久化。

### 2.2 Catalog 和 NameView：身份不随显示名称变化

```text
CatalogId = 本次来源快照 + 后端实例/目录代次
EntryRef  = { catalog_id, native_index }
Entry     = { ref, kind, sizes?, encrypted?, method?, crc?, block?, native_name, raw_name? }
NameView  = { catalog_id, revision, encoding_policy, entry_ref_to_display_and_output_name }
Selection = All | Entries(EntryRef[])
```

Catalog 是只读元数据值，不持有 worker 或任务执行资源。加密头未打开时返回 Locked/Challenge，不制造空目录来表示“没有文件”。目录可在数据未验证时正常浏览，条目分别标记加密/未知状态。

ZIP 切换编码只重建 NameView：保留 EntryRef、已选条目和目录元数据，不重读载荷、不重新用显示路径找成员。写盘计划绑定 NameView revision，以便重新检测转码后的路径冲突。

不同 adapter/新目录代次的索引不通用。全量操作可对新目录重新规划；子集操作切换后端时必须建立可验证的一一对应，无法唯一映射就返回需要重新选择的结果，不能静默取第一个同名文件。

### 2.3 ArchivePlan：明确目的，不按 CLI 子命令组织

```text
Inspect(name_view_requests)
Read(entry_ref, Complete | Prefix { requested_bytes })
Verify(selection)
Extract(selection, name_view_revision, output_mapping)
```

`ArchiveExecutor` 对调用方提供 inspect / execute；router 在执行前选择满足计划要求的 adapter。`ArchiveAdapter` 隐藏 session、协议和原生参数，将实际结果归一化为同一领域报告。

计划区分请求输出成员与解码依赖。目录选择在 Catalog 上展开成精确索引集合；去重、升序后一次交给库。已知 solid block 时合并同块选择；未知 block 信息就交库处理，不虚构逐文件独立任务。CLI 也支持批量选择，新设计的价值是把选择结果直接接到多路受控输出流和逐项状态。

### 2.4 OperationReport：四种成功分别表达

```text
MetadataOpened                    目录可读
PayloadValidated(entry/block set)  哪些载荷被完整验证
ArtifactPrepared(entry set)        哪些输出已写完、关闭并满足所需校验
OutputCommitted(published mapping) 哪些输出已由事务正式发布
```

报告包含 requested、completed、failed、dependency observations、remaining，以及结构化 StopReason。失败类别沿用主方案的密码/损坏/I/O/取消模型；进度、路径或密码回调次数不替代验证证据。

停止原因至少分为 `Completed`、`PreviewSatisfied`、`NeedsCredential`、`UserCancelled`、`Deadline`、`BudgetExceeded` 和实际故障。native HRESULT 作为证据保留；相同 `E_ABORT` 可以来自不同的宿主决定，领域结果由明确记录的中止原因决定。

### 2.5 依赖方向与注入边界

| 所属模块 | 唯一职责 |
| --- | --- |
| `smartzip-core` | 被事件/调用方共同使用的身份、取消与进度类型；不装配归档服务 |
| 新 `smartzip-sevenzip` | 官方 ABI、安全 Library/ArchiveSession、native callback/stream 适配；接受受控 Rust 流，不依赖 engine、GUI 或数据库 |
| `smartzip-archive` | Catalog/NameView/Plan/Report、SourceProvider、ControlledOutput、router；其 worker binary 装配 native session 和私有协议 |
| `smartzip-engine` | 密码/编码/选择策略、全局 TaskBudget、调度、OutputMaterializer 与历史；注入后端所需执行能力 |
| CLI / GUI | 构造意图、处理交互、展示相同结果模型，不直接访问 FFI |

全局预算不从 worker 反向引用 engine。archive 定义窄的输出额度接口（申请、结算、清理后释放），engine 用现有 TaskBudget 实现并在执行上下文中注入。adapter 父进程将 worker 的额度消息交给该实现；测试可以注入确定的预算实现。IPC、FFI 和纯 reader 不各维护一份业务预算状态。

## 3. 组件改造

### 3.1 浏览与预览

- `archive_browser.rs` 的行、虚拟目录成员集和 `MemberSource` 改为携带 EntryRef，显示路径不再充当 selector。同名成员均可选中，可用索引/大小辅助区分。
- 浏览器增加选择集合，提取选中文件或目录直接编译成 Selection；链接目标和目录不是可执行预览内容。
- 字节读取与文件系统输出使用不同安全边界：普通成员即使带不适合落盘的名字，也能按索引读取内容；其输出路径仍在 Extract 规划时严格检查。预览不跟随归档链接。
- 文本可请求前缀，再按用户操作读取完整内容；请求长度来自本次预览目标/配置，不给所有归档增设统一大小硬限。图片解码器仍按其完整数据和格式要求工作。
- `ReadResult` 返回 bytes、读取范围与验证状态。前缀满足后以 `PreviewSatisfied` 返回，不冒充完整校验；完整读取到结尾并取得成功项结果才可记录相应验证。
- 同一个批次需要多个成员时一次提交索引集合；不同时间的浏览操作先复用 Catalog 值，不预设常驻 worker/无限成员缓存。

前缀 sink 满足后中止的是该次同步 Extract；可能已解码 solid 前置数据。继续丢弃尾部直至校验完成是另一种“完整验证、只保留前缀”的请求，不能混为节省解码的前缀读取。[7zExtract.cpp](https://github.com/ip7z/7zip/blob/26.06/CPP/7zip/Archive/7z/7zExtract.cpp)。

### 3.2 写盘、安全与预算

用一个私有 `ControlledOutput` 实现库后端所有常规文件写入；内存读取复用计数/中止语义，但不经过文件路径写盘层。没有必要为每项功能新增一个公开服务。

```text
GetStream(index)
  → 查计划与输出映射
  → 校验路径/类型，申请创建额度
  → 建立本项输出流
Write(bytes)
  → 检查取消/停止原因
  → 申请该笔写入额度
  → 实际写入并结算处理字节数
SetOperationResult
  → 关闭输出，记录校验、属性与 manifest
```

当前 `budget.rs::monitor_task` 在启用预算时轮询解压树。库路径改为写入前控制：

- 所有 worker 共享父进程 TaskBudget 的准入仲裁；不能给每个 worker 一份完整的剩余额度。受限时通过私有控制通道申请可消费额度并分块使用，未消费额度归还；父进程同时处理控制请求与结果，避免等待 native 调用结束才响应额度请求。
- 输出字节在 Write 之前预留，按实际成功写入结算；部分写入和失败时正确报告 processedSize。不存在“多写后再靠轮询发现”的库路径窗口。
- 文件数量保持当前语义：实际创建的文件、目录和链接均计数，attempt 根不计；只为 solid 依赖解码、不落盘的成员不消耗输出文件额度。
- 额度 0 仍表示不限；此时只统计，不引入每笔写入的跨进程额度往返。小块写入可复用已授予的额度，不逐字节发送 IPC。
- 清理确认后才退还已落盘失败产物的额度。等待密码时保留产物仍占空间/额度；不因释放 CPU lease 就当作没有临时占用。
- 最低空闲空间仍检查文件系统；磁盘满、权限和设备错误保留为实际 I/O。decoder 的 `memx` 不能代替输出或整个进程的硬内存预算。

worker 只向 attempt 目录写入。主进程保留 OutputMaterializer 作为唯一 staging/提交 owner，失败和取消先停写、关闭 worker，再清理。

### 3.3 密码处理：验证到哪里，证据就到哪里

engine 的密码顺序、交互和密码库继续复用，但 ArchiveAccess 不再用单一“本包密码已成功”描述所有访问。

```text
CredentialEvidence {
    candidate_id,
    source_snapshot,
    scope: Header | Entries(entry_refs) | KnownBlock(block_refs),
    result: Required | Rejected | Validated | Indeterminate
}
```

头部打开成功只证明头部访问；未加密目录可读不证明任何候选密码正确。只有实际验证加密载荷，才为相应范围记录成功。批次成功密码仍可提高其他范围的候选优先级，不能直接将其他范围标为已验证。

混合密码 ZIP 按成员组执行，同一次 native 尝试使用确定凭据；不依赖“库每次密码回调都准确带当前成员 ID”的假设。确定失败/不确定失败按主方案分类，候选值不进事件、命令行或恢复快照。[IPassword.h](https://github.com/ip7z/7zip/blob/26.06/CPP/7zip/IPassword.h)。

验证范围由当前操作决定：预览验证目标成员，提取从未完成的加密成员组继续，不为筛选密码固定先做一次全包 test。现有 known-files 单个 `password_id` 继续作为优先候选提示；范围证据在当前任务中按来源和 scope 索引，需要跨次复用时扩展现有历史报告关联 candidate_id，不增加第二套密码/文件数据库，也不持久化临时明文密码。

**利用逐项结果做定向密码重试：**

1. OutputMaterializer 建立一个 native attempt，成员产物有未完成/已验证状态。
2. 因凭据需要继续时，仅保留来源未变、已关闭且已验证的成功产物；失败成员的部分文件删除并核对，再交下一凭据。
3. 下一次只选择剩余成员；相同 solid 依赖可能重解，但已验证文件不重复写盘、不重复记为成功。
4. 等待人工密码时结束 worker、释放执行 lease，仅保存目录/剩余集合和产物事实；获得输入后新建 session。
5. 只有全部要求完成才允许选中成功树并提交。非凭据故障、取消或切换 adapter 时，丢弃整个 attempt，按现有事务清理规则处理，不跨 adapter 拼接未经统一验证的输出树。

这是对旧“每试一个密码就重做整个材质化周期”的替换设计。它保留归档级原子提交，把进度复用限定在同一来源、backend attempt 和计划/NameView revision 的已验证成员；修改输出映射或失效的来源不能沿用旧产物，成功成员不会提前暴露为最终输出。

### 3.4 inventory、嵌套发现与输出事务

`ControlledOutput` 在写出过程中记录 `OutputManifest`：EntryRef、受控相对路径、类型、实际大小、校验结果及已应用属性。失败项不能进入成功 inventory。

- 将完整 manifest 交给现有布局和嵌套发现，替换库路径上的完成后全树大小重建；仍做必要的路径/句柄和最终选中树核对。
- 可在写入时顺手记录文件头或已配置的校验信息；完整嵌入式扫描仍使用可 seek 的来源，不假定只看头部就发现了所有内容。
- 子归档可以提前被识别，但只有其产物已验证、父输出依赖满足后才调度，防止在父事务回滚后继续消费临时路径。
- 内部重名/转码冲突在写入计划中处理；与用户已有目标目录的碰撞仍在实际布局规划后由 CollisionResolver 决策，不提前按归档原始路径猜测。
- Source recycling、历史成功和输出事件仍在最终提交后发生，不能由某次 SetOperationResult 提前触发。

### 3.5 校验和诊断

原生逐项结果让报告可以区分请求成员、完整验证成员、失败成员和仅作为依赖读取的块。`Verify(selection)` 只对所选范围负责；完整归档校验仍是 `Selection::All`。

- 保留库给出的原始项/块上下文，避免把同一 solid 块受牵连的所有条目都标成独立坏文件。
- SourceProvider 可在诊断模式记录合并的逻辑/物理读取区间，为 missing/read-error 提供卷定位；读取过某段是访问证据，checksum 验证是另一类证据。
- 物理卷确认继续交既有 `diagnostic/`，不能用“某成员解码成功”直接把所有卷标为完整。
- 报告保留逻辑 coverage 和物理 evidence 两个维度；CLI JSON、GUI 状态与历史展示读取同一结构，不各自解析一份错误文本。

### 3.6 路由、调度、事件与恢复

路由请求除了 container/codec，还携带本次实际需要的能力，例如 `IndexSelection`、`MemberRead`、`PrefixRead`、`ControlledOutput`。在所需语义下能力不足的 adapter 不进入候选，不能为回退到 Unrar 而静默丢失精确预算或成员身份。

原生库导出表说明 handler/decoder 存在；direct stream、特定属性或操作是否可用还需当前 handler/条目确认。保留必要的能力差异，不把所有标志拓成无消费方的通用插件系统。

- 调度单位保持归档操作/依赖块；不为每个成员建立独立引擎任务。索引选择编译和候选凭据在 engine，native session 只执行确定操作。
- worker 存活期间持有 native session 和执行 lease；浏览 Catalog、等待人工决定只保留值状态。硬终止和 deadline 沿用主方案，不以阻塞回调伪装任意点暂停。
- 统一 TaskEvent 时间线增加 entry_ref、attempt/generation、phase、进度单位和结构化结果；UI 区分解码/验证完成与最终提交。普通进度可合并，最终结果进入 OperationReport；不为每次 Write 写一条数据库事件。
- 历史沿用现有任务、文件和报告模型，按需增加 scope/条目结果，不另建一套成功文件库或默认将所有成员持久化为独立任务行。
- 恢复单位仍是归档节点/已有提交对账。未完成的 native 解码重新开始；定向重试产物默认只在当前活跃 attempt 中复用，崩溃恢复不信任缺少完整验证/持久事实的临时文件。

## 4. 嵌入式与嵌套来源的实际落点

### 嵌入式归档

当前 `nested.rs::materialize_archive_input` 对 offset>0 创建 carve 临时文件。新流程传 `Slice` 给库：scanner 确定片段 → SourceProvider 提供局部 seek → handler 打开 → 正常提取。保留物理来源映射，取消时没有 carve 副本要清理。

### 压缩包里的压缩包

先检查 handler 是否通过 `IInArchiveGetStream` 提供目标流，以及返回流是否能 QI 为可 seek 的 `IInStream`。满足时复用，不复制；不满足时只将所选内层成员解码成受所有权/预算管理的 seekable artifact，再打开内层。

直接流与父 ArchiveSession 在同一个 worker 内按依赖顺序持有和释放，不把 COM 指针跨 IPC。Member 描述传的是来源链和成员身份，worker 重开时校验相应目录上下文。

26.06 的 7z/ZIP/RAR handler 没有实现上述 direct-stream 接口，不能将普通压缩成员当作载体文件中的字节切片。TAR、ISO 等部分 handler 有直接流；XZ 等还有条目/尺寸条件。官方 archive-chain 打开逻辑也是逐步检查接口和 seek 能力后才继续。[OpenArchive.cpp](https://github.com/ip7z/7zip/blob/26.06/CPP/7zip/UI/Common/OpenArchive.cpp#L3280-L3315)、[XZ handler](https://github.com/ip7z/7zip/blob/26.06/CPP/7zip/Archive/XzHandler.cpp#L958-L988)。

嵌套浏览路径使用来源链 + EntryRef，而不是伪造实际文件路径。正常“全部解压”若本来需要发布外层成员，就复用已生成的产物，不为了追求流式而再解一次该成员。

## 5. 不能从接口直接推导的能力

以下事实决定设计边界，组件按对应状态处理：

- solid 选择读取仍可能解码前面的依赖块；合批减少重复工作，不能提供压缩前内容的普遍随机访问。
- `ISetProperties` 和线程/内存参数按 handler 与构建生效；RAR5 某些 `mt` 分支接受属性但不做设置。成功设置不是整个进程的硬限制。[RAR5 SetProperties](https://github.com/ip7z/7zip/blob/26.06/CPP/7zip/Archive/Rar/Rar5Handler.cpp#L3355-L3385)。
- 同步接口没有通用保存/恢复 decoder 状态的 API；暂停在调度边界，进行中的操作可取消后重新解码。
- 换库不增加官方解码器之外的格式，不统一解决所有旧编码，不强制消除加密数据“密码错误/损坏”的歧义；这些分别由能力发现、NameView 和验证证据模型处理。

## 6. 对现有代码的替换清单

| 当前位置 | 新职责 |
| --- | --- |
| archive `types.rs` / `backend.rs` / core `routing.rs` | ArchiveSource、Catalog、EntryRef、NameView、Selection、Plan、Report；接口按意图而非进程形态组织 |
| engine `access.rs` / `types.rs::PreparedArchive` | 准备来源与元数据，不默认物化成路径；凭据访问返回有范围的证据 |
| engine `nested.rs` / `volumes/materialize.rs` | 库路径改流/范围/卷映射；物化仅为确需文件的 adapter 提供 |
| scanner `lib.rs` / `file_scan.rs` | 已有 reader 扫描成为共用入口，复用来源坐标，不重复算法 |
| archive `member.rs` / GUI `archive_browser.rs`、`member_preview.rs` | 精确成员/批量选择与 Full/Prefix 读取，取消显示名选择器 |
| archive `native_zip.rs` / `decoded_zip.rs` / engine `encoding_flow.rs` | raw metadata → NameView；删除第二套 ZIP payload 解码 |
| engine `budget.rs` | 原生路径改写前准入 + manifest；轮询只留给无法控制写流的有效 adapter 路径 |
| engine `extract_workflow.rs` / `materialize.rs` | 执行计划、同 attempt 定向凭据重试、完整 manifest 交付；保留唯一输出事务 owner |
| archive `integrity.rs` / engine `test_workflow.rs` | 请求/验证范围、项/块结果和物理证据分离 |
| core `progress.rs` / engine `events.rs` / GUI `model.rs` | 统一条目进度和四阶段状态；不再以 message 字符串驱动业务 |

## 7. 落地优先级与验收

优先完成对所有上层有收益的契约与数据路径，再开放组合能力：

1. **基础替换一起完成**：来源视图、EntryRef/NameView、确定的 Selection、结构化 Report、受控 writer/预算/manifest、worker 生命周期。不能先把新后端伪装成旧 stdout 模型，再给上层加补丁。
2. **直接开放的用户能力**：同名成员预览、编码切换保持选择、选中项提取、嵌入式免 carve、分卷成员读取；text Prefix 按明确请求提供。
3. **需要较多状态验证的组合**：同 attempt 定向密码重试、混合密码成员分组、solid 合批、direct-stream/spool 嵌套浏览。使用同一数据契约，不另起兼容后端。

验收场景以新能力为单位：

| 场景 | 必须观察到的结果 |
| --- | --- |
| 同名两成员、名字含换行/通配符 | 选哪个 EntryRef 就读哪个；无名称选择器歧义 |
| GB18030 ZIP 切换编码 | 成员身份与选择保留；list/preview/extract 使用同一 NameView |
| 大载体内一个归档片段 | 库只获该 Slice；无 carve 临时副本，来源/诊断坐标仍正确 |
| 同一 solid block 选多个成员 | 一个批量 Extract；不为每项重新解整个块，报告保留依赖范围 |
| 前缀预览 + 中途主动取消 | 前者是 PreviewSatisfied/部分范围，后者是 UserCancelled；两者不污染完整校验记录 |
| 多 worker 接近总输出预算 | 全局仲裁先授予再写入；总量符合显式预算，失败清理后额度正确归还 |
| ZIP 不同成员使用不同密码 | 头部/成员验证分开；已验证文件不因其他密码尝试重写，整组成功前无最终提交 |
| CRC 错、缺卷、输出 I/O 错 | 逐项与原始源错误保留；不误诊为统一密码错误或“所有相关卷均损坏” |
| 原生库退出/来源改变/恢复 | 停写后再清理；旧 Catalog 和临时解码状态不被当成可继续的 decoder 快照 |

本轮只做官方源码与现有实现的静态核对、文档交叉引用检查；未新增或重跑原型测试。前轮实测与新增设计能力按第 1 节分别标注。
