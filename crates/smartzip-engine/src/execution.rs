use async_trait::async_trait;
use smartzip_core::{AttemptId, DecisionId, NodeId, TaskId};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CommitIntent {
    pub commit_id: String,
    pub staging_path: PathBuf,
    pub source_path: PathBuf,
    pub target_path: PathBuf,
    pub marker_path: PathBuf,
    pub backup_path: Option<PathBuf>,
    pub source_identity: ArtifactIdentity,
    pub target_before: Option<ArtifactIdentity>,
    pub output_files: u64,
    pub output_bytes: u64,
    pub success: CommitSuccessFacts,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CommitSuccessFacts {
    pub sample_hash: Option<String>,
    pub file_size: Option<i64>,
    pub embedded_offset: Option<u64>,
    pub has_password: bool,
    pub password_id: Option<i64>,
    pub encoding: Option<String>,
    pub encoding_corrected: bool,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
pub enum CommitRecord {
    Prepared { intent: CommitIntent },
    Published { intent: CommitIntent },
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct StagingArtifact {
    pub path: PathBuf,
}

impl CommitRecord {
    pub fn intent(&self) -> &CommitIntent {
        match self {
            Self::Prepared { intent } | Self::Published { intent } => intent,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ArtifactIdentity {
    pub file_type: String,
    pub len: u64,
    pub modified_ns: Option<u64>,
    #[cfg(unix)]
    pub device: u64,
    #[cfg(unix)]
    pub inode: u64,
}

impl ArtifactIdentity {
    pub fn capture(path: &Path) -> std::io::Result<Self> {
        let metadata = std::fs::symlink_metadata(path)?;
        let file_type = if metadata.file_type().is_dir() {
            "directory"
        } else if metadata.file_type().is_file() {
            "file"
        } else if metadata.file_type().is_symlink() {
            "symlink"
        } else {
            "other"
        };
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;
        Ok(Self {
            file_type: file_type.into(),
            len: metadata.len(),
            modified_ns: metadata
                .modified()
                .ok()
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .and_then(|duration| u64::try_from(duration.as_nanos()).ok()),
            #[cfg(unix)]
            device: metadata.dev(),
            #[cfg(unix)]
            inode: metadata.ino(),
        })
    }

    pub fn matches_object(&self, path: &Path) -> std::io::Result<bool> {
        let actual = Self::capture(path)?;
        #[cfg(unix)]
        return Ok(self.device == actual.device
            && self.inode == actual.inode
            && self.file_type == actual.file_type);
        #[cfg(not(unix))]
        {
            let _ = actual;
            Ok(false)
        }
    }

    pub fn matches_version(&self, path: &Path) -> std::io::Result<bool> {
        let actual = Self::capture(path)?;
        #[cfg(unix)]
        return Ok(self.device == actual.device
            && self.inode == actual.inode
            && self.file_type == actual.file_type
            && self.len == actual.len
            && self.modified_ns == actual.modified_ns);
        #[cfg(not(unix))]
        {
            let _ = actual;
            Ok(false)
        }
    }
}

#[derive(Debug, Clone)]
pub struct ExtractTaskIdentity {
    pub task_id: TaskId,
    pub roots: Vec<ExtractRootIdentity>,
    pub budget: TaskBudgetSnapshot,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TaskBudgetSnapshot {
    pub output_files: u64,
    pub output_bytes: u64,
    pub nested_candidates: usize,
}

#[derive(Debug, Clone)]
pub struct ExtractRootIdentity {
    pub node_id: NodeId,
    pub root_id: NodeId,
    pub generation: u64,
    pub candidate: crate::ExtractionCandidate,
}

impl ExtractTaskIdentity {
    pub fn new(inputs: &[PathBuf]) -> Self {
        Self {
            task_id: TaskId::new(),
            roots: inputs
                .iter()
                .map(|input| {
                    let node_id = NodeId::new();
                    ExtractRootIdentity {
                        root_id: node_id.clone(),
                        node_id,
                        generation: 0,
                        candidate: crate::ExtractionCandidate::root(input.clone()),
                    }
                })
                .collect(),
            budget: TaskBudgetSnapshot::default(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct NodeOutcome {
    pub status: String,
    pub reason: Option<String>,
    pub output_path: Option<PathBuf>,
    pub committed: bool,
    pub sample_hash: Option<String>,
    pub file_size: Option<i64>,
    pub embedded_offset: Option<u64>,
    pub has_password: bool,
    pub password_id: Option<i64>,
    pub encoding: Option<String>,
    pub encoding_corrected: bool,
    pub output_files: u64,
    pub output_bytes: u64,
}

impl NodeOutcome {
    pub fn terminal(
        status: impl Into<String>,
        reason: Option<&str>,
        output_path: Option<&Path>,
        committed: bool,
    ) -> Self {
        Self {
            status: status.into(),
            reason: reason.map(str::to_owned),
            output_path: output_path.map(Path::to_path_buf),
            committed,
            sample_hash: None,
            file_size: None,
            embedded_offset: None,
            has_password: false,
            password_id: None,
            encoding: None,
            encoding_corrected: false,
            output_files: 0,
            output_bytes: 0,
        }
    }
}

#[async_trait(?Send)]
pub trait ExecutionStateRecorder {
    async fn enqueue_child(
        &self,
        task_id: &TaskId,
        node_id: &NodeId,
        parent_id: &NodeId,
        root_id: &NodeId,
        candidate: &crate::ExtractionCandidate,
        generation: u64,
    ) -> smartzip_core::Result<bool>;

    async fn transition(
        &self,
        task_id: &TaskId,
        node_id: &NodeId,
        generation: u64,
        from: &str,
        to: &str,
        stage: crate::coordinator::Stage,
        attempt_id: Option<&AttemptId>,
        cancellation: &tokio_util::sync::CancellationToken,
    ) -> smartzip_core::Result<bool>;

    fn release_stage(&self, task_id: &TaskId, node_id: &NodeId);

    async fn record_staging(
        &self,
        task_id: &TaskId,
        node_id: &NodeId,
        generation: u64,
        path: &Path,
    ) -> smartzip_core::Result<bool>;

    async fn wait_for_decision(
        &self,
        task_id: &TaskId,
        node_id: &NodeId,
        generation: u64,
        stage: crate::coordinator::Stage,
        decision_id: &DecisionId,
        kind: &str,
        evidence: &str,
    ) -> smartzip_core::Result<bool>;

    async fn accept_decision(
        &self,
        task_id: &TaskId,
        node_id: &NodeId,
        generation: u64,
        decision_id: &DecisionId,
    ) -> smartzip_core::Result<bool>;

    async fn begin_commit(
        &self,
        task_id: &TaskId,
        node_id: &NodeId,
        generation: u64,
        intent: &CommitIntent,
    ) -> smartzip_core::Result<bool>;

    async fn commit_published(
        &self,
        task_id: &TaskId,
        node_id: &NodeId,
        generation: u64,
        intent: &CommitIntent,
    ) -> smartzip_core::Result<bool>;

    async fn abort_commit(
        &self,
        task_id: &TaskId,
        node_id: &NodeId,
        generation: u64,
    ) -> smartzip_core::Result<bool>;

    async fn finish_node(
        &self,
        task_id: &TaskId,
        node_id: &NodeId,
        generation: u64,
        outcome: NodeOutcome,
    ) -> smartzip_core::Result<bool>;
}
