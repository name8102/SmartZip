# SmartZip 跨平台重写正式方案

> 状态：正式设计方案 v2
> 技术方向：Rust Core + Rust CLI + GPUI GUI + SQLite + Native ZIP + 7zz fallback
> 平台优先级：Linux / macOS 优先，Windows 后续兼顾  
> 交付目标：GUI + CLI 共同交付  
> 不兼容旧版配置：旧 `SmartZip.ini` 不迁移

## 0. v2 设计基线

本节是 2026-05-31 需求确认后的设计基线，覆盖后文中与其冲突的早期表述。

SmartZip 的近期目标不是建设通用压缩平台，而是可靠处理来自互联网的不可信归档：复杂嵌套、遗忘密码、数万级密码表、混合加密方式、分卷、乱码文件名和伪装文件。

保留现有 workspace crate 边界，不进行一次性整体迁移。核心工作采用渐进式演进：

```text
现有 BFS 解压循环
-> 修复真实行为错误
-> 在 smartzip-engine 内拆分策略对象
-> 引入动态 ArchiveNode 状态
-> 为 GUI 增加事件流和取消
-> 按需要增加持久化恢复
```

暂不新增 `smartzip-graph`、`smartzip-scheduler`、`smartzip-events` 等独立 crate。只有当模块边界稳定且代码规模确实需要时再拆分。

### 0.1 核心对象

```rust
struct ArchiveNode {
    id: ArchiveId,
    parent: Option<ArchiveId>,
    source: ArchiveSource,
    depth: u8,
    detected_format: Option<ArchiveFormat>,
    state: ArchiveState,
    fingerprint: ArchiveFingerprint,
    successful_password: Option<PasswordId>,
}

struct ExtractionLimits {
    max_depth: u8,                    // default: 5
    max_nested_archives: usize,       // default: 100
    max_files: u64,                   // default: 500_000
    min_free_bytes: FreeSpacePolicy,  // max(10 GB, available * 10%)
    max_embedded_findings_per_file: usize, // default: 8
}

struct VolumeSet {
    first_volume: PathBuf,
    members: Vec<PathBuf>,
    missing_indices: Vec<u32>,
}
```

`ArchiveNode` 是动态增长的节点模型，不要求解压前得到完整 DAG。父归档成功解压后，其产物扫描结果才会产生新的节点。

### 0.2 Engine 内部模块

第一阶段在 `smartzip-engine` crate 内拆分模块，不新建 crate：

```text
engine/
├── workflow.rs       # 批次、根任务和动态节点推进
├── identity.rs       # 快速指纹、稳定指纹和内嵌片段身份
├── passwords.rs      # PasswordResolver 与 worker pool
├── limits.rs         # 磁盘、文件数、深度和候选预算
├── materialize.rs    # 临时目录、校验、提交和回滚
├── volumes.rs        # VolumeSet 识别与缺卷检查
└── events.rs         # 实时事件、暂停、取消和用户决策
```

### 0.3 解压状态机

```text
Discovered
-> WaitingForBudget | WaitingForPassword | WaitingForEncoding | WaitingForEmbeddedSelection
-> Testing
-> Extracting
-> Verifying
-> Committing
-> Expanding
-> Completed

任意阶段
-> Skipped | Cancelled | Failed
```

单个节点等待用户处理时，批次中的其他根任务和可执行分支继续运行。

### 0.4 密码策略

SQLite 保存成功密码、导入密码、命名密码表、关联关系和统计数据。默认自动尝试前 1000 条候选；深度模式继续分页遍历剩余候选。所有归档共享一个全局 worker pool。

候选顺序：

```text
命令行手动输入
-> known_files 精确命中密码
-> 当前批次最近交互成功密码
-> 空密码
-> 剪贴板
-> 置顶密码
-> 历史成功密码
-> 从未命中的导入密码
-> 历史失败密码
```

自动模式和深度模式均以性能优先。ZIP 原生验证器默认使用全部可用逻辑核心；`7z` / `7zz` 子进程必须设置硬上限，初始不超过 16。仅明确 `WrongPassword` 时记录失败，且同一密码在同一归档指纹上最多惩罚一次。

### 0.5 后端分工

> 当前实现状态（2026-07-01）：本节描述目标后端分工。代码里 ZIP 的 `list/test/extract` 主路径仍优先走 `SevenZipCliBackend`；`NativeZipBackend` 已存在，但目前主要承担有限特例能力、路径安全校验、ZIP 压缩和后备路径。`NativeSevenZipBackend` 尚未实现。

```text
NativeZipBackend
├── ZIP ZipCrypto / AES
├── 原始文件名字节
├── 两阶段密码验证：快速筛选 -> CRC / test 确认
├── Zip Slip 与危险符号链接检查
└── 输出统计

NativeSevenZipBackend
├── 第一候选：sevenz-rust2
├── 备选：zesven
├── 仅覆盖库能力已验证的 7z 场景
└── 不负责 RAR 和复杂分卷

UnrarBackend
├── 优先使用 unrar crate；若能力或平台覆盖不足，使用 unrar CLI
├── 专门处理 RAR4 / RAR5、加密 RAR 和 RAR 分卷
├── 作为 7zz 解压 RAR 部分失败时的优先 fallback
└── 仅提供 list / test / extract，不提供 compress

SevenZipCliBackend
├── 7z AES
├── RAR、分卷和复杂格式的最终 fallback
├── 个人使用阶段仅查找 PATH 中的 7zz / 7z
└── 保留为疑难格式和库级后端能力缺口的兜底
```

后端路由以格式和能力为先，而不是固定单一后端：目标状态下 ZIP 优先 `NativeZipBackend`；RAR 优先 `UnrarBackend`，失败或不可用时回退 `SevenZipCliBackend`；7z 优先评估 `NativeSevenZipBackend`，未覆盖的 7z AES、分卷或复杂格式回退 `SevenZipCliBackend`。当前实现为了成功率，ZIP 仍默认优先走 `SevenZipCliBackend`。路由层需要记录实际使用的 backend、失败原因和 fallback 链路，便于诊断“部分文件失败”这类问题。

`ArchiveBackend::list()` 需要演进为可表达原始文件名字节的模型，例如 `RawArchiveEntry`。`PathBuf` 只能作为完成编码决策后的输出路径，不能作为归档元数据的唯一表示。

### 0.6 事务式输出

