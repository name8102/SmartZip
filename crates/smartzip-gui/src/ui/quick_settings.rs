//! Shared extraction options for new and selected tasks.
use super::*;

impl View {
    pub(super) fn quick_controls(
        &mut self,
        target: Option<u64>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let state = self.shared.read(cx);
        let job = target.and_then(|id| state.queue.jobs.iter().find(|job| job.id == id));
        if target.is_some() && job.is_none() {
            return div().into_any_element();
        }
        if job.is_some_and(|job| job.request.operation != TaskOperation::Extract) {
            return div()
                .text_sm()
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
        div()
            .id(("quick-config", target.unwrap_or(0)))
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div().flex().gap_2().flex_col().children(
                    [
                        (0u8, "递归解压", recursive),
                        (1, "智能整理", smart),
                        (2, "完成后移入回收站", delete),
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
                                    .label(label)
                                    .checked(checked)
                                    .disabled(!editable)
                                    .on_change(cx.listener(move |this, checked: &bool, _, cx| {
                                        this.shared.update(cx, |state, cx| {
                                            state.edit_settings(target, |settings| match index {
                                                0 => settings.recursive = Some(*checked),
                                                1 => settings.smart_layout = Some(*checked),
                                                2 => settings.delete_source = *checked,
                                                _ => settings.auto_encoding = Some(*checked),
                                            });
                                            cx.notify();
                                        });
                                    })),
                            )
                            .when(overrides[index as usize], |el| {
                                el.child(
                                    div()
                                        .mt_1()
                                        .text_sm()
                                        .text_color(cx.theme().muted_foreground)
                                        .child("单独设置"),
                                )
                            })
                    }),
                ),
            )
            .when(delete, |el| {
                el.child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child("仅在全部成功后回收源文件"),
                )
            })
            .child(div().text_sm().child(format!(
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
