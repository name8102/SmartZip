//! Configuration editor backed by the shared configuration schema.
use crate::{
    library::ConfigSnapshot,
    settings_fields::{Field, Kind, SECTIONS},
};
use gpui::base::Disableable;
use gpui::component::{
    button::{Button, ButtonVariants},
    input::{Input, InputState},
    select::{Select, SelectState},
    switch::Switch,
    tab::{Tab, TabBar},
    ActiveTheme, IndexPath, Sizable,
};
use gpui::Focusable;
use gpui::{
    div, prelude::*, App, AppContext, Context, Entity, IntoElement, Render, SharedString, Window,
};

struct Draft {
    field: &'static Field,
    original: String,
    value: String,
    input: Option<Entity<InputState>>,
    select: Option<Entity<SelectState<Vec<SharedString>>>>,
    reset: bool,
    origin: String,
    explanation: Option<String>,
}

pub struct ConfigForm {
    fields: Vec<Draft>,
    _subscriptions: Vec<gpui::Subscription>,
    section: usize,
    pub saving: bool,
    search: Entity<InputState>,
    backend_list: Entity<crate::config_lists::ConfigLists>,
    source_list: Entity<crate::config_lists::ConfigLists>,
}

impl ConfigForm {
    pub fn new(snapshot: &ConfigSnapshot, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let values = toml::Value::try_from(&snapshot.resolved.values)
            .expect("validated configuration is TOML serializable");
        let explanations = snapshot.resolved.explanation();
        let fields: Vec<Draft> = SECTIONS
            .iter()
            .flat_map(|section| section.fields)
            .map(|field| {
                let current = smartzip_config::value_at(&values, field.key);
                let text = match field.kind {
                    Kind::Text | Kind::Choice(_) => current
                        .and_then(toml::Value::as_str)
                        .unwrap_or("")
                        .to_owned(),
                    _ => current.map(ToString::to_string).unwrap_or_default(),
                };
                let input = matches!(field.kind, Kind::Text | Kind::Number)
                    .then(|| cx.new(|cx| InputState::new(window, cx).default_value(text.clone())));
                let select = if let Kind::Choice(options) = field.kind {
                    let labels: Vec<SharedString> =
                        options.iter().map(|(_, label)| (*label).into()).collect();
                    let selected = options
                        .iter()
                        .position(|(value, _)| *value == text)
                        .map(IndexPath::new);
                    Some(cx.new(|cx| SelectState::new(labels, selected, window, cx)))
                } else {
                    None
                };
                Draft {
                    field,
                    original: text.clone(),
                    value: text,
                    input,
                    select,
                    reset: false,
                    origin: snapshot
                        .resolved
                        .origins
                        .get(field.key)
                        .cloned()
                        .unwrap_or_else(|| "内置默认".into()),
                    explanation: explanations.get(field.key).cloned(),
                }
            })
            .collect();
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("搜索设置…"));
        let backend_list = cx.new(|cx| {
            crate::config_lists::ConfigLists::new(&snapshot.resolved.values, true, window, cx)
        });
        let source_list = cx.new(|cx| {
            crate::config_lists::ConfigLists::new(&snapshot.resolved.values, false, window, cx)
        });
        let mut subscriptions = vec![
            cx.observe(&search, |_, _, cx| cx.notify()),
            cx.observe(&backend_list, |_, _, cx| cx.notify()),
            cx.observe(&source_list, |_, _, cx| cx.notify()),
        ];
        for field in &fields {
            if let Some(input) = &field.input {
                subscriptions.push(cx.observe(input, |_, _, cx| cx.notify()));
            }
            if let Some(select) = &field.select {
                subscriptions.push(cx.observe(select, |_, _, cx| cx.notify()));
            }
        }
        Self {
            fields,
            section: 0,
            saving: false,
            backend_list,
            source_list,
            search,
            _subscriptions: subscriptions,
        }
    }

    /// TOML text is validated by the shared persistence layer, including invalid numeric input.
    pub fn focus_search(&self, window: &mut Window, cx: &mut App) {
        self.search.read(cx).focus_handle(cx).focus(window, cx);
    }
    pub fn patches(&self, cx: &App) -> Vec<(String, Option<String>)> {
        self.fields
            .iter()
            .filter_map(|draft| {
                if draft.field.kind == Kind::ReadOnly {
                    return None;
                }
                if draft.reset {
                    return Some((draft.field.key.into(), None));
                }
                let value = if draft.field.kind == Kind::List {
                    let backends = draft.field.key == "backends.installations";
                    (if backends {
                        &self.backend_list
                    } else {
                        &self.source_list
                    })
                    .read(cx)
                    .value(cx)
                } else if let (Kind::Choice(options), Some(select)) =
                    (draft.field.kind, &draft.select)
                {
                    select
                        .read(cx)
                        .selected_index(cx)
                        .and_then(|index| options.get(index.row))
                        .map(|(value, _)| (*value).to_owned())
                        .unwrap_or_else(|| draft.original.clone())
                } else {
                    draft
                        .input
                        .as_ref()
                        .map(|input| input.read(cx).value().to_string())
                        .unwrap_or_else(|| draft.value.clone())
                };
                if value == draft.original {
                    return None;
                }
                let encoded = encode_edit(draft.field.kind, value);
                Some((draft.field.key.into(), encoded))
            })
            .collect()
    }

    fn row(&self, index: usize, cx: &mut Context<Self>) -> impl IntoElement {
        let draft = &self.fields[index];
        let field = draft.field;
        let editor = if draft.reset {
            div()
                .text_base()
                .text_color(cx.theme().muted_foreground)
                .child("保存后恢复继承值")
                .into_any_element()
        } else {
            match field.kind {
                Kind::List => {
                    let list = if field.key == "backends.installations" {
                        &self.backend_list
                    } else {
                        &self.source_list
                    };
                    list.update(cx, |list, _| list.saving = self.saving);
                    list.clone().into_any_element()
                }
                Kind::Bool => Switch::new(("setting-switch", index))
                    .accessibility_label(field.label)
                    .large()
                    .disabled(self.saving)
                    .checked(draft.value == "true")
                    .on_change(cx.listener(move |this, checked: &bool, _, cx| {
                        this.fields[index].value = checked.to_string();
                        cx.notify();
                    }))
                    .into_any_element(),
                Kind::Choice(_) => Select::new(
                    draft
                        .select
                        .as_ref()
                        .expect("choice field has select state"),
                )
                .large()
                .disabled(self.saving)
                .into_any_element(),
                Kind::Text | Kind::Number => Input::new(
                    draft
                        .input
                        .as_ref()
                        .expect("editable text field has an input"),
                )
                .large()
                .disabled(self.saving)
                .into_any_element(),
                Kind::ReadOnly => div()
                    .text_base()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!("{} · 暂不可用", draft.value))
                    .into_any_element(),
            }
        };
        let (inline_editor, list_editor) = if field.kind == Kind::List {
            (None, Some(editor))
        } else {
            (Some(editor), None)
        };
        let help = format!(
            "{}\n{} · 来源：{}{}",
            field.help,
            field.key,
            draft.origin,
            draft
                .explanation
                .as_ref()
                .map(|text| format!("\n{text}"))
                .unwrap_or_default(),
        );
        div()
            .py_3()
            .flex()
            .flex_col()
            .gap_3()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .w(gpui::px(190.))
                            .flex()
                            .items_center()
                            .gap_1()
                            .text_base()
                            .child(field.label)
                            .child(
                                Button::new(("setting-help", index))
                                    .ghost()
                                    .xsmall()
                                    .label("?")
                                    .tooltip(help),
                            ),
                    )
                    .when_some(inline_editor, |row, editor| {
                        row.child(div().flex_1().min_w(gpui::px(160.)).child(editor))
                    })
                    .when(field.kind != Kind::ReadOnly, |row| {
                        row.child(
                            Button::new(("setting-reset", index))
                                .ghost()
                                .xsmall()
                                .disabled(self.saving)
                                .label(if draft.reset {
                                    "撤销重置"
                                } else {
                                    "重置"
                                })
                                .tooltip("移除此项覆盖，恢复继承值")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.fields[index].reset = !this.fields[index].reset;
                                    cx.notify();
                                })),
                        )
                    }),
            )
            // The ordered editors need the full content width.
            .when_some(list_editor, |row, editor| row.child(editor))
    }
}