默认在目标目录同一文件系统中创建临时目录：

```text
extract -> verify -> conservative normalize -> commit
```

提交优先使用 rename，必要时回退 copy + delete。超大归档满足以下任一条件时提示用户切换快速模式：

```text
预估输出 >= 50 GB
压缩包自身 >= 20 GB
事务模式无法维持磁盘安全余量
```

根归档始终保留。内层归档仅在成功提交后移入回收站；高级设置可改为永久删除。

### 0.7 内嵌归档扫描

根输入表示用户明确希望解压，因此忽略扩展名限制，主动扫描 magic bytes。单个高置信度片段自动切片解压；多个片段交由用户选择。

内层产物默认只自动处理明确归档后缀和分卷包。激进扫描由用户显式启用，并在扫描前应用已知容器排除列表、文件大小筛选和 finding 数量上限，避免展开 EPUB、APK、Office、PAK 等业务容器。

### 0.8 近期交付边界

第一阶段优先 Linux 与 macOS CLI：

- 核心动态嵌套工作流。
- 原生 ZIP backend。
- `7z` / `7zz` fallback。
- `smartzip list`。
- 实时进度和取消。
- 安全预算与事务式输出。
- 命名密码表导入和智能 / 深度密码尝试。

GUI 第一版聚焦任务工作台。压缩、完整预览、系统集成、John the Ripper / Hashcat 外部深度恢复后端、崩溃恢复和 `resume` 均后置。

## 1. 设计结论

SmartZip 新版采用 **Rust workspace 单仓库多 crate 架构**。

核心原则：

1. **核心能力与 GUI 解耦**：智能解压、压缩、密码策略、编码检测、内嵌压缩包扫描均在 Rust core crates 中实现。
2. **GUI 与 CLI 共享同一套核心引擎**：避免 GUI/CLI 行为分叉。
3. **GPUI 只负责界面和交互**：不承载业务规则。
4. **MVP 优先 Linux/macOS**：系统右键菜单等深度集成后置。
5. **Native ZIP + format-specific fallback**：第一阶段实现原生 ZIP 后端；RAR 增加 `UnrarBackend`；7z 评估 `sevenz-rust2`，备选 `zesven`；复杂格式最终回退外部 7zz。
6. **SQLite 管理高频数据**：密码库、排序统计、任务历史、编码检测历史、内嵌检测历史使用 SQLite。

## 2. 总体架构

```text
smartzip/
├── crates/
│   ├── smartzip-core        # 任务模型、智能解压决策、路径策略、规则引擎
│   ├── smartzip-archive     # ArchiveBackend trait + 7zz/libarchive 后端
│   ├── smartzip-passwords   # 密码候选生成、排序、统计、数据库访问
│   ├── smartzip-encoding    # 文件名编码/locale 检测
│   ├── smartzip-scanner     # magic bytes、内嵌压缩包检测
│   ├── smartzip-db          # SQLite schema、迁移、连接池/仓储
│   ├── smartzip-config      # 配置文件、默认值、配置迁移
│   ├── smartzip-platform    # Linux/macOS/Windows 平台能力封装
│   ├── smartzip-cli         # CLI 入口
│   └── smartzip-gui         # GPUI GUI 入口
├── docs/
├── packaging/
└── tests/
```

运行时依赖方向：

```text
smartzip-gui  ─┐
smartzip-cli  ─┼─> smartzip-core ──> smartzip-archive
               │          │          smartzip-passwords
               │          │          smartzip-encoding
               │          │          smartzip-scanner
               │          │          smartzip-db
               │          │          smartzip-config
               │          └───────> smartzip-platform
```

约束：

- `smartzip-core` 不依赖 GPUI。
- `smartzip-core` 不直接依赖具体 7zz 命令实现，只依赖 `ArchiveBackend` trait。
- `smartzip-gui` 不直接访问 SQLite 表，应通过 service/repository API。
- `smartzip-cli` 和 `smartzip-gui` 使用同一套 `SmartZipEngine`。

## 3. crate 职责设计

### 3.1 `smartzip-core`

职责：

- 定义任务模型：解压、压缩、检测、打开。
- 定义智能解压决策。
- 处理输出路径、临时目录、冲突命名。
- 调度密码尝试、编码检测、内嵌扫描、后端执行。
- 统一错误类型与任务事件。

核心类型：

```rust
pub struct SmartZipEngine {
    archive: Arc<dyn ArchiveBackend>,
    passwords: Arc<PasswordService>,
    encoding: Arc<EncodingDetector>,
    scanner: Arc<EmbeddedScanner>,
    db: Arc<SmartZipDb>,
    config: Arc<ConfigService>,
}

pub enum TaskKind {
    Extract,
    Compress,
    Detect,
    Open,
}

pub struct ExtractRequest {
    pub inputs: Vec<PathBuf>,
    pub output_dir: Option<PathBuf>,
    pub encoding: EncodingMode,
    pub scan_embedded: bool,
    pub recursion_limit: u8,
}

pub struct CompressRequest {
    pub inputs: Vec<PathBuf>,
    pub output: Option<PathBuf>,
    pub format: ArchiveFormat,
    pub level: CompressionLevel,
    pub password: Option<String>,
}

pub enum TaskEvent {
    Started { task_id: TaskId },
    Progress { task_id: TaskId, percent: Option<f32>, message: String },
    PasswordTried { task_id: TaskId, candidate_id: Option<PasswordId> },
    EncodingDetected { task_id: TaskId, result: EncodingDetectionResult },
    EmbeddedArchiveFound { task_id: TaskId, finding: EmbeddedArchiveFinding },
    OutputCreated { task_id: TaskId, path: PathBuf },
    Warning { task_id: TaskId, message: String },
    Failed { task_id: TaskId, error: SmartZipError },
    Completed { task_id: TaskId, summary: TaskSummary },
}
```

### 3.2 `smartzip-archive`

职责：

- 定义压缩后端抽象。
- 封装 7zz 命令调用、stdout/stderr 解析、退出码解释。
- 后续支持 libarchive/zip 等库级后端。
- 提供列表、测试、解压、压缩、检测能力。

核心 trait：

