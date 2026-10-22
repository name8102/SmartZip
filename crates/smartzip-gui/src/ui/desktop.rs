use super::*;
use crate::preferences::{ColorMode, Density};
use gpui::{KeyBinding, Menu, MenuItem, PromptLevel};

gpui::actions!(
    smartzip,
    [
        OpenArchive,
        AddExtract,
        ShowSettings,
        ShowAppearance,
        ShowHelp,
        ShowTasks,
        ShowHistory,
        Search,
        ToggleDetails,
        ToggleMode,
        Quit
    ]
);

pub(super) fn install_actions(cx: &mut App) {
    let modifier = if cfg!(target_os = "macos") {
        "cmd"
    } else {
        "ctrl"
    };
    cx.bind_keys([
        KeyBinding::new(&format!("{modifier}-o"), OpenArchive, Some("SmartZip")),
        KeyBinding::new(&format!("{modifier}-shift-o"), AddExtract, Some("SmartZip")),
        KeyBinding::new(&format!("{modifier}-,"), ShowSettings, Some("SmartZip")),
        KeyBinding::new(&format!("{modifier}-f"), Search, Some("SmartZip")),
        KeyBinding::new(&format!("{modifier}-1"), ShowTasks, Some("SmartZip")),
        KeyBinding::new(&format!("{modifier}-2"), ShowHistory, Some("SmartZip")),
        KeyBinding::new(
            &format!("{modifier}-shift-d"),
            ToggleDetails,
            Some("SmartZip"),
        ),
        KeyBinding::new(&format!("{modifier}-q"), Quit, Some("SmartZip")),
        KeyBinding::new(&format!("{modifier}-shift-m"), ToggleMode, Some("SmartZip")),
        KeyBinding::new("f1", ShowHelp, Some("SmartZip")),
    ]);
    cx.set_menus([
        Menu::new("SmartZip").items([
            MenuItem::action("偏好设置…", ShowSettings),
            MenuItem::action("外观与主题", ShowAppearance),
            MenuItem::separator(),
            MenuItem::action("退出 SmartZip", Quit),
        ]),
        Menu::new("文件").items([
            MenuItem::action("打开归档…", OpenArchive),
            MenuItem::action("添加解压…", AddExtract),
        ]),
        Menu::new("视图").items([
            MenuItem::action("任务中心", ShowTasks),
            MenuItem::action("历史记录", ShowHistory),
            MenuItem::action("搜索", Search),
            MenuItem::action("显示 / 隐藏详情", ToggleDetails),
            MenuItem::action("切换快速 / 完整模式", ToggleMode),
        ]),
        Menu::new("帮助").items([MenuItem::action("关于与快捷键", ShowHelp)]),
    ]);
}

pub(super) fn request_exit(shared: Entity<Workspace>, window: &mut Window, cx: &mut App) {
    let state = shared.read(cx);
    if state.quitting || state.exit_prompt_open {
        return;
    }
    let dirty = state
        .settings_form
        .as_ref()
        .is_some_and(|form| !form.read(cx).patches(cx).is_empty());
    let save_failed = state.queue_store.error.is_some();
    let work = state.queue.has_work() || state.previews.has_work();
    if !dirty && !work && !save_failed {
        shared.update(cx, |s, cx| {
            s.quitting = true;
            cx.notify();
        });
        return;
    }
    let can_save_queue = !save_failed
        && !dirty
        && !state.queue.has_active()
        && !state.previews.has_active()
        && state
            .queue
            .jobs
            .iter()
            .filter(|j| j.phase == Phase::Queued)
            .all(|j| {
                j.request.operation == TaskOperation::Extract
                    && j.request.resolved.as_ref().is_some_and(|c| {
                        crate::queue_store::database_path(c)
                            .ok()
                            .flatten()
                            .is_some()
                    })
            });
    shared.update(cx, |s, _| s.exit_prompt_open = true);
    let detail = if save_failed {
        "队列保存失败。退出会取消当前任务并等待清理，但未保存的变更可能丢失，下次启动可能再次显示旧队列。可以继续使用并重试保存。"
    } else {
        match (dirty, work) {
        (true, true) => "偏好设置有未保存的修改，仍有任务正在执行或等待。退出会放弃设置修改、取消任务，并等待后端退出与临时文件清理。",
        (true, false) => "偏好设置有未保存的修改。可以返回设置保存，或放弃修改并退出。",
        _ => "仍有任务正在执行或等待。退出会取消当前任务与等待队列，并等待后端退出与临时文件清理。",
    }
    };
    let mut choices = vec![
        "继续使用",
        if save_failed {
            "接受队列保存失败并退出"
        } else if work {
            "取消任务并退出"
        } else {
            "放弃修改并退出"
        },
    ];
    if can_save_queue && work {
        choices.push("保存等待队列并退出");
    }
    let answer = confirm(
        window,
        PromptLevel::Warning,
        "退出 SmartZip？",
        Some(detail),
        &choices,
        cx,
    );
    cx.spawn(async move |cx| {
        let choice = answer.await.ok();
        shared.update(cx, |state, cx| {
            state.exit_prompt_open = false;
            if choice == Some(2) {
                state.queue.hold_all();
                state.previews.cancel_all();
                state.quitting = true;
            } else if choice == Some(1) {
                state.exit_allow_queue_loss = save_failed;
                state.queue.cancel_all();
                state.previews.cancel_all();
                state.quitting = true;
            }
            cx.notify();
        });
    })
    .detach();
}

