//! Shared stage admission paired with the durable state store.

use crate::coordinator::{
    Dispatch, ResourceCapacity, ResourceRequest, Stage, StageRun, TaskCoordinator, TaskPriority,
};
use crate::state_store::{NodeSubmission, StateStore, StateStoreError, TaskSubmission};
use crate::{ExecutionStateRecorder, ExtractRootIdentity, ExtractTaskIdentity, NodeOutcome};
use async_trait::async_trait;
use smartzip_core::{AttemptId, DecisionId, NodeId, TaskId};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio::sync::Notify;

#[derive(Debug, Clone)]
struct NodeResources {
    root_id: NodeId,
    input: PathBuf,
    output_root: PathBuf,
    source_io_domain: String,
    target_io_domain: String,
}

struct ActiveStage {
    dispatch: Dispatch,
    started: Instant,
}

struct RuntimeState {
    coordinator: TaskCoordinator,
    nodes: HashMap<(TaskId, NodeId), NodeResources>,
    active: HashMap<(TaskId, NodeId), ActiveStage>,
}

pub struct ExecutionCoordinator {
    store: Arc<StateStore>,
    state: Mutex<RuntimeState>,
    recovery: Mutex<Vec<smartzip_db::task_execution::RecoveredTaskRecord>>,
    changed: Notify,
}

impl ExecutionCoordinator {
    pub fn new(
        store: Arc<StateStore>,
        capacity: ResourceCapacity,
    ) -> Result<Self, StateStoreError> {
        let recovery = store.recovery_snapshot().to_vec();
        let runtime = Self {
            store,
            state: Mutex::new(RuntimeState {
                coordinator: TaskCoordinator::new(capacity),
                nodes: HashMap::new(),
                active: HashMap::new(),
            }),
            recovery: Mutex::new(recovery),
            changed: Notify::new(),
        };
        runtime.register_recovery()?;
        Ok(runtime)
    }

    pub fn for_host(store: Arc<StateStore>) -> Result<Self, StateStoreError> {
        let units = std::thread::available_parallelism()?.get() as u32;
        Self::new(
            store,
            ResourceCapacity {
                cpu_units: units,
                backend_processes: 1,
                ..ResourceCapacity::default()
            },
        )
    }

    pub fn recovery_snapshot(&self) -> &[smartzip_db::task_execution::RecoveredTaskRecord] {
        self.store.recovery_snapshot()
    }

    pub fn take_runnable_recovery(&self) -> Vec<smartzip_db::task_execution::RecoveredTaskRecord> {
        let mut recovery = self.recovery.lock().unwrap();
        let (runnable, paused): (Vec<_>, Vec<_>) = std::mem::take(&mut *recovery)
            .into_iter()
            .partition(|task| !task.paused);
        *recovery = paused;
        runnable
    }

