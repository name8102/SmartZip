# SmartZip 跨平台技术栈评估

> 状态：历史研究文档  
> 说明：本文记录早期技术选型判断，其中的“下一步建议”不等同于当前实施顺序；当前实现状态请看 `docs/implementation-progress.md`，设计基线请看 `docs/design.md`。  
> 阶段：技术选型初评  
> 当前倾向：Rust Core + Rust CLI + GPUI GUI  
> 背景假设：Zed 在 Linux 上兼容性表现良好；系统集成需求优先级不高

## 1. 结论

在当前需求下，SmartZip 可以优先评估并尝试采用：

```text
Rust + GPUI + SQLite + 7zz/libarchive 后端抽象
```

理由：

1. 项目核心功能重计算、重 I/O、重文件系统，Rust 适合作为核心语言。
2. 第一版要求 GUI + CLI，Rust 可以同时支撑核心库、CLI 和 GUI。
3. Linux/macOS 优先，而 Zed 已验证 GPUI 在 Linux 上具备较好可用性。
4. 系统集成优先级不高，降低了 GPUI 生态不足带来的风险。
5. SmartZip GUI 主要是任务列表、进度、设置、密码库、日志、检测结果，不需要复杂 Web 生态。

建议决策：

- **首选方案**：Rust + GPUI。
- **后备方案**：Rust + Tauri。
- **不建议首选**：Electron。
- **可选但维护成本较高**：Qt。
- **GUI 强但核心复用不自然**：Flutter。

## 2. 推荐架构

```text
smartzip/
├── crates/
│   ├── smartzip-core        # 任务模型、智能解压、规则、路径冲突处理
│   ├── smartzip-archive     # 7zz/libarchive 后端抽象
│   ├── smartzip-passwords   # 密码候选、排序、SQLite 统计
│   ├── smartzip-encoding    # locale/文件名编码检测
│   ├── smartzip-scanner     # magic bytes / 内嵌压缩包检测
│   ├── smartzip-db          # SQLite schema、迁移、索引
│   ├── smartzip-platform    # Linux/macOS/Windows 平台能力封装
│   ├── smartzip-cli         # 命令行入口
│   └── smartzip-gui         # GPUI 桌面界面
├── docs/
└── packaging/
```

设计原则：

1. GUI 不直接实现业务逻辑。
2. CLI 和 GUI 共享同一套 core/archive/passwords/encoding/scanner/db。
3. 系统集成独立于核心，MVP 不阻塞。
4. 压缩后端抽象，避免被单一库或 CLI 绑定。

## 3. GPUI 适配 SmartZip 的优势

### 3.1 Rust 原生

SmartZip 的核心能力包括：

- 大文件处理
- 外部压缩进程管理
- 压缩库封装
- SQLite 密码数据库
- 文件签名扫描
- locale/编码检测
- CLI

这些用 Rust 实现比较自然。GPUI 同属 Rust 生态，可以减少跨语言桥接。

### 3.2 轻量和高性能

相比 Electron，GPUI 不需要捆绑完整 Chromium，理论上更适合 SmartZip 这种工具型桌面应用。

潜在收益：

- 启动更快
- 内存占用更低
- UI 与核心模型通信更直接
- 更容易做异步任务与进度更新

### 3.3 Linux/macOS 优先与 Zed 验证

用户已确认 Zed 在 Linux 上兼容性表现良好，因此 GPUI 的 Linux 可用性风险可以下调。

仍需项目内验证：

- 目标发行版
- Wayland/X11
- 中文输入法
- 文件拖拽
- 打包产物

### 3.4 系统集成低优先级降低风险

GPUI 生态较小的主要风险之一是系统级能力和插件生态不如 Tauri/Qt/Electron。由于 SmartZip 当前系统集成需求优先级不高，MVP 可以暂时只实现：

- 打开应用
- 拖拽文件到窗口
- CLI 调用
- 基础打开方式

右键菜单、Finder Quick Actions、Nautilus/Dolphin/Thunar 深度集成可后续补齐。

## 4. GPUI 风险

