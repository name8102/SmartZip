use super::*;

impl View {
    pub(super) fn settings(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let state = self.shared.read(cx);
        let revision = state.config_revision;
        let snapshot = state.config.clone();
        let busy = state.library_busy;
        if self.form_revision != revision || self.config_form.is_none() {
            self.config_form = snapshot.as_ref().map(|snapshot| {
                cx.new(|cx| crate::config_form::ConfigForm::new(snapshot, window, cx))
            });
            self.form_revision = revision;
            self.shared.update(cx, |state, _| {
                state.settings_form = self.config_form.clone();
                state.settings_form_revision = revision;
            });
        }
        if let Some(form) = &self.config_form {
            form.update(cx, |form, _| form.saving = busy);
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
                            .on_click(cx.listener(|this, _, window, cx| {
                                let answer = confirm(
                                    window,
                                    gpui::PromptLevel::Warning,
                                    "放弃未保存的修改？",
                                    Some("重新加载磁盘上的配置，当前表单修改将丢失。"),
                                    &["继续编辑", "放弃并重新加载"],
                                    cx,
                                );
                                let shared = this.shared.clone();
                                cx.spawn(async move |_, cx| {
                                    if answer.await == Ok(1) {
                                        shared.update(cx, |state, cx| {
                                            state.background(|| {
                                                LibraryMessage::Config(
                                                        library::load_config(
                                                            &LibraryOptions::default(),
                                                        )
                                                        .map(Box::new),
                                                    )
                                            });
                                            cx.notify();
                                        });
                                    }
                                })
                                .detach();
                            })),
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
                    .child("保存后用于新任务；已排队任务保留原配置。快速模式的显式覆盖优先。"),
            )
            .children(self.config_form.clone())
    }
}
