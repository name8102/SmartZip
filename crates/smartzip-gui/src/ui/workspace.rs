//! Application lifetime, worker messages and persisted workspace data.
use super::*;

pub(super) struct Workspace {
    pub(super) queue: Queue,
    pub(super) queue_store: crate::queue_store::QueueStore,
    pub(super) drafts_loaded: bool,
    pub(super) previews: Queue,
    pub(super) preview_revision: u64,
    pub(super) open_requests: mpsc::Receiver<crate::new_open_request::OpenRequest>,
    pub(super) pending_open: Vec<crate::new_open_request::OpenRequest>,
    pub(super) config: Option<ConfigSnapshot>,
    pub(super) config_revision: u64,
    pub(super) settings: TaskSettings,
    pub(super) message: String,
    pub(super) window: Option<WindowHandle<Root>>,
    pub(super) requested_mode: Option<bool>,
    pub(super) quitting: bool,
    pub(super) exit_allow_queue_loss: bool,
    pub(super) inbox: mpsc::Receiver<LibraryMessage>,
    pub(super) sender: mpsc::Sender<LibraryMessage>,
    pub(super) history: Vec<library::HistorySummary>,
    pub(super) history_query: library::Query,
    pub(super) password_query: library::Query,
    pub(super) history_version: u64,
    pub(super) password_version: u64,
    pub(super) history_more: bool,
    pub(super) password_more: bool,
    pub(super) history_loading: bool,
    pub(super) password_loading: bool,
    pub(super) history_detail: Option<library::TaskDetail>,
    pub(super) history_detail_request: Option<String>,
    pub(super) passwords: Vec<PasswordSummary>,
    pub(super) library_busy: bool,
    pub(super) preferences: crate::preferences::Preferences,
    pub(super) preferences_writer: mpsc::Sender<(u64, crate::preferences::Preferences)>,
    pub(super) preferences_revision: u64,
    pub(super) preferences_saved: u64,
    pub(super) exit_prompt_open: bool,
    pub(super) settings_form: Option<Entity<crate::config_form::ConfigForm>>,
    pub(super) settings_form_revision: u64,
    pub(super) recoveries: Vec<library::RecoverySummary>,
    pub(super) recovery_loading: bool,
    pub(super) requested_page: Option<Page>,
}
pub(super) enum LibraryMessage {
    PreferencesSaved(u64, Result<(), String>),
    Config(Result<Box<ConfigSnapshot>, String>),
    Drafts(Result<Vec<JobRequest>, String>),
    ConfigSaved(Result<Box<ConfigSnapshot>, String>),
    History(u64, Result<library::Page<library::HistorySummary>, String>),
    Passwords(u64, Result<library::Page<PasswordSummary>, String>),
    TaskDetail(String, Result<library::TaskDetail, String>),
    Recovery(Result<Vec<library::RecoverySummary>, String>),
    Mutation(Result<String, String>),
}
impl Workspace {
    pub(super) fn new(
        open_requests: mpsc::Receiver<crate::new_open_request::OpenRequest>,
        preferences: crate::preferences::Preferences,
        cx: &mut Context<Self>,
    ) -> Self {
        let (sender, inbox) = mpsc::channel();
        let (preferences_writer, preference_rx) =
            mpsc::channel::<(u64, crate::preferences::Preferences)>();
        let preference_reply = sender.clone();
        std::thread::spawn(move || {
            while let Ok(mut update) = preference_rx.recv() {
                while let Ok(next) = preference_rx.try_recv() {
                    update = next;
                }
                let result =
                    crate::preferences::Preferences::path().and_then(|path| update.1.save(&path));
                let _ = preference_reply.send(LibraryMessage::PreferencesSaved(update.0, result));
            }
        });
        let tx = sender.clone();
        std::thread::spawn(move || {
            let _ = tx.send(LibraryMessage::Config(
                library::load_config(&LibraryOptions::default()).map(Box::new),
            ));
        });
        cx.spawn(async move |entity, cx| loop {
            cx.background_executor()
                .timer(Duration::from_millis(100))
                .await;
            if entity
                .update(cx, |state, cx| {
                    let previous: std::collections::HashMap<_, _> =
                        state.queue.jobs.iter().map(|j| (j.id, j.phase)).collect();
                    state.queue_store.sync(&mut state.queue);
                    let mut changed = state.queue.tick() | state.previews.tick();
                    if let Some(error) = &state.queue_store.error {
                        if state.message != *error { state.message = error.clone(); changed = true; }
                    }
                    let attention = state
                        .queue
                        .jobs
                        .iter()
                        .filter(|j| {
                            previous.get(&j.id).is_some_and(|p| *p != j.phase)
                                && matches!(
                                    j.phase,
                                    Phase::Waiting
                                        | Phase::Failed
                                        | Phase::Partial
                                        | Phase::Completed
                                )
                        })
                        .count();
                    if attention > 0 && state.preferences.notifications && !state.quitting {
                        cx.show_system_notification(gpui::SystemNotification {
                            tag: "smartzip-tasks".into(),
                            title: "SmartZip · 任务状态已更新".into(),
                            body: format!("{attention} 项任务已结束或需要处理，打开任务中心查看。")
                                .into(),
                            actions: vec![],
                        });
                    }
                    while let Ok(paths) = state.open_requests.try_recv() {
                        state.pending_open.push(paths);
                    }
                    if state.config.is_some() && state.drafts_loaded && !state.pending_open.is_empty() {
                        for request in std::mem::take(&mut state.pending_open) {
                            let quick =
                                request.intent == crate::new_open_request::OpenIntent::QuickExtract;
                            state.enqueue(
                                request.paths,
                                if quick {
                                    TaskOperation::Extract
                                } else {
                                    TaskOperation::List
                                },
                            );
                            open_view(cx.entity(), quick, cx);
                        }
                        changed = true;
                    }
                    while let Ok(message) = state.inbox.try_recv() {
                        changed = true;
                        if !matches!(
                            message,
                            LibraryMessage::PreferencesSaved(..)
                                | LibraryMessage::Drafts(..)
                                | LibraryMessage::History(..)
                                | LibraryMessage::Passwords(..)
                                | LibraryMessage::TaskDetail(..)
                                | LibraryMessage::Recovery(..)
                        ) {
                            state.library_busy = false;
                        }
                        match message {
                            LibraryMessage::PreferencesSaved(revision, result) => {
                                state.preferences_saved = revision;
                                if let Err(error) = result {
                                    state.message = format!("外观偏好保存失败：{error}");
                                }
                            }
                            LibraryMessage::Config(result) => match result {
                                Ok(config) => {
                                    state.message.clear();
                                    if !state.drafts_loaded {
                                        let config = config.resolved.clone();
                                        let tx = state.sender.clone();
                                        std::thread::spawn(move || { let _ = tx.send(LibraryMessage::Drafts(crate::queue_store::load(&config))); });
                                    }
                                    state.config = Some(*config);
                                    state.config_revision += 1;
                                    state.reload_recovery();
                                }
                                Err(e) => {
                                    state.config = None;
                                    state.message = e;
                                }
                            },
                            LibraryMessage::Drafts(result) => {
                                state.drafts_loaded = true;
                                match result {
                                    Ok(requests) => {
                                        let count = requests.len();
                                        for request in requests {
                                            if !state.queue.jobs.iter().any(|j| j.request.settings.queued_task_id == request.settings.queued_task_id) { state.queue.restore(request); }
                                        }
                                        if count > 0 { state.message = format!("已找回 {count} 项等待队列。检查配置后选择开始；临时密码需要重新输入。"); }
                                    }
                                    Err(error) => state.message = error,
                                }
                            }
                            LibraryMessage::ConfigSaved(result) => match result {
                                Ok(config) => {
                                    state.message =
                                        "配置已保存，新任务立即生效；快速配置的显式覆盖仍优先"
                                            .into();
                                    state.config = Some(*config);
                                    state.config_revision += 1;
                                }
                                Err(e) => state.message = format!("保存失败，修改仍保留：{e}"),
                            },
                            LibraryMessage::History(version, result) => {
                                if version == state.history_version {
                                    state.history_loading = false;
                                    match result {
                                        Ok(page) => {
                                            state.history = page.rows;
                                            state.history_more = page.has_more;
                                        }
                                        Err(e) => state.message = e,
                                    }
                                }
                            }
                            LibraryMessage::Passwords(version, result) => {
                                if version == state.password_version {
                                    state.password_loading = false;
                                    match result {
                                        Ok(page) => {
                                            state.passwords = page.rows;
                                            state.password_more = page.has_more;
                                        }
                                        Err(e) => state.message = e,
                                    }
                                }
                            }
                            LibraryMessage::Recovery(result) => {
                                state.recovery_loading = false;
                                match result {
                                    Ok(rows) => state.recoveries = rows,
                                    Err(error) => state.message = error,
                                }
                            }
                            LibraryMessage::TaskDetail(id, result) => {
                                if state.history_detail_request.as_ref() == Some(&id) {
                                    match result {
                                        Ok(detail) => state.history_detail = Some(detail),
                                        Err(e) => state.message = e,
                                    }
                                }
                            }
                            LibraryMessage::Mutation(result) => {
                                let refresh = result.is_ok();
                                state.message = result.unwrap_or_else(|e| e);
                                if refresh {
                                    state.reload_passwords();
                                }
                            }
                        }
                    }
                    state.queue_store.sync(&mut state.queue);
                    if state.quitting
                        && !state.queue.has_active()
                        && !state.previews.has_active()
                        && state.preferences_saved == state.preferences_revision
                        && !state.library_busy
                        && (state.queue_store.settled() || (state.exit_allow_queue_loss && state.queue_store.idle()))
                    {
                        cx.quit();
                    }
                    if changed {
                        cx.notify();
                    }
                })
                .is_err()
            {
                break;
            }
        })
        .detach();
        Self {
            queue: Queue::default(),
            queue_store: Default::default(),
            drafts_loaded: false,
            previews: Queue::default(),
            preview_revision: 0,
            open_requests,
            pending_open: vec![],
            config: None,
            config_revision: 0,
            settings: TaskSettings::default(),
            message: "正在读取配置…".into(),
            window: None,
            requested_mode: None,
            quitting: false,
            exit_allow_queue_loss: false,
            inbox,
            sender,
            history: vec![],
            history_query: library::Query::default(),
            password_query: library::Query::default(),
            history_version: 0,
            password_version: 0,
            history_more: false,
            password_more: false,
            history_loading: false,
            password_loading: false,
            history_detail: None,
            history_detail_request: None,
            passwords: vec![],
            library_busy: false,
            preferences,
            preferences_writer,
            preferences_revision: 0,
            preferences_saved: 0,
            exit_prompt_open: false,
            settings_form: None,
            settings_form_revision: 0,
            recoveries: vec![],
            recovery_loading: false,
            requested_page: None,
        }
    }
    pub(super) fn enqueue(&mut self, paths: Vec<PathBuf>, operation: TaskOperation) {
        self.enqueue_with_hold(paths, operation, false);
    }

