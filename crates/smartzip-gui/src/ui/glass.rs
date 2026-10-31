//! Layout variant for the opt-in material prototype; all actions use the
//! existing workspace and routing, rather than a second application model.
use super::*;
use gpui::base::Selectable;

impl View {
    pub(super) fn glass_sidebar(
        &mut self,
        narrow: bool,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let collapsed = self.shared.read(cx).preferences.sidebar_collapsed || narrow;
        let glass = self.glass.as_ref().unwrap().clone();
        let active = self
            .shared
            .read(cx)
            .queue
            .jobs
            .iter()
            .filter(|job| job.phase.active())
            .count();
        glass
            .surface("sidebar", 28., 0.24, cx)
            .id("glass-sidebar")
            .flex_shrink_0()
            .w(px(if collapsed { 64. } else { 192. }))
            .h_full()
            .flex()
            .flex_col()
            .p_3()
            .gap_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .h(px(64.))
                    .px_2()
                    .when(collapsed, |el| el.justify_center().px_0())
                    .child(
                        Icon::new(IconName::PackageOpen)
                            .size(px(28.))
                            .text_color(cx.theme().primary),
                    )
                    .when(!collapsed, |el| {
                        el.child(
                            div().flex().flex_col().gap_1().child(
                                div()
                                    .text_size(px(20.))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child("SmartZip"),
                            ),
                        )
                    }),
            )
            .when(!collapsed, |el| {
                el.child(
                    div()
                        .px_3()
                        .pt_3()
                        .pb_2()
                        .text_size(px(12.))
                        .text_color(cx.theme().muted_foreground)
                        .child("工作区"),
                )
            })
            .children(
                [
                    (Page::Tasks, "任务中心", IconName::Layers),
                    (Page::Preview, "归档浏览", IconName::FolderSearch),
                    (Page::History, "历史记录", IconName::Clock),
                    (Page::Passwords, "密码库", IconName::KeyRound),
                ]
                .into_iter()
                .enumerate()
                .map(|(i, (page, label, icon))| {
                    let selected =
                        self.page == page || (page == Page::History && self.page == Page::Recovery);
                    Button::new(("glass-nav", i))
                        .ghost()
                        .large()
                        .w_full()
                        .h(px(44.))
                        .rounded(px(16.))
                        .icon(icon)
                        .selected(selected)
                        .when(selected, |button| button.text_color(cx.theme().primary))
                        .when(!collapsed, |button| {
                            button.label(label).justify_start().px_3()
                        })
                        .when(collapsed, |button| button.tooltip(label))
                        .on_click(cx.listener(move |this, _, _, cx| this.page(page, cx)))
                }),
            )
            .child(div().flex_1())
            .when(!collapsed, |el| {
                el.child(
                    div()
                        .px_3()
                        .pb_4()
                        .text_size(px(12.))
                        .text_color(cx.theme().muted_foreground)
                        .child(if active == 0 {
                            "就绪".to_owned()
                        } else {
                            format!("{active} 项进行中")
                        }),
                )
            })
            .children(
                [
                    (Page::Settings, "设置", IconName::SlidersHorizontal),
                    (Page::Help, "关于与帮助", IconName::ShieldCheck),
                ]
                .into_iter()
                .enumerate()
                .map(|(i, (page, label, icon))| {
                    let selected = self.page == page
                        || (page == Page::Settings
                            && matches!(self.page, Page::Appearance | Page::Integration));
                    Button::new(("glass-secondary-nav", i))
                        .ghost()
                        .large()
                        .w_full()
                        .h(px(44.))
                        .rounded(px(16.))
                        .icon(icon)
                        .selected(selected)
                        .when(selected, |button| button.text_color(cx.theme().primary))
                        .when(!collapsed, |button| {
                            button.label(label).justify_start().px_3()
                        })
                        .when(collapsed, |button| button.tooltip(label))
                        .on_click(cx.listener(move |this, _, _, cx| this.page(page, cx)))
                }),
            )
            .when(!narrow, |el| {
                el.child(
                    Button::new("glass-collapse")
                        .ghost()
                        .large()
                        .icon(IconName::PanelLeftOpen)
                        .tooltip(if collapsed {
                            "展开侧栏"
                        } else {
                            "收起侧栏"
                        })
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.shared.update(cx, |state, cx| {
                                state.preferences.sidebar_collapsed =
                                    !state.preferences.sidebar_collapsed;
                                state.persist_preferences();
                                cx.notify();
                            });
                        })),
                )
            })
            .into_any_element()
    }

    pub(super) fn glass_drop_area(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        div()
            .id("archive-drop")
            .flex_1()
            .min_h(px(280.))
            .w_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .px_5()
            .py_6()
            .gap_4()
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| {
                this.shared.update(cx, |state, cx| {
                    state.enqueue(paths.paths().to_vec(), TaskOperation::Extract);
                    cx.notify();
                });
            }))
            .child(
                Icon::new(IconName::PackageOpen)
                    .size(px(48.))
                    .text_color(cx.theme().primary),
            )
            .child(
                div()
                    .text_size(px(22.))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child("拖入归档，开始解压"),
            )
            .child(
                div()
                    .text_size(px(14.))
                    .text_color(cx.theme().muted_foreground)
                    .child("支持批量添加与嵌套归档"),
            )
            .child(
                Button::new("choose")
                    .large()
                    .primary()
                    .h(px(40.))
                    .icon(IconName::Plus)
                    .label("选择归档")
                    .px_6()
                    .rounded(px(20.))
                    .on_click(cx.listener(|this, _, _, cx| this.pick(TaskOperation::Extract, cx))),
            )
            .child(
                div()
                    .text_size(px(11.))
                    .text_color(cx.theme().muted_foreground)
                    .child("ZIP  ·  7z  ·  RAR  ·  TAR  ·  更多格式"),
            )
            .into_any_element()
    }

    pub(super) fn glass_start_page(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let glass = self.glass.as_ref().unwrap().clone();
        let wide = window.viewport_size().width >= px(1060.);
        div()
            .id("glass-start-scroll")
            .flex_1()
            .min_h_0()
            .min_w_0()
            .overflow_y_scroll()
            .child(
                div()
                    .w_full()
                    .max_w(px(1200.))
                    .flex()
                    .flex_col()
                    .gap(px(20.))
                    .child(
                        div()
                            .flex()
                            .items_stretch()
                            .gap(px(20.))
                            .when(!wide, |el| el.flex_col())
                            .child(
                                glass
                                    .surface("drop-zone", 28., 0.36, cx)
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .flex_col()
                                    .p(px(12.))
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_h(px(344.))
                                            .flex()
                                            .flex_col()
                                            .rounded(px(18.))
                                            .border_1()
                                            .border_dashed()
                                            .border_color(cx.theme().primary.opacity(0.28))
                                            .child(self.glass_drop_area(cx)),
                                    ),
                            )
                            .child(
                                glass
                                    .surface("start-options", 28., 0.82, cx)
                                    .flex_shrink_0()
                                    .p(px(22.))
                                    .flex()
                                    .flex_col()
                                    .gap_5()
                                    .when(wide, |el| el.w(px(300.)))
                                    .when(!wide, |el| el.w_full())
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap_2()
                                            .child(
                                                Icon::new(IconName::SlidersHorizontal)
                                                    .size(px(18.))
                                                    .text_color(cx.theme().primary),
                                            )
                                            .child(
                                                div()
                                                    .text_size(px(16.))
                                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                                    .child("解压设置"),
                                            ),
                                    )
                                    .child(self.quick_controls(None, cx)),
                            ),
                    )
                    .child(
                        glass
                            .surface("empty-queue", 26., 0.84, cx)
                            .flex()
                            .flex_col()
                            .px(px(24.))
                            .pt(px(20.))
                            .pb(px(24.))
                            .gap_5()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap_3()
                                            .child(
                                                div()
                                                    .text_size(px(16.))
                                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                                    .child("任务队列"),
                                            )
                                            .child(
                                                div()
                                                    .text_size(px(12.))
                                                    .text_color(cx.theme().muted_foreground)
                                                    .child("0 项"),
                                            ),
                                    )
                                    .child(
                                        Button::new("start-browse")
                                            .ghost()
                                            .icon(IconName::FolderSearch)
                                            .label("浏览归档内容")
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.page(Page::Preview, cx);
                                                this.pick(TaskOperation::List, cx);
                                            })),
                                    ),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .px_1()
                                    .pb_3()
                                    .border_b_1()
                                    .border_color(cx.theme().border.opacity(0.65))
                                    .text_size(px(12.))
                                    .text_color(cx.theme().muted_foreground)
                                    .child(div().flex_1().child("文件"))
                                    .child(div().w(px(110.)).child("状态"))
                                    .child(div().w(px(110.)).child("大小")),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .gap_3()
                                    .py_5()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(Icon::new(IconName::Layers).size(px(20.)))
                                    .child(
                                        div()
                                            .text_size(px(13.))
                                            .child("添加归档后，在这里查看进度与结果"),
                                    ),
                            ),
                    ),
            )
            .into_any_element()
    }
}
