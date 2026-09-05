# Test：分卷损坏定位设计

> 2026-09-05 实现契约。test/t 已接线，默认失败后自动尽力定位并允许追加读取。验收范围与已知边界见 [实施记录](implement.md)。

## 1. 输出必须区分事实与候选

目标命令：`smartzip t movie.part03.rar`。可输入任意一卷，自动收集同组并选正确后端入口。输出示例：

```text
movie · RAR · Integrity failed; diagnosing volumes…
Confirmed damaged: movie.part02.rar
  Reason: packed-data checksum failed within this volume.
Suspected group A: movie.part04.rar, movie.part05.rar, movie.part06.rar
  One or more may be damaged; a failed data range spans these volumes.
Missing: movie.part07.rar
Coverage: partial; some data depends on the missing volume.
Next: replace part02 and restore part07, then test again.
```

这是示意，不是实际实验结果。建议不触发自动修复或下载。

| 结论 | 要求 |
| --- | --- |
| 确认损坏 | 证据明确属于该物理卷，例如可信边界内局部校验失败、确定的结构截断、可靠后端卷级诊断 |
| 疑似组 | 给出失败对象涉及的完整依赖范围；不能宣称组内每卷都坏 |
| 缺失 | 目录、卷序列、头部或后端明确需要的成员不存在 |
| 无法读取 | 文件存在但不能打开/读取，不直接归为内容损坏 |
| 未检查 | 因密码、缺卷、依赖、取消或预算无法检查，不等于通过 |
| 已检查范围 | 明确检查的是头部、分段或完整工作流；局部通过不等于整卷完好 |

日志中的入口路径、正在读取的卷和错误百分比均不是坏卷证明。尺寸异常只作线索；末卷较小或合法不等长卷不能被判坏。

疑似组保留 members、relation、evidence_ids 和 affected_entries。完整依赖闭包使用 `one_or_more`，仅启发式关联使用 `possible` 并保留未知范围。两个失败组 `{2,3}`、`{3,4}` 不能取交集断言 3：也可能是 2 和 4 同时坏。按依据与范围排序，不展示无校准的“损坏概率”。

## 2. 处理流程与预算

```text
参数 → VolumeSet 收集/去重/快照 → 完整后端 test
                                    ↓ 失败或未完成
                 卷内结构与校验 → 失败对象的物理范围映射
                                    ↓ 仍有可增加的信息
                     另一后端/独立范围复核 → 合并报告
```

VolumeSet 包含输入路径、命名家族、数值卷号、成员身份/大小、已知期待成员、归档入口和证据来源。按数值排序，处理重复编号与同组多输入；内容与名称矛盾时报告歧义。只扫描相关目录和 basename 家族，不广泛搜文件。

RAR partNN/旧 rar+r00 与字节切分通常从首卷打开；原生 split ZIP 从末段 .zip 打开。缺首卷或目录时总卷数可能未知，不因不存在下一个编号就虚构缺卷。任意 .001 文件仍需内容证据才能认作归档。

不先跑一次同样昂贵的 probe/test 再完整 test。密码轻量验证只在后端可靠支持时使用；否则 test 可承担验证。损坏报告必须保留已检查对象和范围，发现第一处错误后继续其他独立检查；有缺卷时仍可校验现存卷的独立部分。

新增 `--diagnose auto|off`，默认 auto。off 仅禁追加阶段，首轮已有证据仍输出。现有 --deep 继续表示扫描范围。可选 `--diagnostic-timeout <正整数秒>` 只限制追加阶段，默认无短时限；仍实施明确的内存、元数据数量/大小、诊断条目数和重读次数预算。

追加阶段同一局部范围至多验证一次，复用已验证元数据；首轮后最多一个有不同诊断价值的额外完整后端测试。没有增量证据途径就结束，说明能力边界；不反复跑同一命令，不做删卷/组合试探。取消或预算耗尽保留已有结论和未检查范围。

## 3. 格式策略

### RAR

