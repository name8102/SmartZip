use super::*;
use gpui::component::{
    h_resizable,
    menu::{DropdownMenu, PopupMenuItem},
    resizable_panel,
    tab::{Tab, TabBar},
    v_resizable,
};

impl View {
    fn archive_actions(&self, paths: &[PathBuf]) -> impl IntoElement {
        let paths = paths.to_vec();
        let shared = self.shared.clone();
        control("archive-actions")
            .label("更多")
            .icon(IconName::ChevronDown)
            .disabled(paths.is_empty())
            .dropdown_menu(move |mut menu, _, cx| {
                for (operation, label, icon) in [
                    (TaskOperation::Test, "完整性校验", IconName::ShieldCheck),
                    (TaskOperation::Detect, "检测内嵌归档", IconName::ScanSearch),
                ] {
                    let running = shared.read(cx).queue.jobs.iter().any(|job| {
                        job.request.operation == operation
                            && job.request.paths == paths
                            && (job.phase.active() || job.phase == Phase::Queued)
                    });
                    let shared = shared.clone();
                    let paths = paths.clone();
                    menu = menu.item(
                        PopupMenuItem::new(label)
                            .icon(icon)
                            .disabled(running || paths.is_empty())
                            .on_click(move |_, _, cx| {
                                shared.update(cx, |state, cx| {
                                    state.enqueue(paths.clone(), operation);
                                    cx.notify();
                                });
                            }),
                    );
                }
                menu
            })
    }

