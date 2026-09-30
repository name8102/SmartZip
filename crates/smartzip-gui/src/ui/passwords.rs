use super::*;

impl View {
    pub(super) fn password_file(&mut self, export: bool, cx: &mut Context<Self>) {
        if export {
            let receiver =
                cx.prompt_for_new_path(&std::env::temp_dir(), Some("smartzip-passwords.txt"));
            let shared = self.shared.clone();
            cx.spawn(async move |_, cx| {
                if let Ok(Ok(Some(path))) = receiver.await {
                    shared.update(cx, |state, cx| {
                        state.background(move || {
                            LibraryMessage::Mutation(
                                library::export_passwords(&LibraryOptions::default(), &path)
                                    .map(|n| format!("已导出 {n} 条明文密码")),
                            )
                        });
                        cx.notify();
                    });
                }
            })
            .detach();
        } else {
            let receiver = cx.prompt_for_paths(PathPromptOptions {
                files: true,
                directories: false,
                multiple: false,
                prompt: None,
            });
            let shared = self.shared.clone();
            cx.spawn(async move |_, cx| {
                if let Ok(Ok(Some(paths))) = receiver.await {
                    if let Some(path) = paths.into_iter().next() {
                        shared.update(cx, |state, cx| {
                            state.background(move || {
                                LibraryMessage::Mutation(
                                    library::import_passwords(
                                        &LibraryOptions::default(),
                                        &path,
                                        "gui-import",
                                    )
                                    .map(|n| {
                                        format!("已处理 {n} 个非空输入行（包含重复行），列表已更新")
                                    }),
                                )
                            });
                            cx.notify();
                        });
                    }
                }
            })
            .detach();
        }
    }
    pub(super) fn toggle_password(&mut self, id: i64, cx: &mut Context<Self>) {
        self.password_reveal_generation += 1;
        self.revealed_password = None;
        if self.password_reveal_request == Some(id) {
            self.password_reveal_request = None;
            cx.notify();
            return;
        }
        self.password_reveal_request = Some(id);
        let generation = self.password_reveal_generation;
        let task = cx
            .background_executor()
            .spawn(async move { library::read_password(&LibraryOptions::default(), id) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                if this.password_reveal_generation != generation || this.page != Page::Passwords {
                    return;
                }
                match result {
                    Ok(value) => this.revealed_password = Some((id, value)),
                    Err(error) => {
                        this.password_reveal_request = None;
                        this.shared.update(cx, |state, cx| {
                            state.message = error;
                            cx.notify();
                        });
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn password_action(
        &mut self,
        ids: Vec<i64>,
        action: library::PasswordAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if ids.is_empty() {
            return;
        }
        let shared = self.shared.clone();
        self.revealed_password = None;
        self.password_reveal_request = None;
        self.password_reveal_generation += 1;
        if matches!(action, library::PasswordAction::Delete) {
            let answer = confirm(
                window,
                gpui::PromptLevel::Warning,
                "删除密码记录？",
                Some(&format!(
                    "将从本地密码库删除 {} 条记录。已有归档和输出不受影响，此操作无法撤销。",
                    ids.len()
                )),
                &["保留", "删除"],
                cx,
            );
            cx.spawn(async move |_, cx| {
                if answer.await == Ok(1) {
                    shared.update(cx, |s, cx| {
                        s.background(move || {
                            LibraryMessage::Mutation(library::change_passwords(
                                &LibraryOptions::default(),
                                &ids,
                                action,
                            ))
                        });
                        cx.notify();
                    });
                }
            })
            .detach();
        } else {
            shared.update(cx, |s, cx| {
                s.background(move || {
                    LibraryMessage::Mutation(library::change_passwords(
                        &LibraryOptions::default(),
                        &ids,
                        action,
                    ))
                });
                cx.notify();
            });
        }
    }

    pub(super) fn passwords(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.shared.read(cx);
        let rows = state.passwords.clone();
        let query = state.password_query.clone();
        let more = state.password_more;
        let busy = state.library_busy;
        let loading = state.password_loading;
        let selected = self.password_selection.len();
        div().flex().flex_col().gap_4()
            .child(div().text_sm().text_color(cx.theme().muted_foreground).child("密码保存在本机密码库，可供后续任务自动尝试。仅本次使用的密码请在任务配置中输入。"))
            .child(div().flex().items_center().gap_2()
                .child(div().flex_1().child(Input::new(&self.input).mask_toggle()))
                .child(control("add-password").primary().label("添加密码").disabled(busy).on_click(cx.listener(|this,_,window,cx|{
                    let value=this.input.read(cx).value().to_string();if value.is_empty(){return;}
                    this.shared.update(cx,|s,cx|{s.background(move||LibraryMessage::Mutation(library::add_password(&LibraryOptions::default(),&value,"manual",false).map(|_|"密码已保存".into())));cx.notify();});
                    this.input.update(cx,|input,cx|input.set_value("",window,cx));
                })))
                .child(control("import-passwords").label("导入…").disabled(busy).on_click(cx.listener(|this,_,_,cx|this.password_file(false,cx))))
                .child(control("export-passwords").label("导出…").disabled(busy).on_click(cx.listener(|_,_,window,cx|{
                    let answer=confirm(window,gpui::PromptLevel::Warning,"导出明文密码？",Some("所有启用的密码将写入本地文本文件，请选择可信的保存位置。"),&["取消","选择保存位置"],cx);
                    cx.spawn(async move |this,cx|{if answer.await==Ok(1){let _=this.update(cx,|this,cx|this.password_file(true,cx));}}).detach();
                }))))
            .child(div().flex().gap_2().items_center()
                .child(div().flex_1().child(Input::new(&self.password_search).cleanable(true)))
                .child(control("password-search").label("搜索").disabled(loading).on_click(cx.listener(|this,_,_,cx|{let text=this.password_search.read(cx).value().to_string();this.shared.update(cx,|s,cx|{s.password_query.text=text;s.password_query.offset=0;s.reload_passwords();cx.notify();});})))
                .children([("","全部"),("enabled","启用"),("disabled","禁用"),("pinned","置顶")].into_iter().enumerate().map(|(i,(status,label))|control(("password-filter",i)).label(label).when(query.status==status,|b|b.primary()).on_click(cx.listener(move|this,_,_,cx|this.shared.update(cx,|s,cx|{s.password_query.status=status.into();s.password_query.offset=0;s.reload_passwords();cx.notify();}))))))
            .when(selected>0,|el|el.child(div().flex().gap_2().items_center().child(format!("已选 {selected} 条"))
                .children([(library::PasswordAction::Pin(true),"置顶"),(library::PasswordAction::Enable(false),"禁用"),(library::PasswordAction::Enable(true),"启用"),(library::PasswordAction::Delete,"删除")].into_iter().enumerate().map(|(i,(action,label))|control(("password-bulk",i)).label(label).disabled(busy).on_click(cx.listener(move|this,_,window,cx|this.password_action(this.password_selection.iter().copied().collect(),action,window,cx)))))))
            .when(loading,|el|el.child("正在读取密码库…"))
            .when(rows.is_empty()&&!loading,|el|el.child(empty_state("没有匹配的密码记录","可添加密码、导入文本文件，或调整筛选条件。",cx)))
            .child(div().id("password-list").max_h(px(420.)).overflow_y_scroll().border_1().border_color(cx.theme().border).rounded_lg().children(rows.into_iter().map(|row|{
                let id=row.id;let selected=self.password_selection.contains(&id);
                let value=self.revealed_password.as_ref().filter(|(shown,_)|*shown==id).map(|(_,v)|v.clone()).unwrap_or_else(||row.masked.into());
                div().flex().items_center().gap_3().px_3().py_2().min_h(px(self.shared.read(cx).preferences.density.row_height())).border_b_1().border_color(cx.theme().border)
                    .when(selected,|el|el.bg(cx.theme().sidebar_accent))
                    .child(control(("pw-select",id as u64)).label(if selected{"✓"}else{"选择"}).on_click(cx.listener(move|this,_,_,cx|{if !this.password_selection.remove(&id){this.password_selection.insert(id);}cx.notify();})))
                    .child(div().w(px(48.)).text_xs().child(format!("#{id}")))
                    .child(div().w(px(150.)).overflow_x_hidden().text_ellipsis().child(value))
                    .child(control(("pw-reveal",id as u64)).label(if self.password_reveal_request==Some(id){"隐藏"}else{"显示"}).on_click(cx.listener(move|this,_,_,cx|this.toggle_password(id,cx))))
                    .child(div().flex_1().min_w_0().flex().flex_col().gap_1()
                        .child(div().text_sm().child(format!("{} · 成功 {} / 失败 {}",row.source,row.success_count,row.failure_count)))
                        .child(div().text_xs().text_color(cx.theme().muted_foreground).child(format!("{} · 最近成功 {} · 最近失败 {}",if row.disabled{"已禁用"}else{"已启用"},row.last_success_at.as_deref().unwrap_or("尚无"),row.last_failure_at.as_deref().unwrap_or("尚无")))))
                    .child(control(("pw-pin",id as u64)).label(if row.pinned{"取消置顶"}else{"置顶"}).disabled(busy).on_click(cx.listener(move|this,_,window,cx|this.password_action(vec![id],library::PasswordAction::Pin(!row.pinned),window,cx))))
                    .child(control(("pw-delete",id as u64)).label("删除").disabled(busy).on_click(cx.listener(move|this,_,window,cx|this.password_action(vec![id],library::PasswordAction::Delete,window,cx))))
            })))
            .child(div().flex().items_center().gap_3()
                .child(control("password-prev").label("上一页").disabled(query.offset==0||loading).on_click(cx.listener(|this,_,_,cx|this.shared.update(cx,|s,cx|{s.password_query.offset=s.password_query.offset.saturating_sub(library::PAGE_SIZE);s.reload_passwords();cx.notify();}))))
                .child(format!("第 {} 页",query.offset/library::PAGE_SIZE+1))
                .child(control("password-next").label("下一页").disabled(!more||loading).on_click(cx.listener(|this,_,_,cx|this.shared.update(cx,|s,cx|{s.password_query.offset+=library::PAGE_SIZE;s.reload_passwords();cx.notify();})))))
            .child(div().flex().items_center().gap_3().border_t_1().border_color(cx.theme().border).pt_4()
                .child("保留排名前") .child(div().w(px(90.)).child(Input::new(&self.cleanup_limit))) .child("条启用密码（始终保留置顶项）")
                .child(control("password-cleanup").label("预览清理…").disabled(busy).on_click(cx.listener(|this,_,_,cx|{
                    let Ok(limit)=this.cleanup_limit.read(cx).value().parse::<usize>() else {this.shared.update(cx,|s,cx|{s.message="请输入有效的保留数量".into();cx.notify();});return;};
                    let shared=this.shared.clone();let task=cx.background_executor().spawn(async move{library::cleanup_passwords(&LibraryOptions::default(),limit,None,false)});
                    cx.spawn(async move|this,cx|{let result=task.await;let _=this.update(cx,|this,cx|match result{
                        Ok(ids)=>{this.cleanup_candidates=Some(ids);cx.notify();},
                        Err(e)=>shared.update(cx,|s,cx|{s.message=e;cx.notify();}),
                    });}).detach();
                }))))
            .when_some(self.cleanup_candidates.clone(),|el,ids|{
                let count=ids.len();el.child(div().flex().items_center().gap_3().child(format!("将禁用 {count} 条未置顶密码；可以在禁用列表重新启用。"))
                    .child(control("cleanup-apply").label("确认禁用").disabled(busy||count==0).on_click(cx.listener(move|this,_,window,cx|{this.password_action(ids.clone(),library::PasswordAction::Cleanup,window,cx);this.cleanup_candidates=None;})))
                    .child(control("cleanup-dismiss").label("取消").on_click(cx.listener(|this,_,_,cx|{this.cleanup_candidates=None;cx.notify();}))))
            })
    }
}
