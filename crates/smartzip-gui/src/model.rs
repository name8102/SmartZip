//! Window-independent queue. A dropped batch remains one engine workflow.
use crate::runtime::{
    spawn_job, InteractionRequest, JobHandle, JobMessage, JobOutcome, JobRequest, TaskOperation,
    TaskSettings,
};
use smartzip_core::{RouteEvent, TaskEvent, TaskEventKind};
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Queued,
    Running,
    Waiting,
    Cancelling,
    Completed,
    Partial,
    Failed,
    Cancelled,
}
impl Phase {
    pub fn label(self) -> &'static str {
        match self {
            Self::Queued => "排队",
            Self::Running => "处理中",
            Self::Waiting => "需要处理",
            Self::Cancelling => "正在停止并清理",
            Self::Completed => "已完成",
            Self::Partial => "部分完成",
            Self::Failed => "失败",
            Self::Cancelled => "已取消",
        }
    }
    pub fn active(self) -> bool {
        matches!(self, Self::Running | Self::Waiting | Self::Cancelling)
    }
}
pub struct Job {
    pub id: u64,
    pub request: JobRequest,
    pub phase: Phase,
    pub backend: String,
    pub progress: Option<f32>,
    pub stage: String,
    pub events: Vec<String>,
    pub outputs: Vec<PathBuf>,
    pub result: Option<serde_json::Value>,
    pub prompt: Option<InteractionRequest>,
    pub task_id: Option<String>,
    failure_summary: Option<String>,
    current_route_failure: Option<String>,
    quick_overrides: QuickOverrides,
    handle: Option<JobHandle>,
}

#[derive(Default)]
struct QuickOverrides {
    output: bool,
    recursive: bool,
    smart_layout: bool,
    delete_source: bool,
    auto_encoding: bool,
}
impl Job {
    pub fn backend_label(&self) -> String {
        let backend = self.backend.as_str();
        if backend == "自动 · 待定" || backend.is_empty() {
            return backend.to_owned();
        }
        if let Some(executable) = backend.strip_prefix("sevenzip:") {
            let executable = executable.trim_end_matches(['/', '\\']);
            let executable = executable
                .rsplit(['/', '\\'])
                .next()
                .unwrap_or(executable)
                .strip_suffix(".exe")
                .unwrap_or_else(|| executable.rsplit(['/', '\\']).next().unwrap_or(executable));
            return format!("7-Zip · {executable}");
        }
        if backend == "unrar" || backend.starts_with("unrar:") {
            return "UnRAR".into();
        }
        backend.to_owned()
    }

    pub fn settings_overridden(&self, index: usize) -> bool {
        match index {
            0 => self.quick_overrides.recursive,
            1 => self.quick_overrides.smart_layout,
            2 => self.quick_overrides.delete_source,
            3 => self.quick_overrides.auto_encoding,
            4 => self.quick_overrides.output,
            _ => false,
        }
    }

