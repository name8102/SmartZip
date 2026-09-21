//! Native windows are projections of one queue; archive work never runs on the UI thread.
use crate::{
    library::{self, ConfigSnapshot, LibraryOptions, PasswordSummary},
    model::{Phase, Queue},
    runtime::{InteractionRequest, JobRequest, TaskOperation, TaskSettings},
};
use gpui::assets::IconName;
use gpui::base::Disableable;
use gpui::component::{
    button::{Button, ButtonVariants},
    input::{Input, InputState},
    progress::Progress,
    sidebar::{
        Sidebar, SidebarFooter, SidebarGroup, SidebarHeader, SidebarMenu, SidebarMenuItem,
        SidebarToggleButton,
    },
    switch::Switch,
    ActiveTheme, Icon, Root, Sizable, Theme,
};
use gpui::{
    div, prelude::*, px, size, App, AppContext, Bounds, Context, Entity, ExternalPaths,
    IntoElement, PathPromptOptions, Render, SharedString, Subscription, Window, WindowBounds,
    WindowHandle, WindowOptions,
};
use std::{path::PathBuf, sync::mpsc, time::Duration};

pub struct Workspace {
    queue: Queue,
    previews: Queue,
    preview_revision: u64,
    open_requests: mpsc::Receiver<crate::new_open_request::OpenRequest>,
    pending_open: Vec<crate::new_open_request::OpenRequest>,
    config: Option<ConfigSnapshot>,
    config_revision: u64,
    settings: TaskSettings,
    message: String,
    quick: Option<WindowHandle<Root>>,
    full: Option<WindowHandle<Root>>,
    quitting: bool,
    inbox: mpsc::Receiver<LibraryMessage>,
    sender: mpsc::Sender<LibraryMessage>,
    history: Vec<smartzip_db::task::TaskRecord>,
    passwords: Vec<PasswordSummary>,
    document: String,
    library_busy: bool,
}
enum LibraryMessage {
    Config(Result<Box<ConfigSnapshot>, String>),
    ConfigSaved(Result<Box<ConfigSnapshot>, String>),
    History(Result<Vec<smartzip_db::task::TaskRecord>, String>),
    Passwords(Result<Vec<PasswordSummary>, String>),
    Document(Result<String, String>),
    Mutation(Result<String, String>),
}
impl Workspace {
    fn new(
        open_requests: mpsc::Receiver<crate::new_open_request::OpenRequest>,
        cx: &mut Context<Self>,
    ) -> Self {
        let (sender, inbox) = mpsc::channel();
        let tx = sender.clone();
        std::thread::spawn(move || {
            let _ = tx.send(LibraryMessage::Config(
                library::load_config(&LibraryOptions::default()).map(Box::new),
            ));
        });
        cx.spawn(async move |entity, cx| loop {
            cx.background_executor()
                .timer(Duration::from_millis(100))
                .await;
            if entity
                .update(cx, |state, cx| {
                    let mut changed = state.queue.tick() | state.previews.tick();
                    while let Ok(paths) = state.open_requests.try_recv() {
                        state.pending_open.push(paths);
                    }
                    if state.config.is_some() && !state.pending_open.is_empty() {
                        for request in std::mem::take(&mut state.pending_open) {
                            let quick =
                                request.intent == crate::new_open_request::OpenIntent::QuickExtract;
                            state.enqueue(
                                request.paths,
                                if quick {
                                    TaskOperation::Extract
                                } else {
                                    TaskOperation::List
                                },
                            );
                            open_view(cx.entity(), quick, cx);
                        }
                        changed = true;
                    }
                    while let Ok(message) = state.inbox.try_recv() {
                        changed = true;
                        state.library_busy = false;
                        match message {
                            LibraryMessage::Config(result) => match result {
                                Ok(config) => {
                                    state.message = config.diagnostics.join("\n");
                                    state.config = Some(*config);
                                    state.config_revision += 1;
                                }
                                Err(e) => {
                                    state.config = None;
                                    state.message = e;
                                }
                            },
                            LibraryMessage::ConfigSaved(result) => match result {
                                Ok(config) => {
                                    state.message =
                                        "配置已保存，新任务立即生效；快速配置的显式覆盖仍优先"
                                            .into();
                                    state.config = Some(*config);
                                    state.config_revision += 1;
                                }
                                Err(e) => state.message = format!("保存失败，修改仍保留：{e}"),
                            },
                            LibraryMessage::History(result) => match result {
                                Ok(rows) => state.history = rows,
                                Err(e) => state.message = e,
                            },
                            LibraryMessage::Passwords(result) => match result {
                                Ok(rows) => state.passwords = rows,
                                Err(e) => state.message = e,
                            },
                            LibraryMessage::Document(result) => match result {
                                Ok(text) => state.document = text,
                                Err(e) => state.message = e,
                            },
                            LibraryMessage::Mutation(result) => {
                                state.message = result.unwrap_or_else(|e| e)
                            }
                        }
                    }
                    if state.quitting && !state.queue.has_active() && !state.previews.has_active() {
                        cx.quit();
                    }
                    if changed {
                        cx.notify();
                    }
                })
                .is_err()
            {
                break;
            }
        })
        .detach();
        Self {
            queue: Queue::default(),
            previews: Queue::default(),
            preview_revision: 0,
            open_requests,
            pending_open: vec![],
            config: None,
            config_revision: 0,
            settings: TaskSettings::default(),
            message: "正在读取配置…".into(),
            quick: None,
            full: None,
            quitting: false,
            inbox,
            sender,
            history: vec![],
            passwords: vec![],
            document: String::new(),
            library_busy: false,
        }
    }
    fn enqueue(&mut self, paths: Vec<PathBuf>, operation: TaskOperation) {
        self.enqueue_with_hold(paths, operation, false);
    }

    fn enqueue_with_hold(&mut self, paths: Vec<PathBuf>, operation: TaskOperation, hold: bool) {
        if paths.is_empty() {
            return;
        }
        let Some(config) = &self.config else {
            self.message = "配置尚未就绪，请在设置中重新加载并解决错误".into();
            return;
        };
        let settings = self.settings.clone();
        let resolved = Some(config.resolved.clone());
        if operation == TaskOperation::List {
            for path in paths {
                self.previews.enqueue(JobRequest {
                    operation,
                    paths: vec![path],
                    settings: settings.clone(),
                    resolved: resolved.clone(),
                });
            }
            self.preview_revision += 1;
        } else if operation == TaskOperation::Detect {
            for path in paths {
                self.queue.enqueue(JobRequest {
                    operation,
                    paths: vec![path],
                    settings: settings.clone(),
                    resolved: resolved.clone(),
                });
            }
        } else {
            let request = JobRequest {
                operation,
                paths,
                settings,
                resolved,
            };
            if hold {
                self.queue.enqueue_held(request);
            } else {
                self.queue.enqueue(request);
            }
        }
        self.message.clear();
    }
    fn edit_settings(&mut self, target: Option<u64>, edit: impl FnOnce(&mut TaskSettings)) {
        if let Some(id) = target {
            if let Err(error) = self.queue.update_settings(id, edit) {
                self.message = error.into();
            }
        } else {
            edit(&mut self.settings);
            self.queue.apply_global_settings(&self.settings);
        }
    }
    fn background(&mut self, f: impl FnOnce() -> LibraryMessage + Send + 'static) {
        if self.library_busy {
            self.message = "请等待当前数据操作完成".into();
            return;
        }
        self.library_busy = true;
        let tx = self.sender.clone();
        std::thread::spawn(move || {
            let _ = tx.send(f());
        });
    }
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Tasks,
    History,
    Passwords,
    Preview,
    Settings,
    Integration,
}
pub struct View {
    shared: Entity<Workspace>,
    quick: bool,
    page: Page,
    sidebar_collapsed: bool,
    diagnostics_open: bool,
    task_config_open: bool,
    task_filter: usize,
    selected_file: Option<(u64, smartzip_core::NodeId)>,
    expanded_roots: std::collections::HashSet<smartzip_core::NodeId>,
    queue_settings_open: bool,
    detail_state: Option<(u64, Phase)>,
    prompt_job: Option<(bool, u64)>,
    preview_revision: u64,
    input: Entity<InputState>,
    revealed_password: Option<(i64, String)>,
    password_reveal_request: Option<i64>,
    password_reveal_generation: u64,
    task_password: Entity<InputState>,
    task_password_target: Option<u64>,
    config_form: Option<Entity<crate::config_form::ConfigForm>>,
    member_preview: Entity<crate::member_preview::MemberPreview>,
    member_archive: Option<u64>,
    archive_dir: Vec<String>,
    integration: Entity<crate::integration_ui::SystemIntegration>,
    form_revision: u64,
    prompt_secret: Entity<InputState>,
    _subscription: Subscription,
}
fn control(id: impl Into<gpui::ElementId>) -> Button {
    Button::new(id).small().ghost()
}

fn state_label(state: &str) -> &str {
    match state {
        "queued" | "ready" => "排队中",
        "running" => "处理中",
        "paused" => "已暂停",
        "waiting_resources" => "等待资源",
        "waiting_user" => "需要处理",
        "completed" | "extracted" => "已完成",
        "partial" => "部分完成",
        "failed" => "失败",
        "cancelled" => "已取消",
        "skipped" => "已跳过",
        other => other,
    }
}
fn stage_label(stage: &str) -> String {
    if let Some((name, operation)) = stage.rsplit_once(" · ") {
        return format!("{name} · {}", stage_label(operation));
    }
    match stage {
        "resolve_inputs" => "识别输入",
        "fingerprint" => "检查历史记录",
        "scan_embedded" => "扫描内嵌归档",
        "read_metadata" => "读取归档目录",
        "analyze_encoding" => "分析文件名编码",
        "prepare_access" => "准备归档",
        "extract_attempt" => "解压中",
        "inspect_and_plan" => "检查输出与布局",
        "commit" => "提交输出",
        "discover_children" => "查找嵌套归档",
        "read_member" => "读取成员",
        "decode_preview" => "生成预览",
        "cleanup" => "清理中",
        "password" => "等待密码",
        "encoding" => "等待编码选择",
        "output_collision" => "等待输出冲突处理",
        "incomplete_volume" => "分卷不完整",
        "grouping_ambiguous" => "无法确定分卷组",
        "already_extracted" => "已解压，跳过重复处理",
        "wrong_password" => "密码不正确",
        "task_stopped" => "任务已停止",
        other => state_label(other),
    }
    .into()
}
fn file_backend_label(backend: &str) -> &str {
    if backend.starts_with("sevenzip:") {
        "7-Zip"
    } else if backend.starts_with("unrar") {
        "UnRAR"
    } else {
        backend
    }
}

