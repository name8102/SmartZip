use super::*;
use gpui::component::tab::{Tab, TabBar};

impl View {
    pub(super) fn history(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let state = self.shared.read(cx);
        let rows = state.history.clone();
        let detail = state.history_detail.clone();
        let query = state.history_query.clone();
        let more = state.history_more;
        let loading = state.history_loading;
        let selected = state.history_detail_request.clone();
        let busy = state.library_busy;
        let wide = window.bounds().size.width >= px(1100.);
        let filters = [
            ("", "全部"),
            ("completed", "完成"),
            ("partial", "部分完成"),
            ("failed", "失败"),
            ("cancelled", "取消"),
        ];
        let filter_index = filters
            .iter()
            .position(|(status, _)| *status == query.status)
            .unwrap_or(0);
        let list = div()
            .flex()
            .flex_col()
            .gap_3()
            .flex_1()
            .min_w_0()
            .child(
                TabBar::new("history-filters")
                    .underline()
                    .selected_index(filter_index)
                    .children(filters.iter().map(|(_, label)| Tab::new().label(*label)))
                    .on_click(cx.listener(move |this, index: &usize, _, cx| {
                        this.shared.update(cx, |s, cx| {
                            s.history_query.status = filters[*index].0.into();
                            s.history_query.offset = 0;
                            s.reload_history();
                            cx.notify();
                        })
                    })),
            )
            .when(loading, |el| {
                el.child(
                    div()
                        .text_base()
                        .text_color(cx.theme().muted_foreground)
                        .child("正在读取历史…"),
                )
            })
            .child(
                div()
                    .id("history-list")
                    .max_h(px(520.))
                    .overflow_y_scroll()
                    .when(rows.is_empty() && !loading, |el| {
                        el.child(
                            div()
                                .py_8()
                                .text_base()
                                .text_color(cx.theme().muted_foreground)
                                .child("没有匹配的记录"),
                        )
                    })
                    .children(rows.into_iter().map(|row| {
                        let id = row.task.id.clone();
                        let select_id = id.clone();
                        let name = std::path::Path::new(&row.input)
                            .file_name()
                            .map(|s| s.to_string_lossy().into_owned())
                            .unwrap_or_else(|| row.task.kind.clone());
                        div()
                            .id(SharedString::from(format!("history-{id}")))
                            .flex()
                            .flex_col()
                            .gap_2()
                            .px_3()
                            .py_3()
                            .min_h(px(self.shared.read(cx).preferences.density.row_height()))
                            .border_b_1()
                            .border_color(cx.theme().border)
                            .cursor_pointer()
                            .hover(|el| el.bg(cx.theme().sidebar_accent))
                            .when(selected.as_ref() == Some(&id), |el| {
                                el.bg(cx.theme().sidebar_accent)
                            })
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_3()
                                    .child(
                                        div().flex_1().min_w_0().text_base().text_ellipsis().child(
                                            control(SharedString::from(format!(
                                                "open-history-{id}"
                                            )))
                                            .w_full()
                                            .justify_start()
                                            .px_0()
                                            .accessibility_label(name.clone())
                                            .child(
                                                div()
                                                    .w_full()
                                                    .min_w_0()
                                                    .text_ellipsis()
                                                    .child(name),
                                            )
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                cx.stop_propagation();
                                                this.shared.update(cx, |state, cx| {
                                                    state.load_detail(select_id.clone());
                                                    cx.notify();
                                                });
                                            })),
                                        ),
                                    )
                                    .child(
                                        div()
                                            .text_sm()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(state_label(&row.task.status).to_owned()),
                                    ),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_3()
                                    .text_sm()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .text_ellipsis()
                                            .child(row.task.started_at),
                                    )
                                    .child(format!("{} 项", row.files)),
                            )
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.shared.update(cx, |s, cx| {
                                    s.load_detail(id.clone());
                                    cx.notify();
                                })
                            }))
                    })),
            )
            .child(
                div()
                    .flex()
                    .justify_between()
                    .items_center()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!("第 {} 页", query.offset / library::PAGE_SIZE + 1))
                    .child(
                        div()
                            .flex()
                            .gap_1()
                            .child(
                                control("history-prev")
                                    .label("上一页")
                                    .disabled(query.offset == 0 || loading)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.shared.update(cx, |s, cx| {
                                            s.history_query.offset = s
                                                .history_query
                                                .offset
                                                .saturating_sub(library::PAGE_SIZE);
                                            s.reload_history();
                                            cx.notify();
                                        })
                                    })),
                            )
                            .child(
                                control("history-next")
                                    .label("下一页")
                                    .disabled(!more || loading)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.shared.update(cx, |s, cx| {
                                            s.history_query.offset += library::PAGE_SIZE;
                                            s.reload_history();
                                            cx.notify();
                                        })
                                    })),
                            ),
                    ),
            );
        let detail_panel = div()
            .flex()
            .flex_col()
            .gap_3()
            .flex_1()
            .min_w_0()
            .when(wide, |el| {
                el.border_l_1().border_color(cx.theme().border).pl_5()
            })
            .when(!wide, |el| {
                el.border_t_1().border_color(cx.theme().border).pt_4()
            })
            .when(detail.is_none(), |el| {
                el.child(
                    div()
                        .py_8()
                        .text_base()
                        .text_color(cx.theme().muted_foreground)
                        .child(if selected.is_some() {
                            "正在读取详情…"
                        } else {
                            "选择一条记录查看详情"
                        }),
                )
            })
            .when_some(detail, |el, detail| {
                let output = detail.task.output_path.clone();
                let inputs = detail.inputs.clone();
                let export = detail.clone();
                el.child(
                    div()
                        .text_base()
                        .child(state_label(&detail.task.status).to_owned()),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(detail.task.started_at.clone()),
                )
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .flex_wrap()
                        .child(
                            control("history-output")
                                .label("定位输出")
                                .tooltip("输出不存在时复制路径")
                                .disabled(output.is_none())
                                .on_click(move |_, _, cx| {
                                    if let Some(path) = &output {
                                        if std::path::Path::new(path).exists() {
                                            cx.reveal_path(std::path::Path::new(path));
                                        } else {
                                            cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                                                path.clone(),
                                            ));
                                        }
                                    }
                                }),
                        )
                        .child(
                            control("history-rerun")
                                .label("重新配置")
                                .disabled(inputs.is_empty() || detail.task.kind != "extract")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.shared.update(cx, |s, cx| {
                                        s.enqueue_with_hold(
                                            inputs.clone(),
                                            TaskOperation::Extract,
                                            true,
                                        );
                                        cx.notify();
                                    });
                                    this.page = Page::Tasks;
                                })),
                        )
                        .child(
                            control("history-export")
                                .label("导出诊断…")
                                .disabled(busy)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    let receiver = cx.prompt_for_new_path(
                                        std::path::Path::new("."),
                                        Some("smartzip-diagnostic.json"),
                                    );
                                    let shared = this.shared.clone();
                                    let detail = export.clone();
                                    cx.spawn(async move |_, cx| {
                                        if let Ok(Ok(Some(path))) = receiver.await {
                                            shared.update(cx, |s, cx| {
                                                s.background(move || {
                                                    LibraryMessage::Mutation(
                                                        library::export_diagnostics(&detail, &path)
                                                            .map(|_| {
                                                                "诊断已导出（不含密码）".into()
                                                            }),
                                                    )
                                                });
                                                cx.notify();
                                            });
                                        }
                                    })
                                    .detach();
                                })),
                        ),
                )
                .child(
                    div()
                        .id("history-files")
                        .max_h(px(420.))
                        .overflow_y_scroll()
                        .children(detail.files.iter().map(|file| {
                            div()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .py_3()
                                .border_b_1()
                                .border_color(cx.theme().border)
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(state_label(&file.status).to_owned()),
                                )
                                .child(div().text_base().child(file.input_path.clone()))
                                .when_some(file.output_path.clone(), |el, path| {
                                    el.child(
                                        div()
                                            .text_sm()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(format!("→ {path}")),
                                    )
                                })
                                .when_some(file.reason.clone(), |el, reason| {
                                    el.child(div().text_base().child(reason))
                                })
                        })),
                )
                .child(
                    control("history-events-toggle")
                        .label(if self.diagnostics_open {
                            "收起事件"
                        } else {
                            "事件记录"
                        })
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.diagnostics_open = !this.diagnostics_open;
                            cx.notify();
                        })),
                )
                .when(self.diagnostics_open, |el| {
                    el.child(
                        div()
                            .id("history-events")
                            .max_h(px(260.))
                            .overflow_y_scroll()
                            .children(detail.events.iter().map(|e| {
                                div()
                                    .text_sm()
                                    .py_1()
                                    .child(format!("{}  {}", e.created_at, e.message))
                            })),
                    )
                })
            });
        div()
            .flex()
            .flex_col()
            .gap_4()
            .child(
                div()
                    .flex()
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(Input::new(&self.history_search).large().cleanable(true)),
                    )
                    .child(
                        control("history-search")
                            .primary()
                            .label("搜索")
                            .disabled(loading)
                            .on_click(cx.listener(|this, _, _, cx| {
                                let text = this.history_search.read(cx).value().to_string();
                                this.shared.update(cx, |s, cx| {
                                    s.history_query.text = text;
                                    s.history_query.offset = 0;
                                    s.reload_history();
                                    cx.notify();
                                });
                            })),
                    )
                    .child(
                        control("history-reload")
                            .label("刷新")
                            .disabled(loading)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.shared.update(cx, |s, cx| {
                                    s.reload_history();
                                    cx.notify();
                                })
                            })),
                    ),
            )
            .child(
                div()
                    .flex()
                    .gap_5()
                    .when(!wide, |el| el.flex_col())
                    .child(list)
                    .child(detail_panel),
            )
    }
}
