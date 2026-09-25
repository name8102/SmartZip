//! Compile one resolved configuration snapshot before creating task dependencies.
use crate::{
    layout::{OutputLayoutPolicy, SingleRootNamePolicy},
    ExtractWorkflowRequest,
};
use smartzip_config as config;

#[derive(Debug, Clone)]
pub struct CompiledRunPolicy {
    resolved: config::ResolvedConfig,
}
impl CompiledRunPolicy {
    pub fn compile(resolved: config::ResolvedConfig) -> std::io::Result<Self> {
        resolved.values.validate()?;
        Ok(Self { resolved })
    }
    pub fn values(&self) -> &config::SmartZipConfig {
        &self.resolved.values
    }
    pub fn resolved(&self) -> &config::ResolvedConfig {
        &self.resolved
    }

    /// Build the effective request once. Callers add only invocation-specific
    /// inputs (scanner, manual passwords, force) before passing it to `extract`.
    pub fn extract_request(
        &self,
        inputs: Vec<std::path::PathBuf>,
        output_dir: std::path::PathBuf,
    ) -> ExtractWorkflowRequest {
        let mut request = ExtractWorkflowRequest {
            inputs,
            output_dir,
            recursion_limit: 0,
            encoding_mode: smartzip_core::EncodingMode::Auto,
            scanner: Default::default(),
            password_candidates: Default::default(),
            layout_policy: Default::default(),
            single_root_name_policy: Default::default(),
            embedded_scan_mode: smartzip_core::EmbeddedScanMode::Auto,
            dominant_min_ratio: 0.0,
            confirm_large_scan: false,
            force: false,
            limits: Default::default(),
        };
        self.apply_request(&mut request);
        request
    }

