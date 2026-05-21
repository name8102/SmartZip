# SmartZip 正式架构设计方案

> 技术栈：Rust + GPUI + SQLite + 7zz  
> 交付形态：CLI + GUI 同时交付  
> 平台优先级：Linux / macOS (P0)，Windows (P1)

---

## 1. 项目概述

SmartZip 是一个**智能解压桌面工具**，核心价值不是“通用压缩软件”，而是围绕 7-Zip 提供更智能的批量解压、密码尝试、编码检测、内嵌压缩包扫描、解压后整理和规则化操作。

### 设计原则

1. **核心与 GUI 分离**：核心库（core, archive, passwords, encoding, scanner）可独立单元测试，不依赖 GPUI。
2. **CLI 与 GUI 共享核心**：两者调用同一套核心 API，行为一致。
3. **后端抽象**：压缩后端通过 trait 抽象，可切换 7zz / libarchive / 系统工具。
4. **平台适配隔离**：系统集成（右键菜单、文件管理器动作）不与核心逻辑耦合。
5. **离线 & 隐私**：默认不上传任何数据；所有操作在本地完成。
6. **可观察性**：结构化日志，每个任务有完整记录。

---

## 2. 项目目录结构

```
smartzip/
├── Cargo.toml                     # workspace 根
├── Cargo.lock
├── README.md
├── LICENSE
│
├── crates/
│   ├── smartzip-core/             # 核心业务逻辑
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs             # 公开 API
│   │       ├── task.rs            # Task 模型与状态机
│   │       ├── extract.rs         # 智能解压工作流
│   │       ├── compress.rs        # 压缩工作流
│   │       ├── rules.rs           # 解压后规则引擎
│   │       ├── detect.rs          # 内嵌/格式检测工作流
│   │       ├── conflict.rs        # 路径冲突处理
│   │       └── error.rs           # 统一错误类型
│   │
│   ├── smartzip-archive/          # 压缩后端抽象
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs             # ArchiveBackend trait
│   │       ├── formats.rs         # 格式定义 & magic bytes
│   │       ├── sevenzz.rs         # 7zz CLI 后端实现
│   │       ├── libarchive.rs      # libarchive 后端实现 (可选)
│   │       └── progress.rs        # 进度回传
│   │
│   ├── smartzip-passwords/        # 密码管理
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs             # PasswordDb, Candidate, Source
│   │       ├── strategy.rs        # 排序策略
│   │       └── trapdoor.rs        # 弱混淆 (非加密，仅防随手看)
│   │
│   ├── smartzip-encoding/         # 文件名编码检测
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs             # EncodingDetector trait
│   │       ├── chardetng.rs       # chardetng 实现
│   │       └── table.rs           # 常用编码表
│   │
│   ├── smartzip-scanner/          # 内嵌压缩包扫描 (封装 binwalk)
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs             # Scanner, EmbeddedArchive, 封装 binwalk::Binwalk
│   │       └── limits.rs          # 扫描深度 & 大小限制
│   │
│   ├── smartzip-db/               # SQLite 数据库
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs             # DbPool, Connection, Migration
│   │       ├── schema.rs          # 表结构定义
│   │       ├── migrations/        # SQL 迁移文件
│   │       └── queries/           # 查询封装
│   │
│   ├── smartzip-platform/         # 平台系统集成
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs             # Platform trait + PlatformOps
│   │       ├── linux.rs           # .desktop / DBus / Nautilus
│   │       ├── macos.rs           # macOS Quick Actions / Services
│   │       ├── windows.rs         # Windows 注册表 / COM
│   │       └── file_manager.rs    # 文件管理器分级抽象
│   │
│   ├── smartzip-cli/              # 命令行入口
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── main.rs            # clap 命令行解析
│   │       ├── extract.rs         # 解压子命令
│   │       ├── compress.rs        # 压缩子命令
│   │       ├── detect.rs          # 检测子命令
│   │       └── config.rs          # 配置子命令
│   │
│   └── smartzip-gui/              # GPUI 桌面 GUI
│       ├── Cargo.toml
│       └── src/
│           ├── main.rs            # Application::new().run()
│           ├── app.rs             # 应用状态
│           ├── views/
│           │   ├── main_window.rs # 主窗口 (拖拽区域 + 任务列表)
│           │   ├── task_list.rs   # 任务列表组件
│           │   ├── settings.rs    # 设置页面
│           │   ├── passwords.rs   # 密码库管理
│           │   ├── logs.rs        # 日志查看
│           │   ├── encoding.rs   # 编码检测结果
│           │   └── about.rs       # 关于页面
│           ├── components/
│           │   ├── progress.rs    # 进度条
│           │   ├── file_drop.rs   # 文件拖拽组件
│           │   ├── button.rs      # 按钮
│           │   └── list.rs        # 文件列表
│           └── theme.rs           # 主题 & 深色模式
│
├── resources/
│   ├── icons/
│   ├── locales/                   # i18n (预留)
│   └── default_config.toml        # 默认配置
│
├── packaging/
│   ├── linux/                     # AppImage / deb / rpm
│   ├── macos/                     # .app bundle
│   └── windows/                   # installer
│
└── docs/
    ├── requirements.md
    ├── architecture.md            # 本文件
    ├── gpui-verification.md
    └── tech-evaluation.md
```