    fn archive_operation_results(
        &mut self,
        paths: &[PathBuf],
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let state = self.shared.read(cx);
        let reports: Vec<_> = [TaskOperation::Test, TaskOperation::Detect]
            .into_iter()
            .filter_map(|operation| {
                let job =
                    state.queue.jobs.iter().rev().find(|job| {
                        job.request.operation == operation && job.request.paths == paths
                    })?;
                Some((
                    job.id,
                    operation,
                    job.phase,
                    job.stage.clone(),
                    operation_report(operation, job.result.as_ref()),
                ))
            })
            .collect();
        div()
            .id("archive-operation-results")
            .max_h(px(180.))
            .overflow_y_scroll()
            .flex_shrink_0()
            .flex()
            .flex_col()
            .children(
                reports
                    .into_iter()
                    .map(|(id, operation, phase, stage, report)| {
                        div()
                            .px_3()
                            .py_2()
                            .border_b_1()
                            .border_color(cx.theme().border)
                            .flex()
                            .flex_col()
                            .gap_2()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_3()
                                    .child(div().flex_1().text_base().child(
                                        if operation == TaskOperation::Test {
                                            "完整性校验"
                                        } else {
                                            "内嵌归档检测"
                                        },
                                    ))
                                    .child(
                                        div()
                                            .text_sm()
                                            .text_color(phase_color(phase, cx))
                                            .child(phase.label()),
                                    )
                                    .when(phase == Phase::Queued, |el| {
                                        el.child(
                                            control(("start-archive-action", id))
                                                .label("立即开始")
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.shared.update(cx, |s, cx| {
                                                        s.queue.start(id);
                                                        cx.notify();
                                                    })
                                                })),
                                        )
                                    })
                                    .when(phase.active() || phase == Phase::Queued, |el| {
                                        el.child(
                                            control(("cancel-archive-action", id))
                                                .label("取消")
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.shared.update(cx, |s, cx| {
                                                        s.queue.cancel(id);
                                                        cx.notify();
                                                    })
                                                })),
                                        )
                                    })
                                    .child(
                                        control(("archive-action-detail", id))
                                            .label("任务详情")
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.shared
                                                    .update(cx, |s, _| s.queue.selected = Some(id));
                                                this.selected_file = None;
                                                this.page(Page::Tasks, cx);
                                            })),
                                    ),
                            )
                            .when(phase.active(), |el| {
                                el.child(
                                    Progress::new(("archive-action-progress", id)).loading(true),
                                )
                            })
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(stage),
                            )
                            .children(report.into_iter().map(|line| div().text_base().child(line)))
                    }),
            )
    }

    pub(super) fn preview(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let state = self.shared.read(cx);
        let selected = state.previews.selected();
        let id = selected.map(|j| j.id);
        let paths = selected
            .map(|j| j.request.paths.clone())
            .unwrap_or_default();
        let phase = selected.map(|j| j.phase);
        let stage = selected.map(|j| j.stage.clone()).unwrap_or_default();
        let count = selected
            .and_then(|j| j.result.as_ref())
            .and_then(|r| r.get("entries"))
            .and_then(|v| v.as_array())
            .map_or(0, Vec::len);
        if let Some(job) = selected.filter(|j| j.result.is_some()) {
            self.archive_cache.entry(job.id).or_insert_with(|| {
                let entries = job
                    .result
                    .as_ref()
                    .and_then(|r| r.get("entries"))
                    .and_then(|v| v.as_array())
                    .map(|entries| {
                        entries
                            .iter()
                            .filter_map(|entry| {
                                Some(crate::archive_browser::ArchiveEntry {
                                    path: entry.get("path")?.as_str()?.into(),
                                    is_dir: entry
                                        .get("is_dir")
                                        .and_then(|v| v.as_bool())
                                        .unwrap_or(false),
                                    size: entry.get("uncompressed_size").and_then(|v| v.as_u64()),
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                crate::archive_browser::ArchiveBrowser::new(entries).map(std::sync::Arc::new)
            });
        }
        let query = self.archive_search.read(cx).value().to_lowercase();
        let browser = id.and_then(|id| self.archive_cache.get(&id));
        let error = browser.and_then(|r| r.as_ref().err()).cloned();
        let mut rows = browser
            .and_then(|r| r.as_ref().ok())
            .map(|b| b.children(&self.archive_dir))
            .unwrap_or_default();
        rows.retain(|r| query.is_empty() || r.name.to_lowercase().contains(&query));
        if self.archive_sort_size {
            rows.sort_by_key(|r| {
                (
                    !r.is_dir,
                    std::cmp::Reverse(r.size.unwrap_or(0)),
                    r.name.clone(),
                )
            });
        }
        let height = state.preferences.density.row_height().max(40.);
        let source = selected.and_then(|job| {
            let result = job.result.as_ref()?;
            let resolved = job.request.resolved.as_ref()?;
            Some(crate::member_preview::MemberSource {
                archive: job.request.paths.first()?.clone(),
                member: PathBuf::new(),
                backend: job.backend.clone(),
                config: resolved.values.backends.clone(),
                allow_password: resolved.values.passwords.mode
                    != smartzip_config::PasswordMode::Off,
                max_bytes: match resolved.values.limits.max_output_bytes {
                    0 => 8 * 1024 * 1024,
                    n => n.min(8 * 1024 * 1024) as usize,
                },
                encoding: result
                    .get("encoding")
                    .and_then(|v| v.as_str())
                    .filter(|v| !["auto", "backend"].contains(v))
                    .map(|v| smartzip_core::EncodingMode::Override(v.into()))
                    .unwrap_or(smartzip_core::EncodingMode::Auto),
                unavailable: embedded_preview_notice(result),
            })
        });
        let tabs: Vec<_> = state
            .previews
            .jobs
            .iter()
            .filter(|j| !j.dismiss_requested)
            .map(|j| (j.id, j.name()))
            .collect();
        let active = phase.is_some_and(|p| p.active() || p == Phase::Queued);
        let ready = browser.is_some_and(|result| result.is_ok());
        let wide = window.viewport_size().width >= px(1050.);
        let root = div()
            .id("preview-page")
            .size_full()
            .min_h_0()
            .flex()
            .flex_col()
            .overflow_hidden()
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| {
                this.shared.update(cx, |state, cx| {
                    state.enqueue(paths.paths().to_vec(), TaskOperation::List);
                    cx.notify();
                });
            }));
        if id.is_none() {
            return root
                .items_center()
                .justify_center()
                .gap_4()
                .child(
                    Icon::new(IconName::FileArchive)
                        .size(px(36.))
                        .text_color(cx.theme().muted_foreground),
                )
                .child(div().text_lg().child("打开归档"))
                .child(
                    control("open-archive")
                        .primary()
                        .icon(IconName::FolderOpen)
                        .label("打开归档…")
                        .on_click(cx.listener(|this, _, _, cx| this.pick(TaskOperation::List, cx))),
                )
                .into_any_element();
        }
        let selected_index = tabs
            .iter()
            .position(|(tab, _)| Some(*tab) == id)
            .unwrap_or(0);
        let tab_bar = TabBar::new("archive-tabs")
            .folder()
            .large()
            .menu(true)
            .max_width(px(230.))
            .selected_index(selected_index)
            .children(tabs.into_iter().map(|(tab, name)| {
                Tab::new()
                    .label(name)
                    .prefix(Icon::new(IconName::FileArchive).size(px(18.)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.shared.update(cx, |state, cx| {
                            state.previews.selected = Some(tab);
                            cx.notify();
                        });
                    }))
                    .suffix(
                        control(("close-tab", tab))
                            .label("×")
                            .tooltip("关闭归档")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.archive_cache.remove(&tab);
                                this.archive_positions.remove(&tab);
                                this.shared.update(cx, |state, cx| {
                                    state.previews.close(tab);
                                    cx.notify();
                                });
                            })),
                    )
            }));
        let actions = self.archive_actions(&paths);
        let operation_results = self.archive_operation_results(&paths, cx);
        let toolbar = div()
            .flex_shrink_0()
            .flex()
            .items_center()
            .gap_2()
            .px_3()
            .py_2()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                control("extract-preview")
                    .primary()
                    .icon(IconName::PackageOpen)
                    .label("解压…")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.shared.update(cx, |state, cx| {
                            state.enqueue_with_hold(paths.clone(), TaskOperation::Extract, true);
                            cx.notify();
                        });
                        this.selected_file = None;
                        this.page(Page::Tasks, cx);
                    })),
            )
            .child(actions)
            .when(active, |el| {
                el.child(
                    control("stop-preview")
                        .label("停止读取")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.shared.update(cx, |state, cx| {
                                if let Some(id) = id {
                                    state.previews.cancel(id);
                                }
                                cx.notify();
                            });
                        })),
                )
            })
            .child(div().flex_1())
            .child(
                control("toggle-member-preview")
                    .label(if self.preview_visible {
                        "收起预览"
                    } else {
                        "内容预览"
                    })
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.preview_visible = !this.preview_visible;
                        cx.notify();
                    })),
            )
            .child(
                control("open-archive")
                    .icon(IconName::FolderOpen)
                    .label("打开…")
                    .on_click(cx.listener(|this, _, _, cx| this.pick(TaskOperation::List, cx))),
            );
        let navigation = div()
            .flex_shrink_0()
            .flex()
            .items_center()
            .gap_2()
            .px_3()
            .py_2()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                control("archive-up")
                    .icon(IconName::ChevronLeft)
                    .tooltip("返回上级")
                    .disabled(self.archive_dir.is_empty())
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.archive_dir.pop();
                        this.member_preview
                            .update(cx, |preview, cx| preview.clear(window, cx));
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .id("archive-breadcrumbs")
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .overflow_x_scroll()
                    .child(
                        control("archive-root")
                            .label("根目录")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.archive_dir.clear();
                                this.member_preview
                                    .update(cx, |preview, cx| preview.clear(window, cx));
                                cx.notify();
                            })),
                    )
                    .children(self.archive_dir.iter().enumerate().map(|(index, part)| {
                        let path = self.archive_dir[..=index].to_vec();
                        div()
                            .flex()
                            .items_center()
                            .flex_shrink_0()
                            .child(
                                Icon::new(IconName::ChevronRight)
                                    .large()
                                    .text_color(cx.theme().muted_foreground),
                            )
                            .child(control(("breadcrumb", index)).label(part.clone()).on_click(
                                cx.listener(move |this, _, window, cx| {
                                    this.archive_dir = path.clone();
                                    this.member_preview
                                        .update(cx, |preview, cx| preview.clear(window, cx));
                                    cx.notify();
                                }),
                            ))
                    })),
            )
            .child(
                div()
                    .w(px(if wide { 220. } else { 150. }))
                    .child(Input::new(&self.archive_search).large().cleanable(true)),
            )
            .child(
                control("sort-archive")
                    .label(if self.archive_sort_size {
                        "大小 ↓"
                    } else {
                        "名称 ↑"
                    })
                    .tooltip("切换排序")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.archive_sort_size = !this.archive_sort_size;
                        cx.notify();
                    })),
            );
        let row_count = rows.len();
        let mut files = div().size_full().min_h_0().flex().flex_col();
        if ready && !rows.is_empty() {
            files = files
                .child(div().flex().items_center().px_3().py_2().gap_3().text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .border_b_1().border_color(cx.theme().border)
                    .child(div().flex_1().child("名称"))
                    .child(div().w(px(90.)).child("大小"))
                    .child(div().w(px(34.))))
                .child(gpui::uniform_list("archive-entries", rows.len(), cx.processor(move |_, range: std::ops::Range<usize>, _, cx| {
                    range.map(|index| {
                        let entry = &rows[index];
                        let path = entry.path.clone();
                        let copy_path = path.clone();
                        let member_source = source.clone();
                        let is_dir = entry.is_dir;
                        div().id(("member-row", index)).w_full().h(px(height)).flex().items_center().gap_3().px_3()
                            .hover(|el| el.bg(cx.theme().accent))
                            .child(Icon::new(if is_dir { IconName::FolderOpen } else { IconName::FileArchive }).large()
                                .text_color(cx.theme().muted_foreground))
                            .child(control(("open-member", index)).flex_1().min_w_0().accessibility_label(entry.name.clone())
                                .child(div().w_full().min_w_0().text_ellipsis().child(entry.name.clone()))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    if is_dir {
                                        if let Some(parts) = crate::archive_browser::ArchiveBrowser::path_parts(&path) {
                                            this.archive_dir = parts;
                                        }
                                        this.member_preview.update(cx, |preview, cx| preview.clear(window, cx));
                                    } else if let Some(mut source) = member_source.clone() {
                                        source.member = path.clone().into();
                                        this.preview_visible = true;
                                        this.member_preview.update(cx, |preview, cx| preview.open(source, window, cx));
                                    }
                                    cx.notify();
                                })))
                            .child(div().w(px(90.)).text_sm().text_color(cx.theme().muted_foreground)
                                .child(entry.size.map(size_label).unwrap_or_else(|| "—".into())))
                            .child(control(("copy-member", index)).w(px(34.)).icon(IconName::Copy).tooltip("复制路径")
                                .on_click(move |_, _, cx| cx.write_to_clipboard(gpui::ClipboardItem::new_string(copy_path.clone()))))
                    }).collect::<Vec<_>>()
                })).flex_1().min_h_0().w_full());
        } else {
            files = files.child(
                div()
                    .flex_1()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_base()
                    .text_color(cx.theme().muted_foreground)
                    .child(if ready {
                        "当前目录没有匹配项"
                    } else if active {
                        "正在读取归档…"
                    } else {
                        phase.map(Phase::label).unwrap_or("打开归档")
                    }),
            );
        }
        let contents = if self.preview_visible && ready {
            let group = if wide {
                h_resizable("archive-content-horizontal")
            } else {
                v_resizable("archive-content-vertical")
            };
            group
                .child(
                    resizable_panel()
                        .size_range(px(140.)..px(10000.))
                        .child(files),
                )
                .child(
                    resizable_panel()
                        .size(px(if wide { 360. } else { 260. }))
                        .size_range(px(140.)..px(10000.))
                        .child(
                            div()
                                .size_full()
                                .min_h_0()
                                .when(wide, |el| el.border_l_1())
                                .when(!wide, |el| el.border_t_1())
                                .border_color(cx.theme().border)
                                .child(self.member_preview.clone()),
                        ),
                )
                .into_any_element()
        } else {
            files.into_any_element()
        };
        root.child(tab_bar)
            .child(toolbar)
            .when(!stage.is_empty() && phase != Some(Phase::Completed), |el| {
                el.child(
                    div()
                        .px_3()
                        .py_2()
                        .text_base()
                        .text_color(if matches!(phase, Some(Phase::Failed | Phase::Partial)) {
                            cx.theme().danger
                        } else {
                            cx.theme().muted_foreground
                        })
                        .child(stage),
                )
            })
            .when(active, |el| {
                el.child(Progress::new("listing-progress").loading(true))
            })
            .when_some(error, |el, error| {
                el.child(
                    div()
                        .px_3()
                        .py_2()
                        .text_base()
                        .text_color(cx.theme().danger)
                        .child(error),
                )
            })
            .child(operation_results)
            .when(ready, |el| el.child(navigation))
            .child(div().flex_1().min_h_0().overflow_hidden().child(contents))
            .child(
                div()
                    .flex()
                    .items_center()
                    .px_3()
                    .py_1()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(if ready {
                        format!("{row_count} 项 · 共 {count} 个条目")
                    } else {
                        phase
                            .map(|phase| phase.label().to_owned())
                            .unwrap_or_default()
                    }),
            )
            .into_any_element()
    }
}

