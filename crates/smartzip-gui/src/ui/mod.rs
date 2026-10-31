mod workspace;
use workspace::*;
mod data;
mod glass;
mod navigation;
mod quick_settings;
mod recovery;
mod shell;
use data::*;
mod desktop;
use desktop::*;
mod history;
mod interaction;
mod passwords;
mod preview;
mod settings;
mod task_detail;
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
    menu::DropdownMenu,
    progress::Progress,
    resizable::{h_resizable, resizable_panel},
    sidebar::{Sidebar, SidebarHeader, SidebarMenu, SidebarMenuItem, SidebarToggleButton},
    switch::Switch,
    tab::{Tab, TabBar},
    ActiveTheme, Icon, Root, Sizable,
};
use gpui::{
    div, prelude::*, px, size, App, AppContext, Bounds, Context, Entity, ExternalPaths,
    IntoElement, PathPromptOptions, Render, SharedString, Subscription, Window, WindowBounds,
    WindowHandle, WindowOptions,
};
use std::{path::PathBuf, sync::mpsc, time::Duration};

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
    glass: Option<crate::liquid_glass::Glass>,
    quick: bool,
    page: Page,
    diagnostics_open: bool,
    task_config_open: bool,
    task_filter: usize,
    selected_file: Option<(u64, smartzip_core::NodeId)>,
    task_selection: std::collections::HashSet<(u64, smartzip_core::NodeId)>,
    expanded_roots: std::collections::HashSet<smartzip_core::NodeId>,
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
    Button::new(id).large().ghost()
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
        Phase::Queued | Phase::Waiting => 2,
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
                title: Some(if crate::liquid_glass::enabled() {
                    "SmartZip · Liquid Glass 原型".into()
                } else {
                    "SmartZip".into()
                }),
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
        Self {
            shared,
            glass: crate::liquid_glass::enabled().then(Default::default),
            quick,
            page: Page::Tasks,
            diagnostics_open: false,
            task_config_open: false,
            task_filter: 0,
            selected_file: None,
            task_selection: Default::default(),
            expanded_roots: std::collections::HashSet::new(),
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
            member_preview: cx.new(|cx| crate::member_preview::MemberPreview::new(window, cx)),
            member_archive: None,
            archive_dir: Vec::new(),
            archive_cache: Default::default(),
            archive_positions: Default::default(),
            archive_sort_size: false,
            preview_visible: true,
            integration: cx.new(crate::integration_ui::SystemIntegration::new),
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queued_tasks_belong_to_pending_filter() {
        assert_eq!(job_category(Phase::Queued), 2);
        assert_eq!(job_category(Phase::Waiting), 2);
        assert_eq!(job_category(Phase::Running), 1);
    }
}