---

## 3. Crate 依赖关系

```text
                  ┌──────────────┐
                  │ smartzip-cli │
                  └──────┬───────┘
                         │
                  ┌──────┴───────┐
                  │ smartzip-gui │
                  └──────┬───────┘
                         │
                  ┌──────┴──────────────┐
                  │    smartzip-core    │
                  └──────┬──────┬───────┘
                         │      │
         ┌───────────────┤      ├──────────────┐
         │       │       │      │              │
   ┌─────┴────┐ ┌─┴──────┐ ┌───┴────┐ ┌──────┴──────┐
   │ smartzip │ │smartzip│ │smartzip │ │ smartzip    │
   │-archive  │ │-passwd │ │-encodng│ │ -scanner    │
   └─────┬────┘ └───┬────┘ └───┬────┘ └──────┬──────┘
         │          │          │              │
         └──────────┼──────────┼──────────────┘
                    │          │
              ┌─────┴──────────┴────┐
              │    smartzip-db     │
              └────────────────────┘
```

依赖方向：`cli / gui → core → {archive, passwords, encoding, scanner, db}`

核心 crate (`smartzip-core`) 不依赖 GPUI，不依赖任何 GUI 框架。

---

## 4. 核心数据模型

### 4.1 Task（任务）

```rust
// smartzip-core/src/task.rs

#[derive(Debug, Clone)]
pub struct TaskId(pub Uuid);

#[derive(Debug, Clone)]
pub struct Task {
    pub id: TaskId,
    pub kind: TaskKind,
    pub inputs: Vec<PathBuf>,           // 输入文件路径
    pub output_dir: Option<PathBuf>,    // 输出目录
    pub status: TaskStatus,
    pub progress: Progress,
    pub started_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub result: Option<TaskResult>,
}

#[derive(Debug, Clone)]
pub enum TaskKind {
    Extract(ExtractOptions),
    Compress(CompressOptions),
    Detect(DetectOptions),
}

#[derive(Debug, Clone)]
pub enum TaskStatus {
    Pending,
    Running,
    Paused,
    Completed,
    Failed(String),
    Cancelled,
}

#[derive(Debug, Clone)]
pub struct Progress {
    pub percent: f64,           // 0.0 ~ 100.0
    pub current_item: String,   // 当前正在处理的文件名
    pub bytes_processed: u64,
    pub bytes_total: u64,
    pub items_processed: u64,
    pub items_total: u64,
    pub elapsed: Duration,
    pub estimated_remaining: Option<Duration>,
}

#[derive(Debug, Clone)]
pub struct TaskResult {
    pub files_extracted: u64,
    pub files_created: u64,
    pub bytes_processed: u64,
    pub password_used: Option<String>,
    pub encoding_used: Option<String>,
    pub embedded_archives_found: u64,
    pub warnings: Vec<String>,
}
```

### 4.2 ExtractOptions（解压选项）

