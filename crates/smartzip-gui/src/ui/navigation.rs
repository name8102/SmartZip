//! Primary destinations and contextual navigation; secondary settings stay together.
use super::*;
use gpui::component::{sidebar::SidebarItem, Collapsible};

impl Page {
    pub(super) fn title(self) -> &'static str {
        match self {
            Self::Tasks => "任务中心",
            Self::Preview => "归档浏览",
            Self::History | Self::Recovery => "历史记录",
            Self::Passwords => "密码库",
            Self::Settings | Self::Appearance | Self::Integration => "设置",
            Self::Help => "关于 SmartZip",
        }
    }

    fn settings_group(self) -> bool {
        matches!(self, Self::Settings | Self::Appearance | Self::Integration)
    }
}

impl View {
    pub(super) fn sidebar(
        &mut self,
        narrow: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let collapsed = self.shared.read(cx).preferences.sidebar_collapsed || narrow;
        let queue = &self.shared.read(cx).queue;
        let active = queue.jobs.iter().filter(|job| job.phase.active()).count();
        Sidebar::new("main-sidebar")
            .w(px(184.))
            .collapsed(collapsed)
            .header(
                SidebarHeader::new()
                    .p_0()
                    .when(!collapsed, |header| header.px_2())
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .h(px(48.))
                            .w_full()
                            .when(collapsed, |el| el.justify_center())
                            .text_color(cx.theme().primary)
                            .child(Icon::new(IconName::PackageOpen).size(px(24.)))
                            .when(!collapsed, |el| {
                                el.child(
                                    div()
                                        .text_size(px(20.))
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .text_color(cx.theme().foreground)
                                        .child("SmartZip"),
                                )
                            }),
                    ),
            )
            .child(
                SidebarMenu::new().children(
                    [
                        (Page::Tasks, "任务中心", IconName::Layers),
                        (Page::Preview, "归档浏览", IconName::FolderSearch),
                        (Page::History, "历史记录", IconName::Clock),
                        (Page::Passwords, "密码库", IconName::KeyRound),
                    ]
                    .into_iter()
                    .map(|(page, label, icon)| {
                        SidebarMenuItem::new(label)
                            .icon(Icon::new(icon).size(px(20.)))
                            .text_base()
                            .min_h(px(40.))
                            .when(collapsed, |item| item.px_0())
                            .active(
                                self.page == page
                                    || (page == Page::History && self.page == Page::Recovery),
                            )
                            .on_click(cx.listener(move |this, _, _, cx| this.page(page, cx)))
                    }),
                ),
            )
            .footer(
                div()
                    .w_full()
                    .flex()
                    .flex_col()
                    .items_stretch()
                    .when(active > 0 && !collapsed, |el| {
                        el.child(
                            div()
                                .px_3()
                                .py_2()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child(format!("{active} 项进行中")),
                        )
                    })
                    .child(
                        SidebarMenu::new()
                            .child(
                                SidebarMenuItem::new("设置")
                                    .icon(Icon::new(IconName::SlidersHorizontal).size(px(20.)))
                                    .text_base()
                                    .min_h(px(40.))
                                    .when(collapsed, |item| item.px_0())
                                    .active(self.page.settings_group())
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.page(Page::Settings, cx)),
                                    ),
                            )
                            .child(
                                SidebarMenuItem::new("关于与帮助")
                                    .icon(Icon::new(IconName::ShieldCheck).size(px(20.)))
                                    .text_base()
                                    .min_h(px(40.))
                                    .when(collapsed, |item| item.px_0())
                                    .active(self.page == Page::Help)
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.page(Page::Help, cx)),
                                    ),
                            )
                            .collapsed(collapsed)
                            .render("nav-footer", window, cx),
                    )
                    .when(!narrow, |el| {
                        el.child(
                            div()
                                .flex()
                                .w_full()
                                .when(collapsed, |el| el.justify_center())
                                .when(!collapsed, |el| el.justify_end())
                                .child(SidebarToggleButton::new().collapsed(collapsed).on_click(
                                    cx.listener(|this, _, _, cx| {
                                        this.shared.update(cx, |state, cx| {
                                            state.preferences.sidebar_collapsed =
                                                !state.preferences.sidebar_collapsed;
                                            state.persist_preferences();
                                            cx.notify();
                                        });
                                    }),
                                )),
                        )
                    }),
            )
    }

    pub(super) fn section_tabs(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let pages: Vec<(Page, &str)> = if self.page.settings_group() {
            vec![
                (Page::Settings, "解压与数据"),
                (Page::Appearance, "外观"),
                (Page::Integration, "系统集成"),
            ]
        } else if matches!(self.page, Page::History | Page::Recovery) {
            vec![(Page::History, "历史"), (Page::Recovery, "待恢复")]
        } else {
            return div().into_any_element();
        };
        let selected = pages
            .iter()
            .position(|(page, _)| *page == self.page)
            .unwrap_or(0);
        TabBar::new("section-tabs")
            .underline()
            .selected_index(selected)
            .children(pages.iter().map(|(_, label)| Tab::new().label(*label)))
            .on_click(cx.listener(move |this, index: &usize, _, cx| this.page(pages[*index].0, cx)))
            .into_any_element()
    }
}