    pub(super) fn enqueue_with_hold(
        &mut self,
        paths: Vec<PathBuf>,
        operation: TaskOperation,
        hold: bool,
    ) {
        if self.quitting {
            return;
        }
        if paths.is_empty() {
            return;
        }
        let Some(config) = &self.config else {
            self.message = "配置尚未就绪，请在设置中重新加载并解决错误".into();
            return;
        };
        let settings = self.settings.clone();
        let resolved = Some(config.resolved.clone());
        if operation == TaskOperation::List {
            for path in paths {
                self.previews.enqueue(JobRequest {
                    operation,
                    paths: vec![path],
                    settings: settings.clone(),
                    resolved: resolved.clone(),
                });
            }
            self.preview_revision += 1;
        } else if operation == TaskOperation::Detect {
            for path in paths {
                self.queue.enqueue(JobRequest {
                    operation,
                    paths: vec![path],
                    settings: settings.clone(),
                    resolved: resolved.clone(),
                });
            }
        } else {
            let request = JobRequest {
                operation,
                paths,
                settings,
                resolved,
            };
            if hold {
                self.queue.enqueue_held(request);
            } else {
                self.queue.enqueue(request);
            }
        }
        self.message.clear();
    }
    pub(super) fn edit_settings(
        &mut self,
        target: Option<u64>,
        edit: impl FnOnce(&mut TaskSettings),
    ) {
        if let Some(id) = target {
            if let Err(error) = self.queue.update_settings(id, edit) {
                self.message = error.into();
            }
        } else {
            edit(&mut self.settings);
            self.queue.apply_global_settings(&self.settings);
        }
    }
    pub(super) fn background(&mut self, f: impl FnOnce() -> LibraryMessage + Send + 'static) {
        if self.library_busy {
            self.message = "请等待当前数据操作完成".into();
            return;
        }
        self.library_busy = true;
        let tx = self.sender.clone();
        std::thread::spawn(move || {
            let _ = tx.send(f());
        });
    }
}
