use gpui::prelude::*;
use gpui::{
    div, px, rgb, size, App, Application, Bounds, ExternalPaths, WindowBounds, WindowOptions,
};
use smartzip_engine::{DetectRequest, SmartZipEngine};
use smartzip_scanner::ScannerConfig;
use std::path::PathBuf;

struct SmartZipApp {
    active_tab: Tab,
    dropped_files: Vec<PathBuf>,
    detect_findings: Vec<DetectFinding>,
    status_line: String,
}

#[derive(Clone)]
struct DetectFinding {
    format: String,
    offset: u64,
    size: String,
    description: String,
}

#[derive(Clone, Copy, PartialEq)]
enum Tab {
    Tasks,
    Passwords,
    Rules,
    Logs,
    Settings,
}

impl Tab {
    fn label(&self) -> &'static str {
        match self {
            Tab::Tasks => "任务",
            Tab::Passwords => "密码库",
            Tab::Rules => "规则",
            Tab::Logs => "日志",
            Tab::Settings => "设置",
        }
    }
}

impl SmartZipApp {
    fn new() -> Self {
        Self {
            active_tab: Tab::Tasks,
            dropped_files: Vec::new(),
            detect_findings: Vec::new(),
            status_line: String::new(),
        }
    }
}

impl Render for SmartZipApp {
    fn render(
        &mut self,
        _window: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) -> impl IntoElement {
        let active = self.active_tab;
        let files = self.dropped_files.clone();
        let findings = self.detect_findings.clone();
        let status = self.status_line.clone();

        div()
            .flex()
            .flex_row()
            .size_full()
            .bg(rgb(0xf5f5f5))
            .child(sidebar(active))
            .child(main_area(active, files, findings, status, cx))
    }
}

fn sidebar(active: Tab) -> impl IntoElement {
    let tabs = [
        (Tab::Tasks, "任务"),
        (Tab::Passwords, "密码库"),
        (Tab::Rules, "规则"),
        (Tab::Logs, "日志"),
        (Tab::Settings, "设置"),
    ];

    div()
        .flex()
        .flex_col()
        .w(px(160.0))
        .h_full()
        .bg(rgb(0x2c2c2c))
        .text_color(rgb(0xcccccc))
        .px_4()
        .py_5()
        .gap_1()
        .child(
            div()
                .text_lg()
                .text_color(rgb(0xffffff))
                .font_weight(gpui::FontWeight::BOLD)
                .mb_4()
                .child("SmartZip"),
        )
        .children(tabs.iter().map(|(tab, label)| {
            let is_active = active == *tab;
            div()
                .flex()
                .px_3()
                .py_2()
                .rounded_sm()
                .text_sm()
                .bg(if is_active {
                    rgb(0x404040)
                } else {
                    rgb(0x2c2c2c)
                })
                .text_color(if is_active {
                    rgb(0xffffff)
                } else {
                    rgb(0xcccccc)
                })
                .child(label.to_string())
        }))
}

fn main_area(
    active: Tab,
    files: Vec<PathBuf>,
    findings: Vec<DetectFinding>,
    status: String,
    cx: &mut gpui::Context<SmartZipApp>,
) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .size_full()
        .p_6()
        .child(
            div()
                .text_xl()
                .font_weight(gpui::FontWeight::BOLD)
                .mb_2()
                .child(format!("{}", active.label())),
        )
        .child(match active {
            Tab::Tasks => tasks_view(files, findings, status, cx).into_any_element(),
            Tab::Passwords => passwords_view().into_any_element(),
            Tab::Rules => rules_view().into_any_element(),
            Tab::Logs => logs_view().into_any_element(),
            Tab::Settings => settings_view().into_any_element(),
        })
}

fn tasks_view(
    files: Vec<PathBuf>,
    findings: Vec<DetectFinding>,
    status: String,
    cx: &mut gpui::Context<SmartZipApp>,
) -> impl IntoElement {
    let file_count = files.len();

    div()
        .flex()
        .flex_col()
        .gap_3()
        .when(!status.is_empty(), |el| {
            el.child(
                div()
                    .mb_2()
                    .px_3()
                    .py_1()
                    .bg(rgb(0xe8f5e9))
                    .text_color(rgb(0x2e7d32))
                    .rounded_sm()
                    .text_sm()
                    .child(status),
            )
        })
        .child(drop_zone(file_count, files, cx))
        .when(!findings.is_empty(), |el| {
            el.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(gpui::FontWeight::BOLD)
                            .child("检测结果:"),
                    )
                    .children(findings.iter().map(|f| {
                        div()
                            .flex()
                            .flex_row()
                            .gap_2()
                            .text_sm()
                            .child(div().text_color(rgb(0x1565c0)).child(f.format.clone()))
                            .child(
                                div()
                                    .text_color(rgb(0x666666))
                                    .child(format!("@ 0x{:X}", f.offset)),
                            )
                            .child(
                                div()
                                    .text_color(rgb(0x666666))
                                    .child(format!("size={}", f.size)),
                            )
                            .child(div().text_color(rgb(0x888888)).child(f.description.clone()))
                    })),
            )
        })
}

