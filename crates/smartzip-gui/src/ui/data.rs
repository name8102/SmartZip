use super::*;

impl Workspace {
    pub(super) fn reload_history(&mut self) {
        self.history_version += 1;
        self.history_loading = true;
        let version = self.history_version;
        let query = self.history_query.clone();
        let tx = self.sender.clone();
        std::thread::spawn(move || {
            let _ = tx.send(LibraryMessage::History(
                version,
                library::history_page(&LibraryOptions::default(), &query),
            ));
        });
    }
    pub(super) fn reload_passwords(&mut self) {
        self.password_version += 1;
        self.password_loading = true;
        let version = self.password_version;
        let query = self.password_query.clone();
        let tx = self.sender.clone();
        std::thread::spawn(move || {
            let _ = tx.send(LibraryMessage::Passwords(
                version,
                library::password_page(&LibraryOptions::default(), &query),
            ));
        });
    }
    pub(super) fn load_detail(&mut self, id: String) {
        self.history_detail_request = Some(id.clone());
        self.history_detail = None;
        let tx = self.sender.clone();
        std::thread::spawn(move || {
            let _ = tx.send(LibraryMessage::TaskDetail(
                id.clone(),
                library::load_task_detail(&LibraryOptions::default(), &id),
            ));
        });
    }
}

pub(super) fn empty_state(
    title: impl Into<SharedString>,
    detail: impl Into<SharedString>,
    cx: &App,
) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap_3()
        .py_8()
        .px_5()
        .min_h(px(160.))
        .child(
            Icon::new(IconName::FolderSearch)
                .size(px(30.))
                .text_color(cx.theme().muted_foreground),
        )
        .child(div().text_base().child(title.into()))
        .child(
            div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(detail.into()),
        )
}

pub(super) fn size_label(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024. && unit < UNITS.len() - 1 {
        value /= 1024.;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// The component dialog supplies wrapping text, theme tokens and focus restoration on every platform.
/// Enter/Escape never accepts a destructive choice by accident.
pub(super) fn confirm(
    window: &mut Window,
    _level: gpui::PromptLevel,
    title: &str,
    detail: Option<&str>,
    choices: &[&str],
    cx: &mut App,
) -> tokio::sync::oneshot::Receiver<usize> {
    use gpui::component::WindowExt;
    let (tx, rx) = tokio::sync::oneshot::channel();
    let sender = std::rc::Rc::new(std::cell::RefCell::new(Some(tx)));
    let title = title.to_owned();
    let detail = detail.unwrap_or_default().to_owned();
    let choices: Vec<String> = choices.iter().map(|s| (*s).to_owned()).collect();
    window.open_dialog(cx, move |dialog, window, _cx| {
        let dismiss = sender.clone();
        dialog
            .title(title.clone())
            .width(px(f32::from(window.viewport_size().width).min(560.) - 40.))
            .overlay_closable(false)
            .child(
                div()
                    .w_full()
                    .text_sm()
                    .whitespace_normal()
                    .child(detail.clone()),
            )
            .on_ok(|_, _, _| false)
            .on_close(move |_, _, _| {
                if let Some(tx) = dismiss.borrow_mut().take() {
                    let _ = tx.send(0);
                }
            })
            .footer(div().flex().flex_wrap().gap_2().justify_end().children(
                choices.iter().enumerate().map(|(i, label)| {
                    let sender = sender.clone();
                    Button::new(("confirm-choice", i))
                        .small()
                        .label(label.clone())
                        .when(i == 0, |b| b.primary())
                        .on_click(move |_, window, cx| {
                            if let Some(tx) = sender.borrow_mut().take() {
                                let _ = tx.send(i);
                            }
                            window.close_dialog(cx);
                        })
                }),
            ))
    });
    rx
}