fn job_category(phase: Phase) -> usize {
    match phase {
        Phase::Completed => 3,
        Phase::Partial | Phase::Failed | Phase::Cancelled => 4,
        Phase::Waiting => 2,
        _ => 1,
    }
}
fn file_category(file: &smartzip_engine::root_management::FileTaskSnapshot) -> usize {
    match file.root_outcome.as_deref() {
        Some("completed") => 3,
        Some(_) => 4,
        None if file.pause_requested
            || file.state == "waiting_user"
            || file
                .root_activity
                .as_ref()
                .is_some_and(|(state, _)| state == "waiting_user") =>
        {
            2
        }
        None => 1,
    }
}
fn file_status(file: &smartzip_engine::root_management::FileTaskSnapshot) -> String {
    if let Some(outcome) = &file.root_outcome {
        return state_label(outcome).into();
    }
    if file.cancel_requested {
        return "停止并清理中".into();
    }
    if file.pause_requested
        && file.state != "paused"
        && !file
            .root_activity
            .as_ref()
            .is_some_and(|(state, _)| state == "paused")
    {
        return "暂停中 · 等待阶段结束".into();
    }
    if let Some((state, _)) = &file.root_activity {
        return state_label(state).into();
    }
    if file.parent_id.is_none()
        && matches!(file.state.as_str(), "completed" | "extracted" | "skipped")
    {
        return "正在处理子项".into();
    }
    state_label(&file.state).into()
}

fn phase_color(phase: Phase, cx: &App) -> gpui::Hsla {
    match phase {
        Phase::Completed => gpui::rgb(if cx.theme().is_dark() {
            0x72d6a0
        } else {
            0x23835a
        })
        .into(),
        Phase::Failed => gpui::rgb(if cx.theme().is_dark() {
            0xff9595
        } else {
            0xc23e48
        })
        .into(),
        Phase::Partial | Phase::Waiting => gpui::rgb(if cx.theme().is_dark() {
            0xf0c271
        } else {
            0x9c6b16
        })
        .into(),
        Phase::Running => cx.theme().primary,
        _ => cx.theme().muted_foreground,
    }
}

pub fn start(cx: &mut App, open_requests: mpsc::Receiver<crate::new_open_request::OpenRequest>) {
    let theme = Theme::global_mut(cx);
    theme.font_size = px(13.);
    theme.radius = px(8.);
    let dark = theme.is_dark();
    theme.colors.primary = gpui::rgb(if dark { 0x779cff } else { 0x355cda }).into();
    theme.colors.primary_hover = gpui::rgb(if dark { 0x93afff } else { 0x294bc0 }).into();
    theme.colors.primary_active = theme.colors.primary_hover;
    theme.colors.primary_foreground = gpui::rgb(if dark { 0x101827 } else { 0xffffff }).into();
    theme.colors.sidebar_accent = gpui::rgb(if dark { 0x25334d } else { 0xe9efff }).into();
    theme.colors.sidebar_accent_foreground = theme.colors.primary;
    theme.colors.sidebar = gpui::rgb(if dark { 0x171c26 } else { 0xf7f8fb }).into();
    theme.colors.border = gpui::rgb(if dark { 0x303847 } else { 0xe7eaf0 }).into();
    theme.shadow = false;
    let shared = cx.new(|cx| Workspace::new(open_requests, cx));
    open_view(shared, false, cx);
}
fn open_view(shared: Entity<Workspace>, quick: bool, cx: &mut App) {
    let bounds = Bounds::centered(
        None,
        size(
            px(if quick { 420. } else { 1180. }),
            px(if quick { 650. } else { 780. }),
        ),
        cx,
    );
    cx.spawn(async move |cx| {
        let existing = shared.update(cx, |state, _| if quick { state.quick } else { state.full });
        if let Some(handle) = existing {
            if handle
                .update(cx, |_, window, _| window.activate_window())
                .is_ok()
            {
                return;
            }
        }
        let shared_for_window = shared.clone();
        let result = cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            move |window, cx| {
                let close_state = shared_for_window.clone();
                window.on_window_should_close(cx, move |_, cx| {
                    close_state.update(cx, |state, cx| {
                        let other = if quick {
                            state.full.is_some()
                        } else {
                            state.quick.is_some()
                        };
                        if !other && (state.queue.has_work() || state.previews.has_work()) {
                            state.message =
                                "仍有任务。请保留窗口，或点击「取消全部并退出」等待清理。".into();
                            cx.notify();
                            false
                        } else {
                            if quick {
                                state.quick = None;
                            } else {
                                state.full = None;
                            }
                            if state.quick.is_none() && state.full.is_none() {
                                cx.quit();
                            }
                            true
                        }
                    })
                });
                let view = cx.new(|cx| View::new(shared_for_window, quick, window, cx));
                cx.new(|cx| Root::new(view, window, cx))
            },
        );
        match result {
            Ok(handle) => {
                shared.update(cx, |state, cx| {
                    if quick {
                        state.quick = Some(handle);
                    } else {
                        state.full = Some(handle);
                    }
                    cx.notify();
                });
            }
            Err(error) => {
                shared.update(cx, |state, cx| {
                    state.message = format!("无法打开窗口：{error}");
                    cx.notify();
                });
            }
        }
    })
    .detach();
}
impl View {
    fn new(
        shared: Entity<Workspace>,
        quick: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let subscription = cx.observe(&shared, |_, _, cx| cx.notify());
        Self {
            shared,
            quick,
            page: Page::Tasks,
            sidebar_collapsed: false,
            diagnostics_open: false,
            task_config_open: false,
            task_filter: 0,
            selected_file: None,
            expanded_roots: std::collections::HashSet::new(),
            queue_settings_open: true,
            detail_state: None,
            prompt_job: None,
            preview_revision: 0,
            input: cx.new(|cx| {
                InputState::new(window, cx)
                    .masked(true)
                    .placeholder("输入要添加到密码库的密码")
            }),
            revealed_password: None,
            password_reveal_request: None,
            password_reveal_generation: 0,
            task_password: cx.new(|cx| {
                InputState::new(window, cx)
                    .masked(true)
                    .placeholder("输入仅此任务使用的密码")
            }),
            task_password_target: None,
            config_form: None,
            member_preview: cx.new(|cx| crate::member_preview::MemberPreview::new(window, cx)),
            member_archive: None,
            archive_dir: Vec::new(),
            integration: cx.new(crate::integration_ui::SystemIntegration::new),
            form_revision: 0,
            prompt_secret: cx.new(|cx| {
                InputState::new(window, cx)
                    .masked(true)
                    .placeholder("请输入归档密码")
            }),
            _subscription: subscription,
        }
    }
    fn pick(&mut self, operation: TaskOperation, cx: &mut Context<Self>) {
        self.pick_with_hold(operation, false, cx);
    }

