//! Selected batch and file inspector.
use super::*;

impl View {
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
        let output = file.output.clone();
        Some(
            div()
                .flex()
                .flex_col()
                .gap_4()
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(if file.parent_id.is_none() {
                            "根归档详情"
                        } else if file.depth == 0 {
                            "候选入口详情"
                        } else {
                            "嵌套归档详情"
                        }),
                )
                .child(
                    div()
                        .text_size(px(20.))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(
                            file.path
                                .file_name()
                                .unwrap_or(file.path.as_os_str())
                                .to_string_lossy()
                                .into_owned(),
                        ),
                )
                .child(div().text_base().child(file_status(&file)))
                .child(div().text_sm().child(file.path.display().to_string()))
                .child(
                    div()
                        .text_base()
                        .child(format!("当前阶段：{}", stage_label(&file.stage))),
                )
                .when(!file.backend.is_empty(), |el| {
                    el.child(
                        div()
                            .text_base()
                            .child(format!("后端：{}", file_backend_label(&file.backend))),
                    )
                })
                .when(file.committed, |el| {
                    el.child(div().text_base().child("本文件输出已成功提交"))
                })
                .when_some(file.root_outcome.clone(), |el, outcome| {
                    el.child(
                        div()
                            .text_base()
                            .child(format!("包含嵌套归档的结果：{}", state_label(&outcome))),
                    )
                })
                .when_some(file.volumes.clone(), |el, volumes| {
                    el.child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .border_t_1()
                            .border_color(cx.theme().border)
                            .pt_3()
                            .child(div().text_base().child(if volumes.candidates.len() > 1 {
                                "分卷候选"
                            } else {
                                "输入成员"
                            }))
                            .when(!volumes.diagnostic.is_empty(), |el| {
                                el.child(
                                    div()
                                        .text_sm()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(volumes.diagnostic.clone()),
                                )
                            })
                            .children(volumes.candidates.iter().enumerate().map(
                                |(index, candidate)| {
                                    let adopted = volumes.selected.iter().any(|selected| {
                                        selected.len() == candidate.len()
                                            && selected.iter().all(|path| candidate.contains(path))
                                    });
                                    div()
                                        .flex()
                                        .flex_col()
                                        .gap_1()
                                        .py_2()
                                        .child(div().text_sm().child(format!(
                                            "候选 {} · {} 卷{}",
                                            index + 1,
                                            candidate.len(),
                                            if adopted { " · 已采用" } else { "" }
                                        )))
                                        .children(candidate.iter().map(|path| {
                                            div()
                                                .text_sm()
                                                .text_color(cx.theme().muted_foreground)
                                                .child(path.display().to_string())
                                        }))
                                },
                            ))
                            .when(volumes.candidates.is_empty(), |el| {
                                el.children(
                                    volumes.members.iter().map(|path| {
                                        div().text_sm().child(path.display().to_string())
                                    }),
                                )
                            }),
                    )
                })
                .child(
                    control("file-output")
                        .icon(IconName::FolderOpen)
                        .label("打开输出目录")
                        .disabled(output.is_none())
                        .on_click(move |_, _, cx| {
                            if let Some(path) = &output {
                                cx.reveal_path(path);
                            }
                        }),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(div().text_base().child("文件记录"))
                        .children(file.events.iter().rev().take(30).map(|event| {
                            div()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child(event.clone())
                        })),
                )
                .into_any_element(),
        )
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
                    .text_size(px(20.))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(name),
            )
            .child(
                div()
                    .text_sm()
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
                    .text_sm()
                    .child(Icon::new(IconName::Cpu).small())
                    .child(backend),
            )
            .when(phase == Phase::Queued, |el| {
                el.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(div().text_base().child("此任务临时密码"))
                        .child(Input::new(&self.task_password).large().mask_toggle())
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
                                .text_sm()
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
                                    "任务配置"
                                } else {
                                    "配置快照"
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
                        .child(div().text_sm().child(inputs))
                        .child(
                            div()
                                .id("event-log")
                                .max_h(px(150.))
                                .overflow_y_scroll()
                                .text_sm()
                                .children(
                                    events.into_iter().map(|event| div().py_1().child(event)),
                                ),
                        ),
                )
            })
            .into_any_element()
    }
}
