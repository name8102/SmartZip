# SmartZip 跨平台桌面版重写更新

> **文档阶段**：需求冻结评审稿
> **面向平台**：Linux / macOS (P0 核心)，Windows (P1 兼容)
> **交付形态**：高性能 CLI 引擎 + 响应式本地 GUI (基于 Rust & GPUI)

---

## 1. 项目目标与核心价值

SmartZip 绝非传统意义上的通用压缩软件，其核心价值在于围绕现代桌面工作流，提供**全自动高匿爆破、嵌套/内嵌压缩包深度提取、乱码文件名智能纠正、解压后拓扑结构智能坍塌与清洗**的自动化管线。本次跨平台重构旨在全面剔除旧版本对 Windows 平台的深耦合，采用 Rust 语言重构出高并发、低延迟、高健壮性的跨平台底层。

---

## 2. 现代多层架构边界 (System Architecture Boundaries)

项目彻底采用解耦的 Multi-crate 工作空间架构，各模块职责严格定义如下：

```
┌─────────────────────────────────────────────────────────────┐
│                 smartzip-ui / smartzip-cli                  │
└──────────────────────────────┬──────────────────────────────┘
                               ▼
┌─────────────────────────────────────────────────────────────┐
│                      smartzip-engine                        │
│  (Orchestration Pipeline, BFS Queue, Filter, Collapse)      │
└────┬─────────────────┬─────────────────┬────────────────────┘
     │                 │                 │
     ▼                 ▼                 ▼
┌──────────┐     ┌───────────┐     ┌──────────┐     ┌─────────┐
│ archive  │     │ passwords │     │ scanner  │     │encoding │
└──────────┘     └───────────┘     └──────────┘     └─────────┘
     ▲                 ▲                 ▲
     └─────────────────┴────────┬────────┴────────────────────┘
                                ▼
                   ┌──────────────────────────┐
                   │   smartzip-db (SQLite)   │
                   └──────────────────────────┘

```

1. **`smartzip-core`**：全域通用领域模型、标准错误类型（`SmartZipError`）及进程/任务事件流定义。
2. **`smartzip-engine`**：核心工作流编排引擎。维护广度优先搜索（BFS）解压队列，控制解压、过滤、清洗、目录坍塌等管线的生命周期。
3. **`smartzip-archive`**：多后端解压/压缩抽象层（`ArchiveBackend` 特征）。负责 7zz/纯 Rust 库的底层命令行组装、安全路由与参数隔离。
4. **`smartzip-passwords`**：密码候选生成与权重决策服务。对接持久层，提供并发爆破字典生成。
5. **`smartzip-encoding`**：CJK 乱码文件名高精度文本探测器。
6. **`smartzip-scanner`**：基于 `binwalk` 幻数（Magic Bytes）驱动的二进制流深度扫描器。
7. **`smartzip-db` & `smartzip-config**`：SQLite 异步持久层与本地 TOML 静态配置层。
8. **`smartzip-platform`**：符合 XDG、App Support 规范的平台特异性路径与系统通知层。

---

## 3. 智能解压核心流水线规范 (Smart Extraction Pipeline)

解压引擎必须严格依循以下步骤执行递归提取：

### 3.1 格式与分卷安全预检 (Priority Shift to Magic Bytes)

* **幻数优先级绝对化**：系统路由格式时，**二进制流幻数（Magic Bytes）探测优先级必须高于文件后缀名。** 当遭遇恶意伪装后缀（如 `.zip` 改名为 `.rar`）或无后缀文件时，由 `smartzip-scanner` 提取其真实格式并修正 `ExtractionCandidate::detected_format`。
* **严格分卷阻断**：基于修正后的真实格式应用分卷规则。仅允许第一卷（如 `.part1.rar`、`.001`）进入解压管线。非首卷文件直接标记为 `Skipped`，严禁调起后端，防止重试开销。

### 3.2 乱码文件名探测时机抉择 (Delayed Encoding Detection)