impl Render for ConfigForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let query = self.search.read(cx).value().to_lowercase();
        let dirty = self.patches(cx).len();
        let section = self.section;
        let offset: usize = SECTIONS[..section]
            .iter()
            .map(|section| section.fields.len())
            .sum();
        div()
            .flex()
            .flex_col()
            .gap_4()
            .w_full()
            .child(Input::new(&self.search).cleanable(true))
            .when(dirty > 0, |el| {
                el.child(
                    div()
                        .text_base()
                        .text_color(cx.theme().primary)
                        .child(format!("{dirty} 项未保存")),
                )
            })
            .when(self.saving, |el| el.child("正在保存…"))
            .child(
                TabBar::new("config-sections")
                    .underline()
                    .menu(true)
                    .selected_index(self.section)
                    .children(
                        SECTIONS
                            .iter()
                            .map(|section| Tab::new().label(section.label)),
                    )
                    .on_click(cx.listener(|this, index: &usize, _, cx| {
                        this.section = *index;
                        cx.notify();
                    })),
            )
            .children(
                (0..self.fields.len())
                    .filter(|&index| {
                        if query.is_empty() {
                            (offset..offset + SECTIONS[section].fields.len()).contains(&index)
                        } else {
                            let f = self.fields[index].field;
                            format!("{} {} {}", f.label, f.key, f.help)
                                .to_lowercase()
                                .contains(&query)
                        }
                    })
                    .map(|index| self.row(index, cx))
                    .collect::<Vec<_>>(),
            )
    }
}

fn encode_edit(kind: Kind, value: String) -> Option<String> {
    match kind {
        Kind::Text if value.trim().is_empty() => None,
        Kind::Text | Kind::Choice(_) => Some(toml::Value::String(value).to_string()),
        _ => Some(value),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edits_preserve_types_and_invalid_input_for_validation() {
        assert_eq!(encode_edit(Kind::Text, "  ".into()), None);
        assert_eq!(
            encode_edit(Kind::Number, "not-a-number".into()),
            Some("not-a-number".into())
        );
        let path = "a \"quoted\" directory\\output";
        let encoded = encode_edit(Kind::Text, path.into()).unwrap();
        let parsed = format!("value = {encoded}").parse::<toml::Value>().unwrap();
        assert_eq!(parsed["value"].as_str(), Some(path));
    }
}
