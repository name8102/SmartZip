//! Window composition and action routing.
use super::*;

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
        let header =
            div()
                .flex()
                .flex_shrink_0()
                .items_center()
                .justify_between()
                .h(px(56.))
                .px_5()
                .border_b_1()
                .border_color(cx.theme().border)
                .child(
                    div()
                        .text_size(px(20.))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(if quick {
                            "快速解压"
                        } else {
                            self.page.title()
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
            self.quick_page(cx).into_any_element()
        } else {
            match self.page {
                Page::Tasks => self.tasks_page(window, cx).into_any_element(),
                Page::Preview => div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .child(self.prompt(cx))
                    .child(self.preview(window, cx))
                    .into_any_element(),
                _ => {
                    let tabs = self.section_tabs(cx);
                    let content = match self.page {
                        Page::History => self.history(window, cx).into_any_element(),
                        Page::Passwords => self.passwords(cx).into_any_element(),
                        Page::Settings => self.settings(window, cx).into_any_element(),
                        Page::Integration => self.integration.clone().into_any_element(),
                        Page::Appearance => self.appearance(cx).into_any_element(),
                        Page::Help => self.help(cx).into_any_element(),
                        Page::Recovery => self.recovery(cx).into_any_element(),
                        _ => unreachable!(),
                    };
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_h_0()
                        .min_w_0()
                        .child(div().px_5().child(tabs))
                        .child(
                            div()
                                .id("page-scroll")
                                .flex_1()
                                .min_h_0()
                                .overflow_y_scroll()
                                .p_5()
                                .child(
                                    div()
                                        .w_full()
                                        .max_w(px(1120.))
                                        .mx_auto()
                                        .child(self.prompt(cx))
                                        .child(content),
                                ),
                        )
                        .into_any_element()
                }
            }
        };
        let footer = div()
            .flex_shrink_0()
            .flex()
            .flex_col()
            .when(!message.is_empty(), |el| {
                el.child(
                    div()
                        .flex()
                        .items_start()
                        .gap_2()
                        .px_4()
                        .py_2()
                        .bg(cx.theme().secondary)
                        .child(
                            div()
                                .id("workspace-message")
                                .flex_1()
                                .min_w_0()
                                .max_h(px(100.))
                                .overflow_y_scroll()
                                .text_base()
                                .child(message),
                        )
                        .child(
                            control("dismiss-message")
                                .label("关闭")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.shared.update(cx, |state, cx| {
                                        state.message.clear();
                                        cx.notify();
                                    });
                                })),
                        ),
                )
            })
            .when(self.shared.read(cx).queue_store.error.is_some(), |el| {
                el.child(
                    control("retry-queue-save")
                        .label("重试保存等待队列")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.shared.update(cx, |state, cx| {
                                state.queue_store.retry();
                                cx.notify();
                            })
                        })),
                )
            })
            .when(busy || self.shared.read(cx).quitting, |el| {
                el.child(
                    div()
                        .px_4()
                        .py_2()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(if self.shared.read(cx).quitting {
                            "正在退出，等待任务清理…"
                        } else {
                            "正在处理…"
                        }),
                )
            });
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
            .font_family(cx.theme().font_family.clone())
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
                    if let Some(form) = this.shared.read(cx).settings_form.clone() {
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
            .when(!quick, |el| {
                el.child(self.sidebar(window.viewport_size().width < px(860.), window, cx))
            })
            .child(main)
    }
}