impl Workspace {
    pub(super) fn persist_preferences(&mut self) {
        self.preferences.normalize();
        self.preferences_revision += 1;
        if self
            .preferences_writer
            .send((self.preferences_revision, self.preferences.clone()))
            .is_err()
        {
            self.preferences_saved = self.preferences_revision;
            self.message = "无法保存桌面偏好，当前会话仍可继续使用".into();
        }
    }
}

impl View {
    pub(super) fn change_appearance(
        &mut self,
        edit: impl FnOnce(&mut crate::preferences::Preferences),
        cx: &mut Context<Self>,
    ) {
        let preferences = self.shared.update(cx, |state, cx| {
            edit(&mut state.preferences);
            state.persist_preferences();
            cx.notify();
            state.preferences.clone()
        });
        crate::appearance::apply(&preferences, None, cx);
    }

    pub(super) fn appearance(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        use gpui::component::radio::RadioGroup;

        let p = self.shared.read(cx).preferences.clone();
        let row = |label: &'static str, editor: gpui::AnyElement| {
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap_4()
                .py_4()
                .border_b_1()
                .border_color(cx.theme().border)
                .child(div().w(px(160.)).text_base().child(label))
                .child(div().flex_1().min_w(px(260.)).child(editor))
        };
        div()
            .flex()
            .flex_col()
            .max_w(px(820.))
            .child(row(
                "主题",
                RadioGroup::horizontal("theme-mode")
                    .selected_index(ColorMode::ALL.iter().position(|mode| *mode == p.color_mode))
                    .children(ColorMode::ALL.into_iter().map(|mode| mode.label()))
                    .on_change(cx.listener(|this, index: &usize, _, cx| {
                        this.change_appearance(|p| p.color_mode = ColorMode::ALL[*index], cx);
                    }))
                    .into_any_element(),
            ))
            .child(row(
                "列表密度",
                RadioGroup::horizontal("density")
                    .selected_index(Some(usize::from(p.density == Density::Compact)))
                    .children(["标准", "紧凑"])
                    .on_change(cx.listener(|this, index: &usize, _, cx| {
                        this.change_appearance(
                            |p| {
                                p.density = if *index == 0 {
                                    Density::Comfortable
                                } else {
                                    Density::Compact
                                }
                            },
                            cx,
                        );
                    }))
                    .into_any_element(),
            ))
            .child(row(
                "界面字号",
                RadioGroup::horizontal("font-size")
                    .selected_index(
                        [12., 14., 16., 18.]
                            .iter()
                            .position(|value| *value == p.font_size),
                    )
                    .children(["12 px", "14 px", "16 px", "18 px"])
                    .on_change(cx.listener(|this, index: &usize, _, cx| {
                        this.change_appearance(|p| p.font_size = [12., 14., 16., 18.][*index], cx);
                    }))
                    .into_any_element(),
            ))
            .child(row(
                "减少动态效果",
                Switch::new("reduced-motion")
                    .accessibility_label("减少动态效果")
                    .checked(p.reduced_motion)
                    .on_change(cx.listener(|this, value: &bool, _, cx| {
                        this.change_appearance(|p| p.reduced_motion = *value, cx)
                    }))
                    .into_any_element(),
            ))
            .child(row(
                "系统通知",
                Switch::new("desktop-notifications")
                    .accessibility_label("系统通知")
                    .checked(p.notifications)
                    .on_change(cx.listener(|this, value: &bool, _, cx| {
                        this.change_appearance(|p| p.notifications = *value, cx)
                    }))
                    .into_any_element(),
            ))
    }

    pub(super) fn help(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let modifier = if cfg!(target_os = "macos") {
            "⌘"
        } else {
            "Ctrl"
        };
        div().max_w(px(820.)).flex().flex_col().gap_5()
            .child(div().text_size(px(28.)).font_weight(gpui::FontWeight::SEMIBOLD).child("SmartZip"))
            .child(format!("版本 {} · 本地智能解压与归档浏览", env!("CARGO_PKG_VERSION")))
            .child("将文件拖入任务中心立即解压；拖入归档预览只查看内容。需要先选择输出位置或输入临时密码时，使用「添加并配置」。")
            .child("快速模式在当前窗口内切换，返回完整模式会保留原页面和输入。完整性校验和内嵌检测位于归档预览的「归档操作」菜单。")
            .child("暂停自动启动只影响新任务。归档暂停在阶段边界生效；取消会等待后端退出和清理。密码库供后续任务使用，临时密码仅用于当前任务。")
            .children([("O", "打开归档预览"), ("Shift+O", "添加解压"), (",", "偏好设置"), ("F", "搜索当前页面"), ("1", "任务中心"), ("2", "历史记录"), ("Shift+D", "显示 / 隐藏详情"), ("Shift+M", "切换快速 / 完整模式"), ("Q", "安全退出")].into_iter().map(|(key, label)| div().flex().gap_4().py_2().border_b_1().border_color(cx.theme().border).child(div().w(px(150.)).child(format!("{modifier}+{key}"))).child(label)))
            .child(div().text_base().text_color(cx.theme().muted_foreground).child("内容预览支持 UTF-8 文本、PNG 和 JPEG，并设有大小与时间限制。其他内容可解压后查看。"))
    }
}
