# GPUI 可用性验证报告

> 验证方式：通过 rust-docs MCP skill 缓存并查询 `gpui v0.2.2` 文档 & 源码  
> 验证日期：2026-05-20  
> 环境：Arch Linux x86_64, Rust 1.95.0, Wayland/X11

## 验证结论

**GPUI 可以满足 SmartZip 跨平台重写的基本 GUI 需求。**

关键能力清单：

| 需求 | 状态 | 依据 |
| --- | --- | --- |
| 窗口生命周期 | ✅ 支持 | `Application::new()` → `.run()` → `App::open_window()` |
| 基础 UI 渲染 | ✅ 支持 | `Render` trait + `div()` + tailwind 风格 CSS |
| 中文文本显示 | ✅ 支持 | 基于 cosmic-text + 系统字体回退栈 |
| 外部文件拖拽 | ✅ 支持 | `FileDropEvent::Entered { paths }` → `div().on_drop::<ExternalPaths>()` |
| 剪贴板读取 | ✅ 支持 | `App::read_from_clipboard()` → `ClipboardItem` |
| 文件选择对话框 | ✅ 支持 | `App::prompt_for_paths()` → `PathPromptOptions` |
| 异步后台任务 | ✅ 支持 | `AsyncWindowContext` + `BackgroundExecutor` + `Task` |
| 系统打开/展示文件 | ✅ 支持 | `App::open_with_system()`, `App::reveal_path()` |
| 窗口原生弹窗 | ✅ 支持 | `Window::prompt()` |
| Linux X11 支持 | ✅ 支持 | feature `x11` default 包含 |
| Linux Wayland 支持 | ✅ 支持 | feature `wayland` default 包含 |
| macOS 支持 | ✅ 支持 | `platform/mac/` 源码确认 |
| Windows 支持 | ⚠️ 源码存在但非 P0 | `platform/windows/` 源码完整 |
| 系统集成（右键菜单等） | ⬜ 由 platform crate 负责 | GPUI 不提供此能力 |

---

## 详细验证记录

### 1. Application 与 Window 生命周期

源文件：`src/app.rs`

```rust
// 启动应用
Application::new().run(|cx: &mut App| {
    let bounds = Bounds::centered(None, size(px(800.), px(600.)), cx);
    cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            ..Default::default()
        },
        |_, cx| {
            cx.new(|_| HelloWorld { text: "世界".into() })
        },
    ).unwrap();
    cx.activate(true);
});
```

`WindowOptions` 支持：
- `window_bounds`：位置和尺寸
- `titlebar`：标题栏配置
- `focus`、`show`、`kind`
- `is_movable`、`is_resizable`、`is_minimizable`
- `display_id`、`app_id`
- `window_background`：背景外观（透明/模糊/不透明）
- `window_min_size`：最小窗口尺寸
- `window_decorations`：客户端/服务端装饰（Wayland）

### 2. UI 渲染（Render + div）

源文件：`src/element.rs`、`src/elements/div.rs`

```rust
struct MyView { text: SharedString }

impl Render for MyView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap_3()
            .bg(rgb(0x505050))
            .size(px(500.))
            .justify_center()
            .items_center()
            .text_xl()
            .text_color(rgb(0xffffff))
            .child(format!("你好, {}!", &self.text))
    }
}
```

SmartZip 需要的 GUI 元素都可以用 div + tailwind 样式表达：
- ✅ 列表（任务列表、日志）
- ✅ 按钮
- ✅ 输入框
- ✅ 进度条
- ✅ 表格/数据列表

### 3. 外部文件拖拽（FileDrop）

源文件：`src/interactive.rs`、`src/window.rs`、`src/elements/div.rs`

```rust
// 文件拖拽进入窗口被转换为内部拖放事件
// div 上监听文件落下：
use gpui::interactive::ExternalPaths;

div()
    .on_drop::<ExternalPaths>(|paths: &ExternalPaths, window, cx| {
        // paths: SmallVec<[PathBuf; 2]>
        for path in paths.iter() {
            println!("Dropped: {}", path.display());
        }
    })
```

文件拖拽流：
1. 系统文件拖入窗口 → `FileDropEvent::Entered { position, paths: ExternalPaths }`
2. 转换为内部 `AnyDrag { value: Arc::new(paths) }`
3. 鼠标释放 → `FileDropEvent::Submit { position }` → MouseUp → 分发到包含 `on_drop` 的元素

`ExternalPaths` 内部为 `SmallVec<[PathBuf; 2]>`，即包含多个拖拽文件路径。

✅ Linux X11 实现确认（`src/platform/linux/x11/client.rs:875`）
✅ Linux Wayland 实现确认（`src/platform/linux/wayland/client.rs:1953`）
✅ macOS 实现确认（`src/platform/mac/window.rs:2411`）

### 4. 剪贴板读取

源文件：`src/app.rs`

```rust
// 读取剪贴板
if let Some(item) = cx.read_from_clipboard() {
    if let Some(text) = item.text() {
        // 尝试用剪贴板文本作为密码
        let password = text.trim();
    }
}

// 写入剪贴板
cx.write_to_clipboard(ClipboardItem::new_string("some text"));

// Linux 主选择缓冲区
cx.read_from_primary();
cx.write_to_primary(item);
```

### 5. 文件选择对话框

源文件：`src/app.rs`、`src/platform.rs`

```rust
use gpui::PathPromptOptions;

let receiver = cx.prompt_for_paths(PathPromptOptions {
    files: true,
    directories: false,
    multiple: true,
    prompt: Some("选择压缩包".into()),
});
// 通过 oneshot::Receiver 异步获取结果
```

