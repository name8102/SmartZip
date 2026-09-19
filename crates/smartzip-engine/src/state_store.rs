//! Dedicated SQLite execution-state thread.

use smartzip_core::{NodeId, TaskId};
use smartzip_db::task_execution::{
    NewNode, NewTaskExecution, PendingDecision, Priority, RecoveredTaskRecord,
    TaskExecutionRepository,
};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use tokio::sync::oneshot;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PersistedExtractPlan {
    pub policy: smartzip_config::SmartZipConfig,
    pub request: crate::ExtractWorkflowRequest,
}

impl PersistedExtractPlan {
    pub fn new(
        policy: smartzip_config::SmartZipConfig,
        mut request: crate::ExtractWorkflowRequest,
    ) -> Self {
        request.password_candidates.manual.clear();
        request.password_candidates.clipboard = None;
        Self { policy, request }
    }
}

fn execution_error(error: StateStoreError) -> smartzip_core::SmartZipError {
    smartzip_core::SmartZipError::BackendFailed {
        backend: "state-store".into(),
        exit_code: None,
        stderr: error.to_string(),
    }
}

#[derive(Debug, thiserror::Error)]
pub enum StateStoreError {
    #[error(transparent)]
    Database(#[from] smartzip_db::DbError),
    #[error("execution state store stopped")]
    Closed,
    #[error("invalid execution submission: {0}")]
    InvalidSubmission(String),
    #[error(transparent)]
    Host(#[from] std::io::Error),
}

#[derive(Debug, Clone)]
pub struct TaskSubmission {
    pub task_id: TaskId,
    pub kind: String,
    pub output_path: Option<PathBuf>,
    pub started_at: String,
    pub inputs_json: String,
    pub config_snapshot_json: String,
    pub priority: Priority,
    pub queue_position: i64,
    pub recoverable: bool,
    pub roots: Vec<NodeSubmission>,
}

#[derive(Debug, Clone)]
pub struct NodeSubmission {
    pub node_id: NodeId,
    pub parent_id: Option<NodeId>,
    pub root_id: NodeId,
    pub input_path: PathBuf,
    pub input_ref_json: String,
    pub config_revision: i64,
    pub generation: i64,
}

enum Command {
    Submit(TaskSubmission, Reply<()>),
    EnqueueChild(TaskId, NodeSubmission, Reply<bool>),
    RecordArtifacts {
        task_id: TaskId,
        node_id: NodeId,
        generation: i64,
        artifact_refs_json: String,
        reply: Reply<bool>,
    },
    SetPriority(TaskId, Priority, Reply<bool>),
    SetPaused(TaskId, bool, Reply<bool>),
    Reorder(TaskId, i64, Reply<bool>),
    Transition {
        task_id: TaskId,
        node_id: NodeId,
        generation: i64,
        from: String,
        to: String,
        stage: String,
        attempt_id: Option<String>,
        reply: Reply<bool>,
    },
    WaitForDecision {
        task_id: TaskId,
        node_id: NodeId,
        stage: String,
        decision: PendingDecision,
        reply: Reply<bool>,
    },
    AcceptDecision {
        task_id: TaskId,
        node_id: NodeId,
        generation: i64,
        decision_id: String,
        reply: Reply<bool>,
    },
    BeginCommit {
        task_id: TaskId,
        node_id: NodeId,
        generation: i64,
        commit_json: String,
        reply: Reply<bool>,
    },
    CommitPublished {
        task_id: TaskId,
        node_id: NodeId,
        generation: i64,
        commit_json: String,
        reply: Reply<bool>,
    },
    AbortCommit {
        task_id: TaskId,
        node_id: NodeId,
        generation: i64,
        reply: Reply<bool>,
    },
    FinishNode {
        task_id: TaskId,
        node_id: NodeId,
        generation: i64,
        outcome: crate::NodeOutcome,
        reply: Reply<bool>,
    },
    FinishPendingTask {
        task_id: TaskId,
        status: String,
        reason: String,
        reply: Reply<()>,
    },
    Shutdown,
}

type Reply<T> = oneshot::Sender<smartzip_db::Result<T>>;

pub struct StateStore {
    commands: mpsc::Sender<Command>,
    thread: Option<std::thread::JoinHandle<()>>,
    epoch: i64,
    recovery: Vec<RecoveredTaskRecord>,
}

impl StateStore {
    pub fn start(path: impl AsRef<Path>) -> Result<Self, StateStoreError> {
        let path = path.as_ref().to_path_buf();
        let (commands, receiver) = mpsc::channel();
        let (started_tx, started_rx) = mpsc::sync_channel(1);
        let thread = std::thread::Builder::new()
            .name("smartzip-state-store".into())
            .spawn(move || run(path, receiver, started_tx))
            .map_err(|error| StateStoreError::Database(error.into()))?;
        let (epoch, recovery) = started_rx.recv().map_err(|_| StateStoreError::Closed)??;
        Ok(Self {
            commands,
            thread: Some(thread),
            epoch,
            recovery,
        })
    }

    pub fn owner_epoch(&self) -> i64 {
        self.epoch
    }

    pub fn recovery_snapshot(&self) -> &[RecoveredTaskRecord] {
        &self.recovery
    }

    pub async fn submit(&self, task: TaskSubmission) -> Result<(), StateStoreError> {
        let (reply, receiver) = oneshot::channel();
        self.send(Command::Submit(task, reply))?;
        receive(receiver).await
    }

    pub async fn enqueue_child(
        &self,
        task_id: TaskId,
        node: NodeSubmission,
    ) -> Result<bool, StateStoreError> {
        let (reply, receiver) = oneshot::channel();
        self.send(Command::EnqueueChild(task_id, node, reply))?;
        receive(receiver).await
    }

    pub async fn record_staging(
        &self,
        task_id: TaskId,
        node_id: NodeId,
        generation: i64,
        path: PathBuf,
    ) -> Result<bool, StateStoreError> {
        let artifact_refs_json = serde_json::to_string(&crate::StagingArtifact { path })
            .map_err(|error| StateStoreError::InvalidSubmission(error.to_string()))?;
        let (reply, receiver) = oneshot::channel();
        self.send(Command::RecordArtifacts {
            task_id,
            node_id,
            generation,
            artifact_refs_json,
            reply,
        })?;
        receive(receiver).await
    }

    pub async fn set_priority(
        &self,
        task_id: TaskId,
        priority: Priority,
    ) -> Result<bool, StateStoreError> {
        let (reply, receiver) = oneshot::channel();
        self.send(Command::SetPriority(task_id, priority, reply))?;
        receive(receiver).await
    }

    pub async fn set_paused(&self, task_id: TaskId, paused: bool) -> Result<bool, StateStoreError> {
        let (reply, receiver) = oneshot::channel();
        self.send(Command::SetPaused(task_id, paused, reply))?;
        receive(receiver).await
    }

    pub async fn reorder(
        &self,
        task_id: TaskId,
        queue_position: i64,
    ) -> Result<bool, StateStoreError> {
        let (reply, receiver) = oneshot::channel();
        self.send(Command::Reorder(task_id, queue_position, reply))?;
        receive(receiver).await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn transition(
        &self,
        task_id: TaskId,
        node_id: NodeId,
        generation: i64,
        from: impl Into<String>,
        to: impl Into<String>,
        stage: impl Into<String>,
        attempt_id: Option<String>,
    ) -> Result<bool, StateStoreError> {
        let (reply, receiver) = oneshot::channel();
        self.send(Command::Transition {
            task_id,
            node_id,
            generation,
            from: from.into(),
            to: to.into(),
            stage: stage.into(),
            attempt_id,
            reply,
        })?;
        receive(receiver).await
    }

    pub async fn wait_for_decision(
        &self,
        task_id: TaskId,
        node_id: NodeId,
        stage: impl Into<String>,
        decision: PendingDecision,
    ) -> Result<bool, StateStoreError> {
        let (reply, receiver) = oneshot::channel();
        self.send(Command::WaitForDecision {
            task_id,
            node_id,
            stage: stage.into(),
            decision,
            reply,
        })?;
        receive(receiver).await
    }

    pub async fn accept_decision(
        &self,
        task_id: TaskId,
        node_id: NodeId,
        generation: i64,
        decision_id: impl Into<String>,
    ) -> Result<bool, StateStoreError> {
        let (reply, receiver) = oneshot::channel();
        self.send(Command::AcceptDecision {
            task_id,
            node_id,
            generation,
            decision_id: decision_id.into(),
            reply,
        })?;
        receive(receiver).await
    }

    pub async fn begin_commit(
        &self,
        task_id: TaskId,
        node_id: NodeId,
        generation: i64,
        commit_json: String,
    ) -> Result<bool, StateStoreError> {
        let (reply, receiver) = oneshot::channel();
        self.send(Command::BeginCommit {
            task_id,
            node_id,
            generation,
            commit_json,
            reply,
        })?;
        receive(receiver).await
    }

    pub async fn commit_published(
        &self,
        task_id: TaskId,
        node_id: NodeId,
        generation: i64,
        commit_json: String,
    ) -> Result<bool, StateStoreError> {
        let (reply, receiver) = oneshot::channel();
        self.send(Command::CommitPublished {
            task_id,
            node_id,
            generation,
            commit_json,
            reply,
        })?;
        receive(receiver).await
    }

    pub async fn abort_commit(
        &self,
        task_id: TaskId,
        node_id: NodeId,
        generation: i64,
    ) -> Result<bool, StateStoreError> {
        let (reply, receiver) = oneshot::channel();
        self.send(Command::AbortCommit {
            task_id,
            node_id,
            generation,
            reply,
        })?;
        receive(receiver).await
    }

    pub async fn finish_node(
        &self,
        task_id: TaskId,
        node_id: NodeId,
        generation: i64,
        outcome: crate::NodeOutcome,
    ) -> Result<bool, StateStoreError> {
        let (reply, receiver) = oneshot::channel();
        self.send(Command::FinishNode {
            task_id,
            node_id,
            generation,
            outcome,
            reply,
        })?;
        receive(receiver).await
    }

    pub async fn finish_pending_task(
        &self,
        task_id: TaskId,
        status: String,
        reason: String,
    ) -> Result<(), StateStoreError> {
        let (reply, receiver) = oneshot::channel();
        self.send(Command::FinishPendingTask {
            task_id,
            status,
            reason,
            reply,
        })?;
        receive(receiver).await
    }

    fn send(&self, command: Command) -> Result<(), StateStoreError> {
        self.commands
            .send(command)
            .map_err(|_| StateStoreError::Closed)
    }
}

impl Drop for StateStore {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Shutdown);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[async_trait::async_trait(?Send)]
impl crate::ExecutionStateRecorder for StateStore {
    async fn enqueue_child(
        &self,
        task_id: &TaskId,
        node_id: &NodeId,
        parent_id: &NodeId,
        root_id: &NodeId,
        candidate: &crate::ExtractionCandidate,
        generation: u64,
    ) -> smartzip_core::Result<bool> {
        StateStore::enqueue_child(
            self,
            task_id.clone(),
            NodeSubmission {
                node_id: node_id.clone(),
                parent_id: Some(parent_id.clone()),
                root_id: root_id.clone(),
                input_path: candidate.path.clone(),
                input_ref_json: serde_json::to_string(candidate).map_err(|error| {
                    execution_error(StateStoreError::InvalidSubmission(error.to_string()))
                })?,
                config_revision: 0,
                generation: generation as i64,
            },
        )
        .await
        .map_err(execution_error)
    }

    async fn transition(
        &self,
        task_id: &TaskId,
        node_id: &NodeId,
        generation: u64,
        from: &str,
        to: &str,
        stage: crate::coordinator::Stage,
        attempt_id: Option<&smartzip_core::AttemptId>,
        _cancellation: &tokio_util::sync::CancellationToken,
    ) -> smartzip_core::Result<bool> {
        StateStore::transition(
            self,
            task_id.clone(),
            node_id.clone(),
            generation as i64,
            from,
            to,
            stage.as_str(),
            attempt_id.map(ToString::to_string),
        )
        .await
        .map_err(execution_error)
    }

    fn release_stage(&self, _task_id: &TaskId, _node_id: &NodeId) {}

    async fn record_staging(
        &self,
        task_id: &TaskId,
        node_id: &NodeId,
        generation: u64,
        path: &Path,
    ) -> smartzip_core::Result<bool> {
        StateStore::record_staging(
            self,
            task_id.clone(),
            node_id.clone(),
            generation as i64,
            path.to_path_buf(),
        )
        .await
        .map_err(execution_error)
    }

    async fn wait_for_decision(
        &self,
        task_id: &TaskId,
        node_id: &NodeId,
        generation: u64,
        stage: crate::coordinator::Stage,
        decision_id: &smartzip_core::DecisionId,
        kind: &str,
        evidence: &str,
    ) -> smartzip_core::Result<bool> {
        StateStore::wait_for_decision(
            self,
            task_id.clone(),
            node_id.clone(),
            stage.as_str(),
            PendingDecision {
                id: decision_id.to_string(),
                generation: generation as i64,
                kind: kind.into(),
                evidence: evidence.into(),
            },
        )
        .await
        .map_err(execution_error)
    }

    async fn accept_decision(
        &self,
        task_id: &TaskId,
        node_id: &NodeId,
        generation: u64,
        decision_id: &smartzip_core::DecisionId,
    ) -> smartzip_core::Result<bool> {
        StateStore::accept_decision(
            self,
            task_id.clone(),
            node_id.clone(),
            generation as i64,
            decision_id.to_string(),
        )
        .await
        .map_err(execution_error)
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
        .map_err(|error| execution_error(StateStoreError::InvalidSubmission(error.to_string())))?;
        StateStore::begin_commit(
            self,
            task_id.clone(),
            node_id.clone(),
            generation as i64,
            commit_json,
        )
        .await
        .map_err(execution_error)
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
        .map_err(|error| execution_error(StateStoreError::InvalidSubmission(error.to_string())))?;
        StateStore::commit_published(
            self,
            task_id.clone(),
            node_id.clone(),
            generation as i64,
            commit_json,
        )
        .await
        .map_err(execution_error)
    }

    async fn abort_commit(
        &self,
        task_id: &TaskId,
        node_id: &NodeId,
        generation: u64,
    ) -> smartzip_core::Result<bool> {
        StateStore::abort_commit(self, task_id.clone(), node_id.clone(), generation as i64)
            .await
            .map_err(execution_error)
    }

    async fn finish_node(
        &self,
        task_id: &TaskId,
        node_id: &NodeId,
        generation: u64,
        outcome: crate::NodeOutcome,
    ) -> smartzip_core::Result<bool> {
        StateStore::finish_node(
            self,
            task_id.clone(),
            node_id.clone(),
            generation as i64,
            outcome,
        )
        .await
        .map_err(execution_error)
    }
}

async fn receive<T>(
    receiver: oneshot::Receiver<smartzip_db::Result<T>>,
) -> Result<T, StateStoreError> {
    receiver
        .await
        .map_err(|_| StateStoreError::Closed)?
        .map_err(Into::into)
}

fn run(
    path: PathBuf,
    receiver: mpsc::Receiver<Command>,
    started: mpsc::SyncSender<smartzip_db::Result<(i64, Vec<RecoveredTaskRecord>)>>,
) {
    let mut owner = match smartzip_db::SmartZipDb::acquire_execution(&path) {
        Ok(owner) => owner,
        Err(error) => {
            let _ = started.send(Err(error));
            return;
        }
    };
    let epoch = owner.epoch();
    let recovery = TaskExecutionRepository::new(owner.database_mut().connection_mut())
        .claim_recoverable(epoch);
    let recovery = match recovery.and_then(|mut recovery| {
        reconcile_staging(owner.database_mut(), &mut recovery)?;
        reconcile_commits(owner.database_mut(), &mut recovery)?;
        Ok(recovery)
    }) {
        Ok(recovery) => recovery,
        Err(error) => {
            let _ = started.send(Err(error));
            return;
        }
    };
    if started.send(Ok((epoch, recovery))).is_err() {
        return;
    }

    for command in receiver {
        let mut repo = TaskExecutionRepository::new(owner.database_mut().connection_mut());
        match command {
            Command::Submit(task, reply) => {
                let output = task
                    .output_path
                    .as_ref()
                    .map(|path| path.to_string_lossy().into_owned());
                let root_paths: Vec<_> = task
                    .roots
                    .iter()
                    .map(|root| root.input_path.to_string_lossy().into_owned())
                    .collect();
                let roots: Vec<_> = task
                    .roots
                    .iter()
                    .zip(root_paths.iter())
                    .map(|(root, path)| NewNode {
                        id: root.node_id.as_str(),
                        parent_id: root.parent_id.as_ref().map(NodeId::as_str),
                        root_id: root.root_id.as_str(),
                        input_path: path,
                        input_ref_json: &root.input_ref_json,
                        config_revision: root.config_revision,
                        generation: root.generation,
                    })
                    .collect();
                let result = repo.submit(
                    NewTaskExecution {
                        id: task.task_id.as_str(),
                        kind: &task.kind,
                        output_path: output.as_deref(),
                        started_at: &task.started_at,
                        inputs_json: &task.inputs_json,
                        config_snapshot_json: &task.config_snapshot_json,
                        priority: task.priority,
                        queue_position: task.queue_position,
                        recoverable: task.recoverable,
                        owner_epoch: epoch,
                    },
                    &roots,
                );
                let _ = reply.send(result);
            }
            Command::EnqueueChild(task_id, node, reply) => {
                let path = node.input_path.to_string_lossy().into_owned();
                let result = repo.enqueue_child(
                    task_id.as_str(),
                    NewNode {
                        id: node.node_id.as_str(),
                        parent_id: node.parent_id.as_ref().map(NodeId::as_str),
                        root_id: node.root_id.as_str(),
                        input_path: &path,
                        input_ref_json: &node.input_ref_json,
                        config_revision: node.config_revision,
                        generation: node.generation,
                    },
                );
                let _ = reply.send(result);
            }
            Command::RecordArtifacts {
                task_id,
                node_id,
                generation,
                artifact_refs_json,
                reply,
            } => {
                let result = repo.record_artifacts(
                    task_id.as_str(),
                    node_id.as_str(),
                    generation,
                    &artifact_refs_json,
                );
                let _ = reply.send(result);
            }
            Command::SetPriority(task_id, priority, reply) => {
                let result = repo.set_priority(task_id.as_str(), priority);
                let _ = reply.send(result);
            }
            Command::SetPaused(task_id, paused, reply) => {
                let result = repo.set_paused(task_id.as_str(), paused);
                let _ = reply.send(result);
            }
            Command::Reorder(task_id, queue_position, reply) => {
                let result = repo.reorder(task_id.as_str(), queue_position);
                let _ = reply.send(result);
            }
            Command::Transition {
                task_id,
                node_id,
                generation,
                from,
                to,
                stage,
                attempt_id,
                reply,
            } => {
                let result = repo.transition(
                    task_id.as_str(),
                    node_id.as_str(),
                    generation,
                    &from,
                    &to,
                    &stage,
                    attempt_id.as_deref(),
                );
                let _ = reply.send(result);
            }
            Command::WaitForDecision {
                task_id,
                node_id,
                stage,
                decision,
                reply,
            } => {
                let result =
                    repo.wait_for_decision(task_id.as_str(), node_id.as_str(), &stage, &decision);
                let _ = reply.send(result);
            }
            Command::BeginCommit {
                task_id,
                node_id,
                generation,
                commit_json,
                reply,
            } => {
                let result =
                    repo.begin_commit(task_id.as_str(), node_id.as_str(), generation, &commit_json);
                let _ = reply.send(result);
            }
            Command::CommitPublished {
                task_id,
                node_id,
                generation,
                commit_json,
                reply,
            } => {
                let result = repo.commit_published(
                    task_id.as_str(),
                    node_id.as_str(),
                    generation,
                    &commit_json,
                );
                let _ = reply.send(result);
            }
            Command::AbortCommit {
                task_id,
                node_id,
                generation,
                reply,
            } => {
                let result = repo.abort_commit(task_id.as_str(), node_id.as_str(), generation);
                let _ = reply.send(result);
            }
            Command::AcceptDecision {
                task_id,
                node_id,
                generation,
                decision_id,
                reply,
            } => {
                let result = repo.accept_decision_reply(
                    task_id.as_str(),
                    node_id.as_str(),
                    generation,
                    &decision_id,
                    epoch,
                );
                let _ = reply.send(result);
            }
            Command::FinishNode {
                task_id,
                node_id,
                generation,
                outcome,
                reply,
            } => {
                let output = outcome
                    .output_path
                    .as_ref()
                    .map(|path| path.to_string_lossy().into_owned());
                let result = repo.finish_node(
                    task_id.as_str(),
                    node_id.as_str(),
                    generation,
                    &outcome.status,
                    outcome.reason.as_deref(),
                    output.as_deref(),
                    outcome.committed,
                    outcome.sample_hash.as_deref(),
                    outcome.file_size,
                    outcome.embedded_offset.map(|offset| offset as i64),
                    outcome.has_password,
                    outcome.password_id,
                    outcome.encoding.as_deref(),
                    outcome.encoding_corrected,
                    outcome.output_files,
                    outcome.output_bytes,
                );
                let _ = reply.send(result);
            }
            Command::FinishPendingTask {
                task_id,
                status,
                reason,
                reply,
            } => {
                let result = repo.finish_pending_task(task_id.as_str(), &status, &reason);
                let _ = reply.send(result);
            }
            Command::Shutdown => break,
        }
    }
}

enum CommitRecovery {
    Published {
        output: PathBuf,
        cleanup_warning: Option<String>,
    },
    Retry,
    Failed(String),
}

fn reconcile_staging(
    db: &mut smartzip_db::SmartZipDb,
    tasks: &mut Vec<RecoveredTaskRecord>,
) -> smartzip_db::Result<()> {
    for task in tasks.iter_mut() {
        let output_root = task
            .output_path
            .as_deref()
            .map(Path::new)
            .map(Path::to_path_buf);
        let mut remaining = Vec::with_capacity(task.nodes.len());
        for mut node in task.nodes.drain(..) {
            let Some(artifact_json) = node.artifact_refs_json.as_deref() else {
                remaining.push(node);
                continue;
            };
            if node.commit_json.is_some() {
                remaining.push(node);
                continue;
            }
            let artifact = serde_json::from_str::<crate::StagingArtifact>(artifact_json)?;
            let owned = output_root.as_ref().is_some_and(|output_root| {
                artifact
                    .path
                    .parent()
                    .is_some_and(|parent| parent.starts_with(output_root))
                    && artifact
                        .path
                        .file_name()
                        .is_some_and(|name| name.to_string_lossy().starts_with(".smartzip-"))
            });
            let cleanup = if owned {
                std::fs::remove_dir_all(&artifact.path)
            } else {
                Err(std::io::Error::other(format!(
                    "staging path is outside the task output directory: {}",
                    artifact.path.display()
                )))
            };
            let cleanup = match cleanup {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(error),
            };
            let mut repo = TaskExecutionRepository::new(db.connection_mut());
            match cleanup {
                Ok(()) => {
                    repo.clear_artifacts(&node.task_id, &node.node_id, node.generation)?;
                    node.artifact_refs_json = None;
                    remaining.push(node);
                }
                Err(error) => {
                    repo.finish_node(
                        &node.task_id,
                        &node.node_id,
                        node.generation,
                        "failed",
                        Some(&format!("staging_cleanup_failed: {error}")),
                        None,
                        false,
                        None,
                        None,
                        None,
                        false,
                        None,
                        None,
                        false,
                        0,
                        0,
                    )?;
                }
            }
        }
        task.nodes = remaining;
    }
    tasks.retain(|task| !task.nodes.is_empty());
    Ok(())
}

fn reconcile_commits(
    db: &mut smartzip_db::SmartZipDb,
    tasks: &mut Vec<RecoveredTaskRecord>,
) -> smartzip_db::Result<()> {
    for task in tasks.iter_mut() {
        let snapshot = task.config_snapshot_json.clone();
        let mut nested_count = task.nested_candidate_count;
        let mut stop_reason = None;
        let mut remaining = Vec::with_capacity(task.nodes.len());
        for mut node in task.nodes.drain(..) {
            let Some(commit_json) = node.commit_json.as_deref() else {
                remaining.push(node);
                continue;
            };
            let (recovery, output_files, output_bytes, success) =
                match serde_json::from_str::<crate::CommitRecord>(commit_json) {
                    Ok(record) => (
                        reconcile_commit_record(&record),
                        record.intent().output_files,
                        record.intent().output_bytes,
                        record.intent().success.clone(),
                    ),
                    Err(error) => (
                        CommitRecovery::Failed(format!("invalid_commit_record: {error}")),
                        0,
                        0,
                        crate::CommitSuccessFacts::default(),
                    ),
                };
            let mut repo = TaskExecutionRepository::new(db.connection_mut());
            match recovery {
                CommitRecovery::Published {
                    output,
                    cleanup_warning,
                } => {
                    let mut reason = cleanup_warning;
                    if stop_reason.is_none() {
                        let snapshot = snapshot.as_deref().ok_or_else(|| {
                            std::io::Error::new(
                                std::io::ErrorKind::InvalidData,
                                format!(
                                    "recoverable task {} has no configuration snapshot",
                                    task.task_id
                                ),
                            )
                        })?;
                        let plan: PersistedExtractPlan = serde_json::from_str(snapshot)?;
                        let discovered = recover_published_children(
                            &mut repo,
                            &plan,
                            &node,
                            &output,
                            &mut nested_count,
                        )?;
                        remaining.extend(discovered.nodes);
                        if discovered.limit_exceeded {
                            let limit_reason = "nested_candidate_limit_exceeded".to_string();
                            reason = Some(match reason {
                                Some(warning) => format!("{warning}; {limit_reason}"),
                                None => limit_reason.clone(),
                            });
                            stop_reason = Some(limit_reason);
                        }
                    }
                    let finished = repo.finish_node(
                        &node.task_id,
                        &node.node_id,
                        node.generation,
                        "extracted",
                        reason.as_deref(),
                        Some(&output.to_string_lossy()),
                        true,
                        success.sample_hash.as_deref(),
                        success.file_size,
                        success.embedded_offset.map(|offset| offset as i64),
                        success.has_password,
                        success.password_id,
                        success.encoding.as_deref(),
                        success.encoding_corrected,
                        output_files,
                        output_bytes,
                    )?;
                    if !finished {
                        return Err(std::io::Error::other(format!(
                            "published recovery lost node {} generation {}",
                            node.node_id, node.generation
                        ))
                        .into());
                    }
                    task.committed_output_files =
                        task.committed_output_files.saturating_add(output_files);
                    task.committed_output_bytes =
                        task.committed_output_bytes.saturating_add(output_bytes);
                }
                CommitRecovery::Retry if stop_reason.is_none() => {
                    if repo.reset_commit_for_retry(&node.task_id, &node.node_id, node.generation)? {
                        node.generation += 1;
                        node.stage = "resolve_inputs".into();
                        node.execution_state = "ready".into();
                        node.commit_json = None;
                        remaining.push(node);
                    }
                }
                CommitRecovery::Retry => {
                    repo.finish_node(
                        &node.task_id,
                        &node.node_id,
                        node.generation,
                        "failed",
                        stop_reason.as_deref(),
                        None,
                        false,
                        None,
                        None,
                        None,
                        false,
                        None,
                        None,
                        false,
                        0,
                        0,
                    )?;
                }
                CommitRecovery::Failed(reason) => {
                    repo.finish_node(
                        &node.task_id,
                        &node.node_id,
                        node.generation,
                        "failed",
                        Some(&reason),
                        None,
                        false,
                        None,
                        None,
                        None,
                        false,
                        None,
                        None,
                        false,
                        0,
                        0,
                    )?;
                }
            }
        }
        task.nested_candidate_count = nested_count;
        if let Some(reason) = stop_reason {
            let mut repo = TaskExecutionRepository::new(db.connection_mut());
            repo.finish_pending_task(&task.task_id, "failed", &reason)?;
            repo.set_terminal_task_status(&task.task_id, "failed")?;
            remaining.clear();
        }
        task.nodes = remaining;
    }
    tasks.retain(|task| !task.nodes.is_empty());
    Ok(())
}

struct RecoveredChildren {
    nodes: Vec<smartzip_db::task_execution::NodeRecord>,
    limit_exceeded: bool,
}

fn recover_published_children(
    repo: &mut TaskExecutionRepository<'_>,
    plan: &PersistedExtractPlan,
    parent: &smartzip_db::task_execution::NodeRecord,
    output: &Path,
    nested_count: &mut usize,
) -> smartzip_db::Result<RecoveredChildren> {
    let mut candidate: crate::ExtractionCandidate = serde_json::from_str(&parent.input_ref_json)?;
    candidate.relative_path =
        crate::nested::output_relative_path_for(&plan.request.output_dir, output);
    if candidate.depth >= plan.request.recursion_limit {
        return Ok(RecoveredChildren {
            nodes: Vec::new(),
            limit_exceeded: false,
        });
    }

    let scanner = smartzip_scanner::EmbeddedScanner::new(plan.request.scanner.clone());
    let mut policy = crate::policy::embedded_policy_from_request(&plan.request);
    policy.mode = match plan.policy.extraction.embedded.nested {
        smartzip_config::NestedScan::Largest => smartzip_core::EmbeddedScanMode::Largest,
        smartzip_config::NestedScan::Off => smartzip_core::EmbeddedScanMode::Ignore,
        smartzip_config::NestedScan::Auto => smartzip_core::EmbeddedScanMode::Auto,
        smartzip_config::NestedScan::Ask => smartzip_core::EmbeddedScanMode::Ask,
        smartzip_config::NestedScan::Aggressive => smartzip_core::EmbeddedScanMode::Aggressive,
        smartzip_config::NestedScan::All => smartzip_core::EmbeddedScanMode::All,
    };
    let nested_embedded_enabled = plan.policy.extraction.embedded.nested
        != smartzip_config::NestedScan::Off
        && policy.mode != smartzip_core::EmbeddedScanMode::Ignore;
    let aggressive = matches!(
        plan.policy.extraction.embedded.nested,
        smartzip_config::NestedScan::Aggressive | smartzip_config::NestedScan::All
    );
    let discovered = crate::nested::discover_nested_candidates(
        &scanner,
        output,
        candidate.depth + 1,
        &crate::nested::candidate_output_relative_path(&candidate),
        &policy,
        nested_embedded_enabled,
        aggressive,
        &tokio_util::sync::CancellationToken::new(),
    );
    let mut recovered = Vec::new();
    let mut limit_exceeded = false;
    for child in discovered {
        let child_id = NodeId::new();
        let input_ref_json = serde_json::to_string(&child)?;
        if *nested_count >= plan.request.limits.max_nested_candidates {
            if repo.child_exists(&parent.task_id, &parent.node_id, &input_ref_json)? {
                continue;
            }
            limit_exceeded = true;
            break;
        }
        let inserted = repo.enqueue_child(
            &parent.task_id,
            NewNode {
                id: child_id.as_str(),
                parent_id: Some(&parent.node_id),
                root_id: &parent.root_node_id,
                input_path: &child.path.to_string_lossy(),
                input_ref_json: &input_ref_json,
                config_revision: 0,
                generation: 0,
            },
        )?;
        if inserted {
            *nested_count += 1;
            recovered.push(smartzip_db::task_execution::NodeRecord {
                task_id: parent.task_id.clone(),
                node_id: child_id.to_string(),
                parent_node_id: Some(parent.node_id.clone()),
                root_node_id: parent.root_node_id.clone(),
                generation: 0,
                input_path: child.path.to_string_lossy().into_owned(),
                input_ref_json,
                stage: "resolve_inputs".into(),
                execution_state: "ready".into(),
                decision: None,
                artifact_refs_json: None,
                commit_json: None,
            });
        }
    }
    Ok(RecoveredChildren {
        nodes: recovered,
        limit_exceeded,
    })
}

fn reconcile_commit_record(record: &crate::CommitRecord) -> CommitRecovery {
    let intent = record.intent();
    let marker_matches = match std::fs::read_to_string(&intent.marker_path) {
        Ok(value) if value == intent.commit_id => true,
        Ok(_) => return CommitRecovery::Failed("commit_marker_changed".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => {
            return CommitRecovery::Failed(format!("commit_marker_inspection_failed: {error}"));
        }
    };
    let target_is_published = match intent.source_identity.matches_object(&intent.target_path) {
        Ok(matches) => matches,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => {
            return CommitRecovery::Failed(format!("commit_target_inspection_failed: {error}"));
        }
    };

    if matches!(record, crate::CommitRecord::Published { .. }) || marker_matches {
        if target_is_published {
            return CommitRecovery::Published {
                output: intent.target_path.clone(),
                cleanup_warning: cleanup_reconciled_commit(intent),
            };
        }
    }
    if matches!(record, crate::CommitRecord::Published { .. }) {
        return CommitRecovery::Failed("published_commit_output_changed".into());
    }

    let source_matches = match intent.source_identity.matches_object(&intent.source_path) {
        Ok(matches) => matches,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => {
            return CommitRecovery::Failed(format!("commit_source_inspection_failed: {error}"));
        }
    };
    if !source_matches {
        return CommitRecovery::Failed("prepared_commit_artifacts_changed".into());
    }
    let backup = match intent.backup_path.as_ref() {
        Some(path) => match std::fs::symlink_metadata(path.join("original")) {
            Ok(_) => Some(path),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => {
                return CommitRecovery::Failed(format!("commit_backup_inspection_failed: {error}"));
            }
        },
        None => None,
    };
    if let Some(backup) = backup {
        match std::fs::symlink_metadata(&intent.target_path) {
            Ok(_) => {
                return CommitRecovery::Failed(
                    "prepared_commit_target_and_backup_both_exist".into(),
                );
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return CommitRecovery::Failed(format!(
                    "prepared_commit_target_inspection_failed: {error}"
                ));
            }
        }
        if let Err(error) =
            crate::materialize::rename_no_replace(&backup.join("original"), &intent.target_path)
        {
            return CommitRecovery::Failed(format!("commit_backup_restore_failed: {error}"));
        }
        if let Err(error) = std::fs::remove_dir(backup) {
            if backup.exists() {
                return CommitRecovery::Failed(format!("commit_backup_cleanup_failed: {error}"));
            }
        }
    } else {
        match target_matches_before(intent) {
            Ok(true) => {}
            Ok(false) => {
                return CommitRecovery::Failed("prepared_commit_target_changed".into());
            }
            Err(error) => return CommitRecovery::Failed(error),
        }
    }

    if let Err(error) = std::fs::remove_dir_all(&intent.staging_path) {
        if intent.staging_path.exists() {
            return CommitRecovery::Failed(format!("commit_staging_cleanup_failed: {error}"));
        }
    }
    if marker_matches {
        if let Err(error) = std::fs::remove_file(&intent.marker_path) {
            if intent.marker_path.exists() {
                return CommitRecovery::Failed(format!("commit_marker_cleanup_failed: {error}"));
            }
        }
    }
    CommitRecovery::Retry
}

fn target_matches_before(intent: &crate::CommitIntent) -> Result<bool, String> {
    match &intent.target_before {
        Some(identity) => identity
            .matches_version(&intent.target_path)
            .map_err(|error| format!("prepared_commit_target_inspection_failed: {error}")),
        None => match std::fs::symlink_metadata(&intent.target_path) {
            Ok(_) => Ok(false),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(true),
            Err(error) => Err(format!("prepared_commit_target_inspection_failed: {error}")),
        },
    }
}

fn cleanup_reconciled_commit(intent: &crate::CommitIntent) -> Option<String> {
    let mut failures = Vec::new();
    if let Some(backup) = &intent.backup_path {
        if let Err(error) = std::fs::remove_dir_all(backup) {
            if backup.exists() {
                failures.push(format!("{}: {error}", backup.display()));
            }
        }
    }
    if let Err(error) = std::fs::remove_file(&intent.marker_path) {
        if intent.marker_path.exists() {
            failures.push(format!("{}: {error}", intent.marker_path.display()));
        }
    }
    if let Err(error) = std::fs::remove_dir_all(&intent.staging_path) {
        if intent.staging_path.exists() {
            failures.push(format!("{}: {error}", intent.staging_path.display()));
        }
    }
    (!failures.is_empty()).then(|| format!("cleanup_incomplete: {}", failures.join("; ")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn extract_plan(input: &Path, output: &Path) -> PersistedExtractPlan {
        let mut policy = smartzip_config::SmartZipConfig::default();
        policy.extraction.recursion.enabled = true;
        policy.extraction.recursion.max_depth = 3;
        policy.extraction.embedded.nested = smartzip_config::NestedScan::Off;
        PersistedExtractPlan::new(
            policy,
            crate::ExtractWorkflowRequest {
                inputs: vec![input.to_path_buf()],
                output_dir: output.to_path_buf(),
                recursion_limit: 3,
                scanner: smartzip_scanner::ScannerConfig::default(),
                encoding_mode: smartzip_core::EncodingMode::Auto,
                password_candidates: smartzip_passwords::PasswordCandidateRequest::default(),
                layout_policy: crate::layout::OutputLayoutPolicy::default(),
                single_root_name_policy: crate::layout::SingleRootNamePolicy::default(),
                embedded_scan_mode: smartzip_core::EmbeddedScanMode::Auto,
                dominant_min_ratio: 0.7,
                confirm_large_scan: false,
                force: false,
                limits: smartzip_config::ExtractionLimits::default(),
            },
        )
    }

    fn task_submission(
        task_id: &TaskId,
        node_id: &NodeId,
        input: &Path,
        output: &Path,
    ) -> TaskSubmission {
        let candidate = crate::ExtractionCandidate::root(input.to_path_buf());
        TaskSubmission {
            task_id: task_id.clone(),
            kind: "extract".into(),
            output_path: Some(output.to_path_buf()),
            started_at: "2026-09-19T00:00:00Z".into(),
            inputs_json: serde_json::to_string(&[input]).unwrap(),
            config_snapshot_json: serde_json::to_string(&extract_plan(input, output)).unwrap(),
            priority: Priority::Normal,
            queue_position: 0,
            recoverable: true,
            roots: vec![NodeSubmission {
                node_id: node_id.clone(),
                parent_id: None,
                root_id: node_id.clone(),
                input_path: input.to_path_buf(),
                input_ref_json: serde_json::to_string(&candidate).unwrap(),
                config_revision: 0,
                generation: 0,
            }],
        }
    }

    #[tokio::test]
    async fn state_store_serializes_submission_and_transitions() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("state.db");
        let store = StateStore::start(&path).unwrap();
        let task_id = TaskId::new();
        let node_id = NodeId::new();
        store
            .submit(TaskSubmission {
                task_id: task_id.clone(),
                kind: "extract".into(),
                output_path: Some(root.path().join("out")),
                started_at: "2026-09-19T00:00:00Z".into(),
                inputs_json: "[]".into(),
                config_snapshot_json: "{}".into(),
                priority: Priority::Normal,
                queue_position: 0,
                recoverable: true,
                roots: vec![NodeSubmission {
                    node_id: node_id.clone(),
                    parent_id: None,
                    root_id: node_id.clone(),
                    input_path: root.path().join("input.zip"),
                    input_ref_json: "{}".into(),
                    config_revision: 0,
                    generation: 0,
                }],
            })
            .await
            .unwrap();
        assert!(store
            .transition(
                task_id.clone(),
                node_id.clone(),
                0,
                "ready",
                "running",
                "scan_embedded",
                Some("attempt".into()),
            )
            .await
            .unwrap());
        assert!(!store
            .transition(
                task_id,
                node_id,
                0,
                "ready",
                "running",
                "scan_embedded",
                None,
            )
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn restart_removes_owned_staging_before_retry() {
        let root = tempfile::tempdir().unwrap();
        let db_path = root.path().join("state.db");
        let input = root.path().join("input.zip");
        let output = root.path().join("out");
        std::fs::write(&input, b"input").unwrap();
        let task_id = TaskId::new();
        let node_id = NodeId::new();
        {
            let store = StateStore::start(&db_path).unwrap();
            store
                .submit(task_submission(&task_id, &node_id, &input, &output))
                .await
                .unwrap();
            assert!(store
                .transition(
                    task_id.clone(),
                    node_id.clone(),
                    0,
                    "ready",
                    "running",
                    "extract_attempt",
                    Some("attempt".into()),
                )
                .await
                .unwrap());
            std::fs::create_dir_all(&output).unwrap();
            let staging = output.join(".smartzip-crashed");
            std::fs::create_dir(&staging).unwrap();
            std::fs::write(staging.join("partial"), b"partial").unwrap();
            assert!(store
                .record_staging(task_id.clone(), node_id.clone(), 0, staging)
                .await
                .unwrap());
        }

        let store = StateStore::start(&db_path).unwrap();
        let node = &store.recovery_snapshot()[0].nodes[0];
        assert_eq!(node.generation, 1);
        assert_eq!(node.execution_state, "ready");
        assert!(node.artifact_refs_json.is_none());
        assert!(!output.join(".smartzip-crashed").exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn published_parent_recovery_discovers_children_without_duplicates() {
        let root = tempfile::tempdir().unwrap();
        let db_path = root.path().join("state.db");
        let input = root.path().join("input.zip");
        let output_root = root.path().join("out");
        let staging = root.path().join(".smartzip-published");
        let target = output_root.join("input");
        let marker = root.path().join("commit-marker");
        std::fs::write(&input, b"input").unwrap();
        std::fs::create_dir_all(&staging).unwrap();
        std::fs::write(staging.join("child.zip"), b"nested").unwrap();
        let intent = crate::CommitIntent {
            commit_id: "commit-id".into(),
            staging_path: staging.clone(),
            source_path: staging.clone(),
            target_path: target.clone(),
            marker_path: marker.clone(),
            backup_path: None,
            source_identity: crate::ArtifactIdentity::capture(&staging).unwrap(),
            target_before: None,
            output_files: 1,
            output_bytes: 6,
            success: crate::CommitSuccessFacts {
                sample_hash: Some("recovery-hash".into()),
                file_size: Some(5),
                embedded_offset: Some(11),
                has_password: true,
                password_id: None,
                encoding: Some("shift_jis".into()),
                encoding_corrected: true,
            },
        };
        let task_id = TaskId::new();
        let node_id = NodeId::new();
        let existing_child_id = NodeId::new();
        let existing_child = crate::ExtractionCandidate {
            path: target.join("child.zip"),
            relative_path: PathBuf::from("input/child"),
            depth: 1,
            source: crate::CandidateSource::ExtractedFile,
            detected_format: Some(smartzip_core::ArchiveFormat::Zip),
            embedded_offset: None,
            embedded_size: None,
        };
        {
            let store = StateStore::start(&db_path).unwrap();
            store
                .submit(task_submission(&task_id, &node_id, &input, &output_root))
                .await
                .unwrap();
            assert!(store
                .transition(
                    task_id.clone(),
                    node_id.clone(),
                    0,
                    "ready",
                    "running",
                    "commit",
                    Some("attempt".into()),
                )
                .await
                .unwrap());
            assert!(store
                .begin_commit(
                    task_id.clone(),
                    node_id.clone(),
                    0,
                    serde_json::to_string(&crate::CommitRecord::Prepared {
                        intent: intent.clone(),
                    })
                    .unwrap(),
                )
                .await
                .unwrap());
            std::fs::write(&marker, &intent.commit_id).unwrap();
            std::fs::create_dir_all(&output_root).unwrap();
            std::fs::rename(&staging, &target).unwrap();
            assert!(store
                .commit_published(
                    task_id.clone(),
                    node_id.clone(),
                    0,
                    serde_json::to_string(&crate::CommitRecord::Published {
                        intent: intent.clone(),
                    })
                    .unwrap(),
                )
                .await
                .unwrap());
            assert!(store
                .enqueue_child(
                    task_id.clone(),
                    NodeSubmission {
                        node_id: existing_child_id.clone(),
                        parent_id: Some(node_id.clone()),
                        root_id: node_id.clone(),
                        input_path: existing_child.path.clone(),
                        input_ref_json: serde_json::to_string(&existing_child).unwrap(),
                        config_revision: 0,
                        generation: 0,
                    },
                )
                .await
                .unwrap());
        }

        let store = StateStore::start(&db_path).unwrap();
        let recovery = &store.recovery_snapshot()[0];
        assert_eq!(recovery.nodes.len(), 1);
        assert_eq!(recovery.nodes[0].node_id, existing_child_id.as_str());
        let (_, identity) =
            crate::execution_runtime::ExecutionCoordinator::decode_recovery(recovery).unwrap();
        assert_eq!(identity.roots[0].candidate, existing_child);
        assert_eq!(
            identity.budget,
            crate::TaskBudgetSnapshot {
                output_files: 1,
                output_bytes: 6,
                nested_candidates: 1,
            }
        );
        let db = smartzip_db::SmartZipDb::open_read_only(&db_path).unwrap();
        let facts: (
            Option<String>,
            Option<i64>,
            Option<i64>,
            bool,
            Option<String>,
            bool,
        ) = db
            .connection()
            .query_row(
                "SELECT sample_hash, file_size, offset, has_password, encoding, \
                 encoding_corrected FROM file_extractions WHERE task_id=?1 AND node_id=?2",
                [task_id.as_str(), node_id.as_str()],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get::<_, i64>(3)? != 0,
                        row.get(4)?,
                        row.get::<_, i64>(5)? != 0,
                    ))
                },
            )
            .unwrap();
        assert_eq!(
            facts,
            (
                Some("recovery-hash".into()),
                Some(5),
                Some(11),
                true,
                Some("shift_jis".into()),
                true,
            )
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn published_recovery_records_nested_limit_and_stops_task() {
        let root = tempfile::tempdir().unwrap();
        let db_path = root.path().join("state.db");
        let input = root.path().join("input.zip");
        let output_root = root.path().join("out");
        let staging = root.path().join(".smartzip-published-limit");
        let target = output_root.join("input");
        let marker = root.path().join("commit-marker-limit");
        std::fs::write(&input, b"input").unwrap();
        std::fs::create_dir_all(&staging).unwrap();
        std::fs::write(staging.join("child.zip"), b"nested").unwrap();
        let intent = crate::CommitIntent {
            commit_id: "commit-limit".into(),
            staging_path: staging.clone(),
            source_path: staging.clone(),
            target_path: target.clone(),
            marker_path: marker.clone(),
            backup_path: None,
            source_identity: crate::ArtifactIdentity::capture(&staging).unwrap(),
            target_before: None,
            output_files: 1,
            output_bytes: 6,
            success: crate::CommitSuccessFacts::default(),
        };
        let task_id = TaskId::new();
        let node_id = NodeId::new();
        let mut submission = task_submission(&task_id, &node_id, &input, &output_root);
        let mut plan: PersistedExtractPlan =
            serde_json::from_str(&submission.config_snapshot_json).unwrap();
        plan.request.limits.max_nested_candidates = 0;
        submission.config_snapshot_json = serde_json::to_string(&plan).unwrap();
        {
            let store = StateStore::start(&db_path).unwrap();
            store.submit(submission).await.unwrap();
            assert!(store
                .transition(
                    task_id.clone(),
                    node_id.clone(),
                    0,
                    "ready",
                    "running",
                    "commit",
                    Some("attempt".into()),
                )
                .await
                .unwrap());
            assert!(store
                .begin_commit(
                    task_id.clone(),
                    node_id.clone(),
                    0,
                    serde_json::to_string(&crate::CommitRecord::Prepared {
                        intent: intent.clone(),
                    })
                    .unwrap(),
                )
                .await
                .unwrap());
            std::fs::write(&marker, &intent.commit_id).unwrap();
            std::fs::create_dir_all(&output_root).unwrap();
            std::fs::rename(&staging, &target).unwrap();
            assert!(store
                .commit_published(
                    task_id.clone(),
                    node_id,
                    0,
                    serde_json::to_string(&crate::CommitRecord::Published { intent }).unwrap(),
                )
                .await
                .unwrap());
        }

        let store = StateStore::start(&db_path).unwrap();
        assert!(store.recovery_snapshot().is_empty());
        let db = smartzip_db::SmartZipDb::open_read_only(&db_path).unwrap();
        let row: (String, String, Option<String>, i64, i64) = db
            .connection()
            .query_row(
                "SELECT t.status, f.status, f.reason, t.committed_output_files, \
                 t.committed_output_bytes FROM tasks t JOIN file_extractions f \
                 ON f.task_id=t.id WHERE t.id=?1",
                [task_id.as_str()],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(row.0, "failed");
        assert_eq!(row.1, "extracted");
        assert_eq!(row.2.as_deref(), Some("nested_candidate_limit_exceeded"));
        assert_eq!((row.3, row.4), (1, 6));
    }

    #[cfg(unix)]
    #[test]
    fn prepared_commit_with_completed_rename_is_reconciled_as_published() {
        let root = tempfile::tempdir().unwrap();
        let staging = root.path().join("staging");
        let target = root.path().join("target");
        let marker = root.path().join("commit-marker");
        std::fs::create_dir(&staging).unwrap();
        std::fs::write(staging.join("file.txt"), b"content").unwrap();
        let intent = crate::CommitIntent {
            commit_id: "commit-id".into(),
            staging_path: staging.clone(),
            source_path: staging.clone(),
            target_path: target.clone(),
            marker_path: marker.clone(),
            backup_path: None,
            source_identity: crate::ArtifactIdentity::capture(&staging).unwrap(),
            target_before: None,
            output_files: 1,
            output_bytes: 7,
            success: crate::CommitSuccessFacts::default(),
        };
        std::fs::write(&marker, &intent.commit_id).unwrap();
        std::fs::rename(&staging, &target).unwrap();

        let recovery = reconcile_commit_record(&crate::CommitRecord::Prepared { intent });
        assert!(matches!(
            recovery,
            CommitRecovery::Published { output, cleanup_warning: None } if output == target
        ));
        assert_eq!(std::fs::read(target.join("file.txt")).unwrap(), b"content");
        assert!(!marker.exists());
    }
}
