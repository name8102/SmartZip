use super::*;

impl View {
    pub(super) fn selected_prompt(&self, cx: &App) -> Option<(bool, u64, u64)> {
        let state = self.shared.read(cx);
        let all = || {
            [(false, &state.queue), (true, &state.previews)]
                .into_iter()
                .flat_map(|(preview, q)| {
                    q.jobs
                        .iter()
                        .filter(|j| j.prompt.is_some())
                        .map(move |j| (preview, j.id, j.prompt_revision))
                })
        };
        self.pending_selection
            .and_then(|(preview, id)| all().find(|p| p.0 == preview && p.1 == id))
            .or_else(|| all().next())
    }
    pub(super) fn prompt(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let Some((is_preview, id, revision)) = self.selected_prompt(cx) else {
            return div().into_any_element();
        };
        let state = self.shared.read(cx);
        let pending: Vec<_> = [(false, &state.queue), (true, &state.previews)]
            .into_iter()
            .flat_map(|(preview, q)| {
                q.jobs
                    .iter()
                    .filter(|j| j.prompt.is_some())
                    .map(move |j| (preview, j.id, j.name()))
            })
            .collect();
        let job = (if is_preview {
            &state.previews
        } else {
            &state.queue
        })
        .jobs
        .iter()
        .find(|j| j.id == id)
        .unwrap();
        let mut encodings = vec![];
        let (title, kind, details) = match job.prompt.as_ref().unwrap() {
            InteractionRequest::Password { path, .. } => (
                "需要归档密码",
                0,
                format!("{}\n输入密码继续尝试，或跳过此归档。", path.display()),
            ),
            InteractionRequest::Output { path, output, .. } => (
                "输出位置已存在",
                1,
                format!(
                    "来源：{}\n目标：{}\n选择仅作用于此冲突，覆盖将替换目标内容。",
                    path.display(),
                    output.display()
                ),
            ),
            InteractionRequest::Embedded { path, decision, .. } => (
                "发现内嵌归档",
                2,
                format!(
                    "{}\n{}\n{}",
                    path.display(),
                    decision.reason,
                    decision
                        .findings_summary
                        .iter()
                        .map(|f| format!(
                            "{} · 偏移 {} · {}",
                            f.format,
                            f.offset,
                            f.size.map(size_label).unwrap_or_else(|| "大小未知".into())
                        ))
                        .collect::<Vec<_>>()
                        .join("\n")
                ),
            ),
            InteractionRequest::Encoding { path, context, .. } => {
                encodings = context
                    .detected
                    .candidates
                    .iter()
                    .map(|c| c.name.clone())
                    .collect();
                for name in ["utf-8", "gb18030", "big5", "shift_jis", "euc-kr"] {
                    if !encodings.iter().any(|e| e == name) {
                        encodings.push(name.into());
                    }
                }
                (
                    "确认文件名编码",
                    3,
                    format!(
                        "{}\n检测可信度 {:.0}%\n当前文件名：\n{}\n{}",
                        path.display(),
                        context.detected.confidence * 100.,
                        context.preview_names.join("\n"),
                        context.suspicious_reasons.join("；")
                    ),
                )
            }
        };
        let mut choices: Vec<(String, u8)> = match kind {
            0 => vec![("尝试密码".into(), 0), ("跳过归档".into(), 1)],
            1 => vec![
                ("自动重命名".into(), 0),
                ("覆盖此项".into(), 1),
                ("跳过".into(), 2),
            ],
            2 => vec![
                ("提取推荐项".into(), 0),
                ("全部提取".into(), 1),
                ("跳过".into(), 2),
            ],
            _ => vec![("接受检测结果".into(), 0), ("跳过归档".into(), 1)],
        };
        if kind == 3 {
            choices.extend(
                encodings
                    .iter()
                    .enumerate()
                    .take(200)
                    .map(|(i, name)| (format!("使用 {name}"), (i + 2) as u8)),
            );
        }
        div().border_1().border_color(cx.theme().primary).rounded_lg().p_4().flex().flex_col().gap_3()
            .child(div().flex().items_center().justify_between().child(div().text_base().font_weight(gpui::FontWeight::SEMIBOLD).child(format!("待处理 {} · {title}",pending.len())))
                .child(control("locate-prompt").label("定位任务").on_click(cx.listener(move|this,_,_,cx|{this.page=if is_preview{Page::Preview}else{Page::Tasks};this.selected_file=None;this.shared.update(cx,|s,cx|{(if is_preview{&mut s.previews}else{&mut s.queue}).selected=Some(id);cx.notify();});}))))
            .when(pending.len()>1,|el|el.child(div().flex().gap_2().flex_wrap().children(pending.into_iter().enumerate().map(|(i,(preview,job,name))|control(("pending",i)).label(format!("{} · {name}",if preview{"预览"}else{"解压"})).when((preview,job)==(is_preview,id),|b|b.primary()).on_click(cx.listener(move|this,_,_,cx|{this.pending_selection=Some((preview,job));cx.notify();}))))))
            .child(div().id("prompt-evidence").max_h(px(180.)).overflow_y_scroll().text_sm().child(details))
            .when(kind==0,|el|el.child(Input::new(&self.prompt_secret).mask_toggle()))
            .child(div().flex().gap_2().flex_wrap().children(choices.into_iter().enumerate().map(|(index,(label,choice))|{
                let encodings=encodings.clone();
                control(("prompt-choice",index)).label(label).when(choice==0,|b|b.primary()).on_click(cx.listener(move|this,_,window,cx|{
                    let password=this.prompt_secret.read(cx).value().to_string();
                    if kind==0&&choice==0&&password.is_empty(){return;}
                    this.prompt_secret.update(cx,|input,cx|input.set_value("",window,cx));
                    this.shared.update(cx,|s,cx|{
                        let q=if is_preview{&mut s.previews}else{&mut s.queue};
                        let Some(job)=q.jobs.iter_mut().find(|j|j.id==id&&j.prompt_revision==revision)else{return;};
                        if let Some(prompt)=job.prompt.take(){match prompt{
                            InteractionRequest::Password{respond,..}=>{let _=respond.send(if choice==0{Some(password)}else{None});},
                            InteractionRequest::Output{respond,..}=>{let _=respond.send(match choice{0=>smartzip_engine::OutputCollisionStrategy::Rename,1=>smartzip_engine::OutputCollisionStrategy::Overwrite,_=>smartzip_engine::OutputCollisionStrategy::Skip});},
                            InteractionRequest::Embedded{respond,..}=>{let _=respond.send(match choice{0=>smartzip_engine::EmbeddedSelectionChoice::Extract,1=>smartzip_engine::EmbeddedSelectionChoice::ExtractAll,_=>smartzip_engine::EmbeddedSelectionChoice::Skip});},
                            InteractionRequest::Encoding{respond,..}=>{let _=respond.send(match choice{0=>smartzip_engine::EncodingConfirmationChoice::AcceptDetected,1=>smartzip_engine::EncodingConfirmationChoice::SkipArchive,_=>encodings.get(choice as usize-2).map(|v|smartzip_engine::EncodingConfirmationChoice::Override(v.clone())).unwrap_or(smartzip_engine::EncodingConfirmationChoice::SkipArchive)});},
                        }job.phase=Phase::Running;job.stage="继续处理".into();}cx.notify();
                    });
                }))
            }))).into_any_element()
    }
}
