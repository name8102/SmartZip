mod data;
mod recovery;
use data::*;
mod desktop;
use desktop::*;
mod history;
mod interaction;
mod passwords;
mod preview;
mod settings;
mod tasks;

// One native window projects the workspace in two display modes; archive work stays off the UI thread.
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
    ActiveTheme, Icon, Root, Sizable,
};
use gpui::{
    div, prelude::*, px, size, App, AppContext, Bounds, Context, Entity, ExternalPaths,
    IntoElement, PathPromptOptions, Render, SharedString, Subscription, Window, WindowBounds,
    WindowHandle, WindowOptions,
};
use std::{path::PathBuf, sync::mpsc, time::Duration};

pub struct Workspace {
    queue: Queue,
    queue_store: crate::queue_store::QueueStore,
    drafts_loaded: bool,
    previews: Queue,
    preview_revision: u64,
    open_requests: mpsc::Receiver<crate::new_open_request::OpenRequest>,
    pending_open: Vec<crate::new_open_request::OpenRequest>,
    config: Option<ConfigSnapshot>,
    config_revision: u64,
    settings: TaskSettings,
    message: String,
    window: Option<WindowHandle<Root>>,
    requested_mode: Option<bool>,
    quitting: bool,
    exit_allow_queue_loss: bool,
    inbox: mpsc::Receiver<LibraryMessage>,
    sender: mpsc::Sender<LibraryMessage>,
    history: Vec<library::HistorySummary>,
    history_query: library::Query,
    password_query: library::Query,
    history_version: u64,
    password_version: u64,
    history_more: bool,
    password_more: bool,
    history_loading: bool,
    password_loading: bool,
    history_detail: Option<library::TaskDetail>,
    history_detail_request: Option<String>,
    passwords: Vec<PasswordSummary>,
    document: String,
    library_busy: bool,
    preferences: crate::preferences::Preferences,
    preferences_writer: mpsc::Sender<(u64, crate::preferences::Preferences)>,
    preferences_revision: u64,
    preferences_saved: u64,
    exit_prompt_open: bool,
    settings_form: Option<Entity<crate::config_form::ConfigForm>>,
    settings_form_revision: u64,
    recoveries: Vec<library::RecoverySummary>,
    recovery_loading: bool,
    requested_page: Option<Page>,
}
enum LibraryMessage {
    PreferencesSaved(u64, Result<(), String>),
    Config(Result<Box<ConfigSnapshot>, String>),
    Drafts(Result<Vec<JobRequest>, String>),
    ConfigSaved(Result<Box<ConfigSnapshot>, String>),
    History(u64, Result<library::Page<library::HistorySummary>, String>),
    Passwords(u64, Result<library::Page<PasswordSummary>, String>),
    TaskDetail(String, Result<library::TaskDetail, String>),
    Recovery(Result<Vec<library::RecoverySummary>, String>),
    Document(Result<String, String>),
    Mutation(Result<String, String>),
}
impl Workspace {
    fn new(
        open_requests: mpsc::Receiver<crate::new_open_request::OpenRequest>,
        preferences: crate::preferences::Preferences,
        cx: &mut Context<Self>,
    ) -> Self {
        let (sender, inbox) = mpsc::channel();
        let (preferences_writer, preference_rx) =
            mpsc::channel::<(u64, crate::preferences::Preferences)>();
        let preference_reply = sender.clone();
        std::thread::spawn(move || {
            while let Ok(mut update) = preference_rx.recv() {
                while let Ok(next) = preference_rx.try_recv() {
                    update = next;
                }
                let result =
                    crate::preferences::Preferences::path().and_then(|path| update.1.save(&path));
                let _ = preference_reply.send(LibraryMessage::PreferencesSaved(update.0, result));
            }
        });
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
                    let previous: std::collections::HashMap<_, _> =
                        state.queue.jobs.iter().map(|j| (j.id, j.phase)).collect();
                    state.queue_store.sync(&mut state.queue);
                    let mut changed = state.queue.tick() | state.previews.tick();
                    if let Some(error) = &state.queue_store.error {
                        if state.message != *error { state.message = error.clone(); changed = true; }
                    }
                    let attention = state
                        .queue
                        .jobs
                        .iter()
                        .filter(|j| {
                            previous.get(&j.id).is_some_and(|p| *p != j.phase)
                                && matches!(
                                    j.phase,
                                    Phase::Waiting
                                        | Phase::Failed
                                        | Phase::Partial
                                        | Phase::Completed
                                )
                        })
                        .count();
                    if attention > 0 && state.preferences.notifications && !state.quitting {
                        cx.show_system_notification(gpui::SystemNotification {
                            tag: "smartzip-tasks".into(),
                            title: "SmartZip · 任务状态已更新".into(),
                            body: format!("{attention} 项任务已结束或需要处理，打开任务中心查看。")
                                .into(),
                            actions: vec![],
                        });
                    }
                    while let Ok(paths) = state.open_requests.try_recv() {
                        state.pending_open.push(paths);
                    }
                    if state.config.is_some() && state.drafts_loaded && !state.pending_open.is_empty() {
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
                        if !matches!(
                            message,
                            LibraryMessage::PreferencesSaved(..)
                                | LibraryMessage::Drafts(..)
                                | LibraryMessage::History(..)
                                | LibraryMessage::Passwords(..)
                                | LibraryMessage::TaskDetail(..)
                                | LibraryMessage::Recovery(..)
                        ) {
                            state.library_busy = false;
                        }
                        match message {
                            LibraryMessage::PreferencesSaved(revision, result) => {
                                state.preferences_saved = revision;
                                if let Err(error) = result {
                                    state.message = format!("外观偏好保存失败：{error}");
                                }
                            }
                            LibraryMessage::Config(result) => match result {
                                Ok(config) => {
                                    state.message = config.diagnostics.join("\n");
                                    if !state.drafts_loaded {
                                        let config = config.resolved.clone();
                                        let tx = state.sender.clone();
                                        std::thread::spawn(move || { let _ = tx.send(LibraryMessage::Drafts(crate::queue_store::load(&config))); });
                                    }
                                    state.config = Some(*config);
                                    state.config_revision += 1;
                                    state.reload_recovery();
                                }
                                Err(e) => {
                                    state.config = None;
                                    state.message = e;
                                }
                            },
                            LibraryMessage::Drafts(result) => {
                                state.drafts_loaded = true;
                                match result {
                                    Ok(requests) => {
                                        let count = requests.len();
                                        for request in requests {
                                            if !state.queue.jobs.iter().any(|j| j.request.settings.queued_task_id == request.settings.queued_task_id) { state.queue.restore(request); }
                                        }
                                        if count > 0 { state.message = format!("已找回 {count} 项等待队列。检查配置后选择开始；临时密码需要重新输入。"); }
                                    }
                                    Err(error) => state.message = error,
                                }
                            }
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
                            LibraryMessage::History(version, result) => {
                                if version == state.history_version {
                                    state.history_loading = false;
                                    match result {
                                        Ok(page) => {
                                            state.history = page.rows;
                                            state.history_more = page.has_more;
                                        }
                                        Err(e) => state.message = e,
                                    }
                                }
                            }
                            LibraryMessage::Passwords(version, result) => {
                                if version == state.password_version {
                                    state.password_loading = false;
                                    match result {
                                        Ok(page) => {
                                            state.passwords = page.rows;
                                            state.password_more = page.has_more;
                                        }
                                        Err(e) => state.message = e,
                                    }
                                }
                            }
                            LibraryMessage::Recovery(result) => {
                                state.recovery_loading = false;
                                match result {
                                    Ok(rows) => state.recoveries = rows,
                                    Err(error) => state.message = error,
                                }
                            }
                            LibraryMessage::TaskDetail(id, result) => {
                                if state.history_detail_request.as_ref() == Some(&id) {
                                    match result {
                                        Ok(detail) => state.history_detail = Some(detail),
                                        Err(e) => state.message = e,
                                    }
                                }
                            }
                            LibraryMessage::Document(result) => match result {
                                Ok(text) => state.document = text,
                                Err(e) => state.message = e,
                            },
                            LibraryMessage::Mutation(result) => {
                                let refresh = result.is_ok();
                                state.message = result.unwrap_or_else(|e| e);
                                if refresh {
                                    state.reload_passwords();
                                }
                            }
                        }
                    }
                    state.queue_store.sync(&mut state.queue);
                    if state.quitting
                        && !state.queue.has_active()
                        && !state.previews.has_active()
                        && state.preferences_saved == state.preferences_revision
                        && !state.library_busy
                        && (state.queue_store.settled() || (state.exit_allow_queue_loss && state.queue_store.idle()))
                    {
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
            queue_store: Default::default(),
            drafts_loaded: false,
            previews: Queue::default(),
            preview_revision: 0,
            open_requests,
            pending_open: vec![],
            config: None,
            config_revision: 0,
            settings: TaskSettings::default(),
            message: "正在读取配置…".into(),
            window: None,
            requested_mode: None,
            quitting: false,
            exit_allow_queue_loss: false,
            inbox,
            sender,
            history: vec![],
            history_query: library::Query::default(),
            password_query: library::Query::default(),
            history_version: 0,
            password_version: 0,
            history_more: false,
            password_more: false,
            history_loading: false,
            password_loading: false,
            history_detail: None,
            history_detail_request: None,
            passwords: vec![],
            document: String::new(),
            library_busy: false,
            preferences,
            preferences_writer,
            preferences_revision: 0,
            preferences_saved: 0,
            exit_prompt_open: false,
            settings_form: None,
            settings_form_revision: 0,
            recoveries: vec![],
            recovery_loading: false,
            requested_page: None,
        }
    }
    fn enqueue(&mut self, paths: Vec<PathBuf>, operation: TaskOperation) {
        self.enqueue_with_hold(paths, operation, false);
    }

