use super::*;
use smartzip_core::PathMappingReport;

impl View {
    pub(super) fn mapping_report(
        &mut self,
        key: String,
        report: PathMappingReport,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let page_size = crate::path_reports::PAGE_SIZE;
        let total = report.entries.len();
        let max_offset = total.saturating_sub(1) / page_size * page_size;
        let offset = self
            .mapping_pages
            .get(&key)
            .copied()
            .unwrap_or(0)
            .min(max_offset);
        let previous = key.clone();
        let next = key.clone();
        div()
            .flex()
            .flex_col()
            .gap_2()
            .border_t_1()
            .border_color(cx.theme().border)
            .pt_3()
            .child(div().text_sm().child(format!(
                "{} {} 个名称 · 完整映射共 {} 项{}",
                if report.tentative {
                    "计划调整"
                } else {
                    "已调整"
                },
                report.changed_count(),
                total,
                if report.tentative {
                    " · 预检计划，尚未应用"
                } else {
                    ""
                }
            )))
            .when(!report.archive_path.is_empty(), |el| {
                let archive = std::path::Path::new(&report.archive_path);
                el.child(div().text_xs().child(format!(
                        "归档：{}",
                        archive
                            .file_name()
                            .unwrap_or(archive.as_os_str())
                            .to_string_lossy()
                    )))
            })
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!(
                        "模式：{} · 文件系统：{}",
                        if report.policy.mode == smartzip_core::PathMode::Portable {
                            "Portable"
                        } else {
                            "Native"
                        },
                        report.policy.fs_kind
                    )),
            )
            .child(
                div()
                    .id(SharedString::from(format!("mapping-{key}")))
                    .max_h(px(300.))
                    .overflow_y_scroll()
                    .children(report.entries.into_iter().skip(offset).take(page_size).map(
                        |entry| {
                            let reasons = entry
                                .reasons
                                .into_iter()
                                .map(crate::path_reports::reason_label)
                                .collect::<Vec<_>>()
                                .join("、");
                            div()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .py_2()
                                .child(div().text_xs().child(format!(
                                    "{} → {}",
                                    entry.display_name, entry.final_relative
                                )))
                                .when(!reasons.is_empty(), |el| {
                                    el.child(
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(reasons),
                                    )
                                })
                        },
                    )),
            )
            .child(
                div()
                    .flex()
                    .gap_2()
                    .items_center()
                    .child(
                        control(SharedString::from(format!("mapping-prev-{key}")))
                            .label("上一页")
                            .disabled(offset == 0)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.mapping_pages
                                    .insert(previous.clone(), offset.saturating_sub(page_size));
                                cx.notify();
                            })),
                    )
                    .child(div().text_xs().child(format!(
                        "第 {} / {} 页",
                        offset / page_size + 1,
                        total.div_ceil(page_size).max(1)
                    )))
                    .child(
                        control(SharedString::from(format!("mapping-next-{key}")))
                            .label("下一页")
                            .disabled(offset + page_size >= total)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.mapping_pages.insert(next.clone(), offset + page_size);
                                cx.notify();
                            })),
                    ),
            )
            .into_any_element()
    }
}