```rust
// smartzip-core/src/task.rs

#[derive(Debug, Clone)]
pub struct ExtractOptions {
    pub password_strategy: PasswordStrategy,
    pub output_rules: OutputRules,
    pub encoding_policy: EncodingPolicy,
    pub nested_extract: NestedExtractPolicy,
    pub scan_embedded: bool,
    pub delete_after_extract: bool,
}

#[derive(Debug, Clone)]
pub enum PasswordStrategy {
    /// 自动尝试：空密码 → 剪贴板 → 上次成功 → 密码库
    Auto,
    /// 指定单密码
    Fixed(String),
    /// 手动输入
    Manual,
    /// 仅尝试密码库
    DatabaseOnly,
    /// 跳过所有密码尝试（不解密加密包）
    SkipEncrypted,
}

#[derive(Debug, Clone)]
pub struct OutputRules {
    pub remove_empty_dirs: bool,
    pub delete_patterns: Vec<GlobPattern>,
    pub rename_rules: Vec<RenameRule>,
    pub skip_patterns: Vec<GlobPattern>,
}

#[derive(Debug, Clone)]
pub struct RenameRule {
    pub pattern: GlobPattern,           // 匹配原文件名
    pub replacement: String,            // 替换模板，支持 $1..$N
}

#[derive(Debug, Clone)]
pub enum EncodingPolicy {
    /// 自动检测，低置信度时询问
    Auto,
    /// 自动检测，低置信度时使用指定备选
    AutoWithFallback(String),
    /// 固定编码
    Fixed(String),
}

#[derive(Debug, Clone)]
pub enum NestedExtractPolicy {
    /// 不处理嵌套压缩包
    None,
    /// 单文件嵌套解压
    SingleOnly,
    /// 第一层嵌套
    FirstLevel,
    /// 递归全部
    Recursive,
}
```

---

## 5. 压缩后端抽象

### 5.1 ArchiveBackend Trait

```rust
// smartzip-archive/src/lib.rs

use async_trait::async_trait;

#[async_trait]
pub trait ArchiveBackend: Send + Sync + Debug {
    /// 后端名称，用于日志和错误消息
    fn name(&self) -> &str;

    /// 支持的格式列表
    fn supported_formats(&self) -> &[ArchiveFormat];

    /// 检查文件是否需要密码
    async fn needs_password(&self, path: &Path) -> Result<bool, ArchiveError>;

    /// 列出压缩包内容
    async fn list(
        &self,
        path: &Path,
        password: Option<&str>,
    ) -> Result<Vec<ArchiveEntry>, ArchiveError>;

    /// 解压
    async fn extract(
        &self,
        path: &Path,
        output: &Path,
        password: Option<&str>,
        progress: ProgressSender,
    ) -> Result<ExtractResult, ArchiveError>;

    /// 创建压缩包
    async fn create(
        &self,
        output: &Path,
        inputs: &[PathBuf],
        options: &CompressOptions,
        progress: ProgressSender,
    ) -> Result<(), ArchiveError>;

    /// 测试完整性
    async fn test(
        &self,
        path: &Path,
        password: Option<&str>,
    ) -> Result<TestResult, ArchiveError>;
}
```

### 5.2 7zz 后端实现

```rust
// smartzip-archive/src/sevenzz.rs

pub struct SevenZZBackend {
    binary_path: PathBuf,
    /// 是否使用系统安装的 7zz，还是打包的
    source: BinarySource,
}

pub enum BinarySource {
    /// 系统 PATH 中的 7zz
    SystemPath,
    /// 随应用打包的 7zz
    Bundled(PathBuf),
    /// 用户配置的自定义路径
    Custom(PathBuf),
}
```

7zz 后端特点：

- 通过 `tokio::process::Command` 异步启动 7zz 进程
- 解析 stdout/stderr 获取进度信息
- 通过 `ProgressSender` 回传进度到核心层
- 通过退出码判断成功/失败
- 识别密码错误、文件损坏、格式不支持等不同错误

错误分类示例：

```rust
pub enum ArchiveError {
    PasswordRequired,
    PasswordIncorrect,
    Corrupted,
    FormatNotSupported,
    VolumeMissing(PathBuf),
    IoError(std::io::Error),
    BackendError(String),
    Cancelled,
}
```

### 5.3 进度回传

```rust
// smartzip-archive/src/progress.rs

/// 从异步任务向调用方回传进度
pub type ProgressSender = tokio::sync::mpsc::UnboundedSender<ProgressEvent>;

#[derive(Debug, Clone)]
pub enum ProgressEvent {
    /// 进度更新
    Update(Progress),
    /// 单个文件开始处理
    FileStarted(PathBuf),
    /// 单个文件完成
    FileCompleted(PathBuf),
    /// 需要用户输入（如密码）
    InputRequired(InputRequest),
    /// 警告
    Warning(String),
}

#[derive(Debug, Clone)]
pub enum InputRequest {
    Password,
}
```

