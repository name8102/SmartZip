use super::*;
#[derive(Clone)]
enum TaskRow {
    Batch(u64, String, Phase, bool, usize),
    File(
        u64,
        Box<smartzip_engine::root_management::FileTaskSnapshot>,
        usize,
        Phase,
        bool,
    ),
}

impl View {
    pub(super) fn drop_area(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        if self.glass.is_some() {
            return self.glass_drop_area(cx);
        }
        use gpui::component::empty::{Empty, EmptyHeader, EmptyMedia, EmptyTitle};
        div()
            .id("archive-drop")
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(180.))
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| {
                this.shared.update(cx, |state, cx| {
                    state.enqueue(paths.paths().to_vec(), TaskOperation::Extract);
                    cx.notify();
                });
            }))
            .child(
                Empty::new()
                    .header(
                        EmptyHeader::new()
                            .media(
                                EmptyMedia::new().child(
                                    Icon::new(IconName::PackageOpen)
                                        .size(px(36.))
                                        .text_color(cx.theme().primary),
                                ),
                            )
                            .title(EmptyTitle::new().child("拖入归档开始解压")),
                    )
                    .child(
                        Button::new("choose")
                            .primary()
                            .icon(IconName::Plus)
                            .label("选择归档")
                            .on_click(
                                cx.listener(|this, _, _, cx| this.pick(TaskOperation::Extract, cx)),
                            ),
                    ),
            )
            .into_any_element()
    }

    pub(super) fn actions(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.shared.read(cx);
        let paused = state.queue.paused;
        let view = cx.entity();
        let has_finished = state
            .queue
            .jobs
            .iter()
            .any(|job| !job.phase.active() && job.phase != Phase::Queued);
        div()
            .flex()
            .items_center()
            .gap_2()
            .flex_wrap()
            .child(
                Button::new("extract")
                    .large()
                    .primary()
                    .icon(IconName::Plus)
                    .label("添加归档")
                    .on_click(cx.listener(|this, _, _, cx| this.pick(TaskOperation::Extract, cx))),
            )
            .child(
                control("queue-options")
                    .icon(IconName::SlidersHorizontal)
                    .label("解压选项")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.shared.update(cx, |state, cx| {
                            state.preferences.queue_settings_open =
                                !state.preferences.queue_settings_open;
                            state.persist_preferences();
                            cx.notify();
                        });
                    })),
            )
            .child(
                control("pause")
                    .label(if paused {
                        "开始等待任务"
                    } else {
                        "暂停自动启动"
                    })
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.shared.update(cx, |state, cx| {
                            if state.queue.paused {
                                state.queue.start_all();
                            } else {
                                state.queue.paused = true;
                            }
                            cx.notify();
                        })
                    })),
            )
            .child(
                control("queue-menu")
                    .icon(IconName::Ellipsis)
                    .tooltip("更多操作")
                    .dropdown_menu(move |menu, _, _| {
                        let prepare = view.clone();
                        let details = view.clone();
                        let clear = view.clone();
                        menu.item(
                            gpui::component::menu::PopupMenuItem::new("添加并配置…").on_click(
                                move |_, _, cx| {
                                    prepare.update(cx, |this, cx| {
                                        this.pick_with_hold(TaskOperation::Extract, true, cx);
                                    })
                                },
                            ),
                        )
                        .item(
                            gpui::component::menu::PopupMenuItem::new("显示 / 隐藏详情").on_click(
                                move |_, _, cx| {
                                    details.update(cx, |this, cx| {
                                        this.change_appearance(
                                            |p| p.detail_visible = !p.detail_visible,
                                            cx,
                                        );
                                    })
                                },
                            ),
                        )
                        .item(
                            gpui::component::menu::PopupMenuItem::new("移除已结束项")
                                .disabled(!has_finished)
                                .on_click(move |_, _, cx| {
                                    clear.update(cx, |this, cx| {
                                        this.shared.update(cx, |state, cx| {
                                            state.queue.remove_finished();
                                            cx.notify();
                                        });
                                        this.selected_file = None;
                                        this.task_selection.clear();
                                    })
                                }),
                        )
                    }),
            )
            .when(!self.task_selection.is_empty(), |el| {
                el.child(
                    control("cancel-selected")
                        .label(format!("取消所选 {} 项", self.task_selection.len()))
                        .on_click(cx.listener(|this, _, _, cx| {
                            let selected: Vec<_> = this.task_selection.drain().collect();
                            this.shared.update(cx, |state, cx| {
                                for (id, root) in selected {
                                    state.queue.cancel_root(id, &root);
                                }
                                cx.notify();
                            });
                        })),
                )
                .child(
                    control("retry-selected")
                        .label("重试所选")
                        .on_click(cx.listener(|this, _, _, cx| {
                            let selected: Vec<_> = this.task_selection.drain().collect();
                            this.shared.update(cx, |state, cx| {
                                let count = selected
                                    .into_iter()
                                    .filter(|(id, root)| {
                                        state.queue.retry_root(*id, root).is_some()
                                    })
                                    .count();
                                state.message = format!("已添加 {count} 个重试任务");
                                cx.notify();
                            });
                        })),
                )
            })
    }

    pub(super) fn task_overview(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut counts = [0usize; 5];
        for job in &self.shared.read(cx).queue.jobs {
            if job.files.is_empty() {
                counts[0] += job.request.paths.len();
                counts[job_category(job.phase)] += job.request.paths.len();
            } else {
                for file in job.files.iter().filter(|file| file.parent_id.is_none()) {
                    counts[0] += 1;
                    counts[file_category(file)] += 1;
                }
            }
        }
        TabBar::new("task-filter")
            .underline()
            .when(self.glass.is_some(), |tabs| tabs.segmented())
            .small()
            .selected_index(self.task_filter)
            .children(
                ["全部", "进行中", "待处理", "已完成", "失败 / 取消"]
                    .into_iter()
                    .enumerate()
                    .map(|(index, label)| Tab::new().label(format!("{label}  {}", counts[index]))),
            )
            .on_click(cx.listener(|this, index: &usize, _, cx| {
                this.task_filter = *index;
                cx.notify();
            }))
    }

    pub(super) fn tasks_page(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        if self.glass.is_some() && self.shared.read(cx).queue.jobs.is_empty() {
            return self.glass_start_page(window, cx);
        }
        use gpui::component::resizable::v_resizable;
        let state = self.shared.read(cx);
        let empty = state.queue.jobs.is_empty();
        let detail_width = state.preferences.detail_width;
        let queue_settings_open = state.preferences.queue_settings_open;
        let resize_workspace = self.shared.clone();
        let show_detail = state.preferences.detail_visible && state.queue.selected().is_some();
        let inspector = (queue_settings_open || show_detail) && self.selected_prompt(cx).is_none();
        let list = div()
            .flex()
            .flex_col()
            .size_full()
            .min_h_0()
            .min_w_0()
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| {
                this.shared.update(cx, |state, cx| {
                    state.enqueue(paths.paths().to_vec(), TaskOperation::Extract);
                    cx.notify();
                })
            }))
            .when(!empty, |el| {
                el.child(
                    div()
                        .px_5()
                        .py_2()
                        .flex_shrink_0()
                        .child(self.task_overview(cx)),
                )
                .child(
                    div()
                        .px_5()
                        .pb_3()
                        .flex_shrink_0()
                        .child(Input::new(&self.search).large().cleanable(true)),
                )
                .child(self.tasks(cx))
            })
            .when(empty, |el| el.child(self.drop_area(cx)));
        let panes = if inspector {
            let details = div()
                .id("task-inspector")
                .size_full()
                .min_w_0()
                .overflow_y_scroll()
                .p_4()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .mb_4()
                        .child(
                            div()
                                .text_base()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child(if queue_settings_open {
                                    "解压选项"
                                } else {
                                    "详情"
                                }),
                        )
                        .child(
                            control("close-inspector")
                                .icon(IconName::X)
                                .tooltip("关闭侧栏")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.shared.update(cx, |state, cx| {
                                        if state.preferences.queue_settings_open {
                                            state.preferences.queue_settings_open = false;
                                        } else {
                                            state.preferences.detail_visible = false;
                                        }
                                        state.persist_preferences();
                                        cx.notify();
                                    });
                                })),
                        ),
                )
                .child(if queue_settings_open {
                    self.quick_controls(None, cx).into_any_element()
                } else {
                    self.detail(cx).into_any_element()
                });
            if window.viewport_size().width >= px(1000.) {
                h_resizable("task-panes")
                    .on_resize(move |panes, _, cx| {
                        if let Some(width) = panes.read(cx).sizes().last().copied() {
                            resize_workspace.update(cx, |state, _| {
                                state.preferences.detail_width = width.as_f32();
                                state.persist_preferences();
                            });
                        }
                    })
                    .child(
                        resizable_panel()
                            .size_range(px(300.)..px(5000.))
                            .child(list),
                    )
                    .child(
                        resizable_panel()
                            .size(px(detail_width))
                            .size_range(px(280.)..px(520.))
                            .fixed()
                            .child(details),
                    )
                    .into_any_element()
            } else {
                v_resizable("task-panes-narrow")
                    .child(
                        resizable_panel()
                            .size_range(px(180.)..px(5000.))
                            .child(list),
                    )
                    .child(
                        resizable_panel()
                            .size(px(280.))
                            .size_range(px(160.)..px(5000.))
                            .child(details),
                    )
                    .into_any_element()
            }
        } else {
            list.into_any_element()
        };
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .child(div().px_5().py_3().flex_shrink_0().child(self.actions(cx)))
            .child(self.prompt(cx))
            .child(div().flex_1().min_h_0().child(panes))
            .into_any_element()
    }

    pub(super) fn quick_page(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let empty = self.shared.read(cx).queue.jobs.is_empty();
        div()
            .id("quick-scroll")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .p_4()
            .flex()
            .flex_col()
            .gap_4()
            .child(self.drop_area(cx))
            .child(self.quick_controls(None, cx))
            .child(self.prompt(cx))
            .when(!empty, |el| el.child(self.detail(cx)))
    }

    pub(super) fn tasks(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.shared.read(cx);
        let query = self.search.read(cx).value().to_lowercase();
        let height = state.preferences.density.row_height() + 36.;
        let mut rows = Vec::new();
        for job in &state.queue.jobs {
            let mut descendants: std::collections::HashMap<_, Vec<_>> = Default::default();
            for file in job.files.iter().filter(|f| f.parent_id.is_some()) {
                descendants
                    .entry(file.root_id.clone())
                    .or_default()
                    .push(file.clone());
            }
            let roots: Vec<_> = job
                .files
                .iter()
                .filter(|f| {
                    f.parent_id.is_none()
                        && (self.task_filter == 0 || file_category(f) == self.task_filter)
                })
                .filter(|f| {
                    query.is_empty()
                        || f.path.to_string_lossy().to_lowercase().contains(&query)
                        || descendants.get(&f.root_id).is_some_and(|children| {
                            children
                                .iter()
                                .any(|c| c.path.to_string_lossy().to_lowercase().contains(&query))
                        })
                })
                .cloned()
                .collect();
            if roots.is_empty() && !job.files.is_empty() {
                continue;
            }
            if job.files.is_empty()
                && ((self.task_filter != 0 && self.task_filter != job_category(job.phase))
                    || (!query.is_empty()
                        && !job
                            .request
                            .paths
                            .iter()
                            .any(|p| p.to_string_lossy().to_lowercase().contains(&query))))
            {
                continue;
            }
            rows.push(TaskRow::Batch(
                job.id,
                job.name(),
                job.phase,
                state.queue.is_held(job.id),
                job.request.paths.len(),
            ));
            for root in roots {
                let children = descendants.remove(&root.root_id).unwrap_or_default();
                let expanded = self.expanded_roots.contains(&root.node_id);
                rows.push(TaskRow::File(
                    job.id,
                    Box::new(root),
                    children.len(),
                    job.phase,
                    false,
                ));
                if expanded {
                    for child in children {
                        rows.push(TaskRow::File(job.id, Box::new(child), 0, job.phase, true));
                    }
                }
            }
        }
        if state.queue.jobs.is_empty() {
            return div().into_any_element();
        }
        if rows.is_empty() {
            return empty_state("没有匹配的任务", "调整筛选条件，或添加归档开始处理。", cx)
                .into_any_element();
        }
        gpui::uniform_list(
            "task-rows",
            rows.len(),
            cx.processor(move |this, range: std::ops::Range<usize>, window, cx| {
                range
                    .map(|index| {
                        let row = match &rows[index] {
                            TaskRow::Batch(id, name, phase, held, inputs) => {
                                let id = *id;
                                div()
                                    .w_full()
                                    .flex()
                                    .items_center()
                                    .gap_3()
                                    .px_5()
                                    .bg(cx.theme().sidebar)
                                    .border_b_1()
                                    .border_color(cx.theme().border)
                                    .child(
                                        div().flex_1().min_w_0().flex().items_center().child(
                                            control(("batch", id))
                                                .px_0()
                                                .w_full()
                                                .min_w_0()
                                                .accessibility_label(format!("批次 {id} · {name}"))
                                                .child(
                                                    div()
                                                        .w_full()
                                                        .min_w_0()
                                                        .text_ellipsis()
                                                        .child(format!("批次 {id} · {name}")),
                                                )
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.selected_file = None;
                                                    this.shared.update(cx, |s, cx| {
                                                        s.queue.selected = Some(id);
                                                        cx.notify();
                                                    });
                                                })),
                                        ),
                                    )
                                    .child(div().text_sm().child(format!(
                                        "{inputs} 个输入 · {}",
                                        if *held { "等待配置" } else { phase.label() }
                                    )))
                                    .when(*phase == Phase::Queued, |el| {
                                        el.child(
                                            control(("start-batch", id))
                                                .primary()
                                                .label("开始任务")
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.shared.update(cx, |s, cx| {
                                                        s.queue.start(id);
                                                        cx.notify();
                                                    })
                                                })),
                                        )
                                    })
                                    .h(px(height))
                                    .into_any_element()
                            }
                            TaskRow::File(id, file, children, phase, nested) => div()
                                .w_full()
                                .h(px(height))
                                .overflow_hidden()
                                .child(this.file_task_row(
                                    *id,
                                    *file.clone(),
                                    *children,
                                    *phase,
                                    *nested,
                                    window.viewport_size().width < px(760.),
                                    cx,
                                ))
                                .into_any_element(),
                        };
                        row
                    })
                    .collect::<Vec<_>>()
            }),
        )
        .flex_1()
        .min_h_0()
        .w_full()
        .into_any_element()
    }

    pub(super) fn file_task_row(
        &mut self,
        id: u64,
        file: smartzip_engine::root_management::FileTaskSnapshot,
        children: usize,
        batch_phase: Phase,
        nested: bool,
        compact: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let node = file.node_id.clone();
        let bulk_node = file.root_id.clone();
        let bulk_selected = self.task_selection.contains(&(id, bulk_node.clone()));
        let selected = self
            .selected_file
            .as_ref()
            .is_some_and(|(job, n)| *job == id && *n == node);
        let done = file.root_outcome.is_some() || !batch_phase.active();
        let cancelled = file.cancel_requested;
        let paused = file.pause_requested;
        let status = file_status(&file);
        let name = file
            .path
            .file_name()
            .unwrap_or(file.path.as_os_str())
            .to_string_lossy()
            .into_owned();
        let name = match &file.volumes {
            Some(v) if v.candidates.len() > 1 => {
                format!("{name} · {} 个候选分卷组", v.candidates.len())
            }
            Some(v) if v.members.len() > 1 => format!("{name} · {} 卷", v.members.len()),
            _ if nested && file.depth == 0 => format!("候选入口 · {name}"),
            _ => name,
        };
        let selected_node = node.clone();
        let toggle_node = node.clone();
        let pause_node = node.clone();
        let cancel_node = node.clone();
        let retry_node = node.clone();
        let retry = matches!(
            file.root_outcome.as_deref(),
            Some("failed" | "partial" | "cancelled")
        );
        let expanded = self.expanded_roots.contains(&node);
        div()
            .id(SharedString::from(format!("file-{}", node)))
            .flex()
            .items_center()
            .gap_3()
            .px_5()
            .py_2()
            .min_h(px(
                self.shared.read(cx).preferences.density.row_height() + 36.
            ))
            .border_t_1()
            .border_color(cx.theme().border)
            .when(nested, |row| {
                row.pl(px(40. + 12. * f32::from(file.depth.min(4))))
            })
            .when(selected, |row| row.bg(cx.theme().sidebar_accent))
            .hover(|row| row.bg(cx.theme().sidebar))
            .when(!nested, |row| {
                row.child(
                    gpui::component::checkbox::Checkbox::new(SharedString::from(format!(
                        "bulk-{node}"
                    )))
                    .checked(bulk_selected)
                    .accessibility_label(format!("选择 {name}"))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !this.task_selection.remove(&(id, bulk_node.clone())) {
                            this.task_selection.insert((id, bulk_node.clone()));
                        }
                        cx.notify();
                    })),
                )
            })
            .child(
                control(SharedString::from(format!("expand-{node}")))
                    .label(if children == 0 {
                        "·".into()
                    } else if expanded {
                        format!("▾ {children}")
                    } else {
                        format!("▸ {children}")
                    })
                    .disabled(children == 0)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !this.expanded_roots.remove(&toggle_node) {
                            this.expanded_roots.insert(toggle_node.clone());
                        }
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .when(compact, |el| {
                        el.child(
                            div()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child(status.clone()),
                        )
                    })
                    .child(
                        control(SharedString::from(format!("select-{node}")))
                            .justify_start()
                            .px_0()
                            .w_full()
                            .min_w_0()
                            .accessibility_label(name.clone())
                            .child(div().w_full().min_w_0().text_ellipsis().child(name))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.selected_file = Some((id, selected_node.clone()));
                                this.shared.update(cx, |state, cx| {
                                    state.queue.selected = Some(id);
                                    cx.notify();
                                });
                            })),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_ellipsis()
                            .text_color(cx.theme().muted_foreground)
                            .child(stage_label(
                                &file
                                    .root_activity
                                    .as_ref()
                                    .map(|(_, stage)| stage.clone())
                                    .unwrap_or_else(|| file.stage.clone()),
                            )),
                    ),
            )
            .when(!compact, |row| {
                row.child(div().w(px(104.)).text_sm().child(status.clone()))
            })
            .when(file.state == "running" && file.progress.is_some(), |row| {
                row.child(
                    div()
                        .w(px(45.))
                        .text_sm()
                        .child(format!("{:.0}%", file.progress.unwrap_or_default())),
                )
            })
            .when(!nested, |row| {
                row.child(
                    div()
                        .flex()
                        .gap_1()
                        .when(!done, |actions| {
                            actions
                                .child(
                                    control(SharedString::from(format!("pause-{node}")))
                                        .label(if paused { "继续" } else { "暂停" })
                                        .disabled(cancelled)
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.shared.update(cx, |state, cx| {
                                                state.queue.pause_root(id, &pause_node, !paused);
                                                cx.notify();
                                            });
                                        })),
                                )
                                .child(
                                    control(SharedString::from(format!("cancel-{node}")))
                                        .label("取消")
                                        .disabled(cancelled)
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.shared.update(cx, |state, cx| {
                                                state.queue.cancel_root(id, &cancel_node);
                                                cx.notify();
                                            });
                                        })),
                                )
                        })
                        .when(retry, |actions| {
                            actions.child(
                                control(SharedString::from(format!("retry-{node}")))
                                    .label(if batch_phase.active() {
                                        "批次结束后重试"
                                    } else {
                                        "重试"
                                    })
                                    .disabled(batch_phase.active())
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.shared.update(cx, |state, cx| {
                                            state.queue.retry_root(id, &retry_node);
                                            cx.notify();
                                        });
                                    })),
                            )
                        }),
                )
            })
    }
}
