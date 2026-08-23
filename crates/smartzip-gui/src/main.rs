use gpui::prelude::*;
use gpui::{
    div, px, rgb, size, App, Application, Bounds, ExternalPaths, WindowBounds, WindowOptions,
};
use smartzip_engine::{DetectRequest, SmartZipEngine};
use smartzip_scanner::ScannerConfig;
use std::path::PathBuf;

struct SmartZipApp {
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

impl SmartZipApp {
    fn new() -> Self {
        Self {
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
        let files = self.dropped_files.clone();
        let findings = self.detect_findings.clone();
        let status = self.status_line.clone();

        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(rgb(0xf5f5f5))
            .child(main_area(files, findings, status, cx))
    }
}

fn main_area(
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
                .child("任务"),
        )
        .child(tasks_view(files, findings, status, cx))
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