```rust
#[async_trait]
pub trait ArchiveBackend: Send + Sync {
    async fn probe(&self, path: &Path) -> Result<ArchiveProbe>;
    async fn list(&self, request: ListRequest) -> Result<ArchiveListing>;
    async fn test(&self, request: TestRequest) -> Result<TestResult>;
    async fn extract(&self, request: ExtractArchiveRequest, events: EventSink) -> Result<ExtractArchiveResult>;
    async fn compress(&self, request: CompressArchiveRequest, events: EventSink) -> Result<CompressArchiveResult>;
    fn capabilities(&self) -> BackendCapabilities;
}
```

MVP 后端：

> 当前实现状态（2026-07-01）：`BackendRouter`、`UnrarBackend`、`SevenZipCliBackend`、`NativeZipBackend` 已存在；但 ZIP 在 router 中默认是 `SevenZipCliBackend` 优先、`NativeZipBackend` 后备。`NativeSevenZipBackend` 和 `LibArchiveBackend` 仍未实现。

```text
BackendRouter
├── 按格式和 capabilities 选择后端
├── 记录实际后端、失败原因和 fallback 链路
└── 对 engine 暴露统一 ArchiveBackend 行为

NativeZipBackend
├── 目标：zip crate / async_zip 等 Rust 原生能力
├── 目标：ZIP ZipCrypto / AES
├── 目标：原始文件名字节和编码恢复
└── 当前：少量 ZIP 特例能力、路径安全检查、压缩与后备路径

UnrarBackend
├── unrar crate 或 unrar CLI
├── RAR4 / RAR5、加密 RAR、RAR 分卷
├── list / test / extract
└── 作为 RAR 默认后端；失败或不可用时回退 SevenZipCliBackend

SevenZipCliBackend
├── 查找 bundled 7zz
├── 查找系统 PATH 中的 7zz/7z
├── 解析 7zz list/test/extract 输出
└── 统一错误码：密码错误、文件损坏、格式不支持、取消、IO 错误
```

后续后端：

```text
NativeSevenZipBackend
├── 优先评估 sevenz-rust2
├── 备选 zesven
└── 覆盖已验证的 7z 场景，复杂 7z 继续回退 SevenZipCliBackend

LibArchiveBackend
├── tar/gz/bz2/xz 等常见格式
├── 更细粒度路径安全检查
└── 用作 7zz 的补充或替代
```

### 3.3 `smartzip-passwords`

职责：

- 生成密码候选。
- 对候选排序。
- 记录成功/失败统计。
- 管理本地明文密码数据库。

密码来源优先级：

1. 当前任务命令行手动密码。
2. `known_files` 对当前文件的精确命中密码。
3. 当前批次刚交互成功的密码。
4. 空密码与剪贴板密码。
5. 用户置顶密码。
6. 全局成功率 Top N。
7. 长尾密码。

排序策略：

```text
score =
  pinned_bonus
+ source_bonus
+ global_success_weight * log(success_count + 1)
+ recent_success_weight * recency_score(last_success_at)
+ path_match_weight * path_similarity
+ filename_match_weight * filename_similarity
- failure_weight * bounded_recent_failures
- stale_penalty
```

关键约束：

- 同一任务内同一密码不得重复尝试。
- 交互密码验证成功后立即写入密码库并加入任务内缓存，供同批后续文件复用。
- 单次批量任务不能过度惩罚某个密码。
- 大密码库下先取候选 Top N，再补充特定来源候选。
- 密码可明文保存，但 GUI 必须提示。

### 3.4 `smartzip-encoding`

职责：

- 自动检测压缩包中文件名编码。
- 防止解压后乱码。
- 提供候选编码和置信度。
- 记录用户修正历史。

候选编码：

- UTF-8
- GB18030/GBK
- Big5
- Shift_JIS
- EUC-KR

流程：

```text
1. 调用 ArchiveBackend::list 获取原始文件名/可用元信息
2. 若后端可提供原始字节，则基于字节检测
3. 对候选编码逐一解码并评分
4. 检查乱码特征、Unicode replacement、控制字符、常见语言字符分布
5. 结合历史相似路径/文件名修正记录
6. 输出最佳编码 + 候选列表 + 置信度
7. 低置信度时 GUI 提示用户确认；CLI 输出 warning
```

### 3.5 `smartzip-scanner`

职责：

- 基于 magic bytes 检测文件真实格式。
- 检测文件内容中的内嵌压缩包。
- 识别自解压包、伪装后缀文件、拼接文件。
- 将 `binwalk` 的通用嵌入式文件识别结果映射为 SmartZip 的扫描结果。