### 6. 异步与后台任务

源文件：`src/app/async_context.rs`、`src/executor.rs`

```rust
// 在窗口上下文中启动异步任务
cx.spawn(|mut cx| async move {
    // 非阻塞执行压缩/解压操作
    let result = do_heavy_work().await;
    cx.update(|_, cx| {
        // 更新 GUI
    });
}).detach();
```

### 7. 中文/东亚文本支持

GPUI 在 Linux 上通过 `cosmic-text` + `font-kit` 进行文本布局和渲染。

系统字体回退栈：

```rust
fallback_font_stack: smallvec![
    font(".ZedMono"),
    font(".ZedSans"),
    font("Helvetica"),        // macOS
    font("Segoe UI"),         // Windows
    font("Ubuntu"),           // Gnome (Ubuntu)
    font("Adwaita Sans"),     // Gnome 47
    font("Cantarell"),        // Gnome
    font("Noto Sans"),        // KDE
    font("DejaVu Sans"),
    font("Arial"),            // macOS, Windows
]
```

支持：

- ✅ `all_font_names()` 列出所有系统字体
- ✅ `FontWeight` 细粒度字重控制
- ✅ Unicode 特性通过 `icu_properties` 系列 crate 实现
- ✅ CJK 文本通过系统字体回退显示（如 Noto Sans CJK）
- 中文输入：XIM 模块已确认存在（`src/platform/linux/client.rs` + `xim` crate）

### 8. 系统打开/展示

```rust
// 用系统默认程序打开文件
cx.open_with_system(Path::new("archive.zip"));

// 在文件管理器中展示
cx.reveal_path(Path::new("/path/to/archive"));

// 用默认浏览器打开 URL
cx.open_url("https://example.com");
```

### 9. 窗口弹窗

```rust
let answer = window.prompt(
    PromptLevel::Info,
    "确定要删除吗？",
    None,
    &["确定", "取消"],
    cx,
);
```

---

## 需要原型验证的项目（Spike 清单）

以下内容无法仅通过源码分析确认，需要实际编译运行测试：

### P0 - 必须验证

| # | 验证项 | 说明 |
| --- | --- | --- |
| 1 | Linux 窗口启动 | `Application::new().run()` 在 Wayland/X11 正常 |
| 2 | 中文显示 | 中文文本在窗口内无乱码 |
| 3 | 中文输入 | Fcitx/IBus 输入法在 GPUI 窗口内可用 |
| 4 | 文件拖拽 | 从文件管理器拖拽文件到窗口，`on_drop::<ExternalPaths>` 可获取路径 |
| 5 | 文件选择器 | `prompt_for_paths()` 可弹出并返回路径 |
| 6 | 剪贴板读取 | `read_from_clipboard()` 正常返回文本 |
| 7 | 异步任务进度 | 后台线程执行任务，UI 不阻塞，进度可更新 |
| 8 | SQLite 读取 | `rusqlite` + 异步集成正常 |
| 9 | 读取 7zz 输出 | 异步执行外部进程并解析 stdout/stderr |

### P1 - 建议验证

| # | 验证项 | 说明 |
| --- | --- | --- |
| 10 | macOS 构建 | 确认 macOS 编译和基础运行 |
| 11 | 深色模式 | 跟随系统深色主题 |
| 12 | 高 DPI | 字体和元素在 HiDPI 下清晰 |
| 13 | 长列表性能 | 大量文件列表（上千条）不卡顿 |
| 14 | 日志列表渲染 | GPUI `UniformList` 或元素复用情况 |
| 15 | 多窗口 | 设置窗口和主窗口同时打开 |

---

## GPUI 对 SmartZip 的充分性评估

### SmartZip GUI 需求映射

| SmartZip GUI 需求 | GPUI 实现方案 | 可行性 |
| --- | --- | --- |
| 拖拽文件到窗口 | `div().on_drop::<ExternalPaths>()` | ✅ |
| 点击按钮触发操作 | `div().on_click()` | ✅ |
| 文件列表显示 | `UniformList` / `List` | ✅ |
| 任务进度条 | `div()` + 自定义渲染 | ✅ |
| 密码库管理界面 | `div()` + `List` + 输入框 | ✅ |
| 设置页面 | `div()` + `InteractiveElement` | ✅ |
| 日志查看 | `UniformList` + 搜索 | ✅ |
| 编码检测结果展示 | `div()` + 文本 | ✅ |
| 深色模式 | `WindowAppearance` + `Colors` | ✅ |
| 多语言 | `SharedString` + 翻译层 | 需自行实现 |

### GPUI 不提供的功能

SmartZip 需要的这些功能需在 platform crate 或外部实现：

1. **系统集成**（右键菜单、Quick Actions、文件管理器动作）
2. **自动更新**
3. **托盘图标**（不确定 GPUI 是否支持）
4. **打包/安装器**

这些在 SmartZip 需求中优先级不高（MVP 后补齐），不影响 GPUI 选型决策。

---

## 结论

GPUI v0.2.2 为 SmartZip 提供了足够的 GUI 基础能力：

1. ✅ 窗口管理与基础 UI
2. ✅ 外部文件拖拽（FileDropEvent + ExternalPaths）
3. ✅ 剪贴板读写
4. ✅ 文件选择对话框
5. ✅ 异步任务执行
6. ✅ 中文文本显示
7. ✅ Linux/macOS/Windows 平台抽象

**建议进入下一步：创建 GPUI Spike 项目**，编译运行验证以上 P0 清单中的 9 项。
