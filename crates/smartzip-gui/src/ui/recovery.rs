use super::*;

impl Workspace {
    pub(super) fn reload_recovery(&mut self) {
        if self.recovery_loading {
            return;
        }
        self.recovery_loading = true;
        let tx = self.sender.clone();
        std::thread::spawn(move || {
            let _ = tx.send(LibraryMessage::Recovery(library::recoverable_tasks(
                &LibraryOptions::default(),
            )));
        });
    }
}
impl View {
    pub(super) fn recovery(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.shared.read(cx);
        let rows: Vec<_> = state
            .recoveries
            .iter()
            .filter(|r| {
                !state.queue.jobs.iter().any(|j| {
                    (j.phase.active() || j.phase == Phase::Queued)
                        && (j.request.settings.recovery_task_id.as_ref() == Some(&r.id)
                            || j.task_id.as_ref() == Some(&r.id))
                })
            })
            .cloned()
            .collect();
        let loading = state.recovery_loading;
        let writable = state.config.as_ref().is_some_and(|c| {
            c.resolved.values.state.mode == smartzip_config::StateMode::ReadWrite
                && c.resolved.values.state.history
        });
        div().flex().flex_col().gap_4()
            .child(div().text_lg().child("继续未完成的任务"))
            .child(div().text_sm().text_color(cx.theme().muted_foreground).child("这里仅检查历史状态。选择恢复后才会对账并继续执行；已经提交的节点不会重复解压。临时密码需要重新输入。"))
            .child(control("refresh-recovery").label("重新检查").disabled(loading).on_click(cx.listener(|this,_,_,cx|this.shared.update(cx,|s,cx|{s.reload_recovery();cx.notify();}))))
            .when(!writable,|el|el.child("当前配置未启用可写任务历史，请在设置中启用后恢复。"))
            .when(loading,|el|el.child("正在检查未完成任务…"))
            .when(rows.is_empty()&&!loading,|el|el.child(empty_state("没有待恢复任务","正常完成的记录可在历史页面查看。",cx)))
            .children(rows.into_iter().map(|row|{
                let name=row.inputs.first().and_then(|p|p.file_name()).map(|s|s.to_string_lossy().into_owned()).unwrap_or_else(||row.id.clone());
                div().flex().flex_col().gap_3().p_4().border_1().border_color(cx.theme().border).rounded_lg()
                    .child(div().text_base().child(name))
                    .child(div().text_sm().child(format!("{} · 待处理 {} 项 · 已完成 {} 项{}",state_label(&row.status),row.pending,row.completed,if row.paused{" · 原任务已暂停"}else{""})))
                    .children(row.inputs.iter().take(4).map(|p|div().text_xs().text_color(cx.theme().muted_foreground).child(p.display().to_string())))
                    .child(control(SharedString::from(format!("recover-{}",row.id))).primary().label("恢复此任务").disabled(!writable||row.inputs.is_empty()).on_click(cx.listener(move|this,_,_,cx|{
                        this.shared.update(cx,|s,cx|{
                            if s.queue.jobs.iter().any(|j|(j.phase.active() || j.phase == Phase::Queued) && j.request.settings.recovery_task_id.as_ref()==Some(&row.id)){return;}
                            s.queue.enqueue(JobRequest {operation:TaskOperation::Recover,paths:row.inputs.clone(),settings:TaskSettings{recovery_task_id:Some(row.id.clone()),..Default::default()},resolved:s.config.as_ref().map(|c|c.resolved.clone())});cx.notify();
                        });this.page=Page::Tasks;this.selected_file=None;
                    })))
            }))
    }
}