---

## 6. 异步模型

### 6.1 总体架构

```text
GPUI Event Loop (main thread)
│
├─ cx.update()  ──→ 更新 View 状态  ──→ GPU 重新渲染
│
├─ cx.spawn()   ──→ 后台异步任务 (tokio)
│                    │
│                    ├─ ArchiveBackend::extract()
│                    │   └─ 7zz CLI process
│                    │       └─ stdout/stderr 解析
│                    │
│                    ├─ Scanner::scan()
│                    │
│                    ├─ EncodingDetector::detect()
│                    │
│                    └─ ProgressSender → cx.update() 更新 UI 进度
│
└─ 文件拖拽/按钮点击 → 创建 Task → 加入队列 → spawn
```

### 6.2 任务队列

```rust
// smartzip-core/src/task.rs

pub struct TaskQueue {
    queue: VecDeque<Task>,
    running: Option<TaskId>,
    sender: ProgressSender,
}

impl TaskQueue {
    /// 添加任务
    pub fn enqueue(&mut self, task: Task) -> TaskId;
    
    /// 开始下一个任务（由 spawn 驱动）
    pub fn start_next(&mut self, cx: &mut AsyncApp) -> Option<TaskId>;
    
    /// 取消任务
    pub fn cancel(&mut self, id: TaskId);
    
    /// 暂停/恢复
    pub fn pause(&mut self, id: TaskId);
    pub fn resume(&mut self, id: TaskId);
}
```

### 6.3 密码输入流

```text
TaskQueue 发现需要密码
│
├─ 先自动尝试 PasswordStrategy
│   ├─ 空密码
│   ├─ 剪贴板
│   ├─ 上次成功密码
│   └─ 密码库 Top N 候选
│
└─ 全部失败 → 向 UI 发出 InputRequest::Password
               │
               ├─ GUI 弹窗要求输入
               │
               └─ 用户输入 → 继续尝试 → 成功则记录到数据库
```

---

## 7. 数据库设计

使用 SQLite，放在平台标准数据目录。

### 7.1 表结构

```sql
-- 密码库
CREATE TABLE passwords (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    password    TEXT NOT NULL,
    source      TEXT NOT NULL,      -- 'manual', 'imported', 'clipboard', 'derived'
    created_at  TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at  TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX idx_passwords_password ON passwords(password);

-- 密码使用统计
CREATE TABLE password_stats (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    password_id     INTEGER NOT NULL REFERENCES passwords(id),
    archive_path    TEXT,                 -- 匹配的压缩包路径模式
    success_count   INTEGER NOT NULL DEFAULT 0,
    failure_count   INTEGER NOT NULL DEFAULT 0,
    last_success_at TEXT,
    last_failure_at TEXT,
    created_at      TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX idx_password_stats_password ON password_stats(password_id);
CREATE INDEX idx_password_stats_success ON password_stats(success_count DESC);
CREATE INDEX idx_password_stats_last_success ON password_stats(last_success_at DESC);

-- 任务历史
CREATE TABLE tasks (
    id              TEXT PRIMARY KEY,     -- UUID
    kind            TEXT NOT NULL,         -- 'extract', 'compress', 'detect'
    status          TEXT NOT NULL,
    input_paths     TEXT NOT NULL,         -- JSON array
    output_dir      TEXT,
    started_at      TEXT,
    completed_at    TEXT,
    duration_ms     INTEGER,
    result_json     TEXT,                 -- 成功结果 (JSON)
    error_message   TEXT,
    password_used   TEXT,
    encoding_used   TEXT,
    embedded_found  INTEGER DEFAULT 0,
    files_processed INTEGER DEFAULT 0,
    bytes_processed INTEGER DEFAULT 0
);
CREATE INDEX idx_tasks_started ON tasks(started_at DESC);
CREATE INDEX idx_tasks_kind ON tasks(kind);

-- 编码检测历史（用于改进后续检测）
CREATE TABLE encoding_detections (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    archive_path    TEXT,
    filename_pattern TEXT,
    detected_encoding TEXT,
    confidence      REAL,
    user_correction  TEXT,
    created_at      TEXT NOT NULL DEFAULT (datetime('now'))
);

-- 内嵌压缩包检测记录
CREATE TABLE embedded_detections (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    file_path       TEXT NOT NULL,
    format          TEXT,
    offset          INTEGER,
    confidence      REAL,
    extracted       INTEGER DEFAULT 0,
    created_at      TEXT NOT NULL DEFAULT (datetime('now'))
);
```

