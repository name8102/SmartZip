//! Configuration editor backed by the shared configuration schema.
use crate::{
    library::ConfigSnapshot,
    settings_fields::{Field, Kind, SECTIONS},
};
use gpui::component::{
    button::{Button, ButtonVariants},
    input::{Input, InputState},
    select::{Select, SelectState},
    switch::Switch,
    ActiveTheme, IndexPath, Sizable,
};
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
    section: usize,
}

impl ConfigForm {
    pub fn new(snapshot: &ConfigSnapshot, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let values = toml::Value::try_from(&snapshot.resolved.values)
            .expect("validated configuration is TOML serializable");
        let explanations = snapshot.resolved.explanation();
        let fields = SECTIONS
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
                let input = matches!(field.kind, Kind::Text | Kind::Number | Kind::Toml)
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
        Self { fields, section: 0 }
    }

    /// TOML text is validated by the shared persistence layer, including invalid numeric input.
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
                let value = if let (Kind::Choice(options), Some(select)) =
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
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child("保存后恢复继承值")
                .into_any_element()
        } else {
            match field.kind {
                Kind::Bool => Switch::new(("setting-switch", index))
                    .small()
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
                .small()
                .into_any_element(),
                Kind::Text | Kind::Number | Kind::Toml => Input::new(
                    draft
                        .input
                        .as_ref()
                        .expect("editable text field has an input"),
                )
                .small()
                .into_any_element(),
                Kind::ReadOnly => div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!("{} · 暂不可用", draft.value))
                    .into_any_element(),
            }
        };
        div()
            .p_3()
            .flex()
            .flex_col()
            .gap_2()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(field.label),
                    )
                    .when(field.kind != Kind::ReadOnly, |row| {
                        row.child(
                            Button::new(("setting-reset", index))
                                .ghost()
                                .xsmall()
                                .label(if draft.reset {
                                    "撤销恢复"
                                } else {
                                    "恢复继承"
                                })
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.fields[index].reset = !this.fields[index].reset;
                                    cx.notify();
                                })),
                        )
                    }),
            )
            .child(editor)
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(field.help),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!("{} · 来源：{}", field.key, draft.origin)),
            )
            .when_some(draft.explanation.clone(), |row, explanation| {
                row.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(explanation),
                )
            })
    }
}

impl Render for ConfigForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let section = self.section;
        let offset: usize = SECTIONS[..section]
            .iter()
            .map(|section| section.fields.len())
            .sum();
        div()
            .flex()
            .flex_col()
            .gap_2()
            .w_full()
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_1()
                    .children(SECTIONS.iter().enumerate().map(|(index, section)| {
                        Button::new(("config-section", index))
                            .small()
                            .label(section.label)
                            .when(index == self.section, |button| button.primary())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.section = index;
                                cx.notify();
                            }))
                    })),
            )
            .children(
                (offset..offset + SECTIONS[section].fields.len()).map(|index| self.row(index, cx)),
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

    #[test]
    fn advanced_arrays_round_trip_as_editable_toml() {
        let mut config = smartzip_config::SmartZipConfig::default();
        config
            .backends
            .installations
            .push(smartzip_config::BackendInstallation {
                id: "custom".into(),
                family: smartzip_config::AdapterFamily::SevenZipCli,
                executable: "/Applications/7 Zip/7zz".into(),
                declared_version: None,
                enabled: true,
                priority: 10,
            });
        let config = toml::Value::try_from(&config).unwrap();
        for key in ["passwords.sources", "backends.installations"] {
            let value = smartzip_config::value_at(&config, key).unwrap();
            let edited = encode_edit(Kind::Toml, value.to_string()).unwrap();
            let parsed = format!("value = {edited}").parse::<toml::Value>().unwrap();
            assert_eq!(&parsed["value"], value, "{key}");
        }
    }
}