    /// Configure borrowed state capabilities without opening a database or
    /// changing its access mode. CLI and GUI supply their own connection.
    pub fn services<'a>(&self, connection: Option<&'a rusqlite::Connection>) -> RunServices<'a> {
        use crate::history::{DbKnownFileStore, DbTaskHistoryRecorder};
        use config::{PasswordMode, PasswordSource, StateMode};
        let c = self.values();
        RunServices {
            passwords: smartzip_passwords::PasswordService::configured(
                connection.map(smartzip_db::password::PasswordRepository::new),
                c.passwords.clone(),
                c.state.mode,
            ),
            history: connection
                .filter(|_| self.writes_state() && c.state.history)
                .map(DbTaskHistoryRecorder::new),
            known_files: connection
                .filter(|_| self.reads_state() && c.state.known_files != StateMode::Off)
                .map(|connection| DbKnownFileStore {
                    connection,
                    writable: self.writes_state() && c.state.known_files == StateMode::ReadWrite,
                    password_hint: c.extraction.reuse.password_hint
                        && c.passwords.mode == PasswordMode::Auto
                        && c.passwords.sources.contains(&PasswordSource::Known),
                    encoding_hint: c.extraction.reuse.encoding_hint
                        && c.extraction.encoding.mode == "auto",
                }),
        }
    }
    pub fn reads_state(&self) -> bool {
        self.values().state.mode != config::StateMode::Off
    }
    pub fn writes_state(&self) -> bool {
        self.values().state.mode == config::StateMode::ReadWrite
    }
    pub fn needs_database(&self) -> bool {
        let c = self.values();
        self.reads_state()
            && (c.state.known_files != config::StateMode::Off
                || self.writes_state()
                    && (c.state.history
                        || c.passwords.mode != config::PasswordMode::Off
                            && !c.passwords.sources.is_empty()
                            && (c.passwords.save_success || c.passwords.record_statistics))
                || c.passwords.mode == config::PasswordMode::Auto
                    && c.passwords.sources.iter().any(|s| {
                        matches!(
                            s,
                            config::PasswordSource::Known | config::PasswordSource::Database
                        )
                    }))
    }
    pub fn apply_request(&self, request: &mut ExtractWorkflowRequest) {
        let c = self.values();
        request.recursion_limit = if c.extraction.recursion.enabled {
            c.extraction.recursion.max_depth
        } else {
            0
        };
        request.password_candidates.limit = c.passwords.database_limit;
        request.layout_policy = match c.extraction.output.layout {
            config::Layout::Conservative => OutputLayoutPolicy::Conservative,
            config::Layout::Smart => OutputLayoutPolicy::Smart,
            config::Layout::Raw => OutputLayoutPolicy::Raw,
            config::Layout::FlatSingle => OutputLayoutPolicy::FlatSingle,
        };
        request.single_root_name_policy = match c.extraction.output.single_root_name {
            config::SingleRootName::Auto => SingleRootNamePolicy::Auto,
            config::SingleRootName::Archive => SingleRootNamePolicy::PreferArchiveName,
            config::SingleRootName::Inner => SingleRootNamePolicy::PreferInnerName,
            config::SingleRootName::PreserveBoth => SingleRootNamePolicy::PreserveBoth,
        };
        request.encoding_mode = if ["auto", "backend"]
            .contains(&c.extraction.encoding.mode.to_ascii_lowercase().as_str())
        {
            smartzip_core::EncodingMode::Auto
        } else {
            smartzip_core::EncodingMode::Override(c.extraction.encoding.mode.clone())
        };
        request.dominant_min_ratio = c.extraction.embedded.dominant_min_ratio;
        request.limits = c.limits.clone();
        if let Some(directory) = &c.extraction.output.directory {
            if c.extraction.output.destination == config::Destination::Directory {
                request.output_dir = directory.clone();
            }
        }
    }
    pub(crate) fn embedded_mode(&self, root: bool) -> smartzip_core::EmbeddedScanMode {
        use smartzip_core::EmbeddedScanMode as Mode;
        let c = &self.values().extraction.embedded;
        if root {
            match c.root {
                config::RootScan::Largest => Mode::Largest,
                config::RootScan::Off => Mode::Ignore,
                config::RootScan::Auto => Mode::Auto,
                config::RootScan::Ask => Mode::Ask,
                config::RootScan::All => Mode::All,
            }
        } else {
            match c.nested {
                config::NestedScan::Largest => Mode::Largest,
                config::NestedScan::Off => Mode::Ignore,
                config::NestedScan::Auto => Mode::Auto,
                config::NestedScan::Ask => Mode::Ask,
                config::NestedScan::Aggressive => Mode::Aggressive,
                config::NestedScan::All => Mode::All,
            }
        }
    }
    pub fn stage_plan(&self) -> Vec<smartzip_core::TaskEventKind> {
        let mut plan = Vec::new();
        for (key, reason) in self.resolved.explanation() {
            plan.push(self.decision_kind("policy", "skip", &reason, &key));
        }
        let c = self.values();
        for (stage, key, enabled) in [
            (
                "nested_discovery",
                "extraction.recursion.enabled",
                c.extraction.recursion.enabled && c.extraction.recursion.max_depth > 0,
            ),
            (
                "root_scan",
                "extraction.embedded.root",
                c.extraction.embedded.root != config::RootScan::Off,
            ),
            (
                "nested_scan",
                "extraction.embedded.nested",
                c.extraction.recursion.enabled
                    && c.extraction.recursion.max_depth > 0
                    && c.extraction.embedded.nested != config::NestedScan::Off,
            ),
            (
                "volume_discovery",
                "extraction.volumes.auto_discover",
                c.extraction.volumes.auto_discover,
            ),
            (
                "encoding_detection",
                "extraction.encoding.mode",
                c.extraction.encoding.mode == "auto",
            ),
            (
                "nested_cleanup",
                "extraction.cleanup.nested_archives",
                c.extraction.cleanup.nested_archives != config::Cleanup::Keep,
            ),
        ] {
            plan.push(self.decision_kind(
                stage,
                if enabled { "allow" } else { "skip" },
                if enabled {
                    "enabled_by_config"
                } else {
                    "disabled_by_config"
                },
                key,
            ));
        }
        plan
    }

    pub(crate) fn emit_plan(
        &self,
        events: &crate::events::EventSink,
        task_id: &smartzip_core::TaskId,
    ) {
        use smartzip_core::{TaskEvent, TaskEventKind};
        events.push(TaskEvent {
            task_id: task_id.clone(),
            kind: TaskEventKind::Decision {
                stage: "task_policy".into(),
                action: "snapshot".into(),
                reason: "resolved_configuration".into(),
                policy_key: "defaults_version".into(),
                source: "resolved".into(),
                detail: Some(
                    serde_json::to_string(&self.resolved).expect("configuration is serializable"),
                ),
            },
        });
        for kind in self.stage_plan() {
            events.push(TaskEvent {
                task_id: task_id.clone(),
                kind,
            });
        }
    }
    pub(crate) fn decision(
        &self,
        events: &crate::events::EventSink,
        task_id: &smartzip_core::TaskId,
        stage: &str,
        action: &str,
        reason: &str,
        key: &str,
    ) {
        events.push(smartzip_core::TaskEvent {
            task_id: task_id.clone(),
            kind: self.decision_kind(stage, action, reason, key),
        });
    }
    fn decision_kind(
        &self,
        stage: &str,
        action: &str,
        reason: &str,
        key: &str,
    ) -> smartzip_core::TaskEventKind {
        smartzip_core::TaskEventKind::Decision {
            stage: stage.into(),
            action: action.into(),
            reason: reason.into(),
            policy_key: key.into(),
            source: self
                .resolved
                .origins
                .get(key)
                .cloned()
                .unwrap_or_else(|| "defaults-v1".into()),
            detail: None,
        }
    }
}