### 7.2 数据库版本管理

```rust
// smartzip-db/src/migrations.rs

pub trait Migration {
    fn version(&self) -> u32;
    fn sql(&self) -> &str;
}

// 首次启动自动创建，之后自动迁移
pub fn run_migrations(conn: &Connection) -> Result<()>;
```

---

## 8. 密码策略详细设计

### 8.1 候选生成

```rust
// smartzip-passwords/src/strategy.rs

impl PasswordDb {
    pub fn candidates(&self, archive: &Path) -> Vec<PasswordCandidate> {
        let mut candidates = Vec::new();
        
        // 1. 空密码 (优先级最高)
        candidates.push(empty_password());
        
        // 2. 剪贴板文本
        if let Some(clip) = current_clipboard() {
            candidates.push(clipboard_password(clip));
        }
        
        // 3. 上次成功密码（全局最近一次）
        if let Some(last) = self.last_successful() {
            candidates.push(last_success(last));
        }
        
        // 4. 文件名/目录名派生密码
        candidates.extend(self.derive_from_path(archive));
        
        // 5. 与当前压缩包同来源/同目录的历史成功密码
        candidates.extend(self.history_matches(archive));
        
        // 6. 全局高成功率 Top N
        candidates.extend(self.top_n(20));
        
        // 7. 用户手动置顶密码
        candidates.extend(self.pinned());
        
        // 8. 长尾低频率密码
        candidates.extend(self.tail(5));
        
        // 按优先级排序，去重
        candidates.sort_by(|a, b| b.priority.total_cmp(&a.priority));
        candidates.dedup_by(|a, b| a.password == b.password);
        candidates.truncate(100);
        
        candidates
    }
}
```

### 8.2 排序因素

```
priority = w1 * recency_score
         + w2 * success_rate
         + w3 * path_match_score
         + w4 * filename_match_score
         + w5 * global_success_count
         + w6 * pinned_bonus
```

各因素权重可通过配置调整，默认：

- `w1 = 2.0` (最近使用时间)
- `w2 = 3.0` (成功率)
- `w3 = 5.0` (路径匹配度)
- `w4 = 4.0` (文件名匹配度)
- `w5 = 1.0` (全局成功次数)
- `w6 = 10.0` (手动置顶)

### 8.3 密码清理

```rust
/// 清理低价值密码：
/// 1. 超过 N 次失败且从未成功过的
/// 2. 超过 180 天未使用的
/// 3. 密码数量超过上限时，移除综合评分最低的
pub fn prune(&mut self, max_count: usize) -> PruneResult;
```

---

## 9. 编码检测设计

```rust
// smartzip-encoding/src/lib.rs

/// 编码检测器
pub struct EncodingDetector {
    inner: chardetng::EncodingDetector,  // 底层使用 chardetng
    known_encodings: Vec<EncodingInfo>,
}

pub struct EncodingInfo {
    pub name: &'static str,
    pub encoding: &'static Encoding,
    pub language: &'static str,     // "zh", "ja", "ko", ...
    pub aliases: &'static [&'static str],
}

/// 检测结果
pub struct EncodingResult {
    pub primary: &'static Encoding,
    pub confidence: f64,
    pub candidates: Vec<(&'static Encoding, f64)>,
}

impl EncodingDetector {
    pub fn new() -> Self;
    
    /// 检测字节数组的编码
    pub fn detect(&self, bytes: &[u8]) -> EncodingResult;
    
    /// 检测压缩包内文件名的编码
    /// 收集所有文件名 → 拼接后一次性检测
    pub fn detect_filenames(&self, entries: &[ArchiveEntry]) -> EncodingResult;
    
    /// 检测低置信度时，返回候选编码供用户选择
    pub fn candidates_below_threshold(&self, result: &EncodingResult, threshold: f64)
        -> Vec<(&'static Encoding, f64)>;
}
```

### 编码候选表

