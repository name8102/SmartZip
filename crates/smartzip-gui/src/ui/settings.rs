use super::*;
use gpui::component::menu::{DropdownMenu, PopupMenuItem};

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
        let shared = self.shared.clone();
        div()
            .flex()
            .flex_col()
            .gap_5()
            .max_w(px(960.))
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(path),
            )
            .child(
                div()
                    .flex()
                    .gap_2()
                    .items_center()
                    .flex_wrap()
                    .child(
                        control("save-config")
                            .primary()
                            .label("保存修改")
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
                            .ghost()
                            .label("重新加载")
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
                        control("config-actions")
                            .ghost()
                            .label("更多")
                            .icon(IconName::ChevronDown)
                            .disabled(busy)
                            .dropdown_menu(move |menu, _, _| {
                                let initialize = shared.clone();
                                let migrate = shared.clone();
                                menu.item(PopupMenuItem::new("初始化配置").on_click(
                                    move |_, _, cx| {
                                        initialize.update(cx, |state, cx| {
                                            state.background(|| {
                                                LibraryMessage::ConfigSaved(
                                                    library::init_config(
                                                        &LibraryOptions::default(),
                                                        false,
                                                    )
                                                    .map(Box::new),
                                                )
                                            });
                                            cx.notify();
                                        });
                                    },
                                ))
                                .item(
                                    PopupMenuItem::new("迁移预览").on_click(move |_, _, cx| {
                                        migrate.update(cx, |state, cx| {
                                            state.background(|| {
                                                LibraryMessage::Document(library::migrate_config(
                                                    &LibraryOptions::default(),
                                                    false,
                                                ))
                                            });
                                            cx.notify();
                                        });
                                    }),
                                )
                            }),
                    ),
            )
            .children(self.config_form.clone())
    }
}