MVP 采用 [`binwalk`](https://crates.io/crates/binwalk) crate 作为 scanner 主实现。

`binwalk v3.1.0` 能力：

- Rust library，可直接调用 `Binwalk::new()` / `Binwalk::scan(&[u8])`。
- 使用 Aho-Corasick 一次扫描多种 magic patterns。
- 返回 `SignatureResult`：`offset`、`size`、`name`、`confidence`、`description`、`id`。
- 支持 zip、7z、rar、gzip、xz、bzip2、tarball、cab、dmg、iso/文件系统/固件等大量签名。
- zip/7z/rar parser 会尝试验证结构并推导 size，适合内嵌压缩包定位。

SmartZip 封装接口：

```rust
pub struct EmbeddedScanner {
    binwalk: binwalk::Binwalk,
    config: ScannerConfig,
}

pub struct ScannerConfig {
    pub mode: ScanMode,
    pub max_scan_bytes: Option<u64>,
    pub max_findings: usize,
    pub include_formats: Vec<EmbeddedFormat>,
    pub min_confidence: Confidence,
}

pub enum ScanMode {
    Fast,
    Deep,
}

pub struct EmbeddedArchiveFinding {
    pub offset: u64,
    pub size: Option<u64>,
    pub format: EmbeddedFormat,
    pub confidence: Confidence,
    pub description: String,
}
```

扫描策略：

```text
Fast mode:
  - 读取文件头/尾关键区间
  - 或仅扫描配置限制内的数据
  - 使用 binwalk include filter 限制 zip/7z/rar/gzip/xz/bzip2/tar/cab 等 SmartZip 关心的格式

Deep mode:
  - 读取完整文件或在 max_scan_bytes 限制内扫描
  - 调用 Binwalk::scan(&file_data)
  - 将 SignatureResult 映射为 EmbeddedArchiveFinding
  - 过滤低置信度、非压缩类、重叠/重复结果
```

注意事项：

- `Binwalk::scan(&[u8])` 需要传入内存中的字节；大文件必须由 SmartZip 控制 `max_scan_bytes`，避免无界读入。
- `binwalk` 也提供 extraction 能力，但 SmartZip MVP 只使用 scan，不直接使用 binwalk extraction；实际提取仍交给 `ArchiveBackend` / 7zz，避免安全边界分散。
- binwalk 的签名面向固件分析，结果需要按 SmartZip 支持格式白名单过滤。

安全限制：

- 默认最大扫描大小。
- 默认最大递归深度。
- 默认最大候选数量。
- 不自动执行未知内嵌内容，只提示或在用户开启时提取。

### 3.6 `smartzip-db`

职责：

- SQLite schema。
- schema migration。
- repository API。
- 批量写入和索引优化。

建议使用：

- `rusqlite` 或 `sqlx` SQLite。
- MVP 若想简单稳定，优先 `rusqlite`。
- 数据访问包装在 blocking task 中，避免阻塞 GUI。

### 3.7 `smartzip-config`

职责：

- 保存低频配置。
- 提供默认值。
- 管理配置版本。

格式建议：TOML。

配置文件位置：

- Linux：`~/.config/smartzip/config.toml`
- macOS：`~/Library/Application Support/SmartZip/config.toml`
- Windows：`%APPDATA%/SmartZip/config.toml`

数据库位置：

- Linux：`~/.local/share/smartzip/smartzip.db`
- macOS：`~/Library/Application Support/SmartZip/smartzip.db`
- Windows：`%APPDATA%/SmartZip/smartzip.db`

### 3.8 `smartzip-platform`

职责：

- 平台路径。
- 回收站/废纸篓删除。
- 查找 7zz。
- 后续系统集成：desktop file、Finder Quick Actions、Windows context menu。

MVP 范围：

- 平台标准目录。
- 回收站删除。
- 查找 bundled/system 7zz。
- 打开文件/目录。

后置范围：

- Linux 文件管理器动作。
- macOS Finder Quick Actions。
- Windows 右键菜单。

### 3.9 `smartzip-cli`

职责：

- 命令行参数解析。
- 调用 `SmartZipEngine`。
- 输出人类可读日志或 JSON。
- 返回明确 exit code。

当前使用 `clap`。可用命令示例：

```bash
smartzip extract <paths...>
smartzip extract --encoding auto <paths...>
smartzip extract --encoding gb18030 <paths...>
smartzip extract --deep --embedded ask <paths...>
smartzip detect <path>
smartzip list <path>
smartzip enc <path>
smartzip password list
smartzip password add <password>
smartzip password remove <id>
smartzip history tasks
```

`test` / `t` 已实现完整校验、默认分卷诊断和历史报告；`compress` 仍为占位。`open`、`config path`、`db path` 尚无 CLI 实现。

命令短别名已实现：`extract → x`、`list → l`、`detect → d`、`test → t`、`compress → c`、`password → pw`、`history → hist`。编码预览主命令为 `enc`，旧名称 `encoding-preview` 继续兼容。

当前退出码（实现事实）：

| code | 含义 |
| --- | --- |
| 0 | 成功 |
| 1 | 全失败、通用错误或命令尚未实现 |
| 2 | extract 部分处理或 test 部分组完整；clap 参数错误也使用此值 |
| 130 | test 用户取消 |

test 执行阶段采用 0/1/2/130；旧 test 草案中的 3/4/5 不适用。extract 的退出码保持不变，其他命令的统一 JSON 错误信封仍待 CLI 交互任务实施。

### 3.10 `smartzip-gui`

职责：

- GPUI 主窗口。
- 任务列表和进度。
- 拖拽文件入口。
- 密码库管理。
- 设置页。
- 日志页。
- 编码检测和内嵌压缩包结果展示。

GPUI 已确认支持：

- `Application::new().run()`
- `App::open_window()`
- `Render` + `div()`
- `div().on_drop::<ExternalPaths>()`
- `App::read_from_clipboard()`
- `App::prompt_for_paths()`
- `Window::prompt()`
- `cx.spawn()` 异步任务

GUI 状态模型：

```rust
pub struct MainWindowState {
    pub active_tab: MainTab,
    pub tasks: Vec<TaskViewModel>,
    pub selected_task: Option<TaskId>,
    pub password_stats: PasswordStatsViewModel,
    pub settings: SettingsViewModel,
    pub logs: Vec<LogEntryViewModel>,
}

pub enum MainTab {
    Tasks,
    Passwords,
    Rules,
    Logs,
    Settings,
}
```

## 4. 数据库设计

> **v3 重构（2026-07-02，锚定任务 `.trellis/tasks/2026-07/07-02-file-grain-history`）**：历史模型从**操作级**改为**文件级**。相对 v2 的变化：
> - **DROP** `password_matches`、`encoding_detections`、`embedded_archive_detections` 三张表。它们的职责被更精确的机制取代：精确密码记忆和确认编码复用改由 `known_files` 承担；`password_matches` 的 filename/path pattern 泛化匹配是只写不读的死表，泛化匹配将来改由配置文件正则/通配实现。
> - `tasks` **瘦身**为纯操作级父表（删 `input_summary / error_code / error_message / password_attempts / encoding_selected / embedded_found`，只留 `id / kind / status / output_path / started_at / finished_at`）。这些聚合列的明细全部下沉到 `file_extractions`。
> - 新增 `file_extractions`（append-only 文件级日志，一行 = 一次解压动作）。
> - 新增 `known_files`（`UNIQUE(sample_hash, size)` 去重/复用索引）。
> - 旧库无有效数据，v3 迁移直接 DROP + 建新、不回填；这是当时的 v3 迁移策略。当前 `LATEST_VERSION = 4`，v4 只 ALTER 增加 nullable `test_report_json`，保留现有任务、文件记录及 known_files。
>
> 使用场景锚定“解压来自网络的压缩包”：路径可明文存、密码只存 `password_id` 不存明文；核心痛点是重复下载、老式非 UTF-8 ZIP 文件名乱码、分卷缺失、内嵌/嵌套档。完整裁决见任务 `prd.md` 的 Confirmed Facts。写入仍为 best-effort（失败降级 `Warning`、不中断解压），归 engine 通过注入的 `TaskHistoryRecorder` 完成，CLI 只建连接 + 注入。
>
> 本次（`07-02-file-grain-history`）只落数据库层 + 闭合 `extract` / `history`；`detect / test / list` 命令的写入接线在后续任务 `.trellis/tasks/2026-07/07-02-file-aware-cli-commands`。hashcat `crack_jobs`、配置通配符密码层、`compress` 写库见各自 Out of Scope。目前 detect/list/test 已接入文件级历史；下面 4.1 `passwords` 不变，4.2–4.6 包含 v4 的 test 报告扩展。

### 4.1 `passwords`

```sql
CREATE TABLE passwords (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  value TEXT NOT NULL UNIQUE,
  source TEXT NOT NULL,
  pinned INTEGER NOT NULL DEFAULT 0,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  last_success_at TEXT,
  last_failure_at TEXT,
  success_count INTEGER NOT NULL DEFAULT 0,
  failure_count INTEGER NOT NULL DEFAULT 0,
  disabled INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX idx_passwords_score ON passwords(pinned, success_count, last_success_at);
CREATE INDEX idx_passwords_disabled ON passwords(disabled);
```

### 4.2 `password_matches` — v3 已删除

v2 的 `password_matches` 记录 password × (archive_format / path_pattern / filename_pattern) 的命中统计，用于泛化排序。v3 **DROP** 这张表：

- 它是**只写不读的死表**——`ranked_candidates` 只按 `passwords` 表自身的 `success_count` / `last_success_at` 排序，从未读 `password_matches` 参与决策；`path_pattern` 列甚至从未被写入。
- 精确的“哪个密码开了哪个确切文件”改由 `known_files.password_id` 承担（按 `sample_hash + size` 命中，更准）。
- 泛化的“文件名/路径 pattern → 密码”将来改由**配置文件正则/通配**实现（本次仅在求密码流程留 TODO 占位）。

对应地，`smartzip-engine` 删除 `record_password_match*` / `normalize_filename_pattern`，`smartzip-db` 删除 `record_match_success/failure` / `matches_for` / `PasswordMatch`。

### 4.3 `tasks` — v3 瘦身为纯操作级父表

```sql
CREATE TABLE tasks (
  id TEXT PRIMARY KEY,
  kind TEXT NOT NULL,           -- extract / detect / test / compress …
  status TEXT NOT NULL,         -- 反规范化聚合缓存，供 history 列表首屏
  output_path TEXT,             -- 用户请求的输出根目录（操作级）
  started_at TEXT NOT NULL,
  finished_at TEXT
);

CREATE INDEX idx_tasks_started_at ON tasks(started_at);
CREATE INDEX idx_tasks_status ON tasks(status);
```

相对 v2 删除了 `input_summary / error_code / error_message / password_attempts / encoding_selected / embedded_found`——这些聚合/明细全部下沉到 `file_extractions`。`status` 保留为反规范化缓存（避免 history 列表首屏为每行现算聚合）；操作级的输出根目录仍留 `output_path`（与 `file_extractions.output_path` 的 per-file 实际落点区分）。

### 4.4 `task_events`

```sql
CREATE TABLE task_events (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
  level TEXT NOT NULL,
  event_type TEXT NOT NULL,
  message TEXT NOT NULL,
  data_json TEXT,
  created_at TEXT NOT NULL
);

CREATE INDEX idx_task_events_task ON task_events(task_id, created_at);
```

### 4.5 `file_extractions` — v3 新增（append-only 文件级日志）

一行 = **一次解压动作**（不是“一个文件”）。root 输入、每层嵌套档、每个 carve 出的内嵌档、以及被跳过的输入都各占一行。取代了 v2 的 `encoding_detections`（编码下沉为 `encoding` / `encoding_corrected` 两列）和 `embedded_archive_detections`（carve 档直接作为独立行、靠 `offset` 区分）。

```sql
CREATE TABLE file_extractions (
  id                   INTEGER PRIMARY KEY AUTOINCREMENT,
  task_id              TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
  input_path           TEXT NOT NULL,      -- 明文；carve 档 = 宿主路径
  sample_hash          TEXT,               -- 头尾采样哈希；size 未知的 carve 档为 NULL
  file_size            INTEGER,            -- 判等的另一半；未知为 NULL
  offset               INTEGER,            -- carve/内嵌档 = Some，普通文件 = NULL
  output_path          TEXT,               -- 该档展平后的实际落点（per-file，区别于 tasks.output_path）
  has_password         INTEGER NOT NULL DEFAULT 0,
  password_id          INTEGER REFERENCES passwords(id) ON DELETE SET NULL,
  status               TEXT NOT NULL,      -- 见下方取值表
  reason               TEXT,               -- status=extracted 时为 NULL
  encoding             TEXT,               -- 最终选定编码名
  encoding_corrected   INTEGER NOT NULL DEFAULT 0,  -- 人工确认过（唯一不可再生的编码信号）
  damaged_volumes_json TEXT,               -- test 确认损坏路径数组，extract 不写
  test_report_json     TEXT,               -- v4：完整分组报告，旧记录 NULL
  created_at           TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX idx_file_extractions_task   ON file_extractions(task_id);
CREATE INDEX idx_file_extractions_status ON file_extractions(status);
CREATE INDEX idx_file_extractions_dedup  ON file_extractions(sample_hash, file_size, created_at);
```

`status` 全局唯一取值（靠值自证来自哪个命令，file 行不冗余 `kind`）：

- `extract`：`extracted / skipped / failed / partial`
- `detect`：`detected / unreadable`（本次仅预留，后续任务接线）
- `test`：`intact / corrupt / partial / skipped / failed`，以 test_report_json 中 integrity、coverage 和 password_status 为准

`reason` 枚举：`not_found / wrong_password / corrupt / target_exists / not_first_volume / recursion_limit / duplicate / business_container / password_required`。“需密码但最终没拿到”（候选试穿 + 用户取消）统一记 `status=skipped` + `reason=password_required`，不为加密单设终态。

### 4.6 `known_files` — v3 新增（去重/复用索引）

每个 `(sample_hash, size)` 恰好一行（UPSERT）。与 4.5 的日志分工：日志保留重复项（同包多次解压 = 多行，回答“上次为什么失败”），索引每个文件一行、只服务匹配热路径（去重跳过、复用确认编码、复用开包密码）。

```sql
CREATE TABLE known_files (
  sample_hash        TEXT NOT NULL,
  size               INTEGER NOT NULL,
  names_offsets_json TEXT NOT NULL DEFAULT '[]',  -- [{name, offset}] 配对，遇新组合追加
  password_id        INTEGER REFERENCES passwords(id) ON DELETE SET NULL,
  confirmed_encoding TEXT,               -- 仅人工确认写；detect 猜测永不覆盖
  last_extract_at    TEXT,               -- 仅 extract 写；非空即“成功解压过”（去重唯一判据）
  PRIMARY KEY (sample_hash, size)
);
```

**sample_hash**：`BLAKE3(前 64KB ‖ 后 64KB)` + `file_size` 一起判等；< 128KB 全量哈希；carve 档对 `[offset, offset+size)` 段做同样采样，size 未知时不参与去重。

**去重**（仅 `extract`）：命中 `last_extract_at` 非空且落在时间窗内（默认 1 个月，预留配置接口）→ 跳过 + 显式提示，`--force` 强制重解。不追踪输出是否还在，靠时间窗兜底。

**求密码候选顺序**（`extract`）：命令行 `--password` → `known_files.password_id`（hash+size 命中，置顶但不独占）→ 当前批次刚交互成功的密码 → 配置通配符层（将来）→ `passwords` 常规排序。交互成功后立即写库并更新任务内缓存，不等 task 完成。**编码复用**顺序：命令行编码 → `known_files.confirmed_encoding`（人工确认过）→ 当场自动检测；编码与密码走同一次 `WHERE sample_hash=? AND file_size=?` 查询。

## 5. 智能解压流程

完整解压工作流是一个递归任务队列，而不是单次“调用 7z 解压”。核心循环如下：

```text
Input paths
  ↓
Normalize + classify inputs
  ↓
Enqueue root candidates
  ↓
while queue not empty and depth <= recursion_limit:
  ↓
  pop candidate
  ↓
  skip non-first volume parts
  ↓
  detect archive candidate:
      1. extension / volume naming
      2. binwalk scanner result
      3. archive.probe/list fallback
  ↓
  if not archive:
      record ignored / continue
  ↓
  generate password candidates:
      empty password
      manual password
      clipboard password
      database-ranked passwords
  ↓
  for candidate password:
      archive.test/list/extract(candidate)
      if success:
          record password success
          break
      else if wrong password:
          record password failure
          continue
      else if unrecoverable backend error:
          fail candidate
  ↓
  decide output structure:
      one item → move to target dir
      multiple items → keep archive-name directory
  ↓
  apply post rules:
      delete empty dirs
      delete matching files
      rename matching files
  ↓
  scan extracted outputs:
      extension scan
      binwalk embedded scan
      archive.probe fallback
  ↓
  enqueue nested archive candidates at depth + 1
  ↓
  emit task summary
```

### 5.1 嵌套解压原则

必须支持：

1. 根输入可以是普通压缩包、伪装后缀文件、分卷首卷、自解压/内嵌压缩包。
2. 每次成功解压后，必须重新扫描输出目录。
3. 如果输出内容中还有压缩包，继续进入同一流程：
   - 检测压缩包
   - 匹配/排序密码
   - 调用 7z 解压
   - 再检测输出
4. 递归必须受 `recursion_limit` 限制。
5. 同一个物理文件/路径在同一任务中不能重复处理，避免循环。
6. 非首部分卷必须跳过，例如 `.part2.rar`、`.002`。
7. 嵌套压缩包解压成功后，是否删除原嵌套包由配置控制，默认不永久删除。

### 5.2 解压候选模型

```rust
pub struct ExtractionCandidate {
    pub path: PathBuf,
    pub depth: u8,
    pub source: CandidateSource,
    pub detected_format: Option<ArchiveFormat>,
    pub embedded_offset: Option<u64>,
    pub embedded_size: Option<u64>,
}

pub enum CandidateSource {
    RootInput,
    ExtractedFile,
    EmbeddedFinding,
}
```

### 5.3 解压工作流模块边界

```text
SmartZipEngine::extract_recursive
├── CandidateQueue
├── CandidateDetector
│   ├── extension / volume naming
│   ├── smartzip-scanner / binwalk
│   └── ArchiveBackend::probe fallback
├── PasswordService::ranked_candidates
├── ArchiveBackend::{test,list,extract}
├── PasswordService::{record_success,record_failure}
└── OutputScanner enqueue nested candidates
```

MVP 可以先实现“路径级嵌套压缩包”：解压后扫描输出目录中的文件，识别 zip/7z/rar 等再入队。文件内 offset 级内嵌提取可以先检测和提示，后续再实现 offset 提取。

## 6. 压缩流程

```text
Input paths
  ↓
Classify selection
  ↓
Choose output naming strategy:
  single file → file.zip / file.7z
  mixed files → current-dir-name.zip
  folders only → one archive per folder
  ↓
Resolve name conflict with _1, _2...
  ↓
Build CompressArchiveRequest
  ↓
archive.compress()
  ↓
Record task and events
```

## 7. GUI 设计

### 7.1 主窗口布局

```text
┌─────────────────────────────────────────────────────────────┐
│ SmartZip                                           [设置]    │
├───────────────┬─────────────────────────────────────────────┤
│ 任务          │ 拖拽文件到此处，或点击选择文件              │
│ 密码库        │                                             │
│ 规则          │ 当前任务列表                                │
│ 日志          │ ┌─────────────────────────────────────────┐ │
│ 设置          │ │ archive.zip   解压中  42%              │ │
│               │ │ photo.7z      等待中                    │ │
│               │ │ data.rar      密码失败                  │ │
│               │ └─────────────────────────────────────────┘ │
└───────────────┴─────────────────────────────────────────────┘
```

### 7.2 任务详情

显示：

- 输入路径。
- 输出路径。
- 当前阶段。
- 进度。
- 密码尝试数量。
- 编码检测结果。
- 内嵌压缩包发现。
- 错误/警告。

### 7.3 密码库页面

显示：

- 密码值（可隐藏/显示）。
- 来源。
- 成功次数。
- 失败次数。
- 最近成功时间。
- 是否置顶。
- 是否禁用。

操作：

- 添加密码。
- 导入密码列表。
- 导出密码列表。
- 清理低价值密码。
- 置顶/取消置顶。
- 删除密码。

### 7.4 编码检测交互

当置信度高：

```text
自动选择：GB18030，置信度 0.93
```

当置信度低：

```text
文件名编码可能为：
[GB18030 0.58] [Shift_JIS 0.51] [Big5 0.43]
请选择后继续解压。
```

### 7.5 内嵌压缩包提示

```text
检测到文件内可能包含压缩包：
- sample.bin @ offset 1048576，格式 zip，置信度 0.96

[提取内嵌压缩包] [继续普通解压] [忽略]
```

## 8. CLI 设计

> 2026-09-05 更新：下一轮以 [CLI 交互设计草案](../.trellis/tasks/09-05-cli-interaction-design/design.md) 为详细提案，优先密码与编码。下述当前命令示例不代表草案新增参数已经实现。

detect 探测并报告，list 继续到可读条目，extract 继续到内容落盘；复用 engine 的归档访问与密码/编码能力。detect 不求密码、不确认编码。test 是独立全量校验命令，extract 不隐式增加 test 步骤。CLI 保持逐行提示与数字选择，复杂编码工作台放在 GUI。

### 8.1 本轮交互目标（待实现）

1. 密码隐藏输入并保留内容，输错可重试；仅可靠验证成功后保存，支持仅本批次使用。普通 ZIP 列目录成功不能作为密码正确证据。
2. 编码选择展示同一批真实名称、复列后显式确认；最终选择携带来源，只有人工确认可写 confirmed_encoding，选择绑定当前文件指纹。
3. 所有提示共享终端规则；提示/进度写 stderr；JSON 与显式非交互模式不读取答案；跳过归档、EOF、取消任务有不同语义。
4. 输出冲突针对布局后的真实目标；批次选择限定问题类型；最终摘要给出路径与原因。

新增选项、交互示例、保存规则和兼容边界以草案为准；[实施切片](../.trellis/tasks/09-05-cli-interaction-design/implement.md) 明确先修密码真实性，再做密码交互、编码对照与批次体验。

### 8.2 解压 `extract`（已接线）

```bash
smartzip extract archive.zip
smartzip extract a.zip b.7z c.rar --output ~/Downloads/out
smartzip extract archive.zip --encoding auto
smartzip extract archive.zip --encoding gb18030
smartzip extract suspicious.bin --deep --embedded ask
smartzip extract archive.zip --force          # 忽略 known_files 去重跳过，强制重解
```

去重：命中 `known_files`（`sample_hash + size`，`last_extract_at` 在时间窗内，默认 1 个月）时**跳过并显式提示**，`--force` 绕过。命中时同时复用 `confirmed_encoding`（人工确认过的）与 `password_id`。

### 8.3 检测 `detect`（已接线）

```bash
smartzip detect file.bin            # 格式 + 猜测编码 + 内嵌计数 + 文件名是否加密（纯报告，非交互）
smartzip detect file.bin --json
smartzip detect file.bin --deep
```

detect 不列条目、不确认编码、不解压。写 `tasks` + `file_extractions`（status=`detected`/`unreadable`），**不写** `known_files`（它没让用户确认编码）。

### 8.4 列条目与编码预览（已接线，交互待完善）

```bash
smartzip list archive.zip                     # 列出条目，用最佳猜测编码
smartzip list archive.zip --encoding gb18030  # 乱码时指定编码重列
smartzip enc archive.zip        # 查看不同编码下的名称
smartzip list archive.zip --pick-encoding     # 当前只列编码名称，尚无对照选择流程
```

list 的目标是看到可读内容：乱码时切换编码，文件名加密时获取访问密码。当前显式 `--encoding` / pick 选择可走确认编码写库路径，交互回调的最终选择仍需统一来源。只有可靠确认有效的密码才应记入 known_files；当前实现误把某些 list 成功当作密码成功，已纳入本轮设计的首个修复切片。

### 8.5 校验与压缩

```bash
smartzip test archive.7z
smartzip t movie.part03.rar other.zip --json
smartzip t archive.z02 --diagnostic-timeout 30
smartzip t archive.zip --diagnose off --no-history
smartzip compress file.txt           # 仍明确返回未实现错误
```

test 已实现任意卷入口与同组去重，失败后默认追加只读诊断；分别报告确认坏卷、疑似组、缺失、不可读和未检查范围。RAR5 使用本卷 header/packed CRC；7z 用可信 header 的 stream/folder 依赖；ZIP 用目录/local header/descriptor、ZIP64 与磁盘号定位，候选包含未受校验保护的参考 CRC 元数据。受加密、缺卷、编码或格式能力限制时保留未知范围。

详细证据契约和当前边界见 [分卷 test 设计](../.trellis/tasks/2026-07/07-03-test-command-backend-split/design.md)。所有 pass 共用 TaskEvent，复核经 executor 调度且尊重 `--backend`；`--diagnose off` 仅禁追加阶段。DB v4 保存完整 JSON 报告，旧 damaged_volumes_json 只投影确认坏卷；test 不写 last_extract_at，也不以首片 hash 代表整组。

执行结果为 0=全部组完整，1=无组完整，2=部分组完整，130=取消。参数错误沿用 clap 的 2。JSON 不交互；缺密码与密码/损坏歧义分别报告，不记虚假密码成功。

### 8.6 历史 `history`（两模式 + 下钻）

```bash
smartzip history tasks                        # 默认：操作级列表（一行一次操作）
smartzip history files                        # 文件级列表（一行一次解压动作）
smartzip history files --status failed        # 按 status 过滤（extracted/skipped/failed/partial/…）
smartzip history files --reason wrong_password # 按 reason 过滤
smartzip history show <task_id>               # 某次操作的事件时间线 + 名下所有 file 行
```

### 8.7 密码库 `password`

```bash
smartzip password list
smartzip password add 'password123'
smartzip password import passwords.txt
smartzip password cleanup --max-passwords 5000
```

cleanup 默认仅预览，`--apply` 才执行。当前 `--db`、`--config`、`--backend`、`--verbose-routing` 必须放在子命令前；真正 global 参数是下一轮目标。

## 9. 错误处理设计

统一错误类型：

```rust
pub enum SmartZipError {
    Io { path: Option<PathBuf>, source: std::io::Error },
    UnsupportedFormat { path: PathBuf, format: Option<String> },
    PasswordRequired { path: PathBuf },
    WrongPassword { path: PathBuf },
    CorruptedArchive { path: PathBuf, detail: String },
    EncodingAmbiguous { candidates: Vec<EncodingCandidate> },
    UnsafeArchivePath { entry: String },
    BackendUnavailable { backend: String },
    BackendFailed { backend: String, exit_code: Option<i32>, stderr: String },
    Cancelled,
}
```

GUI 显示用户友好错误，日志保留详细错误。

## 10. 安全设计

必须实现：

1. Zip Slip 防护：拒绝绝对路径、`../`、跨目标目录路径。
2. 解压到临时目录：成功后再移动到最终位置。
3. 删除默认进回收站，不默认永久删除。
4. 源文件删除默认关闭。
5. 内嵌扫描限制：最大文件大小、最大递归深度、最大候选数。
6. 密码不写入普通日志。
7. 明文密码数据库首次使用时提示用户。

## 11. MVP 范围

### 11.1 MVP 必须完成

1. Rust workspace 初始化。
2. `smartzip-core` 基本任务模型。
3. `smartzip-archive` 7zz 后端。
4. `smartzip-cli`：
   - `extract`
   - `compress`
   - `detect`
5. `smartzip-db`：SQLite 基础 schema。
6. `smartzip-passwords`：候选生成 + 基础排序 + 成功/失败记录。
7. `smartzip-encoding`：自动检测雏形。
8. `smartzip-scanner`：magic bytes + 基础内嵌检测。
9. `smartzip-gui`：
   - 主窗口
   - 拖拽文件
   - 任务列表
   - 进度展示
   - 密码库基础管理
   - 日志查看
10. Linux/macOS 可运行构建。

### 11.2 MVP 不做

1. 旧版 INI 导入。
2. Windows 右键菜单。
3. Linux 文件管理器深度动作。
4. macOS Finder Quick Actions。
5. 自动更新。
6. 插件系统。
7. 完整 archive preview 浏览器。

## 12. 实施切片

### Slice 1：Workspace 与基础类型

交付：

- Rust workspace。
- `smartzip-core`。
- 任务类型、错误类型、事件类型。
- 基础单元测试。

### Slice 2：7zz 后端

交付：

- `ArchiveBackend` trait。
- `SevenZipBackend`。
- `probe/list/test/extract/compress` 基础实现。
- 7zz 查找策略。

### Slice 3：SQLite 数据层

交付：

- schema migration。
- 密码表。
- 任务表。
- task events。
- repository API。

### Slice 4：密码策略

交付：

- 密码候选生成。
- 排序策略。
- 成功/失败记录。
- CLI password 子命令。

### Slice 5：智能解压核心

交付：

- 单文件/多文件输出目录策略。
- 密码尝试循环。
- 任务事件输出。
- 分卷跳过。

### Slice 6：编码检测

交付：

- UTF-8/GB18030/Big5/Shift_JIS/EUC-KR 候选评分。
- 低置信度结果。
- 记录检测历史。

### Slice 7：内嵌压缩包检测

交付：

- 引入 `binwalk` crate。
- `EmbeddedScanner` 封装 `Binwalk::scan(&[u8])`。
- 将 `SignatureResult` 映射为 SmartZip 内部 `EmbeddedArchiveFinding`。
- 支持格式白名单、置信度过滤、最大扫描大小、最大结果数。
- `smartzip detect`。

### Slice 8：GPUI 主界面

交付：

- 主窗口。
- 拖拽文件。
- 任务列表。
- 进度事件订阅。
- 日志页。

### Slice 9：GUI 密码库与设置

交付：

- 密码库管理。
- 基础设置。
- 明文密码提示。

### Slice 10：打包与发布雏形

交付：

- Linux AppImage 或 tarball。
- macOS app bundle 初版。
- bundled 7zz 策略。

## 13. 关键技术决策

| 决策 | 选择 | 原因 |
| --- | --- | --- |
| 主语言 | Rust | 核心、CLI、GUI 共用；性能和安全适合文件工具。 |
| GUI | GPUI | Zed 验证 Linux 可用；原生 Rust；轻量。 |
| 数据库 | SQLite | 密码排序、任务历史、检测历史需要索引。 |
| 配置 | TOML | 低频配置可读可编辑。 |
| 压缩后端 MVP | 7zz | 格式覆盖最强，MVP 快速可用。 |
| 压缩后端长期 | 混合后端 | 常见格式库级处理，复杂格式回退 7zz。 |
| 密码保存 | 本地明文 SQLite | 用户已确认无需加密；需 GUI 提示。 |
| 系统集成 | 后置 | 优先核心 GUI + CLI。 |
| 旧配置兼容 | 不兼容 | 用户已确认不需要。 |

## 14. 风险与缓解

| 风险 | 影响 | 缓解 |
| --- | --- | --- |
| GPUI API 仍 pre-1.0 | 未来升级有破坏性 | 锁定版本；GUI 封装在单 crate；减少 GPUI 扩散。 |
| 7zz 输出解析不稳定 | 进度/错误可能误判 | 封装解析器；建立 fixture 测试。 |
| 编码检测误判 | 文件名乱码 | 解压前预览；低置信度提示；记录用户修正。 |
| 密码库过大 | 排序性能下降 | SQLite 索引 + Top N 候选。 |
| 内嵌扫描耗时 | 大文件性能问题 | 使用 binwalk 时限制读取大小；默认 fast mode；deep mode 手动开启；限制大小和深度。 |
| macOS 打包签名 | 发布复杂 | MVP 先开发者构建，发布阶段再签名/公证。 |
| Windows 后续适配 | 行为不一致 | Windows 降为 P1；核心保持跨平台。 |

## 15. 验收标准

### MVP 技术验收

1. Linux/macOS 可启动 GPUI GUI。
2. GUI 支持拖拽文件并创建任务。
3. CLI 能完成 `extract/compress/detect`。
4. 能调用 7zz 解压 zip/7z/rar/tar/gz/bz2。
5. 能创建 zip，建议能创建 7z。
6. 能读取剪贴板密码。
7. 能记录密码成功/失败并调整排序。
8. 能自动检测编码并输出置信度。
9. 能检测 magic bytes 和基础内嵌压缩包。
10. 任务日志写入 SQLite 并在 GUI 中展示。

### 产品验收

1. 用户能通过 GUI 拖拽压缩包完成智能解压。
2. 用户能通过 CLI 批量解压。
3. 密码库可管理、可排序、可清理。
4. 乱码风险在解压前可提示。
5. 内嵌压缩包可被检测并提示。
6. 批量任务中单个失败不影响后续任务。