    pub fn name(&self) -> String {
        let first = self
            .request
            .paths
            .first()
            .map(|p| {
                p.file_name()
                    .unwrap_or(p.as_os_str())
                    .to_string_lossy()
                    .into_owned()
            })
            .unwrap_or_default();
        if self.request.paths.len() > 1 {
            format!("{first} 等 {} 个输入", self.request.paths.len())
        } else {
            first
        }
    }
    fn event(&mut self, event: TaskEvent) {
        self.task_id = Some(event.task_id.to_string());
        match &event.kind {
            TaskEventKind::Progress(p) => {
                self.progress = p.percent;
                if self.phase != Phase::Cancelling {
                    self.stage = p.message.clone();
                }
                return;
            }
            TaskEventKind::Route(RouteEvent::BackendAttemptStarted { adapter_id }) => {
                self.backend = adapter_id.clone();
                self.current_route_failure = None;
            }
            TaskEventKind::Route(RouteEvent::BackendSelected { adapter_id }) => {
                self.backend = adapter_id.clone();
            }
            TaskEventKind::Route(RouteEvent::RoutePlanned { plan }) => {
                self.current_route_failure = if plan.candidates.is_empty()
                    && plan.rejected.is_empty()
                {
                    Some(
                        "未发现可用的解压后端；请在设置中配置 7-Zip 可执行文件或启用自动发现。未调用解压后端，无法据此判断压缩包是否损坏。"
                            .into(),
                    )
                } else {
                    None
                };
            }
            TaskEventKind::Failed { error } => {
                self.failure_summary = Some(
                    self.current_route_failure
                        .take()
                        .unwrap_or_else(|| error.clone()),
                );
            }
            TaskEventKind::OutputCreated { path } if !self.outputs.contains(path) => {
                self.outputs.push(path.clone());
            }
            _ => {}
        }
        // PasswordTried contains only an ID; core events never carry plaintext credentials.
        self.events
            .push(serde_json::to_string(&event.kind).unwrap_or_else(|_| "事件序列化失败".into()));
        if self.events.len() > 2000 {
            self.events.drain(..500);
        }
    }
    fn finish(&mut self, outcome: JobOutcome) {
        self.phase = match outcome.status.as_str() {
            "completed" => Phase::Completed,
            "partial" => Phase::Partial,
            "cancelled" => Phase::Cancelled,
            _ => Phase::Failed,
        };
        self.stage = match self.phase {
            Phase::Failed | Phase::Partial => self
                .failure_summary
                .clone()
                .unwrap_or_else(|| self.phase.label().into()),
            _ => {
                self.failure_summary = None;
                self.phase.label().into()
            }
        };
        self.events.extend(outcome.warnings);
        self.result = Some(outcome.detail);
        self.prompt = None;
        self.handle = None;
        self.request.settings.passwords.clear();
    }
}
#[derive(Default)]
pub struct Queue {
    pub jobs: Vec<Job>,
    pub paused: bool,
    pub selected: Option<u64>,
    next_id: u64,
}
impl Queue {
    pub fn enqueue(&mut self, request: JobRequest) -> u64 {
        self.next_id += 1;
        let id = self.next_id;
        self.jobs.push(Job {
            id,
            request,
            phase: Phase::Queued,
            backend: "自动 · 待定".into(),
            progress: None,
            stage: "等待启动".into(),
            events: vec![],
            outputs: vec![],
            result: None,
            prompt: None,
            task_id: None,
            failure_summary: None,
            current_route_failure: None,
            quick_overrides: QuickOverrides::default(),
            handle: None,
        });
        self.selected = Some(id);
        id
    }
    pub fn selected(&self) -> Option<&Job> {
        self.jobs.iter().find(|j| Some(j.id) == self.selected)
    }
    pub fn update_settings(
        &mut self,
        id: u64,
        update: impl FnOnce(&mut TaskSettings),
    ) -> Result<(), &'static str> {
        let job = self
            .jobs
            .iter_mut()
            .find(|job| job.id == id)
            .ok_or("任务不存在")?;
        if job.phase != Phase::Queued {
            return Err("只有排队中的任务可以修改快速配置");
        }
        if job.request.operation != TaskOperation::Extract {
            return Err("只有解压任务支持快速配置");
        }
        let before = (
            job.request.settings.output.clone(),
            job.request.settings.recursive,
            job.request.settings.smart_layout,
            job.request.settings.delete_source,
            job.request.settings.auto_encoding,
        );
        update(&mut job.request.settings);
        job.quick_overrides.output |= job.request.settings.output != before.0;
        job.quick_overrides.recursive |= job.request.settings.recursive != before.1;
        job.quick_overrides.smart_layout |= job.request.settings.smart_layout != before.2;
        job.quick_overrides.delete_source |= job.request.settings.delete_source != before.3;
        job.quick_overrides.auto_encoding |= job.request.settings.auto_encoding != before.4;
        Ok(())
    }
    pub fn set_task_password(
        &mut self,
        id: u64,
        value: Option<String>,
    ) -> Result<(), &'static str> {
        let job = self
            .jobs
            .iter_mut()
            .find(|job| job.id == id)
            .ok_or("任务不存在")?;
        if job.phase != Phase::Queued {
            return Err("只有排队中的任务可以设置密码");
        }
        match value {
            Some(value) => {
                job.request.settings.passwords = vec![value];
                job.request.settings.temporary_passwords = true;
            }
            None => {
                job.request.settings.passwords.clear();
                job.request.settings.temporary_passwords = false;
            }
        }
        Ok(())
    }
    pub fn apply_global_settings(&mut self, settings: &TaskSettings) {
        for job in &mut self.jobs {
            if job.phase != Phase::Queued || job.request.operation != TaskOperation::Extract {
                continue;
            }
            if !job.quick_overrides.output {
                job.request.settings.output = settings.output.clone();
            }
            if !job.quick_overrides.recursive {
                job.request.settings.recursive = settings.recursive;
            }
            if !job.quick_overrides.smart_layout {
                job.request.settings.smart_layout = settings.smart_layout;
            }
            if !job.quick_overrides.delete_source {
                job.request.settings.delete_source = settings.delete_source;
            }
            if !job.quick_overrides.auto_encoding {
                job.request.settings.auto_encoding = settings.auto_encoding;
            }
        }
    }
    pub fn reset_settings(&mut self, id: u64, settings: &TaskSettings) -> Result<(), &'static str> {
        let job = self
            .jobs
            .iter_mut()
            .find(|job| job.id == id)
            .ok_or("任务不存在")?;
        if job.phase != Phase::Queued {
            return Err("只有排队中的任务可以重置快速配置");
        }
        if job.request.operation != TaskOperation::Extract {
            return Err("只有解压任务支持快速配置");
        }
        job.request.settings.output = settings.output.clone();
        job.request.settings.recursive = settings.recursive;
        job.request.settings.smart_layout = settings.smart_layout;
        job.request.settings.delete_source = settings.delete_source;
        job.request.settings.auto_encoding = settings.auto_encoding;
        job.quick_overrides = QuickOverrides::default();
        Ok(())
    }
    pub fn has_active(&self) -> bool {
        self.jobs.iter().any(|j| j.phase.active())
    }
    pub fn has_work(&self) -> bool {
        self.jobs
            .iter()
            .any(|j| j.phase.active() || j.phase == Phase::Queued)
    }
    pub fn cancel(&mut self, id: u64) {
        if let Some(job) = self.jobs.iter_mut().find(|j| j.id == id) {
            match job.phase {
                Phase::Queued => {
                    job.phase = Phase::Cancelled;
                    job.request.settings.passwords.clear();
                    job.stage = "启动前取消".into();
                }
                p if p.active() => {
                    if let Some(handle) = &job.handle {
                        handle.cancel();
                    }
                    job.phase = Phase::Cancelling;
                    job.stage = "等待后端退出并完成清理".into();
                    job.prompt = None;
                }
                _ => {}
            }
        }
    }
    pub fn cancel_all(&mut self) {
        self.paused = true;
        let ids: Vec<_> = self.jobs.iter().map(|j| j.id).collect();
        for id in ids {
            self.cancel(id);
        }
    }
    pub fn move_up(&mut self, id: u64) {
        if let Some(index) = self
            .jobs
            .iter()
            .position(|j| j.id == id && j.phase == Phase::Queued)
        {
            if let Some(previous) = (0..index)
                .rev()
                .find(|i| self.jobs[*i].phase == Phase::Queued)
            {
                self.jobs.swap(index, previous);
            }
        }
    }
    pub fn tick(&mut self) -> bool {
        let mut changed = false;
        for job in &mut self.jobs {
            let messages = job.handle.as_ref().map(|h| h.drain()).unwrap_or_default();
            for message in messages {
                changed = true;
                match message {
                    JobMessage::Event(event) => job.event(event),
                    JobMessage::Prompt(prompt) => {
                        if job.phase != Phase::Cancelling {
                            job.prompt = Some(prompt);
                            job.phase = Phase::Waiting;
                            job.stage = "等待用户决定".into();
                        }
                    }
                    JobMessage::Finished(outcome) => {
                        job.finish(outcome);
                    }
                    JobMessage::Failed(error) => {
                        job.phase = Phase::Failed;
                        let summary = job
                            .current_route_failure
                            .take()
                            .unwrap_or_else(|| error.clone());
                        job.stage = summary.clone();
                        job.failure_summary = Some(summary);
                        job.events.push(error);
                        job.prompt = None;
                        job.handle = None;
                        job.request.settings.passwords.clear();
                    }
                }
            }
        }
        // Waiters retain their execution slot and staging ownership.
        if !self.paused && !self.has_active() {
            if let Some(job) = self.jobs.iter_mut().find(|j| j.phase == Phase::Queued) {
                changed = true;
                match spawn_job(job.request.clone()) {
                    Ok(handle) => {
                        job.handle = Some(handle);
                        job.phase = Phase::Running;
                        job.stage = "正在准备".into();
                        job.request.settings.passwords.clear();
                    }
                    Err(error) => {
                        job.phase = Phase::Failed;
                        job.request.settings.passwords.clear();
                        job.failure_summary = Some(error.clone());
                        job.stage = error;
                    }
                }
            }
        }
        changed
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::{TaskOperation, TaskSettings};
    fn request(name: &str) -> JobRequest {
        JobRequest {
            operation: TaskOperation::Extract,
            paths: vec![name.into()],
            settings: TaskSettings::default(),
            resolved: None,
        }
    }
    #[test]
    fn pending_cancellation_never_starts_worker() {
        let mut q = Queue::default();
        let mut draft = request("missing.zip");
        draft
            .settings
            .passwords
            .push("temporary-test-secret".into());
        let id = q.enqueue(draft);
        q.cancel(id);
        assert!(!q.has_work());
        assert!(!q.tick());
        assert_eq!(q.jobs[0].phase, Phase::Cancelled);
        assert!(q.jobs[0].request.settings.passwords.is_empty());
    }
    #[test]
    fn rejected_submission_drops_manual_credentials() {
        let mut q = Queue::default();
        let mut draft = request("missing.zip");
        draft.paths.clear();
        draft
            .settings
            .passwords
            .push("temporary-test-secret".into());
        q.enqueue(draft);
        assert!(q.tick());
        assert_eq!(q.jobs[0].phase, Phase::Failed);
        assert!(q.jobs[0].request.settings.passwords.is_empty());
    }
    #[test]
    fn paused_queue_and_reordering_preserve_batches() {
        let mut q = Queue {
            paused: true,
            ..Default::default()
        };
        let a = q.enqueue(request("a.zip"));
        let b = q.enqueue(request("b.zip"));
        q.move_up(b);
        assert_eq!(q.jobs[0].id, b);
        assert_eq!(q.jobs[1].id, a);
        assert!(!q.tick());
        assert_eq!(q.jobs[0].phase, Phase::Queued);
    }
    #[test]
    fn only_current_operation_has_percentage() {
        let mut q = Queue::default();
        q.enqueue(request("a.zip"));
        let job = &mut q.jobs[0];
        job.event(TaskEvent {
            task_id: smartzip_core::TaskId::new(),
            kind: TaskEventKind::Progress(smartzip_core::TaskProgress::percent(62., "extract")),
        });
        assert_eq!(job.progress, Some(62.));
        job.event(TaskEvent {
            task_id: smartzip_core::TaskId::new(),
            kind: TaskEventKind::Progress(smartzip_core::TaskProgress::indeterminate("scan")),
        });
        assert_eq!(job.progress, None);
    }

    #[test]
    fn quick_settings_update_only_changes_selected_queued_extract_job() {
        let mut q = Queue::default();
        let first = q.enqueue(request("first.zip"));
        q.enqueue(request("second.zip"));
        q.jobs[1].request.resolved = Some(smartzip_config::ResolvedConfig::load(None).unwrap());
        let resolved = q.jobs[1].request.resolved.clone();

        q.update_settings(first, |settings| settings.recursive = Some(false))
            .unwrap();

        assert_eq!(q.jobs[0].request.settings.recursive, Some(false));
        assert_eq!(q.jobs[1].request.settings.recursive, None);
        assert_eq!(q.jobs[1].request.resolved, resolved);
    }

    #[test]
    fn quick_settings_update_rejects_active_and_finished_jobs() {
        let mut q = Queue::default();
        let active = q.enqueue(request("active.zip"));
        let finished = q.enqueue(request("finished.zip"));
        q.jobs[0].phase = Phase::Running;
        q.jobs[1].phase = Phase::Completed;

        assert!(q
            .update_settings(active, |settings| settings.recursive = Some(false))
            .is_err());
        assert!(q
            .update_settings(finished, |settings| settings.recursive = Some(false))
            .is_err());
        assert_eq!(q.jobs[0].request.settings.recursive, None);
        assert_eq!(q.jobs[1].request.settings.recursive, None);
    }

    #[test]
    fn quick_settings_update_rejects_non_extract_jobs() {
        let mut q = Queue::default();
        let id = q.enqueue(JobRequest {
            operation: TaskOperation::List,
            paths: vec!["archive.zip".into()],
            settings: TaskSettings::default(),
            resolved: None,
        });

        assert!(q
            .update_settings(id, |settings| settings.recursive = Some(false))
            .is_err());
        assert_eq!(q.jobs[0].request.settings.recursive, None);
    }

    #[test]
    fn global_settings_follow_unoverridden_queued_jobs_and_skip_active_jobs() {
        let mut q = Queue::default();
        let first = q.enqueue(request("first.zip"));
        let second = q.enqueue(request("second.zip"));
        let active = q.enqueue(request("active.zip"));
        q.jobs[2].phase = Phase::Running;

        q.update_settings(first, |settings| settings.recursive = Some(false))
            .unwrap();
        let global = TaskSettings {
            recursive: Some(true),
            smart_layout: Some(true),
            delete_source: true,
            auto_encoding: Some(false),
            output: Some("global-output".into()),
            ..Default::default()
        };
        q.apply_global_settings(&global);

        assert_eq!(q.jobs[0].request.settings.recursive, Some(false));
        assert_eq!(q.jobs[1].id, second);
        assert_eq!(q.jobs[1].request.settings.recursive, Some(true));
        assert_eq!(q.jobs[1].request.settings.smart_layout, Some(true));
        assert_eq!(q.jobs[1].request.settings.output, global.output);
        assert_eq!(q.jobs[2].request.settings.recursive, None);
        assert_eq!(q.jobs[2].request.settings.smart_layout, None);
        assert!(!q.jobs[2].request.settings.delete_source);
        assert_eq!(q.jobs[2].request.settings.auto_encoding, None);
        assert_eq!(q.jobs[2].request.settings.output, None);
        assert_eq!(q.jobs[2].id, active);
    }

    #[test]
    fn reset_settings_returns_job_to_global_values_and_clears_overrides() {
        let mut q = Queue::default();
        let id = q.enqueue(request("archive.zip"));
        q.jobs[0].request.settings.passwords.push("keep".into());
        q.update_settings(id, |settings| {
            settings.recursive = Some(false);
            settings.delete_source = true;
        })
        .unwrap();
        let global = TaskSettings {
            recursive: Some(true),
            delete_source: false,
            ..Default::default()
        };

        q.reset_settings(id, &global).unwrap();
        q.apply_global_settings(&TaskSettings {
            recursive: Some(false),
            ..global.clone()
        });

        assert_eq!(q.jobs[0].request.settings.recursive, Some(false));
        assert!(!q.jobs[0].request.settings.delete_source);
        assert_eq!(q.jobs[0].request.settings.passwords, vec!["keep"]);
    }

    fn outcome(status: &str) -> JobOutcome {
        JobOutcome {
            status: status.into(),
            detail: serde_json::Value::Null,
            warnings: vec![],
        }
    }

    #[test]
    fn failed_event_survives_finished_and_later_progress() {
        let mut q = Queue::default();
        q.enqueue(request("archive.zip"));
        let job = &mut q.jobs[0];
        let task_id = smartzip_core::TaskId::new();
        job.event(TaskEvent {
            task_id: task_id.clone(),
            kind: TaskEventKind::Failed {
                error: "后端读取失败".into(),
            },
        });
        job.event(TaskEvent {
            task_id,
            kind: TaskEventKind::Progress(smartzip_core::TaskProgress::percent(75., "后续输入")),
        });
        job.finish(outcome("partial"));
        assert_eq!(job.phase, Phase::Partial);
        assert_eq!(job.stage, "后端读取失败");
    }

    #[test]
    fn successful_or_cancelled_finish_clears_failure_summary() {
        let mut q = Queue::default();
        q.enqueue(request("archive.zip"));
        let job = &mut q.jobs[0];
        job.event(TaskEvent {
            task_id: smartzip_core::TaskId::new(),
            kind: TaskEventKind::Failed {
                error: "失败诊断".into(),
            },
        });
        job.finish(outcome("completed"));
        assert_eq!(job.stage, Phase::Completed.label());
        job.event(TaskEvent {
            task_id: smartzip_core::TaskId::new(),
            kind: TaskEventKind::Failed {
                error: "另一条失败诊断".into(),
            },
        });
        job.finish(outcome("cancelled"));
        assert_eq!(job.stage, Phase::Cancelled.label());
    }

    #[test]
    fn empty_route_plan_explains_missing_backend() {
        let mut q = Queue::default();
        q.enqueue(request("archive.zip"));
        q.jobs[0].event(TaskEvent {
            task_id: smartzip_core::TaskId::new(),
            kind: TaskEventKind::Route(RouteEvent::RoutePlanned {
                plan: smartzip_core::RoutePlan {
                    operation: smartzip_core::ArchiveOperation::Extract,
                    container: None,
                    requirements: Default::default(),
                    candidates: vec![],
                    rejected: vec![],
                    forced_adapter: None,
                },
            }),
        });
        q.jobs[0].event(TaskEvent {
            task_id: smartzip_core::TaskId::new(),
            kind: TaskEventKind::Route(RouteEvent::RouteExhausted { attempted: vec![] }),
        });
        q.jobs[0].event(TaskEvent {
            task_id: smartzip_core::TaskId::new(),
            kind: TaskEventKind::Failed {
                error: "路由失败原文".into(),
            },
        });
        q.jobs[0].finish(outcome("failed"));
        assert!(q.jobs[0].stage.contains("未发现可用的解压后端"));
        assert!(q.jobs[0].stage.contains("未调用解压后端"));
    }

    #[test]
    fn new_nonempty_route_does_not_reuse_old_route_failure() {
        let mut q = Queue::default();
        q.enqueue(request("archive.zip"));
        let job = &mut q.jobs[0];
        let task_id = smartzip_core::TaskId::new();
        let plan = |candidates| smartzip_core::RoutePlan {
            operation: smartzip_core::ArchiveOperation::Extract,
            container: None,
            requirements: Default::default(),
            candidates,
            rejected: vec![],
            forced_adapter: None,
        };
        job.event(TaskEvent {
            task_id: task_id.clone(),
            kind: TaskEventKind::Route(RouteEvent::RoutePlanned { plan: plan(vec![]) }),
        });
        job.event(TaskEvent {
            task_id: task_id.clone(),
            kind: TaskEventKind::Route(RouteEvent::RoutePlanned {
                plan: plan(vec![smartzip_core::RouteCandidate {
                    adapter_id: "7zz".into(),
                    priority: 1,
                    notes: vec![],
                }]),
            }),
        });
        job.event(TaskEvent {
            task_id,
            kind: TaskEventKind::Failed {
                error: "新的后端错误".into(),
            },
        });
        job.finish(outcome("failed"));
        assert_eq!(job.stage, "新的后端错误");
    }

    #[test]
    fn backend_label_formats_known_adapters_without_losing_raw_ids() {
        let mut q = Queue::default();
        q.enqueue(request("archive.zip"));
        let job = &mut q.jobs[0];
        assert_eq!(job.backend_label(), "自动 · 待定");

        job.backend = "sevenzip:/nix/store/abc/bin/7z".into();
        assert_eq!(job.backend_label(), "7-Zip · 7z");
        job.backend = r"sevenzip:C:\Program Files\7-Zip\7zz.exe".into();
        assert_eq!(job.backend_label(), "7-Zip · 7zz");
        job.backend = "sevenzip:/Applications/7zz".into();
        assert_eq!(job.backend_label(), "7-Zip · 7zz");
        job.backend = "unrar:/usr/local/bin/unrar".into();
        assert_eq!(job.backend_label(), "UnRAR");
        job.backend = "custom-backend".into();
        assert_eq!(job.backend_label(), "custom-backend");
    }

    #[test]
    fn temporary_task_password_is_isolated_and_queued_only() {
        let mut q = Queue::default();
        let first = q.enqueue(request("first.zip"));
        let second = q.enqueue(request("second.zip"));
        let active = q.enqueue(request("active.zip"));
        q.jobs[2].phase = Phase::Running;

        q.set_task_password(first, Some("temporary".into()))
            .unwrap();
        assert_eq!(q.jobs[0].request.settings.passwords, vec!["temporary"]);
        assert!(q.jobs[0].request.settings.temporary_passwords);
        assert!(q.jobs[1].request.settings.passwords.is_empty());
        assert!(!q.jobs[1].request.settings.temporary_passwords);
        assert!(q.set_task_password(active, Some("nope".into())).is_err());

        q.set_task_password(first, None).unwrap();
        assert!(q.jobs[0].request.settings.passwords.is_empty());
        assert!(!q.jobs[0].request.settings.temporary_passwords);
        assert_eq!(q.jobs[2].request.settings.passwords, Vec::<String>::new());
        assert_eq!(q.jobs[2].id, active);
        assert_eq!(q.jobs[1].id, second);
    }
}
