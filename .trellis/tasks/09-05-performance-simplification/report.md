# 性能优化与范围评估

基线：`291bc3f`。按用户选择，落实保持正常产品行为的性能优化，其余仅评估。没有改变默认 test-before-extract、显式完整性测试、后端路由和输出提交语义。

## 已实现

- 预算检查：树遍历和 statvfs 都通过受控 `spawn_blocking` 执行；同一 monitor 最多一个检查在途。50 ms 磁盘检查节拍使用 Delay；全树检查结束后，根据耗时的四倍安排下一轮，间隔限制在 50 ms–1 s。后端结束时等待在途检查，成功后再做一次全新终检，返回累计 Usage，删除调用方重复终检。扫描不是硬配额，写入仍可能在两次检查间超限；慢文件系统仍需等待当前扫描结束才能清理。
- 密码导入：BufRead 流式读取；复用 UPSERT prepared statement，在一个事务中提交。不再逐行查 ID，保留 trim、忽略空行、重复行计数、已有 pin 和成功统计、更新来源及重新启用行为。读取错误或 SQL 错误回滚整批；相比旧的 SQL 中途失败保留部分写入，失败行为更具原子性，这是批量事务要求的直接结果。
- 密码清理：用 HashSet 排除重复 ID，批量禁用复用 statement 和事务。预览数量、pinned 保护和排序不变。
- 数据库 v5：替换排名索引，完整匹配 disabled 过滤及 pinned、success_count、COALESCE(last_success_at, '')、failure_count、id 的顺序。保留历史数据；首次打开旧库需要一次索引重建，成本不在下面的稳态样本中。
- 分卷：借用任务内目录索引和分组成员，消除整树及每个候选记录的深拷贝。命中完整跨度时复用已解析序号；保留裁剪后出现新 Roman 边界时的原解析路径。
- ZIP 编码：未加密归档一次读取加密标记和原始名称，遇到加密条目仍提前停止；编码评估直接消费名称切片，单次分配拼接缓冲区。假 listing 仅保留为既有测试的适配入口。

## 测量与限制

同一台 Linux 主机、两个 release 二进制都为 Rust stable 1.97.1；`/tmp` 是 **tmpfs**。这些数字反映固定合成数据和进程启动开销，不能当作真实磁盘 fsync、NAS、冷缓存或实际密码库的结果。每次使用新库，初始化和种子写入不计时。保留全部七次样本，没有剔除异常值。

| 工作负载 | 基线中位数 | 优化后中位数 | 比值 |
| --- | ---: | ---: | ---: |
| 导入 5,000 行 | 174.74 ms | 7.19 ms | 24.31× |
| 100,000 条中取前 128 条 | 42.20 ms | 2.37 ms | 17.82× |
| 清理并禁用 5,000 条 | 128.65 ms | 11.99 ms | 10.73× |

执行计划由“索引过滤 + LAST 3 TERMS 临时 B-tree 排序”变成仅索引读取。清理包含新索引的更新成本，但未单独测量高频 record_success/record_failure 写入。原始数据见 [baseline](research/data-baseline.json)、[optimized](research/data-optimized.json)。复现命令：`python3 scripts/bench_cli_data.py <binary> --output <json>`。

补充端到端测试：同一 ZIP 包含 3,000 个 128-byte 文件，七轮交替执行顺序；每次验证全部文件内容。首次中位数为 421.63→520.42 ms，因结果较差又执行一次相同规模复核，中位数为 416.29→403.19 ms。两组都保留，不能用后一组覆盖前一组。

合并 14 次后，中位数为 421.58→502.16 ms，均值为 447.26→457.33 ms；基线范围 368.31–549.55 ms，优化版 346.71–541.97 ms。**没有证实整体解压提速，合并中位数较差；这组样本也不足以把差异归因于预算扫描。** 不承诺整体倍速。
原始数据见 [首组](research/extract-paired.json)、[复核组](research/extract-paired-repeat.json)。脚本为 `scripts/bench_cli_extract.py`，计时覆盖进程启动、完整预检、真实后端解压、布局和嵌套扫描，建包/建库/输出比对在计时外。

早期使用默认 nightly 的探索结果另存 `data-optimized-nightly-exploratory.json`，不用于上表比较；已核对 ELF `.comment` 的编译器版本和二进制 SHA-256。

## 只评估的项目