| 编码 | 语言区域 | 常见场景 |
| --- | --- | --- |
| UTF-8 | 通用 | 现代默认 |
| GBK / GB18030 | 中文 (简体) | 国内 Windows 压缩包 |
| Big5 | 中文 (繁体) | 港澳台地区 |
| Shift_JIS | 日文 | 日本压缩包 |
| EUC-KR | 韩文 | 韩国压缩包 |
| ISO-8859-1 | 西欧 | 欧洲压缩包 |

### 用户修正学习

```sql
-- 当用户手动修正编码时，记录到 encoding_detections 表
-- 后续对相似文件名/同来源的压缩包，优先使用用户上次成功的编码
```

---

## 10. 内嵌压缩包扫描设计（封装 binwalk v3）

不自行实现，直接使用 [binwalk v3](https://github.com/ReFirmLabs/binwalk) Rust 库 (MIT 协议)。

### 10.1 为什么用 binwalk

binwalk v3 是成熟的固件分析工具，重写为 Rust 后可作为库集成。其能力远超自研：

| 能力 | binwalk | 自研方案 |
| --- | --- | --- |
| Magic bytes 签名数 | 数百条（社区持续维护） | 10+ 条 |
| 压缩流检测 | ✅ Deflate/LZMA/bzip2/zlib 原始流 | ❌ |
| 熵分析 | ✅ 识别未知压缩/加密区域 | ❌ |
| CRC 验证降假阳性 | ✅ | ❌ |
| 提取内嵌内容 | ✅ | ❌ |
| 大文件全量扫描 | ✅ 设计用于固件，支持内存映射 | ⚠️ 仅头尾 |

### 10.2 集成方式

```toml
# smartzip-scanner/Cargo.toml
[dependencies]
binwalk = "3.1"
```

```rust
// smartzip-scanner/src/lib.rs

use binwalk::Binwalk;

pub struct Scanner {
    inner: Binwalk,
    max_file_size: u64,          // 超过此大小跳过扫描
    max_scan_duration: Duration, // 最长扫描时间
}

pub struct EmbeddedArchive {
    pub offset: u64,
    pub size: u64,
    pub description: String,
    pub confidence: u8,          // 0/128/250 = LOW/MEDIUM/HIGH
    pub name: String,            // 签名类型名称
}

impl Scanner {
    pub fn new() -> Self { ... }

    /// 扫描文件内嵌压缩包
    /// 读取文件内容后调用 binwalk::Binwalk::scan()
    pub fn scan(&self, path: &Path) -> Result<Vec<EmbeddedArchive>> {
        let data = std::fs::read(path)?;
        let results = self.inner.scan(&data);
        // 过滤出压缩包相关签名，按 offset 去重
        Ok(results.into_iter()
            .filter(|r| is_archive_signature(&r.name))
            .map(|r| EmbeddedArchive {
                offset: r.offset as u64,
                size: r.size as u64,
                description: r.description,
                confidence: r.confidence,
                name: r.name,
            })
            .collect())
    }

    /// 快速判断文件是否为已知压缩包格式
    pub fn quick_check(&self, path: &Path) -> Result<bool> { ... }
}

fn is_archive_signature(name: &str) -> bool {
    matches!(
        name,
        "zip" | "rar" | "7zip" | "gzip" | "bzip2" | "xz"
        | "tar" | "cab" | "lzma" | "zlib" | "iso"
        | "apk" | "ar" | "cpio" | "rpm"
    )
}
```

### 10.3 风险与缓解

| 风险 | 缓解 |
| --- | --- |
| binwalk 将文件全量读入内存 | 超过 `max_file_size` (默认 10GB) 时跳过扫描，提示用户文件过大 |
| binwalk API 变更 | 用 `smartzip-scanner` crate 隔离，版本锁定 |
| 扫描耗时长 | `max_scan_duration` 超时控制，可在后台异步运行 |
| 假阳性 | 只报告 `CONFIDENCE_MEDIUM` 以上的结果 |

---

## 11. 智能解压工作流

完整的解压工作流：

```text
extract(paths, options)
│
├─ 1. 分卷识别
│   ├─ 跳过非首卷 (.part2.rar, .002 等)
│   └─ 收集首卷和分卷路径
│
├─ 2. 格式检测 (binwalk quick_check + 后缀)
│   └─ 如果不支持 → 错误
│
├─ 3. 内嵌压缩包扫描 (可选，使用 binwalk)
│   └─ 如果发现 → 报告并询问
│
├─ 4. 编码检测
│   ├─ 读取压缩包内文件名
│   ├─ 检测编码 → 置信度
│   └─ 低置信度 → 询问用户
│
├─ 5. 密码尝试
│   ├─ 先判断是否需要密码
│   ├─ 按 PasswordStrategy 逐批尝试
│   └─ 全部失败 → 询问用户
│
├─ 6. 执行解压
│   ├─ 带编码参数 (7zz -scsUTF-8 等)
│   ├─ 监控进度
│   └─ 检查退出码
│
├─ 7. 解压后处理
│   ├─ 智能输出目录决策
│   │   ├─ 单文件 → 移动到目标目录
│   │   └─ 多项目 → 创建文件夹
│   ├─ 删除规则
│   ├─ 重命名规则
│   └─ 删除空目录
│
├─ 8. 嵌套压缩包处理 (可选)
│   ├─ 扫描输出目录中的新压缩包
│   └─ 递归调用 extract()
│
└─ 9. 任务记录
    └─ 写入数据库
```

---

## 12. CLI 设计

```text
smartzip extract [OPTIONS] <PATHS>...
  智能解压一个或多个压缩包

  OPTIONS:
    -o, --output-dir <DIR>       解压目标目录
    -p, --password <PASS>        指定密码
    --encoding <ENC>             指定文件名编码 (auto, gbk, shift-jis, ...)
    --encoding-auto              自动检测编码（默认）
    --scan-embedded              扫描内嵌压缩包
    --no-nested                  不处理嵌套压缩包
    --delete-after               解压后删除源文件
    -y, --yes                    自动确认

smartzip compress [OPTIONS] <PATHS>...
  压缩文件或文件夹

  OPTIONS:
    -o, --output <FILE>          输出文件名
    -f, --format <FORMAT>        压缩格式 (zip, 7z)
    --level <LEVEL>              压缩级别 (0-9)
    -p, --password <PASS>        加密密码

smartzip detect <PATHS>...
  检测文件类型，扫描内嵌压缩包

smartzip open <PATH>
  用系统默认程序打开或预览压缩包

smartzip config [KEY] [VALUE]
  查看或修改配置

smartzip passwords [list|add|remove|clear|import|export]
  管理密码库

smartzip logs [--follow]
  查看任务历史

smartzip help
  帮助信息
```

CLI 退出码约定：

| 退出码 | 含义 |
| --- | --- |
| 0 | 成功 |
| 1 | 部分文件处理失败 |
| 2 | 所有文件处理失败 |
| 10 | 密码错误 |
| 11 | 文件不存在或无法读取 |
| 12 | 格式不支持 |
| 20 | 用户取消 |
| 30 | 内部错误 |

---

## 13. GUI 视图结构 (GPUI)

```
MainWindow
├── DropZone                    # 拖拽区域（大幅面，接收 ExternalPaths）
│   ├── 提示文字: "拖拽文件到此" / 选择文件按钮
│   └── 文件路径列表（拖入后显示）
│
├── TaskListView                # 任务列表
│   ├── TaskCard × N            # 每个任务一个卡片
│   │   ├── 文件图标 + 文件名
│   │   ├── 进度条（ProgressBar 组件）
│   │   ├── 状态标签（排队中/进行中/已完成/失败/已取消）
│   │   ├── 按钮：取消 / 重试 / 打开输出目录
│   │   └── 展开详情（日志、编码检测结果、密码信息）
│   └── 空状态提示
│
├── Sidebar (可选)
│   ├── Settings 快捷入口
│   ├── Password Manager 入口
│   ├── Logs 入口
│   └── About
│
└── StatusBar
    ├── 队列总数
    ├── 运行中任务数
    └── 当前正在处理的文件名
```

SettingsWindow（独立窗口）

```
SettingsWindow
├── Tab: General
│   ├── 默认解压目录
│   ├── 默认压缩格式
│   ├── 压缩级别
│   └── 语言 (预留)
│
├── Tab: Passwords
│   ├── 自动保存成功密码 (开关)
│   ├── 剪贴板自动尝试 (开关)
│   ├── 密码数量上限
│   └── 清理按钮
│
├── Tab: Rules
│   ├── 删除规则列表
│   ├── 重命名规则列表
│   ├── 排除规则列表
│   └── 嵌套解压策略
│
├── Tab: Integration
│   ├── 系统集成设置（平台特有）
│   └── 文件关联
│
└── Tab: About
    ├── 版本信息
    ├── 压缩后端信息
    └── 许可证
```

---

## 14. 配置管理

### 14.1 配置文件位置

| 平台 | 路径 |
| --- | --- |
| Linux | `~/.config/smartzip/config.toml` |
| macOS | `~/Library/Application Support/SmartZip/config.toml` |
| Windows | `%APPDATA%/SmartZip/config.toml` |

### 14.2 配置结构

```toml
# 默认配置
[general]
default_output_dir = ""
default_compress_format = "zip"
default_compress_level = 5
language = "zh-CN"
theme = "auto"             # "auto", "light", "dark"

[passwords]
auto_save = true
clipboard_auto_try = true
max_passwords = 500
prune_threshold = 1000

[rules]
remove_empty_dirs = true
nested_extract = "first-level"   # "none", "single", "first-level", "recursive"
scan_embedded = true
delete_after_extract = false

[encoding]
auto_detect = true
fallback_encoding = "UTF-8"

[logging]
level = "info"               # "off", "error", "warn", "info", "debug"
```

---

## 15. 构建与发布

### 15.1 构建策略

```toml
# Cargo.toml (workspace root)
[workspace]
members = [
    "crates/smartzip-core",
    "crates/smartzip-archive",
    "crates/smartzip-passwords",
    "crates/smartzip-encoding",
    "crates/smartzip-scanner",
    "crates/smartzip-db",
    "crates/smartzip-platform",
    "crates/smartzip-cli",
    "crates/smartzip-gui",
]
```

### 15.2 发布产物

```text
Linux (P0):
  ├─ AppImage: smartzip-x86_64.AppImage (推荐)
  ├─ deb: smartzip_1.0.0_amd64.deb
  └─ rpm: smartzip-1.0.0.x86_64.rpm

macOS (P0):
  ├─ .app bundle: SmartZip.app
  └─ 可选公证后 .dmg

Windows (P1):
  ├─ installer: SmartZip-Setup.exe
  └─ portable: SmartZip.exe
```

### 15.3 7zz 打包策略

- Linux：AppImage 内嵌 `7zz` 二进制，也检测 `$PATH` 中是否有 7zz
- macOS：在 `.app` bundle 中嵌入 `7zz`
- Windows：作为资源嵌入或在安装器中选择性安装

优先打包 `7zz`（7-Zip 的 standalone 版本，MIT 许可证），而非完整 7-Zip。

---

## 16. 实施阶段

### 阶段 1：基础设施 (Week 1-2)

- [ ] 初始化 Rust workspace 和所有 crates
- [ ] `smartzip-core`: Task、TaskQueue、错误类型
- [ ] `smartzip-db`: SQLite schema、migration
- [ ] `smartzip-archive`: ArchiveBackend trait、7zz 后端解析原型
- [ ] `smartzip-cli`: 基础 CLI 框架（`extract`、`compress`、`detect`）

### 阶段 2：核心工作流 (Week 3-4)

- [ ] `smartzip-core`: 智能解压完整工作流
- [ ] `smartzip-archive`: 7zz 后端完整实现
- [ ] `smartzip-passwords`: 密码库、候选生成、排序策略
- [ ] `smartzip-encoding`: 编码检测
- [ ] `smartzip-scanner`: 封装 binwalk v3
- [ ] `smartzip-cli`: CLI 完整功能

### 阶段 3：GUI (Week 5-6)

- [ ] `smartzip-gui`: 主窗口、拖拽、任务列表
- [ ] `smartzip-gui`: 设置页面、密码库管理、日志
- [ ] `smartzip-gui`: 进度更新、编码修正、密码弹窗
- [ ] 端到端流程打通

### 阶段 4：完善 (Week 7-8)

- [ ] `smartzip-platform`: 系统集成（Linux 优先）
- [ ] 深色模式、主题
- [ ] 多语言基础框架 + 中英文
- [ ] 错误处理、边界情况打磨
- [ ] 打包：AppImage、.app bundle

### 阶段 5：发布 (Week 9-10)

- [ ] 测试（单元测试 + 集成测试）
- [ ] 文档完善
- [ ] 性能优化
- [ ] 发布