    fn enqueue_with_hold(&mut self, paths: Vec<PathBuf>, operation: TaskOperation, hold: bool) {
        if self.quitting {
            return;
        }
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
    Appearance,
    Help,
    Recovery,
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
    task_selection: std::collections::HashSet<(u64, smartzip_core::NodeId)>,
    expanded_roots: std::collections::HashSet<smartzip_core::NodeId>,
    queue_settings_open: bool,
    detail_state: Option<(u64, Phase)>,
    prompt_job: Option<(bool, u64, u64)>,
    pending_selection: Option<(bool, u64)>,
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
    archive_cache: std::collections::HashMap<
        u64,
        Result<std::sync::Arc<crate::archive_browser::ArchiveBrowser>, String>,
    >,
    archive_positions: std::collections::HashMap<u64, Vec<String>>,
    archive_sort_size: bool,
    preview_visible: bool,
    integration: Entity<crate::integration_ui::SystemIntegration>,
    form_revision: u64,
    prompt_secret: Entity<InputState>,
    focus: gpui::FocusHandle,
    search: Entity<InputState>,
    history_search: Entity<InputState>,
    password_search: Entity<InputState>,
    archive_search: Entity<InputState>,
    password_selection: std::collections::HashSet<i64>,
    password_page_version: u64,
    cleanup_limit: Entity<InputState>,
    cleanup_candidates: Option<Vec<i64>>,
    _subscriptions: Vec<Subscription>,
}
fn control(id: impl Into<gpui::ElementId>) -> Button {
    Button::new(id).small().ghost()
}

fn state_label(state: &str) -> &str {
    match state {
        "queued" | "ready" | "queued_gui" => "排队中",
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
    let loaded = crate::preferences::Preferences::path()
        .and_then(|path| crate::preferences::Preferences::load(&path));
    let error = loaded.as_ref().err().cloned();
    let preferences = loaded.unwrap_or_default();
    crate::appearance::apply(&preferences, None, cx);
    desktop::install_actions(cx);
    cx.set_app_identity("org.smartzip.SmartZip", "SmartZip");
    let shared = cx.new(|cx| Workspace::new(open_requests, preferences, cx));
    if let Some(error) = error {
        shared.update(cx, |s, _| s.message = error);
    }
    cx.on_system_notification_response({
        let shared = shared.clone();
        move |response, cx| {
            if response.tag.as_ref() == "smartzip-tasks" {
                shared.update(cx, |s, _| s.requested_page = Some(Page::Tasks));
                open_view(shared.clone(), false, cx);
            }
        }
    });
    open_view(shared, false, cx);
}
fn open_view(shared: Entity<Workspace>, quick: bool, cx: &mut App) {
    cx.defer(move |cx| open_view_now(shared, quick, cx));
}
fn open_view_now(shared: Entity<Workspace>, quick: bool, cx: &mut App) {
    if let Some(handle) = shared.read(cx).window {
        if handle
            .update(cx, |_, window, _| window.activate_window())
            .is_ok()
        {
            shared.update(cx, |state, cx| {
                state.requested_mode = Some(quick);
                cx.notify();
            });
            return;
        }
    }
    let mut bounds = Bounds::centered(
        None,
        size(
            px(if quick { 420. } else { 1180. }),
            px(if quick { 650. } else { 780. }),
        ),
        cx,
    );
    let prefs = &shared.read(cx).preferences;
    if let Some(saved) = if quick {
        prefs.quick_window
    } else {
        prefs.full_window
    } {
        let candidate = Bounds {
            origin: gpui::point(px(saved.x), px(saved.y)),
            size: size(px(saved.width), px(saved.height)),
        };
        if cx
            .displays()
            .iter()
            .any(|display| display.bounds().contains(&candidate.origin))
        {
            bounds = candidate;
        }
    }
    // Creation is synchronous inside the deferred callback. Consecutive launch requests
    // therefore see the same handle instead of racing to create separate windows.
    let shared_for_window = shared.clone();
    let result = cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(px(360.), px(480.))),
            titlebar: Some(gpui::TitlebarOptions {
                title: Some("SmartZip".into()),
                ..Default::default()
            }),
            ..Default::default()
        },
        move |window, cx| {
            let close_state = shared_for_window.clone();
            window.on_window_should_close(cx, move |window, cx| {
                desktop::request_exit(close_state.clone(), window, cx);
                false
            });
            let view = cx.new(|cx| View::new(shared_for_window, quick, window, cx));
            cx.new(|cx| Root::new(view, window, cx))
        },
    );
    match result {
        Ok(handle) => {
            shared.update(cx, |state, cx| {
                state.window = Some(handle);
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
}
impl View {
    fn switch_mode(&mut self, quick: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.quick == quick {
            return;
        }
        let preferences = self.shared.read(cx).preferences.clone();
        let saved = if quick {
            preferences.quick_window
        } else {
            preferences.full_window
        };
        self.quick = quick;
        // Keep the view and its page, drafts, selections and archive tabs alive.
        // The window manager retains control of maximized/fullscreen windows.
        if !window.is_maximized() && !window.is_fullscreen() {
            let target = saved
                .map(|g| size(px(g.width), px(g.height)))
                .unwrap_or_else(|| {
                    size(
                        px(if quick { 420. } else { 1180. }),
                        px(if quick { 650. } else { 780. }),
                    )
                });
            window.resize(target);
        }
        // The focused input may have been hidden by the layout change.
        window.focus(&self.focus, cx);
        cx.notify();
    }
    fn new(
        shared: Entity<Workspace>,
        quick: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let subscription = cx.observe(&shared, |_, _, cx| cx.notify());
        let preferences = shared.read(cx).preferences.clone();
        let appearance = cx.observe_window_appearance(window, |this, window, cx| {
            let preferences = this.shared.read(cx).preferences.clone();
            crate::appearance::apply(&preferences, Some(window), cx);
        });
        let bounds = cx.observe_window_bounds(window, |this, window, cx| {
            if window.is_maximized() || window.is_fullscreen() {
                return;
            }
            let bounds = window.bounds();
            this.shared.update(cx, |state, cx| {
                let geometry = crate::preferences::Geometry {
                    x: bounds.origin.x.into(),
                    y: bounds.origin.y.into(),
                    width: bounds.size.width.into(),
                    height: bounds.size.height.into(),
                };
                if this.quick {
                    state.preferences.quick_window = Some(geometry);
                } else {
                    state.preferences.full_window = Some(geometry);
                }
                state.persist_preferences();
                cx.notify();
            });
        });
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("搜索名称或路径…"));
        let search_changed = cx.observe(&search, |_, _, cx| cx.notify());
        let history_search =
            cx.new(|cx| InputState::new(window, cx).placeholder("搜索归档路径、输出或日期…"));
        let password_search =
            cx.new(|cx| InputState::new(window, cx).placeholder("搜索来源或密码编号…"));
        let archive_search = cx.new(|cx| InputState::new(window, cx).placeholder("搜索当前目录…"));
        let archive_changed = cx.observe(&archive_search, |_, _, cx| cx.notify());
        let cleanup_limit = cx.new(|cx| InputState::new(window, cx).default_value("128"));
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        let retained_form = shared.read(cx).settings_form.clone();
        let retained_revision = shared.read(cx).settings_form_revision;
        Self {
            shared,
            quick,
            page: Page::Tasks,
            sidebar_collapsed: preferences.sidebar_collapsed,
            diagnostics_open: false,
            task_config_open: false,
            task_filter: 0,
            selected_file: None,
            task_selection: Default::default(),
            expanded_roots: std::collections::HashSet::new(),
            queue_settings_open: preferences.queue_settings_open,
            detail_state: None,
            prompt_job: None,
            pending_selection: None,
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
            config_form: retained_form,
            member_preview: cx.new(|cx| crate::member_preview::MemberPreview::new(window, cx)),
            member_archive: None,
            archive_dir: Vec::new(),
            archive_cache: Default::default(),
            archive_positions: Default::default(),
            archive_sort_size: false,
            preview_visible: true,
            integration: cx.new(crate::integration_ui::SystemIntegration::new),
            form_revision: retained_revision,
            prompt_secret: cx.new(|cx| {
                InputState::new(window, cx)
                    .masked(true)
                    .placeholder("请输入归档密码")
            }),
            focus,
            search,
            history_search,
            password_search,
            archive_search,
            cleanup_limit,
            password_selection: Default::default(),
            password_page_version: 0,
            cleanup_candidates: None,
            _subscriptions: vec![
                subscription,
                appearance,
                bounds,
                search_changed,
                archive_changed,
            ],
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
        if self.quick && page != Page::Tasks {
            self.shared.update(cx, |s, _| s.requested_page = Some(page));
            open_view(self.shared.clone(), false, cx);
            return;
        }
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
                Page::Recovery => state.reload_recovery(),
                Page::History => state.reload_history(),
                Page::Passwords => state.reload_passwords(),
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
                (Page::Recovery, "任务恢复", IconName::Layers),
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
                (Page::Appearance, "外观与主题", IconName::SlidersHorizontal),
                (Page::Integration, "系统集成", IconName::AppWindow),
                (Page::Help, "关于与帮助", IconName::ShieldCheck),
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
                                        let collapsed = this.sidebar_collapsed;
                                        this.shared.update(cx, |s, _| {
                                            s.preferences.sidebar_collapsed = collapsed;
                                            s.persist_preferences();
                                        });
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
        let requested_mode = self.shared.update(cx, |s, _| s.requested_mode.take());
        if let Some(quick) = requested_mode {
            self.switch_mode(quick, window, cx);
        }
        if !self.quick {
            let requested = self.shared.update(cx, |s, _| s.requested_page.take());
            if let Some(page) = requested {
                self.page(page, cx);
            }
        }
        let pw_version = self.shared.read(cx).password_version;
        if self.password_page_version != pw_version {
            self.password_page_version = pw_version;
            self.password_selection.clear();
            self.revealed_password = None;
            self.password_reveal_request = None;
            self.password_reveal_generation += 1;
        }
        let preferences = self.shared.read(cx).preferences.clone();
        window.set_rem_size(px(preferences.font_size));
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
        let prompt_job = self.selected_prompt(cx);
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
            if let Some(previous) = self.member_archive {
                self.archive_positions
                    .insert(previous, self.archive_dir.clone());
            }
            self.member_archive = selected_archive;
            self.archive_dir = selected_archive
                .and_then(|id| self.archive_positions.get(&id).cloned())
                .unwrap_or_default();
            let existing: std::collections::HashSet<_> = self
                .shared
                .read(cx)
                .previews
                .jobs
                .iter()
                .filter(|j| !j.dismiss_requested)
                .map(|j| j.id)
                .collect();
            self.archive_cache.retain(|id, _| existing.contains(id));
            self.archive_positions.retain(|id, _| existing.contains(id));
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
            Page::Appearance => "外观与主题",
            Page::Help => "关于与帮助",
            Page::Recovery => "任务恢复",
        };
        let header =
            div()
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
                    control("toggle-mode")
                        .icon(if quick {
                            IconName::PanelLeftOpen
                        } else {
                            IconName::AppWindow
                        })
                        .label(if quick {
                            "完整模式"
                        } else {
                            "快速模式"
                        })
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.switch_mode(!quick, window, cx)
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
                            .child(Input::new(&self.search).cleanable(true))
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
                            .when(
                                preferences.detail_visible
                                    && window.viewport_size().width < px(1000.),
                                |el| el.child(self.detail(cx)),
                            )
                            .when(count == 0, |el| {
                                el.child(div().py_8().child(self.drop_area(cx)))
                            }),
                    )
                    .when(
                        preferences.detail_visible && window.viewport_size().width >= px(1000.),
                        |el| {
                            el.child(
                                div()
                                    .id("detail-scroll")
                                    .w(px(preferences.detail_width))
                                    .flex_shrink_0()
                                    .border_l_1()
                                    .border_color(cx.theme().border)
                                    .bg(cx.theme().background)
                                    .overflow_y_scroll()
                                    .p_5()
                                    .child(self.detail(cx)),
                            )
                        },
                    )
                    .into_any_element(),
                _ => {
                    let content = match self.page {
                        Page::History => self.history(cx).into_any_element(),
                        Page::Passwords => self.passwords(cx).into_any_element(),
                        Page::Preview => self.preview(cx).into_any_element(),
                        Page::Settings => self.settings(window, cx).into_any_element(),
                        Page::Integration => self.integration.clone().into_any_element(),
                        Page::Appearance => self.appearance(cx).into_any_element(),
                        Page::Help => self.help(cx).into_any_element(),
                        Page::Recovery => self.recovery(cx).into_any_element(),
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
                .gap_2()
                .when(self.shared.read(cx).queue_store.error.is_some(), |el| {
                    el.child(
                        control("retry-queue-save")
                            .label("重试保存等待队列")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.shared.update(cx, |s, cx| {
                                    s.queue_store.retry();
                                    cx.notify();
                                });
                            })),
                    )
                })
                .when(!message.is_empty(), |el| {
                    el.child(
                        div()
                            .flex()
                            .gap_3()
                            .items_center()
                            .child(div().flex_1().min_w_0().text_sm().child(message))
                            .child(control("dismiss-message").label("关闭提示").on_click(
                                cx.listener(|this, _, _, cx| {
                                    this.shared.update(cx, |s, cx| {
                                        s.message.clear();
                                        cx.notify();
                                    })
                                }),
                            )),
                    )
                })
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(if self.shared.read(cx).quitting {
                                    "正在退出 · 等待任务与临时文件清理…"
                                } else if busy {
                                    "正在处理数据…"
                                } else {
                                    "本地处理 · 智能调度"
                                }),
                        )
                        .child(control("quit").label("退出").on_click(cx.listener(
                            |this, _, window, cx| {
                                desktop::request_exit(this.shared.clone(), window, cx)
                            },
                        ))),
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
            .text_size(px(preferences.font_size))
            .key_context("SmartZip")
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &OpenArchive, _, cx| {
                this.page(Page::Preview, cx);
                this.pick(TaskOperation::List, cx);
            }))
            .on_action(
                cx.listener(|this, _: &AddExtract, _, cx| this.pick(TaskOperation::Extract, cx)),
            )
            .on_action(cx.listener(|this, _: &ShowSettings, _, cx| this.page(Page::Settings, cx)))
            .on_action(
                cx.listener(|this, _: &ShowAppearance, _, cx| this.page(Page::Appearance, cx)),
            )
            .on_action(cx.listener(|this, _: &ShowHelp, _, cx| this.page(Page::Help, cx)))
            .on_action(cx.listener(|this, _: &ShowTasks, _, cx| this.page(Page::Tasks, cx)))
            .on_action(cx.listener(|this, _: &ShowHistory, _, cx| this.page(Page::History, cx)))
            .on_action(cx.listener(|this, _: &Search, window, cx| {
                use gpui::Focusable;
                if this.page == Page::Settings {
                    if let Some(form) = &this.config_form {
                        form.update(cx, |form, cx| form.focus_search(window, cx));
                    }
                    return;
                }
                let input = match this.page {
                    Page::History => &this.history_search,
                    Page::Passwords => &this.password_search,
                    Page::Preview => &this.archive_search,
                    _ => &this.search,
                };
                input.focus_handle(cx).focus(window, cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleDetails, _, cx| {
                this.change_appearance(|p| p.detail_visible = !p.detail_visible, cx)
            }))
            .on_action(cx.listener(|this, _: &ToggleMode, window, cx| {
                this.switch_mode(!this.quick, window, cx)
            }))
            .on_action(cx.listener(|this, _: &Quit, window, cx| {
                desktop::request_exit(this.shared.clone(), window, cx)
            }))
            .flex()
            .when(!quick, |el| el.child(self.sidebar(cx)))
            .child(main)
            .children(Root::render_dialog_layer(window, cx))
    }
}
