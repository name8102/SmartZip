//! One resolved extraction snapshot for both new and recovered tasks.

use crate::state_store::{NodeSubmission, PersistedExtractPlan, StateStoreError, TaskSubmission};
use crate::{
    CompiledRunPolicy, ExtractInteraction, ExtractObserver, ExtractTaskIdentity,
    ExtractWorkflowRequest, ExtractWorkflowResult, SmartZipEngine,
};
use smartzip_archive::ArchiveExecutor;

pub struct PreparedExtractTask {
    policy: CompiledRunPolicy,
    request: ExtractWorkflowRequest,
    identity: ExtractTaskIdentity,
}

impl PreparedExtractTask {
    pub fn new(
        policy: CompiledRunPolicy,
        request: ExtractWorkflowRequest,
    ) -> std::io::Result<Self> {
        let request = policy.resolve_request(request)?;
        let identity = ExtractTaskIdentity::new(&request.inputs);
        Ok(Self {
            policy,
            request,
            identity,
        })
    }

    pub fn recover(
        task: &smartzip_db::task_execution::RecoveredTaskRecord,
    ) -> Result<Self, StateStoreError> {
        let (plan, identity) =
            crate::execution_runtime::ExecutionCoordinator::decode_recovery(task)?;
        let policy = CompiledRunPolicy::compile(smartzip_config::ResolvedConfig {
            values: plan.policy,
            origins: Default::default(),
            path: None,
            diagnostics: vec!["resumed from persisted task snapshot".into()],
        })?;
        let request = policy.resolve_request(plan.request)?;
        Ok(Self {
            policy,
            request,
            identity,
        })
    }

    pub fn policy(&self) -> &CompiledRunPolicy {
        &self.policy
    }
    pub fn identity(&self) -> &ExtractTaskIdentity {
        &self.identity
    }

    /// Configure live root controls before the task is submitted.
    pub fn configure_groups(
        &mut self,
        management: &crate::root_management::RootManagement,
        groups: Vec<crate::root_inputs::VolumeGroupDetails>,
    ) -> smartzip_core::Result<()> {
        management.configure_groups(&mut self.identity, groups)
    }

    pub fn submission(&self, queue_position: i64) -> Result<TaskSubmission, StateStoreError> {
        let invalid =
            |error: serde_json::Error| StateStoreError::InvalidSubmission(error.to_string());
        let roots = self
            .identity
            .roots
            .iter()
            .map(|root| {
                Ok(NodeSubmission {
                    node_id: root.node_id.clone(),
                    parent_id: None,
                    root_id: root.root_id.clone(),
                    input_path: root.candidate.path.clone(),
                    input_ref_json: serde_json::to_string(&root.candidate).map_err(invalid)?,
                    config_revision: 0,
                    generation: i64::try_from(root.generation).map_err(|_| {
                        StateStoreError::InvalidSubmission(
                            "node generation exceeds storage range".into(),
                        )
                    })?,
                })
            })
            .collect::<Result<_, StateStoreError>>()?;
        Ok(TaskSubmission {
            task_id: self.identity.task_id.clone(),
            kind: "extract".into(),
            output_path: Some(self.request.output_dir.clone()),
            started_at: smartzip_db::timestamp::now_utc_iso8601(),
            inputs_json: serde_json::to_string(&self.request.inputs).map_err(invalid)?,
            config_snapshot_json: serde_json::to_string(&PersistedExtractPlan::new(
                self.policy.values().clone(),
                self.request.clone(),
            ))
            .map_err(invalid)?,
            priority: smartzip_db::task_execution::Priority::Normal,
            queue_position,
            recoverable: true,
            roots,
        })
    }

    /// Construct dependencies from this task's snapshot, then execute without
    /// applying configuration a second time. Callers retain backend and UI ownership.
    pub async fn run<B: ArchiveExecutor>(
        self,
        engine: SmartZipEngine,
        backend: &B,
        connection: Option<&rusqlite::Connection>,
        interaction: ExtractInteraction<'_>,
        observer: ExtractObserver<'_>,
    ) -> smartzip_core::Result<ExtractWorkflowResult> {
        let services = self.policy.services(connection);
        let stores = services.stores();
        let observer = ExtractObserver {
            listener: observer.listener,
            history: observer
                .history
                .or(services.has_stores().then_some(&stores)),
            execution: observer.execution,
        };
        engine
            .with_run_policy(self.policy)
            .extract_resolved(
                self.identity,
                backend,
                &services.passwords,
                self.request,
                interaction,
                observer,
            )
            .await
    }
}