fn drop_zone(
    file_count: usize,
    files: Vec<PathBuf>,
    cx: &mut gpui::Context<SmartZipApp>,
) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .p_6()
        .h(px(120.0))
        .bg(rgb(0xffffff))
        .rounded_md()
        .border_2()
        .border_color(rgb(0xbdbdbd))
        .border_dashed()
        .on_drop(cx.listener(
            |_app: &mut SmartZipApp, paths: &ExternalPaths, _window, cx| {
                let new_files = paths.paths().to_vec();
                // C4: spawn async detection to avoid blocking the GUI thread
                cx.spawn(
                    |entity: gpui::WeakEntity<SmartZipApp>, cx: &mut gpui::AsyncApp| {
                        let mut async_cx = cx.clone();
                        async move {
                            let engine = SmartZipEngine::default();
                            let mut new_findings = Vec::new();
                            for path in &new_files {
                                if let Ok(result) = engine.detect(DetectRequest {
                                    path: path.clone(),
                                    scanner: ScannerConfig::default(),
                                }) {
                                    for f in &result.findings {
                                        new_findings.push(DetectFinding {
                                            format: f.format.as_str().into(),
                                            offset: f.offset,
                                            size: f
                                                .size
                                                .map(|s| s.to_string())
                                                .unwrap_or_else(|| "?".into()),
                                            description: f.description.clone(),
                                        });
                                    }
                                }
                            }
                            let file_count = new_files.len();
                            let finding_count = new_findings.len();
                            entity
                                .update(&mut async_cx, move |app, cx| {
                                    app.dropped_files = new_files;
                                    app.detect_findings = new_findings;
                                    if app.detect_findings.is_empty() {
                                        app.status_line = format!(
                                            "已接收 {} 个文件, 未检测到内嵌压缩包",
                                            file_count
                                        );
                                    } else {
                                        app.status_line = format!(
                                            "已接收 {} 个文件, 检测到 {} 个内嵌压缩包",
                                            file_count, finding_count
                                        );
                                    }
                                    cx.notify();
                                })
                                .ok();
                        }
                    },
                )
                .detach();
            },
        ))
        .child(if file_count == 0 {
            div()
                .text_color(rgb(0x999999))
                .text_center()
                .child("拖拽文件到此处（自动检测）")
                .into_any_element()
        } else {
            div()
                .flex()
                .flex_col()
                .gap_0()
                .text_sm()
                .children(files.iter().map(|p| {
                    div()
                        .text_color(rgb(0x333333))
                        .child(p.display().to_string())
                }))
                .into_any_element()
        })
}

fn passwords_view() -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(div().text_color(rgb(0x666666)).child("密码数据库管理"))
        .child(
            div()
                .flex()
                .flex_col()
                .p_4()
                .bg(rgb(0xffffff))
                .rounded_md()
                .border_1()
                .border_color(rgb(0xe0e0e0))
                .gap_2()
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .gap_4()
                        .text_sm()
                        .font_weight(gpui::FontWeight::BOLD)
                        .child(div().w(px(140.0)).child("密码"))
                        .child(div().w(px(60.0)).child("来源"))
                        .child(div().w(px(50.0)).child("成功"))
                        .child(div().w(px(50.0)).child("失败")),
                )
                .child(
                    div()
                        .text_color(rgb(0x999999))
                        .child("请使用 smartzip-cli password add 添加密码"),
                ),
        )
}

fn rules_view() -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(div().text_color(rgb(0x666666)).child("解压规则配置"))
        .child(
            div()
                .p_4()
                .bg(rgb(0xffffff))
                .rounded_md()
                .border_1()
                .border_color(rgb(0xe0e0e0))
                .text_sm()
                .child("将在后续版本中完善"),
        )
}

fn logs_view() -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(div().text_color(rgb(0x666666)).child("操作日志"))
        .child(
            div()
                .p_4()
                .bg(rgb(0x1b1b1b))
                .text_color(rgb(0x00ff00))
                .rounded_md()
                .text_sm()
                .child("SmartZip GUI ready.\n拖拽文件到窗口即可自动检测."),
        )
}

fn settings_view() -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(div().text_color(rgb(0x666666)).child("应用设置"))
        .child(setting_row("默认压缩格式", "ZIP"))
        .child(setting_row("压缩级别", "平衡"))
        .child(setting_row("删除源文件", "关闭"))
        .child(setting_row("自动检测编码", "开启"))
        .child(setting_row("深色模式", "跟随系统"))
}

fn setting_row(label: &str, value: &str) -> impl IntoElement {
    div()
        .flex()
        .flex_row()
        .justify_between()
        .px_3()
        .py_2()
        .bg(rgb(0xffffff))
        .rounded_sm()
        .border_1()
        .border_color(rgb(0xe8e8e8))
        .child(div().text_sm().child(label.to_string()))
        .child(
            div()
                .text_sm()
                .text_color(rgb(0x666666))
                .child(value.to_string()),
        )
}

fn main() {
    Application::new().run(|cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(900.0), px(600.0)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |_, cx| cx.new(|_| SmartZipApp::new()),
        )
        .unwrap();
        cx.activate(true);
    });
}