| 项目 | 结论与下一步所需证据 |
| --- | --- |
| 默认 test/extract 顺序 | `extract_workflow.rs` 仍先完整测试。改成直接解压可能改变密码失败分类、fallback 和失败证据；应先统一进程/错误契约，再用加密、损坏和分卷矩阵验收。当前不改。 |
| detect 重复扫描与完整测试 | `workflow.rs::inspect_file_with_listener` 分别调用 root resolve 和 findings 扫描，确实可能重复。需让 root resolve 返回本次 findings 并保留偏移、筛选和显示语义；同时将格式识别与完整测试分清，另立行为契约后实施。 |
| 统一进程执行 | `sevenzz.rs` 两条执行路径使用 process-wrap，`test_output.rs` 仍用 tokio Command/Unix kill。建议有限职责的执行器负责 locale、取消、进程树和有界输出，backend 保留进度与分类；不要同时重写路由接口。 |
| context/facts 接口和 CLI 模块拆分 | 保留 Executor/Adapter 边界，以 context 路径作为权威实现是合理方向；逐个后端迁移并测试取消。CLI 按参数、执行、交互拆分可改善维护性，目前没有证据证明会改善运行性能。 |
| 跨阶段 ZIP Metadata 缓存与 container raw 读取 | 目前只合并编码准备的两次读取。`container.rs` 的 `by_index()` 遇到加密/不支持解码器时可能返回 None，直接改 `by_index_raw()` 会让更多文件被识别为业务容器并跳过。这不是保证行为不变的替换；需明确这类输入应否跳过，并将元数据绑定到材质化后的文件 identity。 |
| zip features | 当前锁定 **8.6.0**（附件引用 8.0.0）。`cargo tree` 确认 AES、Deflate、Bzip2、LZMA、XZ、Zstd、time 都进入 CLI，见 [特性树](research/zip-features.txt)。先处理 container 读取契约，再把测试归档生成与运行时读取的 features 分开；无依据承诺二进制会缩小多少。 |
| binwalk scan-only | SmartZip scanner 只调用 scan，但 3.1.0 manifest 中 plotters、entropy、threadpool 都不是 optional，关闭 default features 不会自动裁掉。建议仅评估一个小的上游 feature 改造；构建依赖、链接结果和内嵌定位/大小/误报语义须分别测量，不替换成裸 magic 搜索。 |
| fs4 / crossterm | fs4 的跨平台 available_space 值得在 Windows 支持任务中替换平台调用，当前 Linux/macOS 没有性能测量支持新增运行时依赖。crossterm 的有界 poll 有助于跨平台输入，但必须保留取消、密码长度限制和终端恢复，当前不新增。 |
| chrono / Criterion / proptest | 时间换算可在单独维护性变更中统一已有 chrono，保持 UTC 格式和边界值。固定样本 CLI 脚本已满足本轮测量；等有持续微基准或组合性质测试需求时再加入开发依赖。保留 process-wrap、Unicode/序号解析库和 lzma-rust2 的实际产品职责。 |

上游依据：[Tokio missed ticks](https://docs.rs/tokio/latest/tokio/time/enum.MissedTickBehavior.html)、[ZIP raw API（附件版本）](https://docs.rs/zip/8.0.0/zip/read/struct.ZipArchive.html)、[binwalk 3.1.0 manifest](https://docs.rs/crate/binwalk/3.1.0/source/Cargo.toml)、[Cargo feature resolver](https://doc.rust-lang.org/cargo/reference/resolver.html)、[fs4](https://docs.rs/fs4/latest/fs4/)、[crossterm poll](https://docs.rs/crossterm/latest/crossterm/event/fn.poll.html)。实际 zip 调用以本地锁定 8.6.0 源码和编译结果为准。

## 验证

- `cargo +stable test --workspace --exclude smartzip-gui --locked`：426 passed，0 failed，0 ignored。
- `cargo +stable clippy --workspace --exclude smartzip-gui --all-targets --locked`：成功退出；仓库仍有既有 warnings，未将其描述成零警告检查。本次引入的多余 lifetime 已修正。
- `cargo +stable build --release --locked -p smartzip-cli`：成功。
- 发布版 `scripts/verify_cli_beta.py`：13 组全部通过，含动态预算、取消/子进程清理、恶意路径、输出回滚和真实加密/分卷；见 [验收记录](research/acceptance.json)。
- 格式检查、routing guards、`git diff --check`：通过。
- 此轮在 Linux 本机验证；没有把旧提交的 macOS CI 结果当作当前改动的验证。没有更改 Cargo.lock 或运行时依赖，也没有读取个人密码数据库。