/// State services selected together from one validated policy snapshot.
pub struct RunServices<'a> {
    pub passwords: smartzip_passwords::PasswordService<'a>,
    history: Option<crate::history::DbTaskHistoryRecorder<'a>>,
    known_files: Option<crate::history::DbKnownFileStore<'a>>,
}
impl RunServices<'_> {
    pub fn stores(&self) -> crate::history::RunStores<'_> {
        crate::history::RunStores {
            history: self
                .history
                .as_ref()
                .map(|s| s as &dyn crate::history::TaskHistoryRecorder),
            known_files: self
                .known_files
                .as_ref()
                .map(|s| s as &dyn crate::history::KnownFileStore),
        }
    }
}

pub(crate) struct PolicyOutputPrompter<'a> {
    pub policy: &'a CompiledRunPolicy,
    pub delegate: Option<&'a dyn crate::InteractiveOutputPrompter>,
}
#[async_trait::async_trait]
impl crate::InteractiveOutputPrompter for PolicyOutputPrompter<'_> {
    async fn prompt(
        &self,
        archive: std::path::PathBuf,
        output: std::path::PathBuf,
    ) -> crate::OutputCollisionStrategy {
        use crate::OutputCollisionStrategy as Action;
        match self.policy.values().extraction.output.on_conflict {
            config::Conflict::Skip => Action::Skip,
            config::Conflict::Overwrite => Action::Overwrite,
            config::Conflict::Rename => Action::Rename,
            config::Conflict::Ask => {
                if self.policy.values().interaction.mode != config::InteractionMode::Never {
                    if let Some(delegate) = self.delegate {
                        return delegate.prompt(archive, output).await;
                    }
                }
                Action::Skip
            }
        }
    }
}
pub(crate) struct PolicyEncodingPrompter<'a> {
    pub policy: &'a CompiledRunPolicy,
    pub delegate: Option<&'a dyn crate::InteractiveEncodingPrompter>,
}
#[async_trait::async_trait]
impl crate::InteractiveEncodingPrompter for PolicyEncodingPrompter<'_> {
    async fn prompt(
        &self,
        path: &std::path::Path,
        context: &crate::EncodingConfirmationContext,
    ) -> crate::EncodingConfirmationChoice {
        use crate::EncodingConfirmationChoice as Choice;
        match self.policy.values().extraction.encoding.on_suspicious {
            config::SuspiciousEncoding::Accept => Choice::AcceptDetected,
            config::SuspiciousEncoding::Skip => Choice::SkipArchive,
            config::SuspiciousEncoding::Ask => {
                if self.policy.values().interaction.mode != config::InteractionMode::Never {
                    if let Some(delegate) = self.delegate {
                        return delegate.prompt(path, context).await;
                    }
                }
                Choice::SkipArchive
            }
        }
    }
}