* **非标头加密归档**：若归档文件名未加密，允许在无密码状态下通过 `backend.list` 提取原始字节流，交由 `chardetng` 探测编码，提前生成编码覆盖上下文（`EncodingMode::Override`）。
* **标头加密归档（Header Encryption）**：对于 7z、RAR 等文件名已被加密的归档，未取得正确密码前 `backend.list` 必然失败。**系统必须将文件名编码探测推迟到密码成功匹配之后、文件物理落盘之前**，或者在盲猜通过测试模式（Test Mode）的瞬间，立即提取列表进行编码分析，防止因使用默认编码解压导致本地文件系统写入乱码或损坏的路径。

### 3.3 高性能非阻塞密码匹配引擎 (High-Performance Async Password Engine)

当面对大密码库（Password Database）时，为规避高昂的磁盘 I/O 损耗与进程调度开销，必须应用以下技术：

* **内存测试探路（Test Mode First）**：盲猜密码阶段，禁止调用 `extract`。必须统一调用后端 `test` 模式（底层对应 `7z t`），只在纯内存中执行解密校验与 CRC 验证。只有当确定正确密码后，才执行唯一一次 `extract` 落盘。
* **异步并发流与取消机制（Parallel Streams & Kill-on-drop）**：利用 Tokio 结合 `futures::stream` 实现进程并发测试（根据 CPU 核心数限制最大并发度，如 4-8）。
* **进程生命周期强绑定**：底层的 `tokio::process::Command` 必须显式开启 **`kill_on_drop(true)`**。一旦并发流中某一个密码率先通过验证，系统在 Drop 掉其余密码任务的 Future 时，操作系统必须瞬间强行终止其余正在验证的 `7z` 进程，彻底杜绝后台僵尸进程堆积和 CPU 轰炸。
* **异步非阻塞交互兜底**：当持久层密码遍历完毕皆失败时，调起的 `InteractivePasswordPrompter::prompt` 必须为 **异步非阻塞特征（Async Future）**，严禁使用同步阻塞阻塞 Tokio 的 Worker 线程池，确保 UI 刷新及其他无密码任务的并发提取不受阻塞。

### 3.4 忽略规则清洗与拓扑坍塌联动 (Filter-Before-Collapse)

解压成功后，目标文件夹的整理必须严格满足两步链式联动：

1. **优先物理清洗（Ignore Filter）**：系统根据配置的 Glob 通配符忽略规则（如 `ignore_patterns: ["__MACOSX", ".DS_Store", "*.log"]`），采用深度优先倒序遍历，物理删除匹配到的垃圾文件及空文件夹。
2. **后置拓扑坍塌（Topology Collapse）**：只有清洗完毕后，系统才评估输出目录的结构。若剩余的有效条目数（Entries Count）绝对等于 1（例如原本包含 `target_dir` 和 `__MACOSX`，清洗后仅剩 `target_dir`），则完美触发层级提升，将该单项移动至上一级父目录，并彻底清除多余的外壳目录。

### 3.5 内嵌压缩包隔离物理切片 (Embedded Archive Carving)

* 当 `smartzip-scanner`（由 `binwalk` 驱动）在物理文件内部（如 `setup.exe` 或 `cover.png`）扫描到内嵌的隐藏压缩包时，系统包装出的候选对象必须包含确切的 `embedded_offset` 和 `embedded_size`。
* **数据切片隔离（Carving）**：在将任务委派给不支持 offset 的命令行后端之前，`smartzip-engine` 必须负责在系统的临时文件目录（`std::env::temp_dir`）中，将这段二进制流物理裁剪出来并写入一个干净的临时文件，再交由后端解压。解压完成后，系统需负责对该临时切片文件执行全生命周期销毁。

---

## 4. 新增进阶特性配置规范

### 4.1 可配置文件夹命名规则

解压目标文件夹不再允许硬编码。系统需在低频配置文件及 CLI/GUI 请求体中支持自定义命名模板字符串（`naming_rule`），其必须支持以下拓扑占位符的动态渲染：

* `{stem}`：原压缩包主文件名。
* `{depth}`：当前递归提取的嵌套深度层级（从 0 开始计数）。
* `{format}`：经幻数校验修正后的真实压缩格式字符串（如 `zip`, `7z`, `rar`）。
* `{offset}`：若属于文件内嵌包，代表其在宿主文件中的十六进制或十进制偏移量（若非内嵌包则默认为 0）。
* **命名碰撞防护**：当计算出的目标路径在本地文件系统中已存在同名实体时，系统禁止调用破坏性的 `remove_dir_all`。必须采用安全追加数字后缀（如 `_collided_1`, `_collided_2`）的温和迭代判定法，直至路径完全可用。

