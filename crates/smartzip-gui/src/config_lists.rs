//! Editors for the two ordered configuration lists, without exposing TOML syntax.
use gpui::base::Disableable;
use gpui::component::{
    button::{Button, ButtonVariants},
    input::{Input, InputState},
    menu::{DropdownMenu, PopupMenuItem},
    radio::RadioGroup,
    switch::Switch,
    ActiveTheme, Sizable,
};
use gpui::{div, prelude::*, App, AppContext, Context, Entity, IntoElement, Render, Window};
use smartzip_config::{AdapterFamily, BackendInstallation, PasswordSource};
struct Installation {
    identity: usize,
    _subscriptions: Vec<gpui::Subscription>,
    id: Entity<InputState>,
    path: Entity<InputState>,
    version: Entity<InputState>,
    priority: Entity<InputState>,
    family: AdapterFamily,
    enabled: bool,
}
impl Installation {
    fn new(
        identity: usize,
        b: BackendInstallation,
        window: &mut Window,
        cx: &mut Context<ConfigLists>,
    ) -> Self {
        let mut row = Self {
            identity,
            _subscriptions: vec![],
            id: cx.new(|cx| {
                InputState::new(window, cx)
                    .default_value(b.id)
                    .placeholder("后端名称")
            }),
            path: cx.new(|cx| {
                InputState::new(window, cx)
                    .default_value(b.executable.to_string_lossy().to_string())
                    .placeholder("可执行文件的完整路径")
            }),
            version: cx.new(|cx| {
                InputState::new(window, cx)
                    .default_value(b.declared_version.unwrap_or_default())
                    .placeholder("声明版本（可留空）")
            }),
            priority: cx
                .new(|cx| InputState::new(window, cx).default_value(b.priority.to_string())),
            family: b.family,
            enabled: b.enabled,
        };
        row._subscriptions = vec![
            cx.observe(&row.id, |_, _, cx| cx.notify()),
            cx.observe(&row.path, |_, _, cx| cx.notify()),
            cx.observe(&row.version, |_, _, cx| cx.notify()),
            cx.observe(&row.priority, |_, _, cx| cx.notify()),
        ];
        row
    }
}
pub struct ConfigLists {
    installations: Vec<Installation>,
    sources: Vec<PasswordSource>,
    next: usize,
    pub saving: bool,
    show_backends: bool,
    diagnostics: Option<Result<Vec<serde_json::Value>, String>>,
    checking: bool,
}
fn source_label(s: PasswordSource) -> &'static str {
    match s {
        PasswordSource::Manual => "手动输入",
        PasswordSource::Known => "已知归档",
        PasswordSource::Batch => "本批已用",
        PasswordSource::Empty => "空密码",
        PasswordSource::Database => "密码库",
    }
}
impl ConfigLists {
    pub fn new(
        config: &smartzip_config::SmartZipConfig,
        backends: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let installations = if backends {
            config
                .backends
                .installations
                .iter()
                .cloned()
                .enumerate()
                .map(|(i, b)| Installation::new(i, b, window, cx))
                .collect()
        } else {
            Vec::new()
        };
        let next = installations.len();
        Self {
            installations,
            sources: if backends {
                Vec::new()
            } else {
                config.passwords.sources.clone()
            },
            next,
            saving: false,
            show_backends: backends,
            diagnostics: None,
            checking: false,
        }
    }
    pub fn value(&self, cx: &App) -> String {
        if !self.show_backends {
            return toml::Value::try_from(&self.sources).unwrap().to_string();
        }
        let rows: Vec<_> = self
            .installations
            .iter()
            .map(|b| {
                let mut table = toml::map::Map::new();
                table.insert("id".into(), b.id.read(cx).value().to_string().into());
                table.insert("family".into(), toml::Value::try_from(&b.family).unwrap());
                table.insert(
                    "executable".into(),
                    b.path.read(cx).value().to_string().into(),
                );
                table.insert("enabled".into(), b.enabled.into());
                let p = b.priority.read(cx).value();
                table.insert(
                    "priority".into(),
                    p.parse::<i64>()
                        .map(toml::Value::Integer)
                        .unwrap_or_else(|_| p.to_string().into()),
                );
                let version = b.version.read(cx).value();
                if !version.trim().is_empty() {
                    table.insert("declared_version".into(), version.to_string().into());
                }
                toml::Value::Table(table)
            })
            .collect();
        toml::Value::Array(rows).to_string()
    }
}
impl Render for ConfigLists {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.show_backends {
            return div()
                .flex()
                .flex_col()
                .gap_2()
                .children(self.sources.iter().copied().enumerate().map(|(i, s)| {
                    div()
                        .flex()
                        .gap_2()
                        .items_center()
                        .child(
                            div()
                                .flex_1()
                                .child(format!("{}. {}", i + 1, source_label(s))),
                        )
                        .child(
                            Button::new(("source-up", i))
                                .ghost()
                                .large()
                                .label("上移")
                                .disabled(self.saving || i == 0)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.sources.swap(i, i - 1);
                                    cx.notify();
                                })),
                        )
                        .child(
                            Button::new(("source-remove", i))
                                .ghost()
                                .large()
                                .label("移除")
                                .disabled(self.saving)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.sources.remove(i);
                                    cx.notify();
                                })),
                        )
                }))
                .child({
                    let entity = cx.entity();
                    let sources = self.sources.clone();
                    Button::new("source-add")
                        .large()
                        .label("添加来源")
                        .disabled(self.saving || sources.len() == 5)
                        .dropdown_menu(move |mut menu, _, _| {
                            for source in [
                                PasswordSource::Manual,
                                PasswordSource::Known,
                                PasswordSource::Batch,
                                PasswordSource::Empty,
                                PasswordSource::Database,
                            ] {
                                if sources.contains(&source) {
                                    continue;
                                }
                                let entity = entity.clone();
                                menu =
                                    menu.item(PopupMenuItem::new(source_label(source)).on_click(
                                        move |_, _, cx| {
                                            entity.update(cx, |this, cx| {
                                                this.sources.push(source);
                                                cx.notify();
                                            });
                                        },
                                    ));
                            }
                            menu
                        })
                })
                .into_any_element();
        }
        div()
            .flex()
            .flex_col()
            .gap_3()
            .children(self.installations.iter().enumerate().map(|(i, b)| {
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .py_4()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .items_center()
                            .child(
                                div()
                                    .flex_1()
                                    .child(Input::new(&b.id).disabled(self.saving)),
                            )
                            .child(
                                Switch::new(("backend-enabled", b.identity))
                                    .label("启用")
                                    .checked(b.enabled)
                                    .disabled(self.saving)
                                    .on_change(cx.listener(move |this, v: &bool, _, cx| {
                                        this.installations[i].enabled = *v;
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new(("backend-remove", b.identity))
                                    .ghost()
                                    .large()
                                    .label("移除")
                                    .disabled(self.saving)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.installations.remove(i);
                                        cx.notify();
                                    })),
                            ),
                    )
                    .child(
                        RadioGroup::horizontal(("backend-family", b.identity))
                            .selected_index(Some(usize::from(b.family == AdapterFamily::UnrarCli)))
                            .children(["7-Zip", "UnRAR"])
                            .disabled(self.saving)
                            .on_change(cx.listener(move |this, index: &usize, _, cx| {
                                this.installations[i].family = if *index == 0 {
                                    AdapterFamily::SevenZipCli
                                } else {
                                    AdapterFamily::UnrarCli
                                };
                                cx.notify();
                            })),
                    )
                    .child(
                        div()
                            .flex()
                            .gap_3()
                            .items_center()
                            .child(div().w(gpui::px(72.)).text_base().child("程序路径"))
                            .child(
                                div()
                                    .flex_1()
                                    .child(Input::new(&b.path).disabled(self.saving)),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .items_center()
                            .child(div().w(gpui::px(72.)).text_base().child("声明版本"))
                            .child(
                                div()
                                    .flex_1()
                                    .child(Input::new(&b.version).disabled(self.saving)),
                            )
                            .child("优先级")
                            .child(
                                div()
                                    .w(gpui::px(90.))
                                    .child(Input::new(&b.priority).disabled(self.saving)),
                            ),
                    )
            }))
            .child(
                Button::new("backend-add")
                    .large()
                    .label("添加后端")
                    .disabled(self.saving)
                    .on_click(cx.listener(|this, _, window, cx| {
                        let i = this.next;
                        this.next += 1;
                        this.installations.push(Installation::new(
                            i,
                            BackendInstallation {
                                id: format!("custom-{i}"),
                                family: AdapterFamily::SevenZipCli,
                                executable: Default::default(),
                                declared_version: None,
                                enabled: true,
                                priority: 0,
                            },
                            window,
                            cx,
                        ));
                        cx.notify();
                    })),
            )
            .child(
                Button::new("backend-check")
                    .large()
                    .ghost()
                    .label(if self.checking {
                        "检测中…"
                    } else {
                        "检测后端"
                    })
                    .tooltip("检测已保存的配置，请先保存修改")
                    .disabled(self.checking)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.checking = true;
                        let task = cx.background_executor().spawn(async {
                            let config = crate::library::load_config(&Default::default())?;
                            let router = smartzip_archive::BackendRouter::from_config(
                                &config.resolved.values.backends,
                            )
                            .map_err(|e| e.to_string())?;
                            if router.adapter_ids().is_empty() {
                                return Err(router.warnings().join("\n"));
                            }
                            Ok(router.diagnostics())
                        });
                        cx.spawn(async move |entity, cx| {
                            let result = task.await;
                            let _ = entity.update(cx, |this, cx| {
                                this.checking = false;
                                this.diagnostics = Some(result);
                                cx.notify();
                            });
                        })
                        .detach();
                    })),
            )
            .when_some(self.diagnostics.clone(), |el, result| match result {
                Err(e) => el.child(
                    div()
                        .text_base()
                        .text_color(cx.theme().danger)
                        .child(format!("检测失败：{e}")),
                ),
                Ok(rows) => el.children(rows.into_iter().map(|row| {
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .text_base()
                        .p_2()
                        .bg(cx.theme().secondary)
                        .rounded_md()
                        .child(format!(
                            "{} · {}",
                            row["family"].as_str().unwrap_or("后端"),
                            row["executable"].as_str().unwrap_or("内置")
                        ))
                        .child(format!(
                            "版本：{}",
                            row["version"].as_str().unwrap_or("未能识别")
                        ))
                        .child(format!(
                            "能力：{}",
                            row["capabilities"]
                                .as_object()
                                .map(|m| m
                                    .iter()
                                    .filter(|(_, v)| v == &&serde_json::Value::Bool(true))
                                    .map(|(k, _)| k.as_str())
                                    .collect::<Vec<_>>()
                                    .join(" · "))
                                .unwrap_or_default()
                        ))
                        .when_some(row["error"].as_str().map(str::to_owned), |el, e| {
                            el.child(e)
                        })
                })),
            })
            .into_any_element()
    }
}
