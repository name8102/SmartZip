use super::*;

impl View {
    pub(super) fn history(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.shared.read(cx);
        let rows = state.history.clone();
        let detail = state.history_detail.clone();
        let query = state.history_query.clone();
        let more = state.history_more;
        let loading = state.history_loading;
        let selected = state.history_detail_request.clone();
        let busy = state.library_busy;
        div().flex().flex_col().gap_4()
            .child(div().flex().gap_2().items_center()
                .child(div().flex_1().child(Input::new(&self.history_search).cleanable(true)))
                .child(control("history-search").primary().label("搜索").disabled(loading).on_click(cx.listener(|this, _, _, cx| {
                    let text = this.history_search.read(cx).value().to_string();
                    this.shared.update(cx, |s, cx| { s.history_query.text = text; s.history_query.offset=0; s.reload_history(); cx.notify(); });
                })))
                .child(control("history-reload").label("刷新").disabled(loading).on_click(cx.listener(|this, _, _, cx| this.shared.update(cx, |s, cx| {s.reload_history();cx.notify();})))))
            .child(div().flex().gap_2().flex_wrap().children([("", "全部"), ("completed", "完成"), ("partial", "部分完成"), ("failed", "失败"), ("cancelled", "取消")].into_iter().enumerate().map(|(i,(status,label))| control(("history-filter",i)).label(label).when(query.status==status, |b| b.primary()).on_click(cx.listener(move |this,_,_,cx| this.shared.update(cx,|s,cx| {s.history_query.status=status.into();s.history_query.offset=0;s.reload_history();cx.notify();}))))))
            .when(loading, |el| el.child(div().text_sm().child("正在读取历史…")))
            .when(rows.is_empty() && !loading, |el| el.child(empty_state("没有历史记录", "尝试调整筛选条件，或添加归档开始处理。", cx)))
            .child(div().id("history-list").max_h(px(380.)).overflow_y_scroll().border_1().border_color(cx.theme().border).rounded_lg()
                .children(rows.into_iter().map(|row| {
                    let id = row.task.id.clone();
                    let name = std::path::Path::new(&row.input).file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| row.task.kind.clone());
                    div().id(SharedString::from(format!("history-{}",id))).flex().items_center().gap_3().px_3().py_3().min_h(px(self.shared.read(cx).preferences.density.row_height())).border_b_1().border_color(cx.theme().border)
                        .when(selected.as_ref()==Some(&id), |el| el.bg(cx.theme().sidebar_accent))
                        .child(control(SharedString::from(id.clone())).label(name).on_click(cx.listener(move |this,_,_,cx| this.shared.update(cx,|s,cx|{s.load_detail(id.clone());cx.notify();}))))
                        .child(div().flex_1().min_w_0().text_xs().text_ellipsis().text_color(cx.theme().muted_foreground).child(row.task.started_at))
                        .child(div().w(px(90.)).text_sm().child(state_label(&row.task.status).to_owned()))
                        .child(div().text_xs().child(format!("{} 项",row.files)))
                })))
            .child(div().flex().items_center().gap_3()
                .child(control("history-prev").label("上一页").disabled(query.offset==0 || loading).on_click(cx.listener(|this,_,_,cx| this.shared.update(cx,|s,cx| {s.history_query.offset=s.history_query.offset.saturating_sub(library::PAGE_SIZE);s.reload_history();cx.notify();}))))
                .child(format!("第 {} 页",query.offset/library::PAGE_SIZE+1))
                .child(control("history-next").label("下一页").disabled(!more || loading).on_click(cx.listener(|this,_,_,cx| this.shared.update(cx,|s,cx|{s.history_query.offset+=library::PAGE_SIZE;s.reload_history();cx.notify();})))))
            .when_some(detail, |el, detail| {
                let output=detail.task.output_path.clone();
                let inputs = detail.inputs.clone();
                let export=detail.clone();
                let mappings = detail.files.iter().filter_map(|file| file.path_report_json.as_deref().and_then(|text| serde_json::from_str(text).ok()).map(|report| (format!("history-{}", file.id), file.input_path.clone(), report))).collect::<Vec<_>>();
                let mapping_panels = mappings.into_iter().map(|(key, input, report)| div().flex().flex_col().gap_2().child(div().text_sm().child(input)).child(self.mapping_report(key, report, cx)).into_any_element()).collect::<Vec<_>>();
                el.child(div().flex().flex_col().gap_3().border_t_1().border_color(cx.theme().border).pt_5()
                    .child(div().text_lg().child(format!("{} · {}",state_label(&detail.task.status),detail.task.started_at)))
                    .child(div().flex().gap_2().flex_wrap()
                        .child(control("history-output").label("定位输出 / 路径失效时复制").disabled(output.is_none()).on_click(move |_,_,cx| {if let Some(path)=&output { if std::path::Path::new(path).exists() {cx.reveal_path(std::path::Path::new(path));} else {cx.write_to_clipboard(gpui::ClipboardItem::new_string(path.clone()));} }}))
                        .child(control("history-rerun").label("重新添加并配置").disabled(inputs.is_empty() || detail.task.kind!="extract").on_click(cx.listener(move |this,_,_,cx| {this.shared.update(cx,|s,cx|{s.enqueue_with_hold(inputs.clone(),TaskOperation::Extract,true);cx.notify();});this.page=Page::Tasks;})))
                        .child(control("history-export").label("导出诊断…").disabled(busy).on_click(cx.listener(move |this,_,_,cx| {
                            let receiver=cx.prompt_for_new_path(std::path::Path::new("."),Some("smartzip-diagnostic.json"));
                            let shared=this.shared.clone();let detail=export.clone();
                            cx.spawn(async move |_,cx|{if let Ok(Ok(Some(path)))=receiver.await {shared.update(cx,|s,cx|{s.background(move || LibraryMessage::Mutation(library::export_diagnostics(&detail,&path).map(|_|"诊断已导出（包含文件路径，不包含密码）".into())));cx.notify();});}}).detach();
                        }))))
                    .child(div().text_xs().text_color(cx.theme().muted_foreground).child("历史记录保留原执行事实；源文件或输出可能已被移动。"))
                    .child(div().id("history-files").max_h(px(360.)).overflow_y_scroll().children(detail.files.iter().map(|file| div().flex().flex_col().gap_1().py_3().border_b_1().border_color(cx.theme().border)
                        .child(div().text_sm().child(format!("{} · {}",state_label(&file.status),file.input_path)))
                        .when_some(file.output_path.clone(),|el,path|el.child(div().text_xs().text_color(cx.theme().muted_foreground).child(format!("输出：{path}"))))
                        .when_some(file.reason.clone(),|el,reason|el.child(div().text_sm().child(reason)))
                        .when_some(file.path_reason.clone(),|el,reason|el.child(div().text_sm().child(format!("路径失败：{} ({reason})", crate::path_reports::failure_label(&reason))))))))
                    .children(mapping_panels)
                    .child(control("history-events-toggle").label(if self.diagnostics_open {"收起事件记录"} else {"查看事件记录"}).on_click(cx.listener(|this,_,_,cx|{this.diagnostics_open= !this.diagnostics_open;cx.notify();})))
                    .when(self.diagnostics_open,|el|el.child(div().id("history-events").max_h(px(260.)).overflow_y_scroll().children(detail.events.iter().map(|e|div().text_xs().py_1().child(format!("{}  {}",e.created_at,e.message)))))))
            })
    }
}