// A scanner may select an ordinary archive at byte zero. It is still readable
// through the original file; only nonzero embedded offsets need materialization.
fn embedded_preview_notice(result: &serde_json::Value) -> Option<String> {
    result
        .get("embedded_offset")
        .and_then(serde_json::Value::as_u64)
        .filter(|offset| *offset > 0)
        .map(|_| "内嵌归档内容暂需解压后查看".into())
}

fn operation_report(operation: TaskOperation, result: Option<&serde_json::Value>) -> Vec<String> {
    let Some(result) = result else {
        return vec![];
    };
    if operation == TaskOperation::Detect {
        let Some(count) = result.get("embedded_count").and_then(|v| v.as_u64()) else {
            return vec![];
        };
        let mut lines = vec![format!("检测到 {} 个内嵌归档候选", count)];
        if let Some(reason) = result.get("reason").and_then(|v| v.as_str()) {
            lines.push(reason.into());
        }
        return lines;
    }
    let mut lines = vec![];
    for file in result
        .get("files")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
    {
        let integrity = match file.get("integrity").and_then(|v| v.as_str()) {
            Some("intact") => "完好",
            Some("corrupt") => "确认损坏",
            Some("incomplete") => "缺少分卷",
            _ => "尚不能确定",
        };
        let coverage = match file.get("coverage").and_then(|v| v.as_str()) {
            Some("complete") => "完整",
            Some("partial") => "部分",
            _ => "未检查",
        };
        lines.push(format!("完整性：{integrity} · 检查范围：{coverage}"));
        for (field, label) in [
            ("confirmed_volumes", "确认损坏的卷"),
            ("suspect_groups", "疑似损坏的分卷组"),
            ("missing_volumes", "缺失卷"),
            ("unreadable_volumes", "无法读取的卷"),
            ("unchecked_volumes", "未检查的卷"),
        ] {
            let count = file
                .get(field)
                .and_then(|v| v.as_array())
                .map_or(0, Vec::len);
            if count > 0 {
                lines.push(format!("{label}：{count}"));
            }
        }
        for reason in file
            .get("stop_reasons")
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
            .filter_map(|v| v.as_str())
            .take(3)
        {
            lines.push(reason.into());
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_zero_archive_can_preview_members() {
        assert!(embedded_preview_notice(&serde_json::json!({"embedded_offset": 0})).is_none());
        assert!(embedded_preview_notice(&serde_json::json!({"embedded_offset": null})).is_none());
        assert!(embedded_preview_notice(&serde_json::json!({"embedded_offset": 512})).is_some());
    }

    #[test]
    fn unknown_integrity_does_not_promote_suspect_volumes_to_confirmed_damage() {
        let result = serde_json::json!({"files": [{"integrity":"unknown", "coverage":"partial", "suspect_groups":[{"members":["a.001","a.002"]}], "unchecked_volumes":["a.002"]}]});
        let report = operation_report(TaskOperation::Test, Some(&result));
        assert_eq!(
            report,
            vec![
                "完整性：尚不能确定 · 检查范围：部分",
                "疑似损坏的分卷组：1",
                "未检查的卷：1"
            ]
        );
    }

    #[test]
    fn failed_detection_does_not_claim_zero_embedded_archives() {
        assert!(operation_report(
            TaskOperation::Detect,
            Some(&serde_json::json!({"error":"unreadable"}))
        )
        .is_empty());
        assert_eq!(
            operation_report(
                TaskOperation::Detect,
                Some(&serde_json::json!({"embedded_count":0}))
            ),
            vec!["检测到 0 个内嵌归档候选"]
        );
    }
}