    fn pick_with_hold(&mut self, operation: TaskOperation, hold: bool, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: None,
        });
        let shared = self.shared.clone();
        cx.spawn(async move |_, cx| {
            if let Ok(Ok(Some(paths))) = receiver.await {
                shared.update(cx, |state, cx| {
                    state.enqueue_with_hold(paths, operation, hold);
                    cx.notify();
                });
            }
        })
        .detach();
    }
    fn output(&mut self, target: Option<u64>, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: None,
        });
        let shared = self.shared.clone();
        cx.spawn(async move |_, cx| {
            if let Ok(Ok(Some(paths))) = receiver.await {
                shared.update(cx, |state, cx| {
                    state.edit_settings(target, |settings| {
                        settings.output = paths.into_iter().next()
                    });
                    cx.notify();
                });
            }
        })
        .detach();
    }
    fn page(&mut self, page: Page, cx: &mut Context<Self>) {
        self.page = page;
        self.revealed_password = None;
        self.password_reveal_request = None;
        self.password_reveal_generation += 1;
        if page == Page::Integration {
            self.integration
                .update(cx, |integration, cx| integration.refresh(cx));
        }
        self.shared.update(cx, |state, cx| {
            state.message.clear();
            match page {
                Page::History => state.background(|| {
                    LibraryMessage::History(library::load_history(&LibraryOptions::default(), 500))
                }),
                Page::Passwords => state.background(|| {
                    LibraryMessage::Passwords(library::list_passwords(
                        &LibraryOptions::default(),
                        1000,
                    ))
                }),
                Page::Settings => state.background(|| {
                    LibraryMessage::Config(
                        library::load_config(&LibraryOptions::default()).map(Box::new),
                    )
                }),
                _ => {}
            }
            cx.notify();
        });
        cx.notify();
    }
    fn quick_controls(&mut self, target: Option<u64>, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.shared.read(cx);
        let job = target.and_then(|id| state.queue.jobs.iter().find(|job| job.id == id));
        if target.is_some() && job.is_none() {
            return div().into_any_element();
        }
        if job.is_some_and(|job| job.request.operation != TaskOperation::Extract) {
            return div()
                .text_xs()
                .child("此任务不执行解压，无解压配置项")
                .into_any_element();
        }
        let settings = job.map(|j| &j.request.settings).unwrap_or(&state.settings);
        let config = if let Some(job) = job {
            job.request.resolved.as_ref().map(|r| &r.values)
        } else {
            state.config.as_ref().map(|c| &c.resolved.values)
        };
        let editable = job.is_none_or(|job| job.phase == Phase::Queued);
        let overrides: [bool; 5] =
            std::array::from_fn(|index| job.is_some_and(|job| job.settings_overridden(index)));
        let recursive = settings
            .recursive
            .unwrap_or_else(|| config.is_some_and(|c| c.extraction.recursion.enabled));
        let smart = settings.smart_layout.unwrap_or_else(|| {
            config.is_some_and(|c| c.extraction.output.layout == smartzip_config::Layout::Smart)
        });
        let encoding = settings
            .auto_encoding
            .unwrap_or_else(|| config.is_some_and(|c| c.extraction.encoding.mode == "auto"));
        let delete = settings.delete_source;
        let output = settings
            .output
            .as_ref()
            .or_else(|| config.and_then(|c| c.extraction.output.directory.as_ref()))
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "首个输入所在目录".into());
        let compact = target.is_none() && !self.quick;
        div()
            .id(("quick-config", target.unwrap_or(0)))
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(Icon::new(IconName::SlidersHorizontal).size(px(16.)))
                    .child(div().font_weight(gpui::FontWeight::MEDIUM).child(
                        if target.is_some() {
                            "此任务配置"
                        } else {
                            "默认解压选项"
                        },
                    )),
            )
            .child(
                div()
                    .flex()
                    .gap_2()
                    .when(!compact, |el| el.flex_col())
                    .children(
                        [
                            (0u8, "递归解压", recursive),
                            (1, "智能整理", smart),
                            (2, "解压后删除", delete),
                            (3, "自动编码", encoding),
                        ]
                        .into_iter()
                        .map(|(index, label, checked)| {
                            div()
                                .flex_1()
                                .min_w_0()
                                .py_1()
                                .child(
                                    Switch::new(("quick-setting", index as usize))
                                        .small()
                                        .label(label)
                                        .checked(checked)
                                        .disabled(!editable)
                                        .on_change(cx.listener(
                                            move |this, checked: &bool, _, cx| {
                                                this.shared.update(cx, |state, cx| {
                                                    state.edit_settings(target, |settings| {
                                                        match index {
                                                            0 => {
                                                                settings.recursive = Some(*checked)
                                                            }
                                                            1 => {
                                                                settings.smart_layout =
                                                                    Some(*checked)
                                                            }
                                                            2 => settings.delete_source = *checked,
                                                            _ => {
                                                                settings.auto_encoding =
                                                                    Some(*checked)
                                                            }
                                                        }
                                                    });
                                                    cx.notify();
                                                });
                                            },
                                        )),
                                )
                                .when(overrides[index as usize], |el| {
                                    el.child(
                                        div()
                                            .mt_1()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child("单独设置"),
                                    )
                                })
                        }),
                    ),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(if !editable {
                        "任务已启动，配置已固定"
                    } else if target.is_some() {
                        "仅修改此任务；未单独设置的选项跟随全局快速配置"
                    } else {
                        "应用于新任务及排队任务 · 单独设置优先"
                    }),
            )
            .when(delete, |el| {
                el.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child("全部成功后原包及成功使用的分卷移入回收站"),
                )
            })
            .child(div().text_xs().child(format!(
                "输出：{output}{}",
                if overrides[4] { " · 单独设置" } else { "" }
            )))
            .child(
                div()
                    .flex()
                    .gap_2()
                    .flex_wrap()
                    .child(
                        control("output")
                            .icon(IconName::FolderOpen)
                            .label("输出目录…")
                            .disabled(!editable)
                            .on_click(cx.listener(move |this, _, _, cx| this.output(target, cx))),
                    )
                    .child(
                        control("settings-inherit")
                            .label("恢复继承")
                            .disabled(!editable)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.shared.update(cx, |state, cx| {
                                    if let Some(id) = target {
                                        if let Err(error) =
                                            state.queue.reset_settings(id, &state.settings)
                                        {
                                            state.message = error.into();
                                        }
                                    } else {
                                        state.edit_settings(None, |settings| {
                                            settings.output = None;
                                            settings.recursive = None;
                                            settings.smart_layout = None;
                                            settings.auto_encoding = None;
                                            settings.delete_source = false;
                                        });
                                    }
                                    cx.notify();
                                });
                            })),
                    ),
            )
            .into_any_element()
    }
    fn drop_area(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("archive-drop")
            .px_4()
            .py_6()
            .items_center()
            .text_center()
            .border_1()
            .border_color(cx.theme().border)
            .rounded_lg()
            .bg(cx.theme().secondary)
            .flex()
            .flex_col()
            .gap_2()
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| {
                this.shared.update(cx, |state, cx| {
                    state.enqueue(paths.paths().to_vec(), TaskOperation::Extract);
                    cx.notify();
                });
            }))
            .child(
                Icon::new(IconName::PackageOpen)
                    .size(px(28.))
                    .text_color(cx.theme().muted_foreground),
            )
            .child(div().text_base().child("拖进来，就开始解压"))
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("多个文件作为同一批次处理"),
            )
            .child(
                control("choose")
                    .primary()
                    .label("选择文件…")
                    .on_click(cx.listener(|this, _, _, cx| this.pick(TaskOperation::Extract, cx))),
            )
    }
    fn actions(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let paused = self.shared.read(cx).queue.paused;
        div()
            .flex()
            .gap_2()
            .flex_wrap()
            .child(
                control("extract")
                    .primary()
                    .icon(IconName::Plus)
                    .label("添加解压")
                    .on_click(cx.listener(|this, _, _, cx| this.pick(TaskOperation::Extract, cx))),
            )
            .child(
                control("prepare-extract")
                    .icon(IconName::SlidersHorizontal)
                    .label("添加并配置…")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.pick_with_hold(TaskOperation::Extract, true, cx);
                    })),
            )
            .child(
                control("list")
                    .icon(IconName::FolderOpen)
                    .label("预览归档")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.page = Page::Preview;
                        this.pick(TaskOperation::List, cx)
                    })),
            )
            .child(
                control("test")
                    .icon(IconName::ShieldCheck)
                    .label("完整性校验")
                    .on_click(cx.listener(|this, _, _, cx| this.pick(TaskOperation::Test, cx))),
            )
            .child(
                control("detect")
                    .icon(IconName::ScanSearch)
                    .label("检测内嵌归档")
                    .on_click(cx.listener(|this, _, _, cx| this.pick(TaskOperation::Detect, cx))),
            )
            .child(
                control("pause")
                    .label(if paused {
                        "开始全部等待任务"
                    } else {
                        "暂停自动启动"
                    })
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.shared.update(cx, |state, cx| {
                            if state.queue.paused {
                                state.queue.start_all();
                            } else {
                                state.queue.paused = true;
                            }
                            cx.notify();
                        })
                    })),
            )
    }
    fn task_overview(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let queue = &self.shared.read(cx).queue;
        let mut counts = [0usize; 5];
        for job in &queue.jobs {
            if job.files.is_empty() {
                counts[0] += job.request.paths.len();
                counts[job_category(job.phase)] += job.request.paths.len();
                continue;
            }
            for file in job.files.iter().filter(|f| f.parent_id.is_none()) {
                counts[0] += 1;
                counts[file_category(file)] += 1;
            }
        }
        div()
            .flex()
            .flex_col()
            .gap_4()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .text_size(px(24.))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child("任务中心"),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("按归档管理进度，展开查看嵌套文件"),
                            ),
                    )
                    .child(
                        control("queue-options")
                            .icon(if self.queue_settings_open {
                                IconName::ChevronDown
                            } else {
                                IconName::SlidersHorizontal
                            })
                            .label(if self.queue_settings_open {
                                "收起默认配置"
                            } else {
                                "展开默认配置"
                            })
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.queue_settings_open = !this.queue_settings_open;
                                cx.notify();
                            })),
                    ),
            )
            .child(
                div().flex().gap_2().flex_wrap().children(
                    ["全部", "进行中", "待处理", "已完成", "失败 / 取消"]
                        .into_iter()
                        .enumerate()
                        .map(|(index, label)| {
                            control(("task-filter", index))
                                .label(format!("{label}  {}", counts[index]))
                                .when(self.task_filter == index, |button| button.primary())
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.task_filter = index;
                                    cx.notify();
                                }))
                        }),
                ),
            )
    }

    fn tasks(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.shared.read(cx);
        let jobs: Vec<_> = state
            .queue
            .jobs
            .iter()
            .map(|j| {
                (
                    j.id,
                    j.name(),
                    j.phase,
                    j.request.paths.len(),
                    j.files.clone(),
                    state.queue.is_held(j.id),
                )
            })
            .collect();
        let mut groups = Vec::new();
        for (id, name, phase, inputs, files, held) in jobs {
            let roots: Vec<_> = files
                .iter()
                .filter(|f| {
                    f.parent_id.is_none()
                        && (self.task_filter == 0 || file_category(f) == self.task_filter)
                })
                .cloned()
                .collect();
            if !files.is_empty() && roots.is_empty() {
                continue;
            }
            if files.is_empty() && self.task_filter != 0 && self.task_filter != job_category(phase)
            {
                continue;
            }
            let mut group = div()
                .flex()
                .flex_col()
                .border_1()
                .border_color(cx.theme().border)
                .rounded_lg()
                .overflow_hidden()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .px_3()
                        .py_2()
                        .bg(cx.theme().sidebar)
                        .child(
                            control(("batch", id))
                                .label(format!("批次 {id} · {name}"))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.selected_file = None;
                                    this.shared.update(cx, |state, cx| {
                                        state.queue.selected = Some(id);
                                        cx.notify();
                                    });
                                })),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(format!(
                                            "{} 个归档 · {}",
                                            files.iter().filter(|f| f.parent_id.is_none()).count(),
                                            if held { "等待配置" } else { phase.label() }
                                        )),
                                )
                                .when(phase == Phase::Queued, |actions| {
                                    actions.child(
                                        control(("start-batch", id))
                                            .primary()
                                            .label("开始任务")
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.shared.update(cx, |state, cx| {
                                                    state.queue.start(id);
                                                    cx.notify();
                                                });
                                            })),
                                    )
                                }),
                        ),
                );
            if files.is_empty() {
                group = group.child(
                    div()
                        .px_4()
                        .py_4()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(format!(
                            "{} · {inputs} 个输入，启动后识别归档与分卷组",
                            if held { "等待配置" } else { phase.label() }
                        )),
                );
            }
            for root in roots {
                let root_id = root.node_id.clone();
                let descendants: Vec<_> = files
                    .iter()
                    .filter(|f| f.root_id == root_id && f.node_id != root_id)
                    .cloned()
                    .collect();
                let expanded = self.expanded_roots.contains(&root_id);
                group =
                    group.child(self.file_task_row(id, root, descendants.len(), phase, false, cx));
                if expanded {
                    for child in descendants {
                        group = group.child(self.file_task_row(id, child, 0, phase, true, cx));
                    }
                }
            }
            groups.push(group);
        }
        div().flex().flex_col().gap_3().children(groups)
    }

    fn file_task_row(
        &mut self,
        id: u64,
        file: smartzip_engine::root_management::FileTaskSnapshot,
        children: usize,
        batch_phase: Phase,
        nested: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let node = file.node_id.clone();
        let selected = self
            .selected_file
            .as_ref()
            .is_some_and(|(job, n)| *job == id && *n == node);
        let done = file.root_outcome.is_some() || !batch_phase.active();
        let cancelled = file.cancel_requested;
        let paused = file.pause_requested;
        let status = file_status(&file);
        let name = file
            .path
            .file_name()
            .unwrap_or(file.path.as_os_str())
            .to_string_lossy()
            .into_owned();
        let name = match &file.volumes {
            Some(v) if v.candidates.len() > 1 => {
                format!("{name} · {} 个候选分卷组", v.candidates.len())
            }
            Some(v) if v.members.len() > 1 => format!("{name} · {} 卷", v.members.len()),
            _ if nested && file.depth == 0 => format!("候选入口 · {name}"),
            _ => name,
        };
        let selected_node = node.clone();
        let toggle_node = node.clone();
        let pause_node = node.clone();
        let cancel_node = node.clone();
        let retry_node = node.clone();
        let retry = matches!(
            file.root_outcome.as_deref(),
            Some("failed" | "partial" | "cancelled")
        );
        let expanded = self.expanded_roots.contains(&node);
        div()
            .id(SharedString::from(format!("file-{}", node)))
            .flex()
            .items_center()
            .gap_3()
            .px_3()
            .py_3()
            .border_t_1()
            .border_color(cx.theme().border)
            .when(nested, |row| {
                row.pl(px(40. + 12. * f32::from(file.depth.min(4))))
            })
            .when(selected, |row| row.bg(cx.theme().sidebar_accent))
            .hover(|row| row.bg(cx.theme().sidebar))
            .child(
                control(SharedString::from(format!("expand-{node}")))
                    .label(if children == 0 {
                        "·".into()
                    } else if expanded {
                        format!("▾ {children}")
                    } else {
                        format!("▸ {children}")
                    })
                    .disabled(children == 0)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !this.expanded_roots.remove(&toggle_node) {
                            this.expanded_roots.insert(toggle_node.clone());
                        }
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        control(SharedString::from(format!("select-{node}")))
                            .justify_start()
                            .label(name)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.selected_file = Some((id, selected_node.clone()));
                                this.shared.update(cx, |state, cx| {
                                    state.queue.selected = Some(id);
                                    cx.notify();
                                });
                            })),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_ellipsis()
                            .text_color(cx.theme().muted_foreground)
                            .child(stage_label(
                                &file
                                    .root_activity
                                    .as_ref()
                                    .map(|(_, stage)| stage.clone())
                                    .unwrap_or_else(|| file.stage.clone()),
                            )),
                    ),
            )
            .child(div().w(px(125.)).text_xs().child(status))
            .when(file.state == "running" && file.progress.is_some(), |row| {
                row.child(
                    div()
                        .w(px(45.))
                        .text_xs()
                        .child(format!("{:.0}%", file.progress.unwrap_or_default())),
                )
            })
            .when(!nested, |row| {
                row.child(
                    div()
                        .flex()
                        .gap_1()
                        .when(!done, |actions| {
                            actions
                                .child(
                                    control(SharedString::from(format!("pause-{node}")))
                                        .label(if paused { "继续" } else { "暂停" })
                                        .disabled(cancelled)
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.shared.update(cx, |state, cx| {
                                                state.queue.pause_root(id, &pause_node, !paused);
                                                cx.notify();
                                            });
                                        })),
                                )
                                .child(
                                    control(SharedString::from(format!("cancel-{node}")))
                                        .label("取消")
                                        .disabled(cancelled)
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.shared.update(cx, |state, cx| {
                                                state.queue.cancel_root(id, &cancel_node);
                                                cx.notify();
                                            });
                                        })),
                                )
                        })
                        .when(retry, |actions| {
                            actions.child(
                                control(SharedString::from(format!("retry-{node}")))
                                    .label(if batch_phase.active() {
                                        "批次结束后重试"
                                    } else {
                                        "重试"
                                    })
                                    .disabled(batch_phase.active())
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.shared.update(cx, |state, cx| {
                                            state.queue.retry_root(id, &retry_node);
                                            cx.notify();
                                        });
                                    })),
                            )
                        }),
                )
            })
    }

    fn file_detail(&mut self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let (job_id, node) = self.selected_file.as_ref()?;
        let job = self
            .shared
            .read(cx)
            .queue
            .jobs
            .iter()
            .find(|j| j.id == *job_id)?;
        let file = job.files.iter().find(|f| f.node_id == *node)?.clone();
        let output = file.output.clone();
        Some(div().flex().flex_col().gap_4()
            .child(div().text_xs().text_color(cx.theme().muted_foreground).child(if file.parent_id.is_none() { "根归档详情" } else if file.depth == 0 { "候选入口详情" } else { "嵌套归档详情" }))
            .child(div().text_size(px(18.)).font_weight(gpui::FontWeight::SEMIBOLD).child(file.path.file_name().unwrap_or(file.path.as_os_str()).to_string_lossy().into_owned()))
            .child(div().text_sm().child(file_status(&file)))
            .child(div().text_xs().child(file.path.display().to_string()))
            .child(div().text_sm().child(format!("当前阶段：{}", stage_label(&file.stage))))
            .when(!file.backend.is_empty(), |el| el.child(div().text_sm().child(format!("后端：{}", file_backend_label(&file.backend)))))
            .when(file.committed, |el| el.child(div().text_sm().child("本文件输出已成功提交")))
            .when_some(file.root_outcome.clone(), |el, outcome| el.child(div().text_sm().child(format!("包含嵌套归档的结果：{}", state_label(&outcome)))))
            .when_some(file.volumes.clone(), |el, volumes| {
                el.child(div().flex().flex_col().gap_2().border_t_1().border_color(cx.theme().border).pt_3()
                    .child(div().text_sm().child(if volumes.candidates.len() > 1 { "分卷候选" } else { "输入成员" }))
                    .when(!volumes.diagnostic.is_empty(), |el| el.child(div().text_xs().text_color(cx.theme().muted_foreground).child(volumes.diagnostic.clone())))
                    .children(volumes.candidates.iter().enumerate().map(|(index, candidate)| {
                        let adopted = volumes.selected.iter().any(|selected| selected.len() == candidate.len() && selected.iter().all(|path| candidate.contains(path)));
                        div().flex().flex_col().gap_1().py_2()
                            .child(div().text_xs().child(format!("候选 {} · {} 卷{}", index + 1, candidate.len(), if adopted { " · 已采用" } else { "" })))
                            .children(candidate.iter().map(|path| div().text_xs().text_color(cx.theme().muted_foreground).child(path.display().to_string())))
                    }))
                    .when(volumes.candidates.is_empty(), |el| el.children(volumes.members.iter().map(|path| div().text_xs().child(path.display().to_string())))))
            })
            .child(control("file-output").icon(IconName::FolderOpen).label("打开输出目录").disabled(output.is_none()).on_click(move |_, _, cx| { if let Some(path) = &output { cx.reveal_path(path); } }))
            .child(div().border_t_1().border_color(cx.theme().border).pt_4().text_xs().text_color(cx.theme().muted_foreground).child("暂停在当前阶段安全结束后生效。取消会停止该根归档及其嵌套文件，并等待清理。"))
            .child(div().flex().flex_col().gap_2().child(div().text_sm().child("文件记录"))
                .children(file.events.iter().rev().take(30).map(|event| div().text_xs().text_color(cx.theme().muted_foreground).child(event.clone()))))
            .into_any_element())
    }

    fn detail(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.quick {
            if let Some(detail) = self.file_detail(cx) {
                return detail;
            }
        }
        let queue = &self.shared.read(cx).queue;
        let selected = if self.quick {
            queue
                .jobs
                .iter()
                .find(|job| job.phase.active())
                .or_else(|| queue.selected())
        } else {
            queue.selected()
        };
        let Some(job) = selected else {
            return div()
                .p_4()
                .text_color(cx.theme().muted_foreground)
                .child("选择或拖入归档开始")
                .into_any_element();
        };
        let id = job.id;
        let name = job.name();
        let stage = job.stage.clone();
        let backend = job.backend_label();
        let inputs = job
            .request
            .paths
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join("\n");
        let events = job
            .events
            .iter()
            .rev()
            .take(60)
            .cloned()
            .collect::<Vec<_>>();
        let output = job.outputs.last().cloned();
        let phase = job.phase;
        let has_task_password = !job.request.settings.passwords.is_empty();
        let percent = job.progress;
        let result = job
            .result
            .as_ref()
            .map(|r| serde_json::to_string_pretty(r).unwrap_or_default());
        if self.detail_state != Some((id, phase)) {
            if self
                .detail_state
                .is_none_or(|(previous_id, _)| previous_id != id)
                || matches!(
                    phase,
                    Phase::Completed | Phase::Partial | Phase::Failed | Phase::Cancelled
                )
            {
                self.task_config_open = phase == Phase::Queued;
            }
            self.detail_state = Some((id, phase));
        }
        div()
            .flex()
            .flex_col()
            .gap_4()
            .min_w_0()
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("任务详情"),
            )
            .child(
                div()
                    .text_size(px(18.))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(name),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(phase_color(phase, cx))
                    .child(stage),
            )
            .when(phase.active(), |el| {
                el.child(
                    Progress::new("current-progress")
                        .small()
                        .value(percent.unwrap_or_default())
                        .loading(percent.is_none() && phase == Phase::Running)
                        .accessibility_label("当前归档操作进度"),
                )
            })
            .child(
                div()
                    .flex()
                    .gap_2()
                    .items_center()
                    .text_xs()
                    .child(Icon::new(IconName::Cpu).small())
                    .child(backend),
            )
            .when(phase == Phase::Queued, |el| {
                el.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(div().text_sm().child("此任务临时密码"))
                        .child(Input::new(&self.task_password).mask_toggle())
                        .child(
                            div()
                                .flex()
                                .gap_2()
                                .child(
                                    control("set-task-password")
                                        .label(if has_task_password {
                                            "替换密码"
                                        } else {
                                            "用于此任务"
                                        })
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            let value =
                                                this.task_password.read(cx).value().to_string();
                                            if value.is_empty() {
                                                return;
                                            }
                                            let result = this.shared.update(cx, |state, cx| {
                                                let result =
                                                    state.queue.set_task_password(id, Some(value));
                                                cx.notify();
                                                result
                                            });
                                            match result {
                                                Ok(()) => {
                                                    this.task_password.update(cx, |input, cx| {
                                                        input.set_value("", window, cx)
                                                    })
                                                }
                                                Err(error) => {
                                                    this.shared.update(cx, |state, cx| {
                                                        state.message = error.into();
                                                        cx.notify();
                                                    })
                                                }
                                            }
                                        })),
                                )
                                .child(
                                    control("clear-task-password")
                                        .label("清除")
                                        .disabled(!has_task_password)
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.shared.update(cx, |state, cx| {
                                                let _ = state.queue.set_task_password(id, None);
                                                cx.notify();
                                            });
                                        })),
                                ),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(if has_task_password {
                                    "已设置 · 仅用于此任务，不保存到密码库"
                                } else {
                                    "仅用于此任务，不保存到密码库"
                                }),
                        ),
                )
            })
            .when(!self.quick, |el| {
                el.child(
                    div()
                        .border_t_1()
                        .border_b_1()
                        .border_color(cx.theme().border)
                        .py_3()
                        .child(
                            control("toggle-task-config")
                                .icon(if self.task_config_open {
                                    IconName::ChevronDown
                                } else {
                                    IconName::ChevronRight
                                })
                                .label(if phase == Phase::Queued {
                                    "任务配置 · 可单独调整"
                                } else {
                                    "任务配置 · 执行快照"
                                })
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.task_config_open = !this.task_config_open;
                                    cx.notify();
                                })),
                        )
                        .when(self.task_config_open, |el| {
                            el.child(self.quick_controls(Some(id), cx))
                        }),
                )
            })
            .child(
                div()
                    .flex()
                    .gap_2()
                    .flex_wrap()
                    .when(phase == Phase::Queued, |el| {
                        el.child(control("start").primary().label("开始任务").on_click(
                            cx.listener(move |this, _, _, cx| {
                                this.shared.update(cx, |state, cx| {
                                    state.queue.start(id);
                                    cx.notify();
                                })
                            }),
                        ))
                    })
                    .when(phase.active() || phase == Phase::Queued, |el| {
                        el.child(
                            control("cancel")
                                .label("取消任务")
                                .disabled(!phase.active() && phase != Phase::Queued)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.shared.update(cx, |state, cx| {
                                        state.queue.cancel(id);
                                        cx.notify();
                                    })
                                })),
                        )
                    })
                    .when(phase == Phase::Queued, |el| {
                        el.child(
                            control("up")
                                .label("提前执行")
                                .disabled(phase != Phase::Queued)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.shared.update(cx, |state, cx| {
                                        state.queue.move_up(id);
                                        cx.notify();
                                    })
                                })),
                        )
                    })
                    .when(!phase.active() && phase != Phase::Queued, |el| {
                        el.child(
                            control("retry")
                                .label("重新运行")
                                .disabled(phase.active() || phase == Phase::Queued)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.shared.update(cx, |state, cx| {
                                        if let Some(job) =
                                            state.queue.jobs.iter().find(|j| j.id == id)
                                        {
                                            let request = job.request.clone();
                                            state.queue.enqueue(request);
                                        }
                                        cx.notify();
                                    })
                                })),
                        )
                    })
                    .child(
                        control("reveal")
                            .primary()
                            .icon(IconName::FolderOpen)
                            .label("打开输出目录")
                            .disabled(output.is_none())
                            .on_click(move |_, _, cx| {
                                if let Some(path) = &output {
                                    cx.reveal_path(path);
                                }
                            }),
                    )
                    .when(self.diagnostics_open, |el| {
                        el.child(
                            control("copy-result")
                                .label("复制结果 JSON")
                                .disabled(result.is_none())
                                .on_click(move |_, _, cx| {
                                    if let Some(result) = &result {
                                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                                            result.clone(),
                                        ));
                                    }
                                }),
                        )
                    }),
            )
            .child(
                control("toggle-diagnostics")
                    .icon(if self.diagnostics_open {
                        IconName::ChevronDown
                    } else {
                        IconName::ChevronRight
                    })
                    .label("技术诊断")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.diagnostics_open = !this.diagnostics_open;
                        cx.notify();
                    })),
            )
            .when(self.diagnostics_open, |el| {
                el.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .min_w_0()
                        .child(div().text_xs().child(inputs))
                        .child(
                            div()
                                .id("event-log")
                                .max_h(px(150.))
                                .overflow_y_scroll()
                                .text_xs()
                                .children(
                                    events.into_iter().map(|event| div().py_1().child(event)),
                                ),
                        ),
                )
            })
            .into_any_element()
    }
    fn prompt(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.shared.read(cx);
        let is_preview = !self.quick && self.page == Page::Preview;
        let queue = if is_preview {
            &state.previews
        } else {
            &state.queue
        };
        let waiting = queue.jobs.iter().find(|job| job.prompt.is_some());
        let Some(job) = waiting else {
            return div().into_any_element();
        };
        let id = job.id;
        let (title, kind, details) = match job.prompt.as_ref().unwrap() {
            InteractionRequest::Password { path, .. } => {
                (format!("需要密码：{}", path.display()), 0, String::new())
            }
            InteractionRequest::Output { path, output, .. } => (
                "输出位置已存在".into(),
                1,
                format!("{} → {}", path.display(), output.display()),
            ),
            InteractionRequest::Embedded { path, decision, .. } => (
                "发现内嵌归档".into(),
                2,
                format!("{}\n{decision:?}", path.display()),
            ),
            InteractionRequest::Encoding { path, context, .. } => (
                "请确认文件名编码".into(),
                3,
                format!(
                    "{}\n{}\n{:?}",
                    path.display(),
                    context.preview_names.join("\n"),
                    context.suspicious_reasons
                ),
            ),
        };
        let choices: Vec<(&'static str, u8)> = match kind {
            0 => vec![("尝试密码", 0), ("跳过归档", 1)],
            1 => vec![("自动重命名", 0), ("覆盖", 1), ("跳过", 2)],
            2 => vec![("提取", 0), ("全部提取", 1), ("跳过", 2)],
            _ => vec![
                ("接受检测", 0),
                ("使用 GB18030", 1),
                ("使用 UTF-8", 2),
                ("跳过", 3),
            ],
        };
        div().p_3().border_1().border_color(cx.theme().primary).rounded_md().flex().flex_col().gap_2().child(title).child(div().text_xs().child(details)).when(kind==0,|el|el.child(Input::new(&self.prompt_secret))).child(div().flex().gap_2().flex_wrap().children(choices.into_iter().map(|(label,choice)|control(label).label(label).on_click(cx.listener(move|this,_,window,cx|{let password=this.prompt_secret.read(cx).value().to_string();if kind==0&&choice==0&&password.is_empty(){return;}this.prompt_secret.update(cx,|input,cx|input.set_value("",window,cx));this.shared.update(cx,|state,cx|{if let Some(job)=(if is_preview { &mut state.previews } else { &mut state.queue }).jobs.iter_mut().find(|j|j.id==id){if let Some(prompt)=job.prompt.take(){match prompt{
            InteractionRequest::Password{respond,..}=>{let _=respond.send(if choice==0{Some(password)}else{None});},
            InteractionRequest::Output{respond,..}=>{let _=respond.send(match choice{0=>smartzip_engine::OutputCollisionStrategy::Rename,1=>smartzip_engine::OutputCollisionStrategy::Overwrite,_=>smartzip_engine::OutputCollisionStrategy::Skip});},
            InteractionRequest::Embedded{respond,..}=>{let _=respond.send(match choice{0=>smartzip_engine::EmbeddedSelectionChoice::Extract,1=>smartzip_engine::EmbeddedSelectionChoice::ExtractAll,_=>smartzip_engine::EmbeddedSelectionChoice::Skip});},
            InteractionRequest::Encoding{respond,..}=>{let _=respond.send(match choice{0=>smartzip_engine::EncodingConfirmationChoice::AcceptDetected,1=>smartzip_engine::EncodingConfirmationChoice::Override("gb18030".into()),2=>smartzip_engine::EncodingConfirmationChoice::Override("utf-8".into()),_=>smartzip_engine::EncodingConfirmationChoice::SkipArchive});}
        }job.phase=Phase::Running;job.stage="继续处理".into();}}cx.notify();});}))))).into_any_element()
    }
    fn history(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let rows = self.shared.read(cx).history.clone();
        let document = self.shared.read(cx).document.clone();
        div().flex().flex_col().gap_3().child(div().flex().gap_2().child(control("reload-history").label("刷新历史").on_click(cx.listener(|this,_,_,cx|this.page(Page::History,cx)))).child(control("file-history").label("逐文件历史").on_click(cx.listener(|this,_,_,cx|this.shared.update(cx,|state,cx|{state.background(||LibraryMessage::Document(library::load_file_history(&LibraryOptions::default(),None,None,500).and_then(|r|serde_json::to_string_pretty(&r).map_err(|e|e.to_string()))));cx.notify();})))))
        .child(div().id("history-list").max_h(px(300.)).overflow_y_scroll().children(rows.into_iter().map(|row|{let id=row.id.clone();div().flex().gap_2().py_1().border_b_1().border_color(cx.theme().border).child(control(SharedString::from(row.id.clone())).label(format!("{} · {} · {}",row.started_at,row.kind,row.status)).on_click(cx.listener(move|this,_,_,cx|{let id=id.clone();this.shared.update(cx,|state,cx|{state.background(move||LibraryMessage::Document(library::load_task_detail(&LibraryOptions::default(),&id).and_then(|r|serde_json::to_string_pretty(&serde_json::json!({"task":r.task,"files":r.files,"events":r.events})).map_err(|e|e.to_string()))));cx.notify();});}))).child(div().flex_1().text_xs().text_ellipsis().child(row.output_path.unwrap_or_default()))})))
        .child(div().id("history-detail").max_h(px(320.)).overflow_y_scroll().text_xs().child(document))
    }
    fn password_file(&mut self, export: bool, cx: &mut Context<Self>) {
        if export {
            let receiver =
                cx.prompt_for_new_path(&std::env::temp_dir(), Some("smartzip-passwords.txt"));
            let shared = self.shared.clone();
            cx.spawn(async move |_, cx| {
                if let Ok(Ok(Some(path))) = receiver.await {
                    shared.update(cx, |state, cx| {
                        state.background(move || {
                            LibraryMessage::Mutation(
                                library::export_passwords(&LibraryOptions::default(), &path)
                                    .map(|n| format!("已导出 {n} 条明文密码")),
                            )
                        });
                        cx.notify();
                    });
                }
            })
            .detach();
        } else {
            let receiver = cx.prompt_for_paths(PathPromptOptions {
                files: true,
                directories: false,
                multiple: false,
                prompt: None,
            });
            let shared = self.shared.clone();
            cx.spawn(async move |_, cx| {
                if let Ok(Ok(Some(paths))) = receiver.await {
                    if let Some(path) = paths.into_iter().next() {
                        shared.update(cx, |state, cx| {
                            state.background(move || {
                                LibraryMessage::Mutation(
                                    library::import_passwords(
                                        &LibraryOptions::default(),
                                        &path,
                                        "gui-import",
                                    )
                                    .map(|n| {
                                        format!("已处理 {n} 个非空输入行（包含重复行），请刷新列表")
                                    }),
                                )
                            });
                            cx.notify();
                        });
                    }
                }
            })
            .detach();
        }
    }
    fn toggle_password(&mut self, id: i64, cx: &mut Context<Self>) {
        self.password_reveal_generation += 1;
        self.revealed_password = None;
        if self.password_reveal_request == Some(id) {
            self.password_reveal_request = None;
            cx.notify();
            return;
        }
        self.password_reveal_request = Some(id);
        let generation = self.password_reveal_generation;
        let task = cx
            .background_executor()
            .spawn(async move { library::read_password(&LibraryOptions::default(), id) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                if this.password_reveal_generation != generation || this.page != Page::Passwords {
                    return;
                }
                match result {
                    Ok(value) => this.revealed_password = Some((id, value)),
                    Err(error) => {
                        this.password_reveal_request = None;
                        this.shared.update(cx, |state, cx| {
                            state.message = error;
                            cx.notify();
                        });
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn passwords(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let rows = self.shared.read(cx).passwords.clone();
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child("密码管理")
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("添加到密码库后，可供后续任务自动尝试。临时密码请在任务详情中设置。"),
            )
            .child(Input::new(&self.input).mask_toggle())
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_2()
                    .child(
                        control("add-password")
                            .primary()
                            .label("添加密码")
                            .on_click(cx.listener(|this, _, window, cx| {
                                let value = this.input.read(cx).value().to_string();
                                if value.is_empty() {
                                    return;
                                }
                                this.input
                                    .update(cx, |input, cx| input.set_value("", window, cx));
                                this.shared.update(cx, |state, cx| {
                                    state.background(move || {
                                        LibraryMessage::Mutation(
                                            library::add_password(
                                                &LibraryOptions::default(),
                                                &value,
                                                "gui",
                                                false,
                                            )
                                            .map(|_| "密码已添加，请刷新列表".into()),
                                        )
                                    });
                                    cx.notify();
                                });
                            })),
                    )
                    .child(
                        control("reload-pw")
                            .label("刷新")
                            .on_click(cx.listener(|this, _, _, cx| this.page(Page::Passwords, cx))),
                    )
                    .child(
                        control("import-pw")
                            .label("导入…")
                            .on_click(cx.listener(|this, _, _, cx| this.password_file(false, cx))),
                    )
                    .child(
                        control("export-pw")
                            .label("导出明文…")
                            .on_click(cx.listener(|this, _, _, cx| this.password_file(true, cx))),
                    )
                    .child(
                        control("cleanup-preview")
                            .label("清理预览（保留前128）")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.shared.update(cx, |state, cx| {
                                    state.background(|| {
                                        LibraryMessage::Document(
                                            library::cleanup_passwords(
                                                &LibraryOptions::default(),
                                                128,
                                                None,
                                                false,
                                            )
                                            .map(|ids| format!("将禁用未置顶候选：{ids:?}")),
                                        )
                                    });
                                    cx.notify();
                                })
                            })),
                    )
                    .child(
                        control("cleanup-apply")
                            .label("禁用排名128以后的未置顶密码")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.shared.update(cx, |state, cx| {
                                    state.background(|| {
                                        LibraryMessage::Mutation(
                                            library::cleanup_passwords(
                                                &LibraryOptions::default(),
                                                128,
                                                None,
                                                true,
                                            )
                                            .map(
                                                |ids| {
                                                    format!("已禁用 {} 个候选，请刷新", ids.len())
                                                },
                                            ),
                                        )
                                    });
                                    cx.notify();
                                })
                            })),
                    ),
            )
            .child(
                div()
                    .id("password-list")
                    .max_h(px(340.))
                    .overflow_y_scroll()
                    .children(rows.into_iter().map(|row| {
                        let id = row.id;
                        let revealed = self
                            .revealed_password
                            .as_ref()
                            .filter(|(shown, _)| *shown == id);
                        let value = revealed
                            .map(|(_, value)| value.clone())
                            .unwrap_or_else(|| row.masked.into());
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .py_1()
                            .border_b_1()
                            .border_color(cx.theme().border)
                            .child(div().text_xs().child(format!("#{}", row.id)))
                            .child(
                                div()
                                    .id(("password-value", id as u64))
                                    .w(px(180.))
                                    .overflow_x_scroll()
                                    .child(value),
                            )
                            .child(
                                control(("show-pw", id as u64))
                                    .label(if self.password_reveal_request == Some(id) {
                                        "隐藏"
                                    } else {
                                        "显示"
                                    })
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.toggle_password(id, cx)
                                    })),
                            )
                            .child(div().flex_1().text_xs().child(format!(
                                "{} · 成功 {} / 失败 {}{}\n最近成功：{} · 最近失败：{}",
                                row.source,
                                row.success_count,
                                row.failure_count,
                                if row.pinned { " · 置顶" } else { "" },
                                row.last_success_at.as_deref().unwrap_or("—"),
                                row.last_failure_at.as_deref().unwrap_or("—")
                            )))
                            .child(control(("remove-pw", id as u64)).label("删除").on_click(
                                cx.listener(move |this, _, _, cx| {
                                    if this.password_reveal_request == Some(id) {
                                        this.toggle_password(id, cx);
                                    }
                                    this.shared.update(cx, |state, cx| {
                                        state.background(move || {
                                            LibraryMessage::Mutation(
                                                library::remove_password(
                                                    &LibraryOptions::default(),
                                                    id,
                                                )
                                                .map(|_| format!("已删除密码 #{id}，请刷新")),
                                            )
                                        });
                                        cx.notify();
                                    })
                                }),
                            ))
                    })),
            )
            .child(div().text_xs().child(self.shared.read(cx).document.clone()))
    }
    fn settings(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.shared.read(cx);
        let revision = state.config_revision;
        let snapshot = state.config.clone();
        let busy = state.library_busy;
        if self.form_revision != revision || self.config_form.is_none() {
            self.config_form = snapshot.as_ref().map(|snapshot| {
                cx.new(|cx| crate::config_form::ConfigForm::new(snapshot, window, cx))
            });
            self.form_revision = revision;
        }
        let path = snapshot
            .as_ref()
            .map(|s| s.edit_path.display().to_string())
            .unwrap_or_else(|| "配置加载失败，请解决错误后重新加载".into());
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(div().text_sm().child("全局配置 · 与 CLI 共用"))
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(path),
            )
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        control("save-config")
                            .primary()
                            .label("保存全部修改")
                            .disabled(busy || self.config_form.is_none())
                            .on_click(cx.listener(|this, _, _, cx| {
                                let Some(form) = &this.config_form else {
                                    return;
                                };
                                let patches = form.read(cx).patches(cx);
                                this.shared.update(cx, |state, cx| {
                                    if patches.is_empty() {
                                        state.message = "没有待保存的修改".into();
                                    } else {
                                        state.background(move || {
                                            LibraryMessage::ConfigSaved(
                                                library::set_config_fields(
                                                    &LibraryOptions::default(),
                                                    &patches,
                                                )
                                                .map(Box::new),
                                            )
                                        });
                                    }
                                    cx.notify();
                                });
                            })),
                    )
                    .child(
                        control("reload-config")
                            .label("放弃修改并重新加载")
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, _, cx| this.page(Page::Settings, cx))),
                    )
                    .child(
                        control("init-config")
                            .label("初始化配置")
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.shared.update(cx, |state, cx| {
                                    state.background(|| {
                                        LibraryMessage::ConfigSaved(
                                            library::init_config(&LibraryOptions::default(), false)
                                                .map(Box::new),
                                        )
                                    });
                                    cx.notify();
                                })
                            })),
                    )
                    .child(
                        control("migrate-preview")
                            .label("迁移预览")
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.shared.update(cx, |state, cx| {
                                    state.background(|| {
                                        LibraryMessage::Document(library::migrate_config(
                                            &LibraryOptions::default(),
                                            false,
                                        ))
                                    });
                                    cx.notify();
                                })
                            })),
                    ),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("保存后用于新任务；已排队任务保留原配置。快速窗口的显式覆盖优先。"),
            )
            .children(self.config_form.clone())
    }
    fn preview(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.shared.read(cx);
        let selected = state.previews.selected();
        let paths = selected
            .map(|job| job.request.paths.clone())
            .unwrap_or_default();
        let id = selected.map(|job| job.id);
        let name = selected
            .map(|job| job.name())
            .unwrap_or_else(|| "打开压缩包，先查看内容".into());
        let stage = selected
            .map(|job| format!("{} · {}", job.phase.label(), job.stage))
            .unwrap_or_else(|| "拖入文件或选择压缩包，不会自动解压".into());
        let backend = selected.map(|job| job.backend.clone()).unwrap_or_default();
        let active = selected.is_some_and(|job| job.phase.active() || job.phase == Phase::Queued);
        let entries = selected
            .and_then(|job| job.result.as_ref())
            .and_then(|value| value.get("entries"))
            .and_then(|value| value.as_array())
            .cloned()
            .unwrap_or_default();
        let browser_result = crate::archive_browser::ArchiveBrowser::new(
            entries
                .iter()
                .filter_map(|entry| {
                    Some(crate::archive_browser::ArchiveEntry {
                        path: entry.get("path")?.as_str()?.to_owned(),
                        is_dir: entry.get("is_dir")?.as_bool().unwrap_or(false),
                        size: entry
                            .get("uncompressed_size")
                            .and_then(|value| value.as_u64()),
                    })
                })
                .collect(),
        );
        let browser_error = browser_result.as_ref().err().cloned();
        let visible_entries = browser_result
            .as_ref()
            .map(|browser| browser.children(&self.archive_dir))
            .unwrap_or_default();
        let source = selected.and_then(|job| {
            let result = job.result.as_ref()?;
            let resolved = job.request.resolved.as_ref()?;
            Some(crate::member_preview::MemberSource {
                archive: job.request.paths.first()?.clone(),
                member: PathBuf::new(),
                backend: job.backend.clone(),
                config: resolved.values.backends.clone(),
                allow_password: resolved.values.passwords.mode
                    != smartzip_config::PasswordMode::Off,
                max_bytes: match resolved.values.limits.max_output_bytes {
                    0 => 8 * 1024 * 1024,
                    limit => limit.min(8 * 1024 * 1024) as usize,
                },
                encoding: result
                    .get("encoding")
                    .and_then(|v| v.as_str())
                    .filter(|v| !["auto", "backend"].contains(v))
                    .map(|v| smartzip_core::EncodingMode::Override(v.into()))
                    .unwrap_or(smartzip_core::EncodingMode::Auto),
                unavailable: result
                    .get("embedded_offset")
                    .filter(|v| !v.is_null())
                    .map(|_| "内嵌归档内容暂需解压后查看".into()),
            })
        });
        let tabs: Vec<_> = state
            .previews
            .jobs
            .iter()
            .map(|job| (job.id, job.name()))
            .collect();
        div()
            .flex()
            .flex_col()
            .gap_3()
            .id("preview-page")
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| {
                this.shared.update(cx, |state, cx| {
                    state.enqueue(paths.paths().to_vec(), TaskOperation::List);
                    cx.notify();
                });
            }))
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        control("open-archive")
                            .primary()
                            .icon(IconName::FolderOpen)
                            .label("打开压缩包…")
                            .on_click(
                                cx.listener(|this, _, _, cx| this.pick(TaskOperation::List, cx)),
                            ),
                    )
                    .child(
                        control("extract-preview")
                            .icon(IconName::PackageOpen)
                            .label("解压此归档")
                            .disabled(paths.is_empty())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.shared.update(cx, |state, cx| {
                                    state.enqueue(paths.clone(), TaskOperation::Extract);
                                    cx.notify();
                                });
                                this.page = Page::Tasks;
                            })),
                    )
                    .child(
                        control("cancel-preview")
                            .label("停止读取")
                            .disabled(!active)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.shared.update(cx, |state, cx| {
                                    if let Some(id) = id {
                                        state.previews.cancel(id);
                                    }
                                    cx.notify();
                                })
                            })),
                    ),
            )
            .child(
                div()
                    .flex()
                    .gap_2()
                    .flex_wrap()
                    .children(tabs.into_iter().map(|(tab_id, name)| {
                        control(("archive-tab", tab_id))
                            .label(name)
                            .when(Some(tab_id) == id, |button| button.primary())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.shared.update(cx, |state, cx| {
                                    state.previews.selected = Some(tab_id);
                                    cx.notify();
                                })
                            }))
                    })),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .py_3()
                    .child(Icon::new(IconName::PackageOpen).size(px(34.)))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(div().text_size(px(20.)).child(name))
                            .child(div().text_xs().child(stage)),
                    ),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!("{} 个条目  ·  {backend}", entries.len())),
            )
            .when(browser_error.is_some(), |el| {
                el.child(
                    div()
                        .text_color(cx.theme().danger)
                        .child(browser_error.clone().unwrap_or_default()),
                )
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .when(!self.archive_dir.is_empty(), |el| {
                        el.child(
                            control("archive-up")
                                .icon(IconName::ChevronLeft)
                                .label("返回")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.archive_dir.pop();
                                    this.member_preview
                                        .update(cx, |preview, cx| preview.clear(window, cx));
                                    cx.notify();
                                })),
                        )
                    })
                    .child(
                        control("archive-root")
                            .label("根目录")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.archive_dir.clear();
                                this.member_preview
                                    .update(cx, |preview, cx| preview.clear(window, cx));
                                cx.notify();
                            })),
                    )
                    .children(self.archive_dir.iter().enumerate().map(|(index, part)| {
                        let path = self.archive_dir[..=index].to_vec();
                        control(("archive-breadcrumb", index))
                            .label(part.clone())
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.archive_dir = path.clone();
                                this.member_preview
                                    .update(cx, |preview, cx| preview.clear(window, cx));
                                cx.notify();
                            }))
                    })),
            )
            .when(id.is_none(), |el| {
                el.child(
                    div()
                        .py_8()
                        .text_center()
                        .text_color(cx.theme().muted_foreground)
                        .child("将压缩包拖到这里预览"),
                )
            })
            .child(self.member_preview.clone())
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("点击文件预览内容 · 文本 / PNG / JPEG"),
            )
            .child(
                div()
                    .flex()
                    .px_2()
                    .py_2()
                    .bg(cx.theme().secondary)
                    .child(div().flex_1().child("归档内路径"))
                    .child(div().w(px(120.)).child("大小")),
            )
            .child(
                div().id("archive-entries").min_h(px(300.)).children(
                    visible_entries
                        .into_iter()
                        .enumerate()
                        .map(|(index, entry)| {
                            let path = entry.path.clone();
                            let is_dir = entry.is_dir;
                            let size = entry
                                .size
                                .map(|n| format!("{n} B"))
                                .unwrap_or_else(|| "—".into());
                            let mut member_source = source.clone();
                            if let Some(source) = &mut member_source {
                                source.member = path.clone().into();
                                if is_dir {
                                    source.unavailable = Some("这是目录，请选择其中的文件".into());
                                }
                            }
                            let child_path = entry.path.clone();
                            div()
                                .id(("member-row", index))
                                .cursor_pointer()
                                .hover(|style| style.bg(cx.theme().secondary))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    if is_dir {
                                        this.member_preview
                                            .update(cx, |preview, cx| preview.clear(window, cx));
                                        if let Some(path) =
                                            crate::archive_browser::ArchiveBrowser::path_parts(
                                                &child_path,
                                            )
                                        {
                                            this.archive_dir = path;
                                            cx.notify();
                                        }
                                        return;
                                    }
                                    if let Some(source) = member_source.clone() {
                                        this.member_preview.update(cx, |preview, cx| {
                                            preview.open(source, window, cx)
                                        });
                                    }
                                }))
                                .flex()
                                .gap_3()
                                .px_2()
                                .py_2()
                                .border_b_1()
                                .border_color(cx.theme().border)
                                .child(
                                    Icon::new(if is_dir {
                                        IconName::FolderOpen
                                    } else {
                                        IconName::FileArchive
                                    })
                                    .small(),
                                )
                                .child(div().flex_1().child(entry.name.clone()))
                                .child(div().w(px(120.)).text_xs().child(size))
                        }),
                ),
            )
    }
}
impl View {
    fn sidebar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let queue = &self.shared.read(cx).queue;
        let count = queue.jobs.len();
        let pending = queue
            .jobs
            .iter()
            .filter(|job| job.phase == Phase::Queued)
            .count();
        let active = queue.jobs.iter().filter(|job| job.phase.active()).count();
        let paused = queue.paused;
        let collapsed = self.sidebar_collapsed;
        let workspace = SidebarMenu::new().children(
            [
                (Page::Preview, "归档预览", IconName::FolderSearch),
                (Page::Tasks, "任务中心", IconName::Layers),
                (Page::History, "历史记录", IconName::Clock),
            ]
            .into_iter()
            .map(|(page, label, icon)| {
                SidebarMenuItem::new(label)
                    .icon(Icon::new(icon).size(px(21.)))
                    .text_size(px(16.))
                    .min_h(px(42.))
                    .px_3()
                    .active(self.page == page)
                    .on_click(cx.listener(move |this, _, _, cx| this.page(page, cx)))
            }),
        );
        let settings = SidebarMenu::new().children(
            [
                (Page::Passwords, "密码管理", IconName::KeyRound),
                (Page::Settings, "偏好设置", IconName::SlidersHorizontal),
                (Page::Integration, "系统集成", IconName::AppWindow),
            ]
            .into_iter()
            .map(|(page, label, icon)| {
                SidebarMenuItem::new(label)
                    .icon(Icon::new(icon).size(px(21.)))
                    .text_size(px(16.))
                    .min_h(px(42.))
                    .px_3()
                    .active(self.page == page)
                    .on_click(cx.listener(move |this, _, _, cx| this.page(page, cx)))
            }),
        );
        Sidebar::new("main-sidebar")
            .w(px(206.))
            .collapsed(collapsed)
            .header(
                SidebarHeader::new().child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .py_2()
                        .child(Icon::new(IconName::PackageOpen).size(px(27.)))
                        .when(!collapsed, |el| {
                            el.child(
                                div()
                                    .text_size(px(21.))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child("SmartZip"),
                            )
                        }),
                ),
            )
            .child(SidebarGroup::new("工作空间").child(workspace))
            .child(SidebarGroup::new("管理").child(settings))
            .footer(
                SidebarFooter::new()
                    .flex_col()
                    .items_stretch()
                    .min_w_0()
                    .when(!collapsed, |footer| {
                        footer.child(
                            div()
                                .w_full()
                                .p_3()
                                .min_w_0()
                                .flex_shrink_0()
                                .border_t_1()
                                .border_color(cx.theme().border)
                                .flex()
                                .flex_col()
                                .gap_2()
                                .child(div().text_size(px(14.)).child(if paused {
                                    "队列已暂停"
                                } else {
                                    "自动调度中"
                                }))
                                .child(
                                    div()
                                        .text_size(px(13.))
                                        .child(format!("{pending} 排队 · {active} 进行中")),
                                ),
                        )
                    })
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .w_full()
                            .min_w_0()
                            .when(collapsed, |el| el.justify_center())
                            .when(!collapsed, |el| {
                                el.child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .text_ellipsis()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(format!("{count} 项任务 · 本地执行")),
                                )
                            })
                            .child(div().flex_shrink_0().child(
                                SidebarToggleButton::new().collapsed(collapsed).on_click(
                                    cx.listener(|this, _, _, cx| {
                                        this.sidebar_collapsed = !this.sidebar_collapsed;
                                        cx.notify();
                                    }),
                                ),
                            )),
                    ),
            )
    }
}
impl Render for View {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.shared.read(cx);
        if !self.quick && self.preview_revision != state.preview_revision {
            self.page = Page::Preview;
            self.preview_revision = state.preview_revision;
        }
        let message = state.message.clone();
        let busy = state.library_busy;
        let count = state.queue.jobs.len();
        let queued = state
            .queue
            .jobs
            .iter()
            .filter(|j| j.phase == Phase::Queued)
            .count();
        let is_preview = !self.quick && self.page == Page::Preview;
        let prompt_job = (if is_preview {
            &state.previews
        } else {
            &state.queue
        })
        .jobs
        .iter()
        .find(|job| job.prompt.is_some())
        .map(|job| (is_preview, job.id));
        let task_password_target = state
            .queue
            .selected()
            .filter(|job| job.phase == Phase::Queued)
            .map(|job| job.id);
        if self.prompt_job != prompt_job {
            self.prompt_job = prompt_job;
            self.prompt_secret
                .update(cx, |input, cx| input.set_value("", window, cx));
        }
        if self.task_password_target != task_password_target {
            self.task_password_target = task_password_target;
            self.task_password
                .update(cx, |input, cx| input.set_value("", window, cx));
        }
        if self.page != Page::Passwords && self.password_reveal_request.is_some() {
            self.revealed_password = None;
            self.password_reveal_request = None;
            self.password_reveal_generation += 1;
        }
        let quick = self.quick;
        let selected_archive = self.shared.read(cx).previews.selected;
        if self.member_archive != selected_archive {
            self.member_archive = selected_archive;
            self.archive_dir.clear();
            self.member_preview
                .update(cx, |preview, cx| preview.clear(window, cx));
        }
        let title = match self.page {
            Page::Tasks => "任务中心",
            Page::History => "历史记录",
            Page::Passwords => "密码管理",
            Page::Preview => "归档预览",
            Page::Settings => "偏好设置",
            Page::Integration => "系统集成",
        };
        let header = div()
            .flex()
            .items_center()
            .justify_between()
            .px_5()
            .py_3()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .text_base()
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .child(if quick { "SmartZip" } else { title }),
                    )
                    .when(!quick, |el| {
                        el.child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(format!("{count} 项任务 · {queued} 项排队")),
                        )
                    }),
            )
            .child(
                control("other-window")
                    .icon(if quick {
                        IconName::PanelLeftOpen
                    } else {
                        IconName::AppWindow
                    })
                    .label(if quick {
                        "任务中心"
                    } else {
                        "快速窗口"
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        open_view(this.shared.clone(), !quick, cx)
                    })),
            );
        let body = if quick {
            div()
                .id("quick-scroll")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .p_5()
                .flex()
                .flex_col()
                .gap_5()
                .child(self.drop_area(cx))
                .child(self.quick_controls(None, cx))
                .child(self.prompt(cx))
                .child(self.detail(cx))
                .into_any_element()
        } else {
            match self.page {
                Page::Tasks => div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .child(
                        div()
                            .id("task-content")
                            .flex_1()
                            .min_w_0()
                            .p_4()
                            .overflow_y_scroll()
                            .flex()
                            .flex_col()
                            .gap_4()
                            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| {
                                this.shared.update(cx, |state, cx| {
                                    state.enqueue(paths.paths().to_vec(), TaskOperation::Extract);
                                    cx.notify();
                                })
                            }))
                            .child(self.task_overview(cx))
                            .child(self.prompt(cx))
                            .child(self.actions(cx))
                            .when(self.queue_settings_open, |el| {
                                el.child(
                                    div()
                                        .p_4()
                                        .border_1()
                                        .border_color(cx.theme().border)
                                        .rounded_lg()
                                        .child(self.quick_controls(None, cx)),
                                )
                            })
                            .child(self.tasks(cx))
                            .when(count == 0, |el| {
                                el.child(div().py_8().child(self.drop_area(cx)))
                            }),
                    )
                    .child(
                        div()
                            .id("detail-scroll")
                            .w(px(320.))
                            .flex_shrink_0()
                            .border_l_1()
                            .border_color(cx.theme().border)
                            .bg(cx.theme().background)
                            .overflow_y_scroll()
                            .p_5()
                            .child(self.detail(cx)),
                    )
                    .into_any_element(),
                _ => {
                    let content = match self.page {
                        Page::History => self.history(cx).into_any_element(),
                        Page::Passwords => self.passwords(cx).into_any_element(),
                        Page::Preview => self.preview(cx).into_any_element(),
                        Page::Settings => self.settings(window, cx).into_any_element(),
                        Page::Integration => self.integration.clone().into_any_element(),
                        _ => unreachable!(),
                    };
                    div()
                        .id("page-scroll")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .p_5()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .child(self.prompt(cx))
                        .child(content)
                        .into_any_element()
                }
            }
        };
        let footer =
            div()
                .px_4()
                .py_2()
                .border_t_1()
                .border_color(cx.theme().border)
                .flex()
                .flex_col()
                .gap_1()
                .when(!message.is_empty(), |el| {
                    el.child(div().text_xs().child(message))
                })
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(if busy {
                                    "正在同步数据…"
                                } else {
                                    "本地处理 · 单任务队列"
                                }),
                        )
                        .child(control("quit").label("退出").on_click(cx.listener(
                            |this, _, _, cx| {
                                this.shared.update(cx, |state, cx| {
                                    if state.queue.has_work() || state.previews.has_work() {
                                        state.message =
                                        "还有任务：再次点击「取消全部并退出」可停止并等待清理。"
                                            .into();
                                    } else {
                                        state.quitting = true;
                                    }
                                    cx.notify();
                                })
                            },
                        )))
                        .when(
                            self.shared.read(cx).message.contains("还有任务")
                                || self.shared.read(cx).message.contains("仍有任务"),
                            |el| {
                                el.child(control("cancel-quit").label("取消全部并退出").on_click(
                                    cx.listener(|this, _, _, cx| {
                                        this.shared.update(cx, |state, cx| {
                                            state.queue.cancel_all();
                                            state.previews.cancel_all();
                                            state.quitting = true;
                                            cx.notify();
                                        })
                                    }),
                                ))
                            },
                        ),
                );
        let main = div()
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .child(header)
            .child(body)
            .child(footer);
        div()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .text_size(px(13.))
            .flex()
            .when(!quick, |el| el.child(self.sidebar(cx)))
            .child(main)
    }
}