RAR5 有头部 CRC，非末尾跨卷文件分段可有本卷 packed-data 校验；末段与加密场景的校验语义不同，不能统一计算“整卷 CRC”。[RARLab 规范](https://www.rarlab.com/technote.htm)

保留 UnRAR 原始卷级诊断，使用有界只读 RAR5 结构/分段校验器生成确认依据。外部日志可能包含伪造文件名，首版不单凭文本提升为确认坏卷。局部确认前验证 block 边界、split 标记、元数据和加密前提。坏头导致长度不可信时停止该解析链，不能用错误长度继续归责别的卷。RAR4 首版保留后端完整测试和原始诊断，本地不解析 RAR4 校验；不能建立可靠范围时输出候选或 unknown。

某跨卷文件在读取末卷时出现总校验错误，不足以证明末卷坏。只有局部证据才能确认具体物理卷。

### 7z

7z 描述 packed streams、folder/coder 关系与可选 digest，这些不是物理卷边界。[7-Zip 格式说明](https://raw.githubusercontent.com/ip7z/7zip/main/DOC/7zFormat.txt)

用只读虚拟分卷 reader，避免合并整套大文件。验证头部后解析 packed ranges，将失败流映射到卷；独立校验范围完全位于单卷且参考元数据可信时才确认该卷。多输入 stream 必须全部纳入依赖。

solid 文件共享解码依赖，只有坏文件名时应扩大到相关 folder，而非按解压大小或进度比例猜压缩偏移。元数据加密/缺失导致无法映射则报告 unknown。缺卷或原始分段长度不确定时，不能直接把当前文件大小前缀和套入原始逻辑偏移，误指后续健康卷。

### ZIP

原生 split ZIP 有磁盘号和 local header 偏移，通常末段名为 .zip，且规范允许不同段长；与 .zip.001 的字节切分不同。[PKWARE APPNOTE](https://pkware.cachefly.net/webdocs/casestudies/APPNOTE.TXT)

结合目录、ZIP64、local header、descriptor 定位 entry 的压缩数据范围。跨卷失败给候选组。还须考虑参考 CRC 本身所在的目录可能损坏：不能证明元数据可靠时，把其所在卷纳入候选。目录不可恢复时诚实报告无法建立完整范围。

## 4. 证据归约与密码边界

Evidence 至少记录 id、kind、adapter/版本或校验器来源、对象身份、逻辑/物理范围、参考校验值位置、密码前提、元数据可信度、可脱敏摘要。

kind 包括局部校验失败、头 CRC 失败、结构截断、后端卷校验错误、entry 校验错误、解码错误、缺失引用、读取失败、尺寸异常；各类证据能提升到的强度必须明确编码。

- 缺卷后的级联 CRC/Data Error 保留在后端 pass，归约时按 missing 阻断，不额外确认其他卷损坏；允许独立局部证据发现并存损坏。
- 密码错与损坏无法区分时保留 indeterminate，不保存密码、不记猜测失败；独立于密码的坏卷证据仍可输出。
- 两后端同意不自动升级为 confirmed，冲突也不能静默覆盖；不同路径的 7z 可能共享同样限制。
- ZIP 使用 directory disk/offset/raw-name 与 local header 校对；7z 对同名文件保留所有匹配 folder 的依赖组，不挑一个猜测。乱码或名称无法匹配时扩大范围并标记原因。
- 文本解析按已验证版本与日志上下文进行，路径须对应组内成员；未识别文本保留原始诊断。文件名伪造日志不能生成确认损坏证据。
- 成员大小、mtime、身份在诊断中变化时标 input_changed，相关跨阶段结论失效。

## 5. 跨层实现

扩展现有 TestResult，包含完整性、覆盖、密码证据、entry/volume 诊断。归档损坏属于一次已执行 test 的结果，不能压成 Err 后丢报告；无法启动/协议失败等仍是运行错误，有效部分报告必须保留。

正常 route 仍不能因 corruption/password 错误 fallback。交叉验证是 engine 发起的独立 diagnostic pass，经 executor 选择有增量价值的 adapter；请求携带 purpose/excluded_adapter_ids 或等价信息。不得绕开 router 直连后端或把损坏伪装成 Unsupported。--backend 指定后不擅自用其他 adapter，本地只读结构检查仍可用。

所有 pass 共享 task_id、各有 pass_id；事件仍归 TaskEvent。archive 层负责读卷与元数据，engine 负责分组、调度、证据归约和历史，CLI 只传请求并展示。

test 不因 known_files 去重而跳过，也不写 last_extract_at。分卷首片 hash 不能代表整组，首期只做任务内组身份复用，不新增以首片 hash 为键的组级密码关联；密码库候选仍可尝试。可靠验证成功才保存非空密码。

## 6. 报告、历史与退出码

TestArchiveReport 包含：

| 字段 | 内容 |
| --- | --- |
| input_paths / entrypoint / volumes | 原始输入、实际入口、卷清单与快照 |
| integrity | intact / corrupt / incomplete / unknown |
| coverage | complete / partial / none，针对声明的检查范围 |
| localization | exact / partial / unknown / not_applicable |
| password_status | not_needed / verified / required / rejected / indeterminate |
| confirmed_volumes | 路径、损坏范围、evidence_ids |
| suspect_groups | members、relation、依据、受影响对象 |
| missing/unreadable/unchecked_volumes | 分开存储，不混进 confirmed |
| checked_scopes / damaged_files / evidence / stop_reasons | 校验范围、坏对象、依据及停止原因 |

存在明确完整性失败可为 corrupt，即使无法定位物理卷；只有缺失/不可读阻断则 incomplete；执行或密码歧义则 unknown。疑似启发式本身不能证明 corrupt。localization=exact 需要全部已观测损坏均被具体定位且检查覆盖完整；确认一卷但其余未测仍为 partial。

JSON 顶层为 schema_version=1、command=test、task_id、files[]（每组一个报告）、events、exit_code。各 field 的可选范围用 null 明示，不用空数组暗示完整。JSON 不提示输入，缺密码时报告 password_required。

DB v4 增加 nullable file_extractions.test_report_json，存版本化完整报告，旧记录保持 NULL。damaged_volumes_json 继续只存确认坏卷路径数组，疑似/缺失不混入。history show/files 读取新字段并展示依据，写库 best-effort。

每组一条 test 文件级记录，保留全部输入路径。行状态映射：intact→intact，corrupt→corrupt，incomplete→partial，运行失败→failed，缺密码→skipped/password_required；完整判断以报告为准。不得给每卷各写一条独立 intact 记录。

首版退出码明确采用 0=所有组完整通过，1=无组完整通过/运行错误，2=部分组完整通过，130=取消。单组只有部分 entry 通过不算 intact。旧草案 3/4/5 数字不再用作实现依据；extract 现有 0/1/2 不变。

以上是执行阶段汇总；参数解析仍沿用 clap 的 exit 2，与部分通过存在重用，保留当前兼容边界并在帮助中说明。

## 7. 验收原则

完好基线先通过，再在拷贝中变异；oracle 独立记录真实修改卷和 offset，生产算法不能读取 oracle。必须验证 confirmed 没有健康卷、候选覆盖合理依赖、missing 不污染 confirmed、多卷故障不被交集算法误缩小。

按 [实施计划](implement.md) 覆盖 RAR5 末段/非末段、RAR4、7z solid/non-solid/多 stream、split ZIP/ZIP64、加密、头损坏、缺卷、合法不等长、文件变化、日志版本、预算与取消。只检查退出码非零不足以验收定位功能。

## 8. 首版实现边界

- 局部诊断上限为 16 MiB 元数据、100000 条格式记录、4096 个证据/范围条目，外部 stdout/stderr 各保留 1 MiB。ZIP 追加解码仅 stored/deflate，单 entry 最多 10 GiB 或 10000 倍声明展开量；超限保留原因。后端主测试仍受其自己的资源限制。
- RAR5 校验未加密 header 与有本段 CRC 的非末段；加密校验变换和末段全文件 CRC 不当作本卷 CRC。RAR4、RAR 恢复记录及任意 codec 本地解码不在首版范围。
- 7z 解压 encoded header 支持受预算约束的 LZMA/LZMA2；AES header 不在本地解密。packed CRC 与 stored-folder CRC 可独立检查；其余数据解码由完整后端 test 承担。多 stream 的映射测试使用合成 CRC 有效元数据，不能视为完整 BCJ2 解码认证。
- 完整后端成功才把归档标 intact。失败流程即使找到一个坏卷，也保守保留整卷健康未确认列表和 coverage=partial；`exact` 暂无通过部分检查产生的路径。
- 先尝试空密码确认是否需要凭据；其后正确密码且完整校验成功才记录命中。`--no-empty` 下外部后端成功若缺少独立加密依据，password_status 仍为 indeterminate，不保存该密码；Native ZIP 可直接验证使用了密码。
- 取消会终止正在等待的外部子进程；本地读取按块检查取消/超时。单次操作系统阻塞读无法保证即时中断。原文件只读，元数据快照变化会使跨阶段结论失效。
