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
        div()
            .id("archive-drop")
            .px_4()
            .py_6()
            .items_center()
            .text_center()
            .border_1()
            .border_color(cx.theme().border)
            .rounded_lg()
            .bg(cx.theme().secondary)
            .flex()
            .flex_col()
            .gap_2()
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| {
                this.shared.update(cx, |state, cx| {
                    state.enqueue(paths.paths().to_vec(), TaskOperation::Extract);
                    cx.notify();
                });
            }))
            .child(
                Icon::new(IconName::PackageOpen)
                    .size(px(28.))
                    .text_color(cx.theme().muted_foreground),
            )
            .child(div().text_base().child("拖进来，就开始解压"))
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("多个文件作为同一批次处理"),
            )
            .child(
                control("choose")
                    .primary()
                    .label("选择文件…")
                    .on_click(cx.listener(|this, _, _, cx| this.pick(TaskOperation::Extract, cx))),
            )
    }
    pub(super) fn actions(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let paused = self.shared.read(cx).queue.paused;
        div()
            .flex()
            .gap_2()
            .flex_wrap()
            .child(
                control("extract")
                    .primary()
                    .icon(IconName::Plus)
                    .label("添加解压")
                    .on_click(cx.listener(|this, _, _, cx| this.pick(TaskOperation::Extract, cx))),
            )
            .child(
                control("prepare-extract")
                    .icon(IconName::SlidersHorizontal)
                    .label("添加并配置…")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.pick_with_hold(TaskOperation::Extract, true, cx);
                    })),
            )
            .child(
                control("toggle-details")
                    .label("详情栏")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.change_appearance(|p| p.detail_visible = !p.detail_visible, cx)
                    })),
            )
            .child(
                control("clear-finished")
                    .label("移除已结束项")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.shared.update(cx, |s, cx| {
                            s.queue.remove_finished();
                            cx.notify();
                        });
                        this.selected_file = None;
                        this.task_selection.clear();
                    })),
            )
            .when(!self.task_selection.is_empty(), |el| {
                el.child(
                    control("cancel-selected")
                        .label(format!("取消所选 {} 项", self.task_selection.len()))
                        .on_click(cx.listener(|this, _, _, cx| {
                            let selected: Vec<_> = this.task_selection.drain().collect();
                            this.shared.update(cx, |s, cx| {
                                for (id, root) in selected {
                                    s.queue.cancel_root(id, &root);
                                }
                                cx.notify();
                            });
                        })),
                )
                .child(
                    control("retry-selected")
                        .label("重试所选失败项")
                        .on_click(cx.listener(|this, _, _, cx| {
                            let selected: Vec<_> = this.task_selection.drain().collect();
                            this.shared.update(cx, |s, cx| {
                                let mut count = 0;
                                for (id, root) in selected {
                                    count += usize::from(s.queue.retry_root(id, &root).is_some());
                                }
                                s.message = format!(
                                    "已添加 {count} 个可重试归档；仍在执行的批次需结束后重试。"
                                );
                                cx.notify();
                            });
                        })),
                )
            })
            .child(
                control("pause")
                    .label(if paused {
                        "开始全部等待任务"
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
    }
    pub(super) fn task_overview(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let queue = &self.shared.read(cx).queue;
        let mut counts = [0usize; 5];
        for job in &queue.jobs {
            if job.files.is_empty() {
                counts[0] += job.request.paths.len();
                counts[job_category(job.phase)] += job.request.paths.len();
                continue;
            }
            for file in job.files.iter().filter(|f| f.parent_id.is_none()) {
                counts[0] += 1;
                counts[file_category(file)] += 1;
            }
        }
        div()
            .flex()
            .flex_col()
            .gap_4()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .text_size(px(24.))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child("任务中心"),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("按归档管理进度，展开查看嵌套文件"),
                            ),
                    )
                    .child(
                        control("queue-options")
                            .icon(if self.queue_settings_open {
                                IconName::ChevronDown
                            } else {
                                IconName::SlidersHorizontal
                            })
                            .label(if self.queue_settings_open {
                                "收起默认配置"
                            } else {
                                "展开默认配置"
                            })
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.queue_settings_open = !this.queue_settings_open;
                                this.shared.update(cx, |s, _| {
                                    s.preferences.queue_settings_open = this.queue_settings_open;
                                    s.persist_preferences();
                                });
                                cx.notify();
                            })),
                    ),
            )
            .child(
                div().flex().gap_2().flex_wrap().children(
                    ["全部", "进行中", "待处理", "已完成", "失败 / 取消"]
                        .into_iter()
                        .enumerate()
                        .map(|(index, label)| {
                            control(("task-filter", index))
                                .label(format!("{label}  {}", counts[index]))
                                .when(self.task_filter == index, |button| button.primary())
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.task_filter = index;
                                    cx.notify();
                                }))
                        }),
                ),
            )
    }

    pub(super) fn tasks(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.shared.read(cx);
        let query = self.search.read(cx).value().to_lowercase();
        let height = state.preferences.density.row_height() + 20.;
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
        let list_height = (rows.len() as f32 * height).clamp(height, 460.);
        gpui::uniform_list(
            "task-rows",
            rows.len(),
            cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
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
                                    .px_3()
                                    .bg(cx.theme().sidebar)
                                    .border_b_1()
                                    .border_color(cx.theme().border)
                                    .child(
                                        div().flex_1().min_w_0().flex().items_center().child(
                                            control(("batch", id))
                                                .label(format!("批次 {id} · {name}"))
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.selected_file = None;
                                                    this.shared.update(cx, |s, cx| {
                                                        s.queue.selected = Some(id);
                                                        cx.notify();
                                                    });
                                                })),
                                        ),
                                    )
                                    .child(div().text_xs().child(format!(
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
                                    cx,
                                ))
                                .into_any_element(),
                        };
                        row
                    })
                    .collect::<Vec<_>>()
            }),
        )
        .h(px(list_height))
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
            .px_3()
            .py_2()
            .min_h(px(
                self.shared.read(cx).preferences.density.row_height() + 20.
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
                    control(SharedString::from(format!("bulk-{node}")))
                        .label(if bulk_selected { "✓" } else { "选择" })
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
                    .child(
                        control(SharedString::from(format!("select-{node}")))
                            .justify_start()
                            .label(name)
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
                            .text_xs()
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
            .child(div().w(px(125.)).text_xs().child(status))
            .when(file.state == "running" && file.progress.is_some(), |row| {
                row.child(
                    div()
                        .w(px(45.))
                        .text_xs()
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

    pub(super) fn file_detail(&mut self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let (job_id, node) = self.selected_file.as_ref()?;
        let job = self
            .shared
            .read(cx)
            .queue
            .jobs
            .iter()
            .find(|j| j.id == *job_id)?;
        let file = job.files.iter().find(|f| f.node_id == *node)?.clone();
        let job_key = *job_id;
        let output = file.output.clone();
        let reports = crate::path_reports::reports_for_file(
            job.result.as_ref(),
            &file.path,
            file.node_id.as_str(),
        );
        let mapping_panels = reports
            .into_iter()
            .map(|report| {
                self.mapping_report(format!("file-{job_key}-{}", report.digest), report, cx)
            })
            .collect::<Vec<_>>();
        Some(div().flex().flex_col().gap_4()
            .child(div().text_xs().text_color(cx.theme().muted_foreground).child(if file.parent_id.is_none() { "根归档详情" } else if file.depth == 0 { "候选入口详情" } else { "嵌套归档详情" }))
            .child(div().text_size(px(18.)).font_weight(gpui::FontWeight::SEMIBOLD).child(file.path.file_name().unwrap_or(file.path.as_os_str()).to_string_lossy().into_owned()))
            .child(div().text_sm().child(file_status(&file)))
            .child(div().text_xs().child(file.path.display().to_string()))
            .child(div().text_sm().child(format!("当前阶段：{}", stage_label(&file.stage))))
            .when(!file.backend.is_empty(), |el| el.child(div().text_sm().child(format!("后端：{}", file_backend_label(&file.backend)))))
            .when(file.committed, |el| el.child(div().text_sm().child("本文件输出已成功提交")))
            .when_some(file.root_outcome.clone(), |el, outcome| el.child(div().text_sm().child(format!("包含嵌套归档的结果：{}", state_label(&outcome)))))
            .when_some(file.volumes.clone(), |el, volumes| {
                el.child(div().flex().flex_col().gap_2().border_t_1().border_color(cx.theme().border).pt_3()
                    .child(div().text_sm().child(if volumes.candidates.len() > 1 { "分卷候选" } else { "输入成员" }))
                    .when(!volumes.diagnostic.is_empty(), |el| el.child(div().text_xs().text_color(cx.theme().muted_foreground).child(volumes.diagnostic.clone())))
                    .children(volumes.candidates.iter().enumerate().map(|(index, candidate)| {
                        let adopted = volumes.selected.iter().any(|selected| selected.len() == candidate.len() && selected.iter().all(|path| candidate.contains(path)));
                        div().flex().flex_col().gap_1().py_2()
                            .child(div().text_xs().child(format!("候选 {} · {} 卷{}", index + 1, candidate.len(), if adopted { " · 已采用" } else { "" })))
                            .children(candidate.iter().map(|path| div().text_xs().text_color(cx.theme().muted_foreground).child(path.display().to_string())))
                    }))
                    .when(volumes.candidates.is_empty(), |el| el.children(volumes.members.iter().map(|path| div().text_xs().child(path.display().to_string())))))
            })
            .children(mapping_panels)
            .child(control("file-output").icon(IconName::FolderOpen).label("打开输出目录").disabled(output.is_none()).on_click(move |_, _, cx| { if let Some(path) = &output { cx.reveal_path(path); } }))
            .child(div().border_t_1().border_color(cx.theme().border).pt_4().text_xs().text_color(cx.theme().muted_foreground).child("暂停在当前阶段安全结束后生效。取消会停止该根归档及其嵌套文件，并等待清理。"))
            .child(div().flex().flex_col().gap_2().child(div().text_sm().child("文件记录"))
                .children(file.events.iter().rev().take(30).map(|event| div().text_xs().text_color(cx.theme().muted_foreground).child(event.clone()))))
            .into_any_element())
    }

    pub(super) fn detail(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.quick {
            if let Some(detail) = self.file_detail(cx) {
                return detail;
            }
        }
        let queue = &self.shared.read(cx).queue;
        let selected = if self.quick {
            queue
                .jobs
                .iter()
                .find(|job| job.phase.active())
                .or_else(|| queue.selected())
        } else {
            queue.selected()
        };
        let Some(job) = selected else {
            return div()
                .p_4()
                .text_color(cx.theme().muted_foreground)
                .child("选择或拖入归档开始")
                .into_any_element();
        };
        let id = job.id;
        let name = job.name();
        let stage = job.stage.clone();
        let backend = job.backend_label();
        let inputs = job
            .request
            .paths
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join("\n");
        let events = job
            .events
            .iter()
            .rev()
            .take(60)
            .cloned()
            .collect::<Vec<_>>();
        let output = job.outputs.last().cloned();
        let phase = job.phase;
        let has_task_password = !job.request.settings.passwords.is_empty();
        let percent = job.progress;
        let renamed_count = job.renamed_count;
        let reports = crate::path_reports::reports_from_result(job.result.as_ref());
        let result = job
            .result
            .as_ref()
            .map(|r| serde_json::to_string_pretty(r).unwrap_or_default());
        if self.detail_state != Some((id, phase)) {
            if self
                .detail_state
                .is_none_or(|(previous_id, _)| previous_id != id)
                || matches!(
                    phase,
                    Phase::Completed | Phase::Partial | Phase::Failed | Phase::Cancelled
                )
            {
                self.task_config_open = phase == Phase::Queued;
            }
            self.detail_state = Some((id, phase));
        }
        div()
            .flex()
            .flex_col()
            .gap_4()
            .min_w_0()
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("任务详情"),
            )
            .child(
                div()
                    .text_size(px(18.))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(name),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(phase_color(phase, cx))
                    .child(stage),
            )
            .when(phase.active(), |el| {
                el.child(
                    Progress::new("current-progress")
                        .small()
                        .value(percent.unwrap_or_default())
                        .loading(percent.is_none() && phase == Phase::Running)
                        .accessibility_label("当前归档操作进度"),
                )
            })
            .child(
                div()
                    .flex()
                    .gap_2()
                    .items_center()
                    .text_xs()
                    .child(Icon::new(IconName::Cpu).small())
                    .child(backend),
            )
            .when(phase == Phase::Queued, |el| {
                el.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(div().text_sm().child("此任务临时密码"))
                        .child(Input::new(&self.task_password).mask_toggle())
                        .child(
                            div()
                                .flex()
                                .gap_2()
                                .child(
                                    control("set-task-password")
                                        .label(if has_task_password {
                                            "替换密码"
                                        } else {
                                            "用于此任务"
                                        })
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            let value =
                                                this.task_password.read(cx).value().to_string();
                                            if value.is_empty() {
                                                return;
                                            }
                                            let result = this.shared.update(cx, |state, cx| {
                                                let result =
                                                    state.queue.set_task_password(id, Some(value));
                                                cx.notify();
                                                result
                                            });
                                            match result {
                                                Ok(()) => {
                                                    this.task_password.update(cx, |input, cx| {
                                                        input.set_value("", window, cx)
                                                    })
                                                }
                                                Err(error) => {
                                                    this.shared.update(cx, |state, cx| {
                                                        state.message = error.into();
                                                        cx.notify();
                                                    })
                                                }
                                            }
                                        })),
                                )
                                .child(
                                    control("clear-task-password")
                                        .label("清除")
                                        .disabled(!has_task_password)
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.shared.update(cx, |state, cx| {
                                                let _ = state.queue.set_task_password(id, None);
                                                cx.notify();
                                            });
                                        })),
                                ),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(if has_task_password {
                                    "已设置 · 仅用于此任务，不保存到密码库"
                                } else {
                                    "仅用于此任务，不保存到密码库"
                                }),
                        ),
                )
            })
            .when(!self.quick, |el| {
                el.child(
                    div()
                        .border_t_1()
                        .border_b_1()
                        .border_color(cx.theme().border)
                        .py_3()
                        .child(
                            control("toggle-task-config")
                                .icon(if self.task_config_open {
                                    IconName::ChevronDown
                                } else {
                                    IconName::ChevronRight
                                })
                                .label(if phase == Phase::Queued {
                                    "任务配置 · 可单独调整"
                                } else {
                                    "任务配置 · 执行快照"
                                })
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.task_config_open = !this.task_config_open;
                                    cx.notify();
                                })),
                        )
                        .when(self.task_config_open, |el| {
                            el.child(self.quick_controls(Some(id), cx))
                        }),
                )
            })
            .child(
                div()
                    .flex()
                    .gap_2()
                    .flex_wrap()
                    .when(phase == Phase::Queued, |el| {
                        el.child(control("start").primary().label("开始任务").on_click(
                            cx.listener(move |this, _, _, cx| {
                                this.shared.update(cx, |state, cx| {
                                    state.queue.start(id);
                                    cx.notify();
                                })
                            }),
                        ))
                    })
                    .when(phase.active() || phase == Phase::Queued, |el| {
                        el.child(
                            control("cancel")
                                .label("取消任务")
                                .disabled(!phase.active() && phase != Phase::Queued)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.shared.update(cx, |state, cx| {
                                        state.queue.cancel(id);
                                        cx.notify();
                                    })
                                })),
                        )
                    })
                    .when(phase == Phase::Queued, |el| {
                        el.child(
                            control("up")
                                .label("提前执行")
                                .disabled(phase != Phase::Queued)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.shared.update(cx, |state, cx| {
                                        state.queue.move_up(id);
                                        cx.notify();
                                    })
                                })),
                        )
                    })
                    .when(!phase.active() && phase != Phase::Queued, |el| {
                        el.child(
                            control("retry")
                                .label("重新运行")
                                .disabled(phase.active() || phase == Phase::Queued)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.shared.update(cx, |state, cx| {
                                        if let Some(job) =
                                            state.queue.jobs.iter().find(|j| j.id == id)
                                        {
                                            let mut request = job.request.clone();
                                            if request.operation == TaskOperation::Extract {
                                                request.settings.queued_task_id = None;
                                            }
                                            state.queue.enqueue(request);
                                        }
                                        cx.notify();
                                    })
                                })),
                        )
                    })
                    .child(
                        control("reveal")
                            .primary()
                            .icon(IconName::FolderOpen)
                            .label("打开输出目录")
                            .disabled(output.is_none())
                            .on_click(move |_, _, cx| {
                                if let Some(path) = &output {
                                    cx.reveal_path(path);
                                }
                            }),
                    )
                    .when(self.diagnostics_open, |el| {
                        el.child(
                            control("copy-result")
                                .label("复制结果 JSON")
                                .disabled(result.is_none())
                                .on_click(move |_, _, cx| {
                                    if let Some(result) = &result {
                                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                                            result.clone(),
                                        ));
                                    }
                                }),
                        )
                    }),
            )
            .when(renamed_count > 0 && reports.is_empty(), |el| {
                el.child(div().text_sm().child(format!(
                    "已调整 {renamed_count} 个名称 · 完整映射将在任务结束后显示"
                )))
            })
            .children(reports.into_iter().map(|report| {
                self.mapping_report(format!("task-{id}-{}", report.digest), report, cx)
            }))
            .child(
                control("toggle-diagnostics")
                    .icon(if self.diagnostics_open {
                        IconName::ChevronDown
                    } else {
                        IconName::ChevronRight
                    })
                    .label("技术诊断")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.diagnostics_open = !this.diagnostics_open;
                        cx.notify();
                    })),
            )
            .when(self.diagnostics_open, |el| {
                el.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .min_w_0()
                        .child(div().text_xs().child(inputs))
                        .child(
                            div()
                                .id("event-log")
                                .max_h(px(150.))
                                .overflow_y_scroll()
                                .text_xs()
                                .children(
                                    events.into_iter().map(|event| div().py_1().child(event)),
                                ),
                        ),
                )
            })
            .into_any_element()
    }
}