### 4.2 过滤与持久层 Schema 扩展

SQLite 持久层及本地 TOML 配置文件必须能够映射这两个高阶控制特征。

#### Local TOML Config (`smartzip-config`)

```toml
[scanner]
mode = "Fast"
max_scan_bytes = 67108864 # 64MB
min_confidence = "Medium"

[extraction]
recursion_limit = 3
delete_source_on_success = false
delete_source_to_trash = true
# 新增高阶控制流配置
naming_rule = "{stem}_depth{depth}_{format}"
ignore_patterns = ["__MACOSX", ".DS_Store", "Thumbs.db", "*.desktop"]

```

#### SQLite Schema (`smartzip-db`)

```sql
-- 确保规则索引与任务历史紧密绑定
CREATE TABLE IF NOT EXISTS task_histories (
    task_id TEXT PRIMARY KEY,
    task_kind TEXT NOT NULL,
    input_paths TEXT NOT NULL,  -- JSON Array
    output_path TEXT NOT NULL,
    applied_naming_rule TEXT,
    ignore_patterns_snapshot TEXT, -- 记录当时清洗的规则快照
    is_success INTEGER NOT NULL DEFAULT 0,
    elapsed_ms INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

```

---

## 5. 关键非功能性安全防线 (Critical Non-Functional Security)

1. **Zip Slip 路径穿越绝对防御**：后端解压释放文件名时，必须由 `smartzip-engine` 在中间层进行前置拓扑扫描。一旦检测到条目路径中包含 `../`、绝对路径或指向外部拓扑的符号链接（Symbolic Link），系统必须立刻拦截并抛出 `SmartZipError::UnsafeArchivePath`，直接熔断任务，绝不允许落盘。
2. **数据资产零破坏原则**：除解压成功后根据用户明确开启的配置将源压缩包移至系统**回收站/废纸篓**（必须使用平台级 Trash API，严禁直接逻辑删除）外，整个解压、重命名、坍塌及重试管线在遭遇冲突时，均不得调用任何针对用户原存数据的物理删除行为。
3. **离线高匿隐私安全**：爆破引擎、智能编码检测及幻数扫描管线必须确保 100% 本地离线可用。系统在任何情况下均不得将用户的明文密码、压缩包文件名、解压路径或操作日志上传至外部网络。

---

## 6. MVP 核心验收标准更新 (MVP Acceptance Criteria)

1. **后缀篡改抗性测试**：将一个标准的加密 `.zip` 文件强行改名为 `.rar` 甚至 `.png`，将其拖入系统后，系统必须能够依靠幻数正确路由至 Zip 后端，并顺畅完成密码爆破与提取。
2. **空外壳与垃圾清理测试**：解压一个包含 `__MACOSX`、`.DS_Store` 及真正内容文件夹的套娃压缩包，系统必须能彻底消灭垃圾文件，且解压后最外层不得出现双层同名目录嵌套（坍塌必须精准生效）。
3. **高频大字典压力测试**：导入包含 10000 条密码的字典，并发爆破一个原生加密 `.7z` 归档。应用必须通过“内存测试探路”在 5 秒内完成碰撞，且在此期间主界面（GUI）刷新率保持顺畅、Tokio 运行时无任何线程挂起、系统无僵尸进程残留。
4. **标头加密编码纠正测试**：解压一个开启了“加密文件名”且内部包含大量繁体/日文字符文件名的 7z 压缩包，系统在爆破成功后必须能够精准识别出 `Big5` 或 `Shift_JIS` 编码，且最终解压落盘的文件名不得出现任何乱码。

---

### 💡 更新说明

本轮更新通过在**编码探测时机、幻数路由优先级、异步进程销毁安全、忽略规则与坍塌先后置拓扑**这几个关键工程设计上树立了极其严苛的规范，消除了代码库中原有的隐式设计冲突，能够完美指引你的软件达到生产环境级别的工艺水平。