| 风险 | 影响 | 缓解 |
| --- | --- | --- |
| 框架生态小 | 组件、文档、打包资料较少 | 先做 Spike；GUI 保持简单；核心不依赖 GUI |
| Windows 成熟度不确定 | Windows 体验可能落后 | Windows 降为 P1，后续补齐 |
| 打包链路需验证 | Linux/macOS 发布可能遇到问题 | 早期建立 packaging 原型 |
| 辅助功能能力不明 | 无障碍需求可能受限 | MVP 不作为阻塞，后续评估 |
| 文件对话框/拖拽等平台能力需确认 | 影响基础交互 | Spike 必测 |
| UI 组件需要自建 | 设置页、表格、日志视图成本增加 | 控制 UI 复杂度，优先满足功能 |

## 5. 压缩后端选择

### 5.1 外部 7zz 优先

优点：

- 7z、rar、分卷、自解压支持强。
- 行为接近旧版 SmartZip。
- MVP 开发速度快。

缺点：

- 需要打包或检测系统安装。
- 需要解析 CLI 输出。
- 进度和错误语义需要封装。

### 5.2 内置库，例如 libarchive

优点：

- 更适合预览、路径安全检查、库级错误处理。
- 不依赖外部进程。

缺点：

- 复杂格式支持可能不如 7zz。
- Rust 绑定、跨平台构建和授权需要验证。

### 5.3 建议

采用后端抽象：

```text
ArchiveBackend
├── SevenZipBackend
└── LibArchiveBackend / NativeZipBackend
```

MVP 可以先实现 `SevenZipBackend`，后续加入库级后端。

## 6. 数据库方案

建议使用 SQLite。

主要表（历史设想，后续已收敛为不同的正式 schema 设计）：

- `passwords`
- `password_stats`
- `password_matches`
- `tasks`
- `task_events`
- `encoding_detections`
- `embedded_archive_detections`
- `settings_overrides` 或配置文件保存设置

密码不加密，但 GUI 需明确提示。

密码排序建议使用候选集策略：

1. 空密码。
2. 剪贴板密码。
3. 最近成功密码。
4. 当前文件名/路径相似命中的密码。
5. 全局高成功率密码 Top N。
6. 用户固定置顶密码。
7. 低优先级长尾密码。

## 7. GPUI Spike 验证清单

正式选定 GPUI 前，建议先完成一个最小原型。

### 7.1 Linux 必测

1. 启动窗口。
2. 中文显示。
3. 中文输入法。
4. 文件/目录选择。
5. 文件拖拽到窗口并获取路径。
6. 长任务进度条更新。
7. 后台线程执行压缩检测，不阻塞 UI。
8. SQLite 读写。
9. 读取剪贴板文本。
10. AppImage 或至少可分发二进制原型。

### 7.2 macOS 必测

1. 启动 `.app` 或开发构建。
2. 文件拖拽。
3. 文件/目录选择。
4. 中文显示和输入。
5. 读取剪贴板文本。
6. 后台任务进度。
7. SQLite 读写。
8. Apple Silicon 构建。

### 7.3 可延后

1. Windows GUI。
2. 右键菜单。
3. Finder Quick Actions。
4. Nautilus/Dolphin/Thunar 动作。
5. 自动更新。
6. 完整无障碍。

## 8. MVP 建议

MVP 聚焦：

1. Rust workspace 初始化。
2. CLI：
   - `smartzip extract`
   - `smartzip compress`
   - `smartzip detect`
3. GPUI GUI：
   - 拖拽文件
   - 任务列表
   - 进度显示
   - 密码库简单管理
   - 日志查看
4. 7zz 后端。
5. SQLite 密码数据库。
6. locale 自动检测雏形。
7. magic bytes 内嵌压缩包检测雏形。

暂不阻塞 MVP：

- 文件管理器深度集成
- 自动更新
- Windows 完整支持
- 旧版配置迁移
- 复杂主题系统

## 9. 当前推荐

在用户确认“Zed Linux 兼容性表现良好，系统集成优先度不高”的前提下，推荐：

```text
首选：Rust + GPUI + SQLite + 7zz 后端
回退：Rust + Tauri + SQLite + 7zz 后端
```

下一步应先做 GPUI Spike，而不是直接全面实现。