    pub fn decode_recovery(
        task: &smartzip_db::task_execution::RecoveredTaskRecord,
    ) -> Result<
        (
            crate::state_store::PersistedExtractPlan,
            ExtractTaskIdentity,
        ),
        StateStoreError,
    > {
        if task
            .nodes
            .iter()
            .any(|node| node.execution_state != "ready")
        {
            return Err(StateStoreError::InvalidSubmission(format!(
                "recoverable task {} requires commit reconciliation",
                task.task_id
            )));
        }
        let snapshot = task.config_snapshot_json.as_ref().ok_or_else(|| {
            StateStoreError::InvalidSubmission(format!(
                "recoverable task {} has no configuration snapshot",
                task.task_id
            ))
        })?;
        let mut plan: crate::state_store::PersistedExtractPlan = serde_json::from_str(snapshot)
            .map_err(|error| {
                StateStoreError::InvalidSubmission(format!(
                    "recoverable task {} has an invalid snapshot: {error}",
                    task.task_id
                ))
            })?;
        plan.request.inputs = task
            .nodes
            .iter()
            .map(|node| {
                serde_json::from_str::<crate::ExtractionCandidate>(&node.input_ref_json)
                    .map(|candidate| candidate.path)
                    .map_err(|error| {
                        StateStoreError::InvalidSubmission(format!(
                            "recoverable node {} has an invalid input reference: {error}",
                            node.node_id
                        ))
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let roots = task
            .nodes
            .iter()
            .map(|node| {
                let generation = u64::try_from(node.generation).map_err(|_| {
                    StateStoreError::InvalidSubmission(format!(
                        "recoverable node {} has a negative generation",
                        node.node_id
                    ))
                })?;
                Ok(ExtractRootIdentity {
                    node_id: NodeId::from_stored(node.node_id.clone()),
                    root_id: NodeId::from_stored(node.root_node_id.clone()),
                    generation,
                    candidate: serde_json::from_str(&node.input_ref_json).map_err(|error| {
                        StateStoreError::InvalidSubmission(format!(
                            "recoverable node {} has an invalid input reference: {error}",
                            node.node_id
                        ))
                    })?,
                })
            })
            .collect::<Result<Vec<_>, StateStoreError>>()?;
        Ok((
            plan,
            ExtractTaskIdentity {
                task_id: TaskId::from_stored(task.task_id.clone()),
                roots,
                budget: crate::TaskBudgetSnapshot {
                    output_files: task.committed_output_files,
                    output_bytes: task.committed_output_bytes,
                    nested_candidates: task.nested_candidate_count,
                },
            },
        ))
    }

    fn register_recovery(&self) -> Result<(), StateStoreError> {
        let mut state = self.state.lock().unwrap();
        for task in self.store.recovery_snapshot() {
            let output_root = task.output_path.as_ref().ok_or_else(|| {
                StateStoreError::InvalidSubmission(format!(
                    "recoverable task {} has no output path",
                    task.task_id
                ))
            })?;
            let queue_position = u64::try_from(task.queue_position).map_err(|_| {
                StateStoreError::InvalidSubmission(format!(
                    "recoverable task {} has a negative queue position",
                    task.task_id
                ))
            })?;
            let task_id = TaskId::from_stored(task.task_id.clone());
            state.coordinator.submit(
                task_id.clone(),
                match task.priority {
                    smartzip_db::task_execution::Priority::Background => TaskPriority::Background,
                    smartzip_db::task_execution::Priority::Normal => TaskPriority::Normal,
                    smartzip_db::task_execution::Priority::High => TaskPriority::High,
                },
            );
            state.coordinator.reorder(&task_id, queue_position);
            state.coordinator.set_paused(&task_id, task.paused);
            for node in &task.nodes {
                let input = PathBuf::from(&node.input_path);
                let output_root = PathBuf::from(output_root);
                let source_io_domain = io_domain(&input);
                let target_io_domain = io_domain(&output_root);
                state
                    .coordinator
                    .set_io_domain_capacity(source_io_domain.clone(), 1);
                state
                    .coordinator
                    .set_io_domain_capacity(target_io_domain.clone(), 1);
                state.nodes.insert(
                    (task_id.clone(), NodeId::from_stored(node.node_id.clone())),
                    NodeResources {
                        root_id: NodeId::from_stored(node.root_node_id.clone()),
                        input,
                        output_root,
                        source_io_domain,
                        target_io_domain,
                    },
                );
            }
        }
        Ok(())
    }

    pub async fn submit(&self, submission: TaskSubmission) -> Result<(), StateStoreError> {
        let output_root = submission.output_path.clone().ok_or_else(|| {
            StateStoreError::InvalidSubmission("extract task requires an output path".into())
        })?;
        let queue_position = submission.queue_position.try_into().map_err(|_| {
            StateStoreError::InvalidSubmission("queue position must be non-negative".into())
        })?;
        self.store.submit(submission.clone()).await?;
        let mut state = self.state.lock().unwrap();
        state.coordinator.submit(
            submission.task_id.clone(),
            match submission.priority {
                smartzip_db::task_execution::Priority::Background => TaskPriority::Background,
                smartzip_db::task_execution::Priority::Normal => TaskPriority::Normal,
                smartzip_db::task_execution::Priority::High => TaskPriority::High,
            },
        );
        state
            .coordinator
            .reorder(&submission.task_id, queue_position);
        for root in submission.roots {
            let source_io_domain = io_domain(&root.input_path);
            let target_io_domain = io_domain(&output_root);
            state
                .coordinator
                .set_io_domain_capacity(source_io_domain.clone(), 1);
            state
                .coordinator
                .set_io_domain_capacity(target_io_domain.clone(), 1);
            state.nodes.insert(
                (submission.task_id.clone(), root.node_id.clone()),
                NodeResources {
                    root_id: root.root_id,
                    input: root.input_path,
                    output_root: output_root.clone(),
                    source_io_domain,
                    target_io_domain,
                },
            );
        }
        Ok(())
    }

    pub async fn stop_task(
        &self,
        task_id: &TaskId,
        status: &str,
        reason: &str,
    ) -> Result<(), StateStoreError> {
        self.release_task(task_id);
        self.store
            .finish_pending_task(task_id.clone(), status.to_owned(), reason.to_owned())
            .await
    }

    pub async fn set_priority(
        &self,
        task_id: &TaskId,
        priority: smartzip_db::task_execution::Priority,
    ) -> Result<bool, StateStoreError> {
        let changed = self.store.set_priority(task_id.clone(), priority).await?;
        if changed {
            self.state.lock().unwrap().coordinator.set_priority(
                task_id,
                match priority {
                    smartzip_db::task_execution::Priority::Background => TaskPriority::Background,
                    smartzip_db::task_execution::Priority::Normal => TaskPriority::Normal,
                    smartzip_db::task_execution::Priority::High => TaskPriority::High,
                },
            );
            self.changed.notify_waiters();
        }
        Ok(changed)
    }

    pub async fn set_paused(
        &self,
        task_id: &TaskId,
        paused: bool,
    ) -> Result<bool, StateStoreError> {
        let changed = self.store.set_paused(task_id.clone(), paused).await?;
        if changed {
            self.state
                .lock()
                .unwrap()
                .coordinator
                .set_paused(task_id, paused);
            if let Some(recovered) = self
                .recovery
                .lock()
                .unwrap()
                .iter_mut()
                .find(|task| task.task_id == task_id.as_str())
            {
                recovered.paused = paused;
            }
            self.changed.notify_waiters();
        }
        Ok(changed)
    }

    pub async fn reorder(
        &self,
        task_id: &TaskId,
        queue_position: u64,
    ) -> Result<bool, StateStoreError> {
        let stored_position = i64::try_from(queue_position).map_err(|_| {
            StateStoreError::InvalidSubmission("queue position exceeds SQLite integer".into())
        })?;
        let changed = self.store.reorder(task_id.clone(), stored_position).await?;
        if changed {
            self.state
                .lock()
                .unwrap()
                .coordinator
                .reorder(task_id, queue_position);
            self.changed.notify_waiters();
        }
        Ok(changed)
    }

    pub fn release_task(&self, task_id: &TaskId) {
        let mut state = self.state.lock().unwrap();
        let keys: Vec<_> = state
            .active
            .keys()
            .filter(|(active_task, _)| active_task == task_id)
            .cloned()
            .collect();
        for key in keys {
            release_active(&mut state, &key);
        }
        state.coordinator.remove(task_id);
        state.nodes.retain(|(node_task, _), _| node_task != task_id);
        let _ = drive(&mut state);
        drop(state);
        self.changed.notify_waiters();
    }

    async fn admit(
        &self,
        task_id: &TaskId,
        node_id: &NodeId,
        generation: u64,
        stage: Stage,
        attempt_id: Option<&AttemptId>,
        cancellation: &tokio_util::sync::CancellationToken,
    ) -> smartzip_core::Result<()> {
        let key = (task_id.clone(), node_id.clone());
        let attempt_id = match attempt_id {
            Some(attempt_id) => attempt_id.clone(),
            None => AttemptId::new(),
        };
        let mut enqueued = false;
        loop {
            let notified = self.changed.notified();
            let (admitted, dispatched) = {
                let mut state = self.state.lock().unwrap();
                if !state.nodes.contains_key(&key) {
                    return Err(smartzip_core::SmartZipError::Cancelled);
                }
                if state.active.contains_key(&key) {
                    (true, false)
                } else {
                    if !enqueued {
                        let node = state.nodes.get(&key).cloned().ok_or_else(|| {
                            smartzip_core::SmartZipError::BackendFailed {
                                backend: "task-coordinator".into(),
                                exit_code: None,
                                stderr: format!("unregistered execution node {node_id}"),
                            }
                        })?;
                        let run = StageRun {
                            task_id: task_id.clone(),
                            node_id: node_id.clone(),
                            root_id: node.root_id.clone(),
                            stage,
                            generation,
                            attempt_id: attempt_id.clone(),
                            resources: resources_for(stage, &node),
                            estimated_cost: estimated_cost(stage),
                        };
                        if !state.coordinator.enqueue(run) {
                            return Err(smartzip_core::SmartZipError::BackendFailed {
                                backend: "task-coordinator".into(),
                                exit_code: None,
                                stderr: format!("unregistered execution task {task_id}"),
                            });
                        }
                        enqueued = true;
                    }
                    let dispatched = drive(&mut state);
                    (state.active.contains_key(&key), dispatched)
                }
            };
            if dispatched {
                self.changed.notify_waiters();
            }
            if admitted {
                return Ok(());
            }
            tokio::select! {
                _ = notified => {}
                _ = cancellation.cancelled() => {
                    let mut state = self.state.lock().unwrap();
                    state.coordinator.remove_queued_stage(
                        task_id,
                        node_id,
                        generation,
                        &attempt_id,
                    );
                    let dispatched = drive(&mut state);
                    drop(state);
                    if dispatched {
                        self.changed.notify_waiters();
                    }
                    return Err(smartzip_core::SmartZipError::Cancelled);
                }
            }
        }
    }

    fn release_node(&self, task_id: &TaskId, node_id: &NodeId) {
        let key = (task_id.clone(), node_id.clone());
        let mut state = self.state.lock().unwrap();
        release_active(&mut state, &key);
        let _ = drive(&mut state);
        drop(state);
        self.changed.notify_waiters();
    }
}

fn drive(state: &mut RuntimeState) -> bool {
    let mut dispatched_any = false;
    while let Some(dispatch) = state.coordinator.dispatch_next() {
        dispatched_any = true;
        let key = (
            dispatch.stage.task_id.clone(),
            dispatch.stage.node_id.clone(),
        );
        state.active.insert(
            key,
            ActiveStage {
                dispatch,
                started: Instant::now(),
            },
        );
    }
    dispatched_any
}

fn release_active(state: &mut RuntimeState, key: &(TaskId, NodeId)) {
    if let Some(active) = state.active.remove(key) {
        let actual_cost = active.started.elapsed().as_millis().max(1) as u64;
        state.coordinator.complete(active.dispatch, actual_cost);
    }
}

fn resources_for(stage: Stage, node: &NodeResources) -> ResourceRequest {
    let mut request = ResourceRequest {
        cpu_units: 1,
        ..ResourceRequest::default()
    };
    match stage {
        Stage::ExtractAttempt | Stage::ReadMetadata | Stage::ReadMember => {
            request.backend_processes = 1;
            request.artifact_reads.push(node.input.clone());
            request.io_domains.insert(node.source_io_domain.clone(), 1);
            if stage == Stage::ExtractAttempt {
                request.io_domains.insert(node.target_io_domain.clone(), 1);
            }
        }
        Stage::ScanEmbedded | Stage::Fingerprint | Stage::PrepareAccess => {
            request.artifact_reads.push(node.input.clone());
            request.io_domains.insert(node.source_io_domain.clone(), 1);
        }
        Stage::Commit => {
            request.target_paths.push(node.output_root.clone());
            request.io_domains.insert(node.target_io_domain.clone(), 1);
        }
        Stage::DiscoverChildren | Stage::DecodePreview => {
            request.artifact_reads.push(node.output_root.clone());
            request.io_domains.insert(node.target_io_domain.clone(), 1);
        }
        Stage::ResolveInputs | Stage::AnalyzeEncoding | Stage::InspectAndPlan | Stage::Cleanup => {}
    }
    request
}

fn io_domain(path: &Path) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if let Some(metadata) = path
            .ancestors()
            .find_map(|candidate| std::fs::symlink_metadata(candidate).ok())
        {
            return format!("device:{}", metadata.dev());
        }
    }
    #[cfg(windows)]
    if let Some(prefix) = path.components().find_map(|component| match component {
        std::path::Component::Prefix(prefix) => Some(prefix.as_os_str()),
        _ => None,
    }) {
        return format!("volume:{}", prefix.to_string_lossy());
    }
    "filesystem:unknown".into()
}

fn estimated_cost(stage: Stage) -> u64 {
    match stage {
        Stage::ExtractAttempt => 100,
        Stage::ScanEmbedded | Stage::DiscoverChildren => 40,
        Stage::ReadMetadata | Stage::PrepareAccess | Stage::ReadMember => 20,
        Stage::Commit | Stage::Cleanup => 5,
        Stage::ResolveInputs
        | Stage::Fingerprint
        | Stage::AnalyzeEncoding
        | Stage::InspectAndPlan
        | Stage::DecodePreview => 10,
    }
}

fn state_error(error: StateStoreError) -> smartzip_core::SmartZipError {
    smartzip_core::SmartZipError::BackendFailed {
        backend: "state-store".into(),
        exit_code: None,
        stderr: error.to_string(),
    }
}

#[async_trait(?Send)]
impl ExecutionStateRecorder for ExecutionCoordinator {
    async fn enqueue_child(
        &self,
        task_id: &TaskId,
        node_id: &NodeId,
        parent_id: &NodeId,
        root_id: &NodeId,
        candidate: &crate::ExtractionCandidate,
        generation: u64,
    ) -> smartzip_core::Result<bool> {
        let output_root = {
            let state = self.state.lock().unwrap();
            state
                .nodes
                .get(&(task_id.clone(), parent_id.clone()))
                .map(|node| node.output_root.clone())
                .ok_or_else(|| smartzip_core::SmartZipError::BackendFailed {
                    backend: "task-coordinator".into(),
                    exit_code: None,
                    stderr: format!("unregistered parent node {parent_id}"),
                })?
        };
        let inserted = self
            .store
            .enqueue_child(
                task_id.clone(),
                NodeSubmission {
                    node_id: node_id.clone(),
                    parent_id: Some(parent_id.clone()),
                    root_id: root_id.clone(),
                    input_path: candidate.path.clone(),
                    input_ref_json: serde_json::to_string(candidate).map_err(|error| {
                        state_error(StateStoreError::InvalidSubmission(error.to_string()))
                    })?,
                    config_revision: generation as i64,
                    generation: generation as i64,
                },
            )
            .await
            .map_err(state_error)?;
        if !inserted {
            return Ok(false);
        }
        let input = &candidate.path;
        let source_io_domain = io_domain(input);
        let target_io_domain = io_domain(&output_root);
        let mut state = self.state.lock().unwrap();
        state
            .coordinator
            .set_io_domain_capacity(source_io_domain.clone(), 1);
        state
            .coordinator
            .set_io_domain_capacity(target_io_domain.clone(), 1);
        state.nodes.insert(
            (task_id.clone(), node_id.clone()),
            NodeResources {
                root_id: root_id.clone(),
                input: input.to_path_buf(),
                output_root,
                source_io_domain,
                target_io_domain,
            },
        );
        Ok(true)
    }

    async fn transition(
        &self,
        task_id: &TaskId,
        node_id: &NodeId,
        generation: u64,
        from: &str,
        _to: &str,
        stage: Stage,
        attempt_id: Option<&AttemptId>,
        cancellation: &tokio_util::sync::CancellationToken,
    ) -> smartzip_core::Result<bool> {
        self.release_node(task_id, node_id);
        let waiting = self
            .store
            .transition(
                task_id.clone(),
                node_id.clone(),
                generation as i64,
                from,
                "waiting_resources",
                stage.as_str(),
                attempt_id.map(ToString::to_string),
            )
            .await
            .map_err(state_error)?;
        if !waiting {
            return Ok(false);
        }
        self.admit(
            task_id,
            node_id,
            generation,
            stage,
            attempt_id,
            cancellation,
        )
        .await?;
        let running = self
            .store
            .transition(
                task_id.clone(),
                node_id.clone(),
                generation as i64,
                "waiting_resources",
                "running",
                stage.as_str(),
                attempt_id.map(ToString::to_string),
            )
            .await
            .map_err(state_error)?;
        if !running {
            self.release_node(task_id, node_id);
        }
        Ok(running)
    }

    fn release_stage(&self, task_id: &TaskId, node_id: &NodeId) {
        self.release_node(task_id, node_id);
    }

    async fn record_staging(
        &self,
        task_id: &TaskId,
        node_id: &NodeId,
        generation: u64,
        path: &Path,
    ) -> smartzip_core::Result<bool> {
        self.store
            .record_staging(
                task_id.clone(),
                node_id.clone(),
                generation as i64,
                path.to_path_buf(),
            )
            .await
            .map_err(state_error)
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
        self.release_node(task_id, node_id);
        self.store
            .wait_for_decision(
                task_id.clone(),
                node_id.clone(),
                stage.as_str(),
                smartzip_db::task_execution::PendingDecision {
                    id: decision_id.to_string(),
                    generation: generation as i64,
                    kind: kind.to_owned(),
                    evidence: evidence.to_owned(),
                },
            )
            .await
            .map_err(state_error)
    }

    async fn accept_decision(
        &self,
        task_id: &TaskId,
        node_id: &NodeId,
        generation: u64,
        decision_id: &DecisionId,
    ) -> smartzip_core::Result<bool> {
        self.store
            .accept_decision(
                task_id.clone(),
                node_id.clone(),
                generation as i64,
                decision_id.to_string(),
            )
            .await
            .map_err(state_error)
    }

    async fn begin_commit(
        &self,
        task_id: &TaskId,
        node_id: &NodeId,
        generation: u64,
        intent: &crate::CommitIntent,
    ) -> smartzip_core::Result<bool> {
        let commit_json = serde_json::to_string(&crate::CommitRecord::Prepared {
            intent: intent.clone(),
        })
        .map_err(|error| smartzip_core::SmartZipError::BackendFailed {
            backend: "task-coordinator".into(),
            exit_code: None,
            stderr: error.to_string(),
        })?;
        self.store
            .begin_commit(
                task_id.clone(),
                node_id.clone(),
                generation as i64,
                commit_json,
            )
            .await
            .map_err(state_error)
    }

    async fn commit_published(
        &self,
        task_id: &TaskId,
        node_id: &NodeId,
        generation: u64,
        intent: &crate::CommitIntent,
    ) -> smartzip_core::Result<bool> {
        let commit_json = serde_json::to_string(&crate::CommitRecord::Published {
            intent: intent.clone(),
        })
        .map_err(|error| smartzip_core::SmartZipError::BackendFailed {
            backend: "task-coordinator".into(),
            exit_code: None,
            stderr: error.to_string(),
        })?;
        self.store
            .commit_published(
                task_id.clone(),
                node_id.clone(),
                generation as i64,
                commit_json,
            )
            .await
            .map_err(state_error)
    }

    async fn abort_commit(
        &self,
        task_id: &TaskId,
        node_id: &NodeId,
        generation: u64,
    ) -> smartzip_core::Result<bool> {
        self.store
            .abort_commit(task_id.clone(), node_id.clone(), generation as i64)
            .await
            .map_err(state_error)
    }

    async fn finish_node(
        &self,
        task_id: &TaskId,
        node_id: &NodeId,
        generation: u64,
        outcome: NodeOutcome,
    ) -> smartzip_core::Result<bool> {
        self.release_node(task_id, node_id);
        self.store
            .finish_node(task_id.clone(), node_id.clone(), generation as i64, outcome)
            .await
            .map_err(state_error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smartzip_db::task_execution::Priority;
    use std::time::Duration;

    fn submission(task_id: &TaskId, node_id: &NodeId, path: &Path) -> TaskSubmission {
        let candidate = crate::ExtractionCandidate::root(path.to_path_buf());
        TaskSubmission {
            task_id: task_id.clone(),
            kind: "extract".into(),
            output_path: Some(path.join("out")),
            started_at: "2026-09-19T00:00:00Z".into(),
            inputs_json: serde_json::json!([path]).to_string(),
            config_snapshot_json: "{}".into(),
            priority: Priority::Normal,
            queue_position: 0,
            recoverable: true,
            roots: vec![NodeSubmission {
                node_id: node_id.clone(),
                parent_id: None,
                root_id: node_id.clone(),
                input_path: path.to_path_buf(),
                input_ref_json: serde_json::to_string(&candidate).unwrap(),
                config_revision: 0,
                generation: 0,
            }],
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn waiting_for_input_releases_stage_capacity() {
        let root = tempfile::tempdir().unwrap();
        let store = Arc::new(StateStore::start(root.path().join("state.db")).unwrap());
        let runtime = ExecutionCoordinator::new(
            store,
            ResourceCapacity {
                cpu_units: 1,
                backend_processes: 1,
                ..ResourceCapacity::default()
            },
        )
        .unwrap();
        let first_task = TaskId::new();
        let first_node = NodeId::new();
        let second_task = TaskId::new();
        let second_node = NodeId::new();
        runtime
            .submit(submission(&first_task, &first_node, root.path()))
            .await
            .unwrap();
        runtime
            .submit(submission(&second_task, &second_node, root.path()))
            .await
            .unwrap();
        let cancellation = tokio_util::sync::CancellationToken::new();

        assert!(runtime
            .transition(
                &first_task,
                &first_node,
                0,
                "ready",
                "running",
                Stage::ExtractAttempt,
                None,
                &cancellation,
            )
            .await
            .unwrap());
        let mut second = Box::pin(runtime.transition(
            &second_task,
            &second_node,
            0,
            "ready",
            "running",
            Stage::ExtractAttempt,
            None,
            &cancellation,
        ));
        assert!(tokio::time::timeout(Duration::from_millis(20), &mut second)
            .await
            .is_err());

        let decision = DecisionId::new();
        assert!(runtime
            .wait_for_decision(
                &first_task,
                &first_node,
                0,
                Stage::ExtractAttempt,
                &decision,
                "password",
                "input.zip",
            )
            .await
            .unwrap());
        assert!(tokio::time::timeout(Duration::from_secs(1), &mut second)
            .await
            .unwrap()
            .unwrap());

        runtime
            .stop_task(&first_task, "cancelled", "test_complete")
            .await
            .unwrap();
        runtime
            .stop_task(&second_task, "cancelled", "test_complete")
            .await
            .unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn waiting_for_stage_admission_stops_on_cancellation() {
        let root = tempfile::tempdir().unwrap();
        let store = Arc::new(StateStore::start(root.path().join("state.db")).unwrap());
        let runtime = ExecutionCoordinator::new(
            store,
            ResourceCapacity {
                cpu_units: 1,
                backend_processes: 1,
                ..ResourceCapacity::default()
            },
        )
        .unwrap();
        let task_id = TaskId::new();
        let first_node = NodeId::new();
        let second_node = NodeId::new();
        let mut task = submission(&task_id, &first_node, root.path());
        let second_candidate = crate::ExtractionCandidate::root(root.path().join("second.zip"));
        task.roots.push(NodeSubmission {
            node_id: second_node.clone(),
            parent_id: None,
            root_id: second_node.clone(),
            input_path: second_candidate.path.clone(),
            input_ref_json: serde_json::to_string(&second_candidate).unwrap(),
            config_revision: 0,
            generation: 0,
        });
        runtime.submit(task).await.unwrap();
        let first_cancellation = tokio_util::sync::CancellationToken::new();
        assert!(runtime
            .transition(
                &task_id,
                &first_node,
                0,
                "ready",
                "running",
                Stage::ExtractAttempt,
                None,
                &first_cancellation,
            )
            .await
            .unwrap());

        let second_cancellation = tokio_util::sync::CancellationToken::new();
        let waiting = runtime.transition(
            &task_id,
            &second_node,
            0,
            "ready",
            "running",
            Stage::ExtractAttempt,
            None,
            &second_cancellation,
        );
        tokio::pin!(waiting);
        assert!(
            tokio::time::timeout(Duration::from_millis(20), &mut waiting)
                .await
                .is_err()
        );
        second_cancellation.cancel();
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(1), &mut waiting).await,
            Ok(Err(smartzip_core::SmartZipError::Cancelled))
        ));
        {
            let state = runtime.state.lock().unwrap();
            assert_eq!(state.coordinator.resources().used().backend_processes, 1);
            assert!(state
                .active
                .contains_key(&(task_id.clone(), first_node.clone())));
        }
        runtime.release_task(&task_id);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn restart_claims_interrupted_node_with_new_generation() {
        let root = tempfile::tempdir().unwrap();
        let db_path = root.path().join("state.db");
        let task_id = TaskId::new();
        let node_id = NodeId::new();
        {
            let store = StateStore::start(&db_path).unwrap();
            store
                .submit(submission(&task_id, &node_id, root.path()))
                .await
                .unwrap();
            assert!(store
                .transition(
                    task_id.clone(),
                    node_id.clone(),
                    0,
                    "ready",
                    "running",
                    Stage::ExtractAttempt.as_str(),
                    Some(AttemptId::new().to_string()),
                )
                .await
                .unwrap());
        }

        let store = Arc::new(StateStore::start(&db_path).unwrap());
        let recovered = &store.recovery_snapshot()[0].nodes[0];
        assert_eq!(recovered.generation, 1);
        assert_eq!(recovered.execution_state, "ready");
        let runtime = ExecutionCoordinator::new(
            store,
            ResourceCapacity {
                cpu_units: 1,
                backend_processes: 1,
                ..ResourceCapacity::default()
            },
        )
        .unwrap();
        let cancellation = tokio_util::sync::CancellationToken::new();
        assert!(runtime
            .transition(
                &task_id,
                &node_id,
                1,
                "ready",
                "running",
                Stage::ResolveInputs,
                None,
                &cancellation,
            )
            .await
            .unwrap());
        runtime
            .stop_task(&task_id, "cancelled", "test_complete")
            .await
            .unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn restart_keeps_paused_task_out_of_recovery_dispatch_until_resumed() {
        let root = tempfile::tempdir().unwrap();
        let db_path = root.path().join("state.db");
        let task_id = TaskId::new();
        let node_id = NodeId::new();
        {
            let store = StateStore::start(&db_path).unwrap();
            store
                .submit(submission(&task_id, &node_id, root.path()))
                .await
                .unwrap();
            assert!(store.set_paused(task_id.clone(), true).await.unwrap());
        }

        let store = Arc::new(StateStore::start(&db_path).unwrap());
        assert!(store.recovery_snapshot()[0].paused);
        let runtime = ExecutionCoordinator::new(
            store,
            ResourceCapacity {
                cpu_units: 1,
                backend_processes: 1,
                ..ResourceCapacity::default()
            },
        )
        .unwrap();
        assert!(runtime.take_runnable_recovery().is_empty());

        let cancellation = tokio_util::sync::CancellationToken::new();
        let waiting = runtime.transition(
            &task_id,
            &node_id,
            1,
            "ready",
            "running",
            Stage::ResolveInputs,
            None,
            &cancellation,
        );
        tokio::pin!(waiting);
        assert!(
            tokio::time::timeout(Duration::from_millis(20), &mut waiting)
                .await
                .is_err()
        );
        assert!(runtime.set_paused(&task_id, false).await.unwrap());
        assert_eq!(runtime.take_runnable_recovery().len(), 1);
        assert!(tokio::time::timeout(Duration::from_secs(1), &mut waiting)
            .await
            .unwrap()
            .unwrap());
        runtime
            .stop_task(&task_id, "cancelled", "test_complete")
            .await
            .unwrap();
    }
}
