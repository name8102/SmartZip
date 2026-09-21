//! Live root controls and node snapshots. Durable execution stays in the state recorder.
use crate::coordinator::Stage;
use crate::{ExecutionStateRecorder, ExtractTaskIdentity, NodeOutcome, TaskCancellation};
use smartzip_core::{AttemptId, DecisionId, NodeId, TaskId};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug, PartialEq)]
pub struct FileTaskSnapshot {
    pub volumes: Option<crate::root_inputs::VolumeGroupDetails>,
    pub events: Vec<String>,
    pub node_id: NodeId,
    pub root_id: NodeId,
    pub parent_id: Option<NodeId>,
    pub path: PathBuf,
    pub depth: u8,
    pub state: String,
    pub stage: String,
    pub progress: Option<f32>,
    pub backend: String,
    pub output: Option<PathBuf>,
    pub committed: bool,
    /// Whole root subtree result; distinct from this node's own completion.
    pub root_outcome: Option<String>,
    pub root_activity: Option<(String, String)>,
    pub pause_requested: bool,
    pub cancel_requested: bool,
}
struct Root {
    cancellation: TaskCancellation,
    paused: bool,
    finished: bool,
    remaining: usize,
    outcomes: Vec<String>,
}
#[derive(Default)]
struct State {
    roots: HashMap<NodeId, Root>,
    nodes: Vec<FileTaskSnapshot>,
    current: HashMap<NodeId, NodeId>,
    volumes: HashMap<NodeId, crate::root_inputs::VolumeGroupDetails>,
}
/// One handle per submitted batch. Controls never cancel sibling roots.
pub struct RootManagement {
    state: Mutex<State>,
    revision: tokio::sync::watch::Sender<u64>,
}
impl Default for RootManagement {
    fn default() -> Self {
        Self {
            state: Mutex::new(State::default()),
            revision: tokio::sync::watch::channel(0).0,
        }
    }
}
impl RootManagement {
    fn changed(&self) {
        self.revision.send_modify(|n| *n = n.wrapping_add(1));
    }
    pub fn snapshot(&self) -> Vec<FileTaskSnapshot> {
        let state = self.state.lock().unwrap();
        let mut nodes = state.nodes.clone();
        for node in nodes
            .iter_mut()
            .filter(|n| n.parent_id.is_none() && n.root_outcome.is_none())
        {
            let active = state
                .nodes
                .iter()
                .filter(|n| {
                    n.root_id == node.root_id
                        && matches!(
                            n.state.as_str(),
                            "running" | "paused" | "waiting_user" | "waiting_resources"
                        )
                })
                .min_by_key(|n| {
                    if n.state == "waiting_user" {
                        0
                    } else if n.state == "paused" {
                        1
                    } else {
                        2
                    }
                });
            if let Some(active) = active {
                let name = active
                    .path
                    .file_name()
                    .unwrap_or(active.path.as_os_str())
                    .to_string_lossy();
                node.root_activity =
                    Some((active.state.clone(), format!("{name} · {}", active.stage)));
            }
        }
        nodes
    }
    pub(crate) fn subscribe(&self) -> tokio::sync::watch::Receiver<u64> {
        self.revision.subscribe()
    }
    pub fn configure_groups(
        &self,
        identity: &mut ExtractTaskIdentity,
        groups: Vec<crate::root_inputs::VolumeGroupDetails>,
    ) -> smartzip_core::Result<()> {
        if groups.iter().any(|g| g.inputs.is_empty())
            || groups.iter().map(|g| g.inputs.len()).sum::<usize>() != identity.roots.len()
            || !groups
                .iter()
                .flat_map(|g| &g.inputs)
                .zip(&identity.roots)
                .all(|(path, root)| *path == root.candidate.path)
        {
            return Err(smartzip_core::SmartZipError::ResourceLimit {
                detail: "root groups do not match task inputs".into(),
            });
        }
        let mut state = self.state.lock().unwrap();
        let mut offset = 0;
        for group in groups {
            let root_id = identity.roots[offset].node_id.clone();
            for root in &mut identity.roots[offset..offset + group.inputs.len()] {
                root.root_id = root_id.clone();
            }
            offset += group.inputs.len();
            state.volumes.insert(root_id, group);
        }
        Ok(())
    }
    pub(crate) fn register(&self, identity: &ExtractTaskIdentity, cancellation: &TaskCancellation) {
        let mut state = self.state.lock().unwrap();
        for root in &identity.roots {
            state
                .roots
                .entry(root.root_id.clone())
                .or_insert_with(|| Root {
                    cancellation: cancellation.fork(),
                    paused: false,
                    finished: false,
                    remaining: 0,
                    outcomes: vec![],
                });
            state.roots.get_mut(&root.root_id).unwrap().remaining += 1;
            state
                .current
                .insert(root.node_id.clone(), root.node_id.clone());
            let parent = (root.node_id != root.root_id).then(|| root.root_id.clone());
            let mut snapshot = node_snapshot(&root.node_id, &root.root_id, parent, &root.candidate);
            if snapshot.parent_id.is_none() {
                snapshot.volumes = state.volumes.get(&root.root_id).cloned();
            }
            state.nodes.push(snapshot);
        }
        drop(state);
        self.changed();
    }
    pub fn set_paused(&self, root_id: &NodeId, paused: bool) -> bool {
        let mut state = self.state.lock().unwrap();
        let Some(root) = state
            .roots
            .get_mut(root_id)
            .filter(|r| !r.finished && !r.cancellation.is_cancelled())
        else {
            return false;
        };
        root.paused = paused;
        for node in &mut state.nodes {
            if node.root_id == *root_id {
                node.pause_requested = paused;
            }
        }
        drop(state);
        self.changed();
        true
    }
    pub fn cancel(&self, root_id: &NodeId) -> bool {
        let mut state = self.state.lock().unwrap();
        let Some(root) = state.roots.get_mut(root_id).filter(|r| !r.finished) else {
            return false;
        };
        root.cancellation.user.cancel();
        root.cancellation.token.cancel();
        root.paused = false;
        for node in &mut state.nodes {
            if node.root_id == *root_id {
                node.cancel_requested = true;
                node.pause_requested = false;
            }
        }
        drop(state);
        self.changed();
        true
    }
    pub(crate) fn cancellation(&self, root: &NodeId) -> TaskCancellation {
        self.state.lock().unwrap().roots[root].cancellation.clone()
    }
    pub(crate) fn paused(&self, root: &NodeId) -> bool {
        self.state.lock().unwrap().roots[root].paused
    }
    pub(crate) fn occupies_slot(&self, root: &NodeId) -> bool {
        let state = self.state.lock().unwrap();
        let current = &state.current[root];
        state
            .nodes
            .iter()
            .find(|n| n.node_id == *current)
            .is_none_or(|n| !matches!(n.state.as_str(), "paused" | "waiting_user"))
    }
    async fn wait(&self, root: &NodeId, token: &CancellationToken) -> smartzip_core::Result<()> {
        let mut revision = self.subscribe();
        loop {
            if token.is_cancelled() {
                return Err(smartzip_core::SmartZipError::Cancelled);
            }
            if !self.paused(root) {
                return Ok(());
            }
            tokio::select! { _ = token.cancelled() => return Err(smartzip_core::SmartZipError::Cancelled), _ = revision.changed() => {} }
        }
    }
    fn update(&self, node: &NodeId, update: impl FnOnce(&mut FileTaskSnapshot)) {
        let mut state = self.state.lock().unwrap();
        if let Some(n) = state.nodes.iter_mut().find(|n| n.node_id == *node) {
            update(n);
        }
        drop(state);
        self.changed();
    }
    pub(crate) fn finish(
        &self,
        root: &NodeId,
        result: &smartzip_core::Result<crate::ExtractWorkflowResult>,
    ) {
        let mut state = self.state.lock().unwrap();

        let outcome = match result {
            Ok(r) => format!("{:?}", r.status).to_ascii_lowercase(),
            Err(smartzip_core::SmartZipError::Cancelled) => "cancelled".into(),
            Err(_) => "failed".into(),
        };
        let control = state.roots.get_mut(root).unwrap();
        control.remaining -= 1;
        control.outcomes.push(outcome);
        if control.remaining > 0 {
            drop(state);
            self.changed();
            return;
        }
        control.finished = true;
        let outcome = if control.outcomes.iter().any(|o| o == "cancelled") {
            "cancelled"
        } else if control.outcomes.iter().all(|o| o == "completed") {
            "completed"
        } else if control.outcomes.iter().all(|o| o == "failed") {
            "failed"
        } else {
            "partial"
        }
        .to_string();
        for n in &mut state.nodes {
            if n.root_id != *root {
                continue;
            }
            n.pause_requested = false;
            if n.parent_id.is_none() {
                n.root_outcome = Some(outcome.clone());
            }
            if matches!(
                n.state.as_str(),
                "queued" | "running" | "paused" | "waiting_user" | "waiting_resources"
            ) {
                n.state = outcome.clone();
            }
        }
        drop(state);
        self.changed();
    }
    pub(crate) fn event(&self, root: &NodeId, event: &smartzip_core::TaskEvent) {
        use smartzip_core::{RouteEvent, TaskEventKind};
        let current = self.state.lock().unwrap().current[root].clone();
        match &event.kind {
            TaskEventKind::Progress(p) => self.update(&current, |n| {
                n.progress = p.percent;
                n.stage = p.message.clone();
            }),
            TaskEventKind::Route(
                RouteEvent::BackendAttemptStarted { adapter_id }
                | RouteEvent::BackendSelected { adapter_id },
            ) => self.update(&current, |n| n.backend = adapter_id.clone()),
            _ => {}
        }
        let message = match &event.kind {
            TaskEventKind::Warning { message } => Some(message.clone()),
            TaskEventKind::Failed { error } => Some(format!("失败：{error}")),
            TaskEventKind::OutputCreated { path } => Some(format!("输出：{}", path.display())),
            TaskEventKind::Route(RouteEvent::BackendAttemptStarted { adapter_id }) => {
                Some(format!("开始调用后端：{adapter_id}"))
            }
            TaskEventKind::Route(RouteEvent::BackendSelected { adapter_id }) => {
                Some(format!("采用后端：{adapter_id}"))
            }
            TaskEventKind::PasswordTried { .. } => Some("尝试密码候选".into()),
            TaskEventKind::EncodingDetected(detection) => Some(format!(
                "文件名编码：{}",
                match &detection.selected {
                    smartzip_core::EncodingMode::Auto => "自动",
                    smartzip_core::EncodingMode::Override(name) => name.as_str(),
                }
            )),
            _ => None,
        };
        if let Some(message) = message {
            self.update(&current, |n| {
                n.events.push(message);
                if n.events.len() > 200 {
                    n.events.remove(0);
                }
            });
        }
    }
}
fn node_snapshot(
    node: &NodeId,
    root: &NodeId,
    parent: Option<NodeId>,
    candidate: &crate::ExtractionCandidate,
) -> FileTaskSnapshot {
    FileTaskSnapshot {
        volumes: None,
        events: vec![],
        node_id: node.clone(),
        root_id: root.clone(),
        parent_id: parent,
        path: candidate.path.clone(),
        depth: candidate.depth,
        state: "queued".into(),
        stage: "等待启动".into(),
        progress: None,
        backend: String::new(),
        output: None,
        committed: false,
        root_outcome: None,
        root_activity: None,
        pause_requested: false,
        cancel_requested: false,
    }
}
pub(crate) struct ManagedRecorder<'a> {
    pub inner: Option<&'a dyn ExecutionStateRecorder>,
    pub management: Arc<RootManagement>,
    pub root: NodeId,
    pub lane: NodeId,
}
#[async_trait::async_trait(?Send)]
impl ExecutionStateRecorder for ManagedRecorder<'_> {
    fn volume_selected(&self, node_id: &NodeId, members: Vec<PathBuf>) {
        self.management.update(&self.root, |n| {
            if let Some(volumes) = &mut n.volumes {
                if !volumes.selected.contains(&members) {
                    volumes.selected.push(members.clone());
                }
            }
        });
        if let Some(inner) = self.inner {
            inner.volume_selected(node_id, members);
        }
    }
    fn durable(&self) -> bool {
        self.inner.is_some_and(|i| i.durable())
    }
    async fn enqueue_child(
        &self,
        task_id: &TaskId,
        node_id: &NodeId,
        parent_id: &NodeId,
        root_id: &NodeId,
        candidate: &crate::ExtractionCandidate,
        generation: u64,
    ) -> smartzip_core::Result<bool> {
        let accepted = match self.inner {
            Some(i) => {
                i.enqueue_child(task_id, node_id, parent_id, root_id, candidate, generation)
                    .await?
            }
            None => true,
        };
        if accepted {
            self.management
                .state
                .lock()
                .unwrap()
                .nodes
                .push(node_snapshot(
                    node_id,
                    root_id,
                    Some(parent_id.clone()),
                    candidate,
                ));
            self.management.changed();
        }
        Ok(accepted)
    }
    async fn transition(
        &self,
        task_id: &TaskId,
        node_id: &NodeId,
        generation: u64,
        from: &str,
        to: &str,
        stage: Stage,
        attempt_id: Option<&AttemptId>,
        cancellation: &CancellationToken,
    ) -> smartzip_core::Result<bool> {
        self.management
            .state
            .lock()
            .unwrap()
            .current
            .insert(self.lane.clone(), node_id.clone());
        if self.management.paused(&self.root) {
            self.release_stage(task_id, node_id);
            self.management.update(node_id, |n| {
                n.state = "paused".into();
                n.progress = None;
            });
            self.management.wait(&self.root, cancellation).await?;
        }
        self.management.update(node_id, |n| {
            n.state = "waiting_resources".into();
            n.stage = stage.as_str().into();
            n.progress = None;
        });
        let accepted = match self.inner {
            Some(i) => {
                i.transition(
                    task_id,
                    node_id,
                    generation,
                    from,
                    to,
                    stage,
                    attempt_id,
                    cancellation,
                )
                .await?
            }
            None => {
                if cancellation.is_cancelled() {
                    return Err(smartzip_core::SmartZipError::Cancelled);
                }
                true
            }
        };
        if accepted {
            // Pause requested while queued for resources: yield the admitted lease,
            // then reacquire through the same durable running-state transition.
            if self.management.paused(&self.root) {
                self.release_stage(task_id, node_id);
                self.management
                    .update(node_id, |n| n.state = "paused".into());
                self.management.wait(&self.root, cancellation).await?;
                return self
                    .transition(
                        task_id,
                        node_id,
                        generation,
                        "running",
                        to,
                        stage,
                        attempt_id,
                        cancellation,
                    )
                    .await;
            }
            self.management
                .update(node_id, |n| n.state = "running".into());
        }
        Ok(accepted)
    }
    fn release_stage(&self, task_id: &TaskId, node_id: &NodeId) {
        if let Some(i) = self.inner {
            i.release_stage(task_id, node_id);
        }
    }
    async fn wait_for_decision(
        &self,
        task_id: &TaskId,
        node_id: &NodeId,
        generation: u64,
        stage: Stage,
        decision_id: &DecisionId,
        kind: &str,
        evidence: &str,
    ) -> smartzip_core::Result<bool> {
        let accepted = match self.inner {
            Some(i) => {
                i.wait_for_decision(
                    task_id,
                    node_id,
                    generation,
                    stage,
                    decision_id,
                    kind,
                    evidence,
                )
                .await?
            }
            None => true,
        };
        if accepted {
            self.management.update(node_id, |n| {
                n.state = "waiting_user".into();
                n.stage = kind.into();
                n.progress = None;
            });
        }
        Ok(accepted)
    }
    async fn finish_node(
        &self,
        task_id: &TaskId,
        node_id: &NodeId,
        generation: u64,
        outcome: NodeOutcome,
    ) -> smartzip_core::Result<bool> {
        let accepted = match self.inner {
            Some(i) => {
                i.finish_node(task_id, node_id, generation, outcome.clone())
                    .await?
            }
            None => true,
        };
        if accepted {
            self.management.update(node_id, |n| {
                n.state = outcome.status;
                n.stage = outcome.reason.unwrap_or_else(|| n.state.clone());
                n.output = outcome.output_path;
                n.committed = outcome.committed;
                n.progress = None;
            });
        }
        Ok(accepted)
    }
    async fn record_staging(
        &self,
        task_id: &TaskId,
        node_id: &NodeId,
        generation: u64,
        path: &Path,
    ) -> smartzip_core::Result<bool> {
        match self.inner {
            Some(i) => i.record_staging(task_id, node_id, generation, path).await,
            None => Ok(true),
        }
    }
    async fn accept_decision(
        &self,
        task_id: &TaskId,
        node_id: &NodeId,
        generation: u64,
        decision_id: &DecisionId,
    ) -> smartzip_core::Result<bool> {
        match self.inner {
            Some(i) => {
                i.accept_decision(task_id, node_id, generation, decision_id)
                    .await
            }
            None => Ok(true),
        }
    }
    async fn begin_commit(
        &self,
        task_id: &TaskId,
        node_id: &NodeId,
        generation: u64,
        intent: &crate::CommitIntent,
    ) -> smartzip_core::Result<bool> {
        match self.inner {
            Some(i) => i.begin_commit(task_id, node_id, generation, intent).await,
            None => Ok(true),
        }
    }
    async fn commit_published(
        &self,
        task_id: &TaskId,
        node_id: &NodeId,
        generation: u64,
        intent: &crate::CommitIntent,
    ) -> smartzip_core::Result<bool> {
        let accepted = match self.inner {
            Some(i) => {
                i.commit_published(task_id, node_id, generation, intent)
                    .await?
            }
            None => true,
        };
        if accepted {
            self.management.update(node_id, |n| {
                n.committed = true;
                n.output = Some(intent.target_path.clone());
            });
        }
        Ok(accepted)
    }
    async fn abort_commit(
        &self,
        task_id: &TaskId,
        node_id: &NodeId,
        generation: u64,
    ) -> smartzip_core::Result<bool> {
        match self.inner {
            Some(i) => i.abort_commit(task_id, node_id, generation).await,
            None => Ok(true),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn paused_root_releases_at_boundary_and_cancel_leaves_sibling_runnable() {
        let identity = ExtractTaskIdentity::new(&["a.zip".into(), "b.zip".into()]);
        let management = Arc::new(RootManagement::default());
        let batch = TaskCancellation::new(CancellationToken::new());
        management.register(&identity, &batch);
        let a = &identity.roots[0].node_id;
        let b = &identity.roots[1].node_id;
        let recorder = |root: &NodeId| ManagedRecorder {
            inner: None,
            management: management.clone(),
            root: root.clone(),
            lane: root.clone(),
        };
        assert!(management.set_paused(a, true));
        let first = recorder(a);
        let first_cancel = management.cancellation(a);
        let wait = first.transition(
            &identity.task_id,
            a,
            0,
            "ready",
            "running",
            Stage::ScanEmbedded,
            None,
            first_cancel.token(),
        );
        tokio::pin!(wait);
        assert!(futures_util::poll!(&mut wait).is_pending());
        assert_eq!(management.snapshot()[0].state, "paused");
        assert!(!management.occupies_slot(a));
        let sibling_cancel = management.cancellation(b);
        assert!(recorder(b)
            .transition(
                &identity.task_id,
                b,
                0,
                "ready",
                "running",
                Stage::ScanEmbedded,
                None,
                sibling_cancel.token()
            )
            .await
            .unwrap());
        assert!(management.cancel(a));
        assert!(matches!(
            wait.await,
            Err(smartzip_core::SmartZipError::Cancelled)
        ));
        assert!(!sibling_cancel.is_cancelled());
        assert!(!batch.is_cancelled());
    }

    #[tokio::test]
    async fn durable_pause_releases_lease_and_resume_reacquires_it() {
        use crate::state_store::{NodeSubmission, StateStore, TaskSubmission};
        let temp = tempfile::tempdir().unwrap();
        let identity =
            ExtractTaskIdentity::new(&[temp.path().join("a.zip"), temp.path().join("b.zip")]);
        let store = Arc::new(StateStore::start(temp.path().join("state.db")).unwrap());
        let execution = crate::execution_runtime::ExecutionCoordinator::new(
            store,
            crate::coordinator::ResourceCapacity {
                cpu_units: 1,
                backend_processes: 1,
                ..Default::default()
            },
        )
        .unwrap();
        execution
            .submit(TaskSubmission {
                task_id: identity.task_id.clone(),
                kind: "extract".into(),
                output_path: Some(temp.path().join("output")),
                started_at: "2026-09-20T00:00:00Z".into(),
                inputs_json: "[]".into(),
                config_snapshot_json: "{}".into(),
                priority: smartzip_db::task_execution::Priority::Normal,
                queue_position: 0,
                recoverable: true,
                roots: identity
                    .roots
                    .iter()
                    .map(|r| NodeSubmission {
                        node_id: r.node_id.clone(),
                        parent_id: None,
                        root_id: r.root_id.clone(),
                        input_path: r.candidate.path.clone(),
                        input_ref_json: serde_json::to_string(&r.candidate).unwrap(),
                        config_revision: 0,
                        generation: 0,
                    })
                    .collect(),
            })
            .await
            .unwrap();
        let management = Arc::new(RootManagement::default());
        management.register(&identity, &TaskCancellation::new(CancellationToken::new()));
        let a = &identity.roots[0].node_id;
        let b = &identity.roots[1].node_id;
        let first = ManagedRecorder {
            inner: Some(&execution),
            management: management.clone(),
            root: a.clone(),
            lane: a.clone(),
        };
        let token = management.cancellation(a);
        assert!(first
            .transition(
                &identity.task_id,
                a,
                0,
                "ready",
                "running",
                Stage::ExtractAttempt,
                None,
                token.token()
            )
            .await
            .unwrap());
        management.set_paused(a, true);
        let continuation = first.transition(
            &identity.task_id,
            a,
            0,
            "running",
            "running",
            Stage::InspectAndPlan,
            None,
            token.token(),
        );
        tokio::pin!(continuation);
        assert!(futures_util::poll!(&mut continuation).is_pending());
        let sibling = management.cancellation(b);
        assert!(tokio::time::timeout(
            std::time::Duration::from_secs(2),
            execution.transition(
                &identity.task_id,
                b,
                0,
                "ready",
                "running",
                Stage::ExtractAttempt,
                None,
                sibling.token()
            )
        )
        .await
        .unwrap()
        .unwrap());
        execution
            .finish_node(
                &identity.task_id,
                b,
                0,
                NodeOutcome::terminal("extracted", None, None, true),
            )
            .await
            .unwrap();
        management.set_paused(a, false);
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(2), continuation)
                .await
                .unwrap()
                .unwrap()
        );
        execution
            .finish_node(
                &identity.task_id,
                a,
                0,
                NodeOutcome::terminal("extracted", None, None, true),
            )
            .await
            .unwrap();
    }

    #[test]
    fn overlapping_candidate_inputs_share_one_control_but_independent_event_lanes() {
        let paths = vec![PathBuf::from("a.001"), PathBuf::from("a.002")];
        let mut identity = ExtractTaskIdentity::new(&paths);
        let management = RootManagement::default();
        management
            .configure_groups(
                &mut identity,
                vec![crate::root_inputs::VolumeGroupDetails {
                    inputs: paths.clone(),
                    members: paths.clone(),
                    candidates: vec![paths],
                    selected: vec![],
                    diagnostic: "ambiguous".into(),
                }],
            )
            .unwrap();
        management.register(&identity, &TaskCancellation::new(CancellationToken::new()));
        assert_eq!(identity.roots[0].root_id, identity.roots[1].root_id);
        let snapshots = management.snapshot();
        assert_eq!(
            snapshots.iter().filter(|n| n.parent_id.is_none()).count(),
            1
        );
        management.event(
            &identity.roots[1].node_id,
            &smartzip_core::TaskEvent {
                task_id: identity.task_id.clone(),
                kind: smartzip_core::TaskEventKind::Warning {
                    message: "second candidate".into(),
                },
            },
        );
        let snapshots = management.snapshot();
        assert!(snapshots[0].events.is_empty());
        assert_eq!(snapshots[1].events, ["second candidate"]);
        management.cancel(&identity.roots[0].root_id);
        assert!(identity
            .roots
            .iter()
            .all(|r| management.cancellation(&r.root_id).is_user_cancelled()));
    }

    #[test]
    fn configured_stop_on_error_remains_batch_wide() {
        let identity = ExtractTaskIdentity::new(&["a.zip".into(), "b.zip".into()]);
        let management = RootManagement::default();
        let batch = TaskCancellation::new(CancellationToken::new());
        management.register(&identity, &batch);
        management
            .cancellation(&identity.roots[0].root_id)
            .stop_on_error();
        assert!(management
            .cancellation(&identity.roots[1].root_id)
            .is_cancelled());
        assert!(batch.stopped_on_error());
        assert!(!batch.is_user_cancelled());
    }

    #[tokio::test]
    async fn child_failure_does_not_erase_committed_parent_or_mix_events() {
        let identity = ExtractTaskIdentity::new(&["a.zip".into(), "b.zip".into()]);
        let management = Arc::new(RootManagement::default());
        management.register(&identity, &TaskCancellation::new(CancellationToken::new()));
        let root = &identity.roots[0].node_id;
        let recorder = ManagedRecorder {
            inner: None,
            management: management.clone(),
            root: root.clone(),
            lane: root.clone(),
        };
        let child = NodeId::new();
        let mut candidate = crate::ExtractionCandidate::root("nested.zip".into());
        candidate.depth = 1;
        recorder
            .enqueue_child(&identity.task_id, &child, root, root, &candidate, 0)
            .await
            .unwrap();
        recorder
            .finish_node(
                &identity.task_id,
                root,
                0,
                NodeOutcome::terminal("extracted", None, Some(Path::new("output")), true),
            )
            .await
            .unwrap();
        recorder
            .finish_node(
                &identity.task_id,
                &child,
                0,
                NodeOutcome::terminal("failed", Some("wrong_password"), None, false),
            )
            .await
            .unwrap();
        management.event(
            root,
            &smartzip_core::TaskEvent {
                task_id: identity.task_id.clone(),
                kind: smartzip_core::TaskEventKind::Warning {
                    message: "only a".into(),
                },
            },
        );
        let files = management.snapshot();
        assert!(files[0].committed);
        assert_eq!(files[0].state, "extracted");
        assert!(files[0].root_outcome.is_none());
        assert!(files[1].events.is_empty());
        assert_eq!(files[2].parent_id.as_ref(), Some(root));
        assert_eq!(files[2].state, "failed");
    }

    #[tokio::test]
    async fn resume_and_batch_cancel_wake_a_parked_root() {
        let identity = ExtractTaskIdentity::new(&["a.zip".into()]);
        let management = RootManagement::default();
        let batch = TaskCancellation::new(CancellationToken::new());
        management.register(&identity, &batch);
        let root = &identity.roots[0].node_id;
        let token = management.cancellation(root);
        management.set_paused(root, true);
        let wait = management.wait(root, token.token());
        tokio::pin!(wait);
        assert!(futures_util::poll!(&mut wait).is_pending());
        management.set_paused(root, false);
        wait.await.unwrap();
        management.set_paused(root, true);
        batch.user.cancel();
        assert!(matches!(
            management.wait(root, token.token()).await,
            Err(smartzip_core::SmartZipError::Cancelled)
        ));
    }
}
