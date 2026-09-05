use crate::backend::{ArchiveAdapter, ArchiveExecutor};
use crate::sevenzz::{SevenZipBackend, SevenZipLocator};
use crate::types::*;
use crate::unrar::{UnrarBackend, UnrarLocator};
use async_trait::async_trait;
use smartzip_config::{AdapterFamily, BackendConfig, BackendInstallation};
use smartzip_core::{
    AdapterCapabilities, ArchiveFacts, ArchiveFormat, ArchiveOperation, ArchiveRequirements,
    NegativeCapabilityKey, RejectedAdapter, Result, RouteCandidate, RouteEvent, RoutePlan,
    SmartZipError, TaskEventSink, TaskExecutionContext, TaskId,
};
use std::collections::HashSet;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;

#[derive(Clone)]
pub struct AdapterRegistration {
    pub adapter: Arc<dyn ArchiveAdapter>,
    pub capabilities: AdapterCapabilities,
    pub priority: i32,
}

impl std::fmt::Debug for AdapterRegistration {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AdapterRegistration")
            .field("adapter_id", &self.adapter.id())
            .field("capabilities", &self.capabilities)
            .field("priority", &self.priority)
            .finish()
    }
}

impl AdapterRegistration {
    pub fn new(
        adapter: impl ArchiveAdapter + 'static,
        capabilities: AdapterCapabilities,
        priority: i32,
    ) -> Self {
        Self {
            adapter: Arc::new(adapter),
            capabilities,
            priority,
        }
    }

    pub fn from_adapter(adapter: impl ArchiveAdapter + 'static, priority: i32) -> Self {
        let capabilities = adapter.capabilities();
        Self::new(adapter, capabilities, priority)
    }
}

#[derive(Debug, Clone)]
pub struct BackendRouter {
    adapters: Vec<AdapterRegistration>,
    forced_adapter: Option<String>,
    warnings: Vec<String>,
}

impl BackendRouter {
    pub fn diagnostics(&self) -> Vec<serde_json::Value> {
        self.adapters.iter().map(|registration| {
            let adapter = &registration.adapter;
            let family = adapter.diagnostic_family().unwrap_or("unknown");
            let version = adapter.executable_path().map(|path| identify_version(path, if family == "unrar" { "unrar-cli" } else { "seven-zip-cli" }));
            serde_json::json!({"id": adapter.id(), "family": family, "executable": adapter.executable_path(),
                "version": version.as_ref().and_then(|v| v.as_ref().ok()),
                "error": version.as_ref().and_then(|v| v.as_ref().err()).map(ToString::to_string),
                "capabilities": registration.capabilities})
        }).collect()
    }

    pub fn from_adapters(adapters: Vec<AdapterRegistration>) -> Self {
        Self {
            adapters,
            forced_adapter: None,
            warnings: Vec::new(),
        }
    }

    /// Build a registry from explicit installations, then supplement it with known tools.
    /// Explicit entries win when discovery resolves to the same normalized executable path.
    pub fn from_config(config: &BackendConfig) -> Result<Self> {
        config
            .validate()
            .map_err(|detail| SmartZipError::BackendProtocolError {
                backend: "backend-configuration".into(),
                detail,
            })?;
        let mut adapters = Vec::new();
        // Every explicit path, including disabled entries, suppresses auto-discovery.
        let explicit_paths: HashSet<PathBuf> = config
            .installations
            .iter()
            .map(|entry| resolve_executable(&entry.executable))
            .collect();
        let mut registered_paths = HashSet::new();
        let mut warnings = Vec::new();

        for installation in config.installations.iter().filter(|entry| entry.enabled) {
            let executable = resolve_executable(&installation.executable);
            if !registered_paths.insert(executable.clone()) {
                warnings.push(format!(
                    "backend {} duplicates an earlier configured executable and was ignored",
                    installation.id
                ));
                continue;
            }
            check_declared_version(installation, &mut warnings);
            let registration = match &installation.family {
                AdapterFamily::SevenZipCli => AdapterRegistration::new(
                    SevenZipBackend::new(executable).with_id(installation.id.clone()),
                    seven_zip_capabilities(),
                    installation.priority,
                ),
                AdapterFamily::UnrarCli => AdapterRegistration::new(
                    UnrarBackend::new(executable).with_id(installation.id.clone()),
                    unrar_capabilities(),
                    installation.priority,
                ),
            };
            adapters.push(registration);
        }
        if config.auto_discover {
            if let Some(executable) = UnrarLocator::default().locate() {
                if !explicit_paths.contains(&resolve_executable(&executable)) {
                    adapters.push(discovered_registration(
                        UnrarBackend::new(executable.clone()),
                        "unrar-cli",
                        &executable,
                        20,
                        &mut warnings,
                    ));
                }
            }
            for executable in SevenZipLocator::default().locate_all() {
                if !explicit_paths.contains(&resolve_executable(&executable)) {
                    adapters.push(discovered_registration(
                        SevenZipBackend::new(executable.clone()),
                        "sevenzip-cli",
                        &executable,
                        10,
                        &mut warnings,
                    ));
                }
            }
        }
        let mut router = Self::from_adapters(adapters);
        router.warnings = warnings;
        Ok(router)
    }

    pub fn with_forced_adapter(mut self, adapter_id: impl Into<String>) -> Self {
        self.forced_adapter = Some(adapter_id.into());
        self
    }

    pub fn adapter_ids(&self) -> Vec<&str> {
        self.adapters
            .iter()
            .map(|registration| registration.adapter.id())
            .collect()
    }

    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    pub fn plan_for_facts(
        &self,
        operation: ArchiveOperation,
        facts: &ArchiveFacts,
        mut intent: ArchiveRequirements,
    ) -> RoutePlan {
        let facts_requirements = ArchiveRequirements::from_facts(facts);
        intent.codecs.extend(facts_requirements.codecs);
        self.plan(operation, facts.container.as_ref(), intent)
    }

    pub fn plan(
        &self,
        operation: ArchiveOperation,
        container: Option<&ArchiveFormat>,
        requirements: ArchiveRequirements,
    ) -> RoutePlan {
        self.plan_with_context(operation, container, requirements, None)
    }

    fn plan_with_context(
        &self,
        operation: ArchiveOperation,
        container: Option<&ArchiveFormat>,
        requirements: ArchiveRequirements,
        context: Option<&TaskExecutionContext>,
    ) -> RoutePlan {
        let mut candidates = Vec::new();
        let mut rejected = Vec::new();
        let container_value = container.cloned();

        for registration in &self.adapters {
            let adapter_id = registration.adapter.id();
            let mut reasons = Vec::new();
            if let Some(forced) = self.forced_adapter.as_deref() {
                if forced == adapter_id {
                    candidates.push(RouteCandidate {
                        adapter_id: adapter_id.to_owned(),
                        priority: registration.priority,
                        notes: vec!["forced for diagnostics; compatibility filters bypassed".into()],
                    });
                    continue;
                }
                reasons.push(format!("forced adapter is {forced}"));
            }

            let negative = NegativeCapabilityKey {
                adapter_id: adapter_id.to_owned(),
                operation,
                container: container_value.clone(),
                codec: None,
            };
            if let Some(context) = context {
                if let Some(reason) = context.rejection(&negative) {
                    reasons.push(format!("task-local incompatibility: {reason}"));
                }
                for codec in &requirements.codecs {
                    let codec_negative = NegativeCapabilityKey {
                        adapter_id: adapter_id.to_owned(),
                        operation,
                        container: container_value.clone(),
                        codec: Some(codec.clone()),
                    };
                    if let Some(reason) = context.rejection(&codec_negative) {
                        reasons.push(format!(
                            "task-local incompatibility for codec {codec}: {reason}"
                        ));
                    }
                }
            }

            if !registration.capabilities.supports(operation, container) {
                reasons.push(match container {
                    Some(container) => format!(
                        "operation/container {operation:?}/{} is unsupported",
                        container.as_str()
                    ),
                    None => format!("operation {operation:?} is unsupported"),
                });
            }
            if requirements.password && !registration.capabilities.supports_passwords {
                reasons.push("password handling is unsupported".into());
            }
            if requirements.charset_override && !registration.capabilities.supports_charset_override
            {
                reasons.push("charset override is unsupported".into());
            }

            if reasons.is_empty() {
                candidates.push(RouteCandidate {
                    adapter_id: adapter_id.to_owned(),
                    priority: registration.priority,
                    notes: Vec::new(),
                });
            } else {
                rejected.push(RejectedAdapter {
                    adapter_id: adapter_id.to_owned(),
                    reasons,
                });
            }
        }

        candidates.sort_by(|left, right| {
            right
                .priority
                .cmp(&left.priority)
                .then_with(|| left.adapter_id.cmp(&right.adapter_id))
        });
        let plan = RoutePlan {
            operation,
            container: container_value,
            requirements,
            candidates,
            rejected,
            forced_adapter: self.forced_adapter.clone(),
        };
        if let Some(context) = context {
            context.emit_route(RouteEvent::RoutePlanned { plan: plan.clone() });
        }
        plan
    }

    fn registration(&self, adapter_id: &str) -> Option<&AdapterRegistration> {
        self.adapters
            .iter()
            .find(|registration| registration.adapter.id() == adapter_id)
    }

    fn emit(&self, context: &TaskExecutionContext, event: RouteEvent) {
        context.emit_route(event);
    }

    fn remember_retryable(
        &self,
        context: &TaskExecutionContext,
        adapter_id: &str,
        operation: ArchiveOperation,
        container: Option<ArchiveFormat>,
        error: &SmartZipError,
    ) {
        let (reason, codec) = match error {
            SmartZipError::UnsupportedContainer { .. } => ("unsupported container", None),
            SmartZipError::UnsupportedCodec {
                codec: Some(codec), ..
            } => ("unsupported codec", Some(codec.clone())),
            // A codec observation without an exact feature is not safe to cache.
            SmartZipError::UnsupportedCodec { codec: None, .. } => return,
            _ => return,
        };
        context.record_rejection(
            NegativeCapabilityKey {
                adapter_id: adapter_id.to_owned(),
                operation,
                container,
                codec,
            },
            reason,
        );
    }

    async fn route<T, F>(
        &self,
        context: std::sync::Arc<TaskExecutionContext>,
        operation: ArchiveOperation,
        container: Option<ArchiveFormat>,
        requirements: ArchiveRequirements,
        mut invoke: F,
    ) -> Result<T>
    where
        F: for<'a> FnMut(
            &'a dyn ArchiveAdapter,
        ) -> Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>,
    {
        let token = context.cancellation_token();
        if token.is_cancelled() {
            return Err(SmartZipError::Cancelled);
        }
        let plan = self.plan_with_context(
            operation,
            container.as_ref(),
            requirements,
            Some(context.as_ref()),
        );
        let mut attempted = Vec::new();
        let mut last_error = None;
        for candidate in &plan.candidates {
            if token.is_cancelled() {
                return Err(SmartZipError::Cancelled);
            }
            let registration = self
                .registration(&candidate.adapter_id)
                .expect("planned adapter must remain registered");
            attempted.push(candidate.adapter_id.clone());
            self.emit(
                context.as_ref(),
                RouteEvent::BackendAttemptStarted {
                    adapter_id: candidate.adapter_id.clone(),
                },
            );
            // Cancellation is now handled inside the backend runner
            // (SevenZip/Unrar select on token, kill the process group,
            // wait, then return Cancelled). The router only does fast-path
            // checks before starting an attempt; it does not drop the
            // in-flight future, otherwise it would bypass the backend's
            // kill+wait contract.
            let result = invoke(registration.adapter.as_ref()).await;
            match result {
                Ok(value) => {
                    self.emit(
                        context.as_ref(),
                        RouteEvent::BackendSelected {
                            adapter_id: candidate.adapter_id.clone(),
                        },
                    );
                    return Ok(value);
                }
                Err(error) => {
                    self.emit(
                        context.as_ref(),
                        RouteEvent::BackendAttemptFailed {
                            adapter_id: candidate.adapter_id.clone(),
                            class: error_class(&error).into(),
                        },
                    );
                    if matches!(error, SmartZipError::Cancelled) {
                        return Err(error);
                    }
                    if !is_retryable(&error) || self.forced_adapter.is_some() {
                        return Err(error);
                    }
                    self.remember_retryable(
                        context.as_ref(),
                        &candidate.adapter_id,
                        operation,
                        container.clone(),
                        &error,
                    );
                    last_error = Some(error);
                }
            }
        }
        self.emit(context.as_ref(), RouteEvent::RouteExhausted { attempted });
        Err(last_error.unwrap_or_else(|| no_compatible_backend(operation, &plan)))
    }

    async fn extract_with_facts_in_context(
        &self,
        request: ExtractArchiveRequest,
        facts: &ArchiveFacts,
        context: std::sync::Arc<TaskExecutionContext>,
    ) -> Result<ExtractArchiveResult> {
        let container = facts
            .container
            .clone()
            .or_else(|| infer_format(request.format.clone(), &request.archive));
        let mut requirements = request_requirements(
            ArchiveOperation::Extract,
            container.as_ref(),
            request.password.as_deref(),
            Some(&request.encoding),
        );
        requirements.codecs.extend(facts.codecs.clone());
        self.extract_isolated_planned(
            request,
            container,
            requirements,
            std::sync::Arc::clone(&context),
        )
        .await
    }

    async fn extract_isolated(
        &self,
        request: ExtractArchiveRequest,
        context: std::sync::Arc<TaskExecutionContext>,
    ) -> Result<ExtractArchiveResult> {
        let container = infer_format(request.format.clone(), &request.archive);
        let requirements = request_requirements(
            ArchiveOperation::Extract,
            container.as_ref(),
            request.password.as_deref(),
            Some(&request.encoding),
        );
        self.extract_isolated_planned(
            request,
            container,
            requirements,
            std::sync::Arc::clone(&context),
        )
        .await
    }

    async fn extract_isolated_planned(
        &self,
        request: ExtractArchiveRequest,
        container: Option<ArchiveFormat>,
        requirements: ArchiveRequirements,
        context: std::sync::Arc<TaskExecutionContext>,
    ) -> Result<ExtractArchiveResult> {
        let token = context.cancellation_token();
        if token.is_cancelled() {
            return Err(SmartZipError::Cancelled);
        }
        let plan = self.plan_with_context(
            ArchiveOperation::Extract,
            container.as_ref(),
            requirements,
            Some(context.as_ref()),
        );
        let mut attempted = Vec::new();
        let mut last_error = None;
        // The caller (normally OutputMaterializer) owns this staging directory.
        // Adapters share it one at a time; failed attempts clear its contents
        // before fallback, so the router owns no second temporary hierarchy.
        ensure_empty_output_dir(&request.output_dir)?;

        for candidate in &plan.candidates {
            if token.is_cancelled() {
                return Err(SmartZipError::Cancelled);
            }
            let registration = self
                .registration(&candidate.adapter_id)
                .expect("planned adapter must remain registered");
            attempted.push(candidate.adapter_id.clone());
            self.emit(
                context.as_ref(),
                RouteEvent::BackendAttemptStarted {
                    adapter_id: candidate.adapter_id.clone(),
                },
            );

            // Cancellation is handled inside the backend: it kills the
            // process group / job object, waits for the child and its
            // descendants, drains readers, then returns Cancelled. The
            // router only waits for that result; it does not drop the
            // future prematurely.
            let result = registration
                .adapter
                .extract_with_context(request.clone(), std::sync::Arc::clone(&context))
                .await;
            match result {
                Ok(_) => {
                    self.emit(
                        context.as_ref(),
                        RouteEvent::BackendSelected {
                            adapter_id: candidate.adapter_id.clone(),
                        },
                    );
                    return Ok(ExtractArchiveResult {
                        output_dir: request.output_dir,
                    });
                }
                Err(error) => {
                    self.emit(
                        context.as_ref(),
                        RouteEvent::BackendAttemptFailed {
                            adapter_id: candidate.adapter_id.clone(),
                            class: error_class(&error).into(),
                        },
                    );
                    // Backend contract: on Cancelled the process tree is
                    // already stopped and no longer writing to output_dir.
                    // Clear and verify before any retry.
                    clear_attempt_output(&request.output_dir)?;
                    self.emit(
                        context.as_ref(),
                        RouteEvent::BackendAttemptCleaned {
                            adapter_id: candidate.adapter_id.clone(),
                        },
                    );
                    if matches!(error, SmartZipError::Cancelled) {
                        return Err(error);
                    }
                    if !is_retryable(&error) || self.forced_adapter.is_some() {
                        return Err(error);
                    }
                    self.remember_retryable(
                        context.as_ref(),
                        &candidate.adapter_id,
                        ArchiveOperation::Extract,
                        container.clone(),
                        &error,
                    );
                    last_error = Some(error);
                }
            }
        }
        self.emit(context.as_ref(), RouteEvent::RouteExhausted { attempted });
        Err(last_error.unwrap_or_else(|| no_compatible_backend(ArchiveOperation::Extract, &plan)))
    }
}

#[async_trait]
impl ArchiveExecutor for BackendRouter {
    fn begin_task(
        &self,
        task_id: TaskId,
        events: Arc<dyn TaskEventSink>,
    ) -> Arc<TaskExecutionContext> {
        Arc::new(TaskExecutionContext::new(task_id, events))
    }

    async fn probe(&self, path: &Path) -> Result<ArchiveProbe> {
        let context = std::sync::Arc::new(TaskExecutionContext::detached());
        self.probe_with_context(path, context).await
    }

    async fn probe_with_context(
        &self,
        path: &Path,
        context: std::sync::Arc<TaskExecutionContext>,
    ) -> Result<ArchiveProbe> {
        let container = format_from_extension(path);
        let requirements = base_requirements(ArchiveOperation::Probe, container.as_ref());
        let path = path.to_path_buf();
        self.route(
            std::sync::Arc::clone(&context),
            ArchiveOperation::Probe,
            container,
            requirements,
            |adapter| {
                let path = path.clone();
                let context = std::sync::Arc::clone(&context);
                Box::pin(async move {
                    let probe = adapter.probe_with_context(&path, context).await?;
                    if probe.supported {
                        Ok(probe)
                    } else {
                        Err(SmartZipError::UnsupportedContainer {
                            backend: adapter.id().to_owned(),
                            path: probe.path,
                            container: probe.format.map(|format| format.as_str().to_owned()),
                        })
                    }
                })
            },
        )
        .await
    }

    async fn list(&self, request: ListRequest) -> Result<ArchiveListing> {
        let context = std::sync::Arc::new(TaskExecutionContext::detached());
        self.list_with_context(request, context).await
    }

    async fn list_with_context(
        &self,
        request: ListRequest,
        context: std::sync::Arc<TaskExecutionContext>,
    ) -> Result<ArchiveListing> {
        let container = infer_format(request.format.clone(), &request.archive);
        let requirements = request_requirements(
            ArchiveOperation::List,
            container.as_ref(),
            request.password.as_deref(),
            Some(&request.encoding),
        );
        self.route(
            std::sync::Arc::clone(&context),
            ArchiveOperation::List,
            container,
            requirements,
            |adapter| {
                let request = request.clone();
                let context = std::sync::Arc::clone(&context);
                Box::pin(async move { adapter.list_with_context(request, context).await })
            },
        )
        .await
    }

    async fn test(&self, request: TestRequest) -> Result<TestResult> {
        let context = std::sync::Arc::new(TaskExecutionContext::detached());
        self.test_with_context(request, context).await
    }

    async fn test_with_context(
        &self,
        request: TestRequest,
        context: std::sync::Arc<TaskExecutionContext>,
    ) -> Result<TestResult> {
        let container = infer_format(request.format.clone(), &request.archive);
        let requirements = request_requirements(
            ArchiveOperation::Test,
            container.as_ref(),
            request.password.as_deref(),
            Some(&request.encoding),
        );
        self.route(
            std::sync::Arc::clone(&context),
            ArchiveOperation::Test,
            container,
            requirements,
            |adapter| {
                let request = request.clone();
                let context = std::sync::Arc::clone(&context);
                Box::pin(async move {
                    let mut result = adapter.test_with_context(request, context).await?;
                    result.diagnostics.adapter_id = adapter.id().to_owned();
                    if result.diagnostics.family.is_empty() {
                        result.diagnostics.family =
                            adapter.diagnostic_family().unwrap_or("unknown").to_owned();
                    }
                    Ok(result)
                })
            },
        )
        .await
    }

    async fn diagnose_test_with_context(
        &self,
        request: TestRequest,
        previous: &crate::integrity::BackendTestDiagnostics,
        multivolume: bool,
        context: std::sync::Arc<TaskExecutionContext>,
    ) -> Result<Option<TestResult>> {
        if self.forced_adapter.is_some() {
            return Ok(None);
        }
        let container = infer_format(request.format.clone(), &request.archive);
        let requirements = request_requirements(
            ArchiveOperation::Test,
            container.as_ref(),
            request.password.as_deref(),
            Some(&request.encoding),
        );
        let mut plan = self.plan_with_context(
            ArchiveOperation::Test,
            container.as_ref(),
            requirements,
            None,
        );
        plan.candidates.retain(|candidate| {
            self.registration(&candidate.adapter_id)
                .is_some_and(|registration| {
                    candidate.adapter_id != previous.adapter_id
                        && registration
                            .adapter
                            .diagnostic_family()
                            .is_some_and(|family| {
                                family != previous.family
                                    && match container {
                                        Some(ArchiveFormat::Rar) => {
                                            matches!(family, "unrar" | "7z")
                                        }
                                        Some(ArchiveFormat::Zip) => {
                                            family == "7z" || family == "native-zip" && !multivolume
                                        }
                                        _ => false,
                                    }
                            })
                })
        });
        // At most one additional full test, with a different implementation.
        plan.candidates.truncate(1);
        let Some(candidate) = plan.candidates.first().cloned() else {
            return Ok(None);
        };
        context.emit_route(RouteEvent::RoutePlanned { plan });
        let Some(registration) = self.registration(&candidate.adapter_id) else {
            return Ok(None);
        };
        context.emit_route(RouteEvent::BackendAttemptStarted {
            adapter_id: candidate.adapter_id.clone(),
        });
        match registration
            .adapter
            .test_with_context(request, context.clone())
            .await
        {
            Ok(mut result) => {
                result.diagnostics.adapter_id = candidate.adapter_id.clone();
                context.emit_route(RouteEvent::BackendSelected {
                    adapter_id: candidate.adapter_id,
                });
                Ok(Some(result))
            }
            Err(error) => {
                context.emit_route(RouteEvent::BackendAttemptFailed {
                    adapter_id: candidate.adapter_id,
                    class: error_class(&error).into(),
                });
                Err(error)
            }
        }
    }

    async fn extract(&self, request: ExtractArchiveRequest) -> Result<ExtractArchiveResult> {
        let context = std::sync::Arc::new(TaskExecutionContext::detached());
        self.extract_with_context(request, context).await
    }

    async fn extract_with_context(
        &self,
        request: ExtractArchiveRequest,
        context: std::sync::Arc<TaskExecutionContext>,
    ) -> Result<ExtractArchiveResult> {
        self.extract_isolated(request, context).await
    }

    async fn extract_with_facts(
        &self,
        request: ExtractArchiveRequest,
        facts: &ArchiveFacts,
    ) -> Result<ExtractArchiveResult> {
        let context = std::sync::Arc::new(TaskExecutionContext::detached());
        self.extract_with_facts_and_context(request, facts, context)
            .await
    }

    async fn extract_with_facts_and_context(
        &self,
        request: ExtractArchiveRequest,
        facts: &ArchiveFacts,
        context: std::sync::Arc<TaskExecutionContext>,
    ) -> Result<ExtractArchiveResult> {
        self.extract_with_facts_in_context(request, facts, context)
            .await
    }

    async fn compress(&self, request: CompressArchiveRequest) -> Result<CompressArchiveResult> {
        let context = std::sync::Arc::new(TaskExecutionContext::detached());
        self.compress_with_context(request, context).await
    }

    async fn compress_with_context(
        &self,
        request: CompressArchiveRequest,
        context: std::sync::Arc<TaskExecutionContext>,
    ) -> Result<CompressArchiveResult> {
        let container = Some(request.format.clone());
        let requirements = request_requirements(
            ArchiveOperation::Compress,
            container.as_ref(),
            request.password.as_deref(),
            None,
        );
        self.route(
            std::sync::Arc::clone(&context),
            ArchiveOperation::Compress,
            container,
            requirements,
            |adapter| {
                let request = request.clone();
                let context = std::sync::Arc::clone(&context);
                Box::pin(async move { adapter.compress_with_context(request, context).await })
            },
        )
        .await
    }
}

fn discovered_registration<A: ArchiveAdapter + 'static>(
    adapter: A,
    family_key: &str,
    executable: &Path,
    priority: i32,
    warnings: &mut Vec<String>,
) -> AdapterRegistration {
    if let Err(error) = identify_version(executable, family_key) {
        warnings.push(format!(
            "auto-discovered backend {} version command failed ({error})",
            executable.display()
        ));
    }
    AdapterRegistration::from_adapter(adapter, priority)
}

pub(crate) fn seven_zip_capabilities() -> AdapterCapabilities {
    AdapterCapabilities {
        operations: vec![
            ArchiveOperation::Probe,
            ArchiveOperation::List,
            ArchiveOperation::Test,
            ArchiveOperation::Extract,
            ArchiveOperation::Compress,
        ],
        read_containers: vec![
            ArchiveFormat::Zip,
            ArchiveFormat::SevenZip,
            ArchiveFormat::Rar,
            ArchiveFormat::Tar,
            ArchiveFormat::Gzip,
            ArchiveFormat::Bzip2,
            ArchiveFormat::Xz,
            ArchiveFormat::Zstd,
            ArchiveFormat::Cab,
        ],
        compress_containers: vec![ArchiveFormat::Zip, ArchiveFormat::SevenZip],
        supports_passwords: true,
        supports_charset_override: true,
    }
}

pub(crate) fn unrar_capabilities() -> AdapterCapabilities {
    AdapterCapabilities {
        operations: vec![
            ArchiveOperation::Probe,
            ArchiveOperation::List,
            ArchiveOperation::Test,
            ArchiveOperation::Extract,
        ],
        read_containers: vec![ArchiveFormat::Rar],
        compress_containers: Vec::new(),
        supports_passwords: true,
        supports_charset_override: false,
    }
}

fn identify_version(executable: &Path, family_key: &str) -> std::io::Result<String> {
    let argument = if family_key == "unrar-cli" { "-v" } else { "i" };
    let output = std::process::Command::new(executable)
        .arg(argument)
        .output()?;
    if !output.status.success() {
        return Err(std::io::Error::other("version command returned failure"));
    }
    let text = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    text.split_whitespace()
        .find(|token| {
            token.as_bytes().first().is_some_and(u8::is_ascii_digit) && token.contains('.')
        })
        .map(|token| {
            token
                .trim_matches(|character: char| {
                    !character.is_ascii_alphanumeric() && character != '.'
                })
                .to_owned()
        })
        .filter(|version| !version.is_empty())
        .ok_or_else(|| std::io::Error::other("version was not present in command output"))
}

fn resolve_executable(path: &Path) -> PathBuf {
    if path.components().count() == 1 {
        if let Ok(found) = which::which(path) {
            return std::fs::canonicalize(&found).unwrap_or(found);
        }
    }
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn check_declared_version(installation: &BackendInstallation, warnings: &mut Vec<String>) {
    let Some(declared) = installation.declared_version.as_deref() else {
        return;
    };
    let output = std::process::Command::new(&installation.executable)
        .arg("i")
        .output();
    match output {
        Ok(output) if output.status.success() => {
            let actual = format!(
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            if !actual.contains(declared) {
                warnings.push(format!(
                    "backend {} declared version {declared}, but executable reported a different version",
                    installation.id
                ));
            }
        }
        Ok(_) => warnings.push(format!(
            "backend {} version command failed; configured adapter remains active",
            installation.id
        )),
        Err(error) => warnings.push(format!(
            "backend {} version command failed ({error}); configured adapter remains active",
            installation.id
        )),
    }
}

fn base_requirements(
    _operation: ArchiveOperation,
    _container: Option<&ArchiveFormat>,
) -> ArchiveRequirements {
    ArchiveRequirements::default()
}

fn request_requirements(
    _operation: ArchiveOperation,
    _container: Option<&ArchiveFormat>,
    password: Option<&str>,
    encoding: Option<&smartzip_core::EncodingMode>,
) -> ArchiveRequirements {
    ArchiveRequirements {
        password: password.is_some_and(|password| !password.is_empty()),
        charset_override: matches!(encoding, Some(smartzip_core::EncodingMode::Override(_))),
        ..ArchiveRequirements::default()
    }
}

fn is_retryable(error: &SmartZipError) -> bool {
    matches!(
        error,
        SmartZipError::UnsupportedContainer { .. }
            | SmartZipError::UnsupportedCodec { .. }
            | SmartZipError::BackendUnavailable { .. }
            | SmartZipError::BackendProtocolError { .. }
    )
}

fn error_class(error: &SmartZipError) -> &'static str {
    match error {
        SmartZipError::UnsupportedContainer { .. } => "unsupported-container",
        SmartZipError::UnsupportedCodec { .. } => "unsupported-codec",
        SmartZipError::BackendUnavailable { .. } => "backend-unavailable",
        SmartZipError::BackendProtocolError { .. } => "backend-protocol-error",
        SmartZipError::WrongPassword { .. } => "wrong-password",
        SmartZipError::PasswordRequired { .. } => "password-required",
        SmartZipError::CorruptedArchive { .. } => "corrupted-archive",
        SmartZipError::UnsafeArchivePath { .. } => "unsafe-path",
        SmartZipError::Cancelled => "cancelled",
        SmartZipError::Io { .. } => "io",
        _ => "terminal-backend-error",
    }
}

fn no_compatible_backend(operation: ArchiveOperation, plan: &RoutePlan) -> SmartZipError {
    let requirements = format!(
        "password={}, charset_override={}, codecs={:?}",
        plan.requirements.password, plan.requirements.charset_override, plan.requirements.codecs
    );
    let rejected = plan
        .rejected
        .iter()
        .map(|adapter| format!("{}: {}", adapter.adapter_id, adapter.reasons.join(", ")))
        .collect::<Vec<_>>()
        .join("; ");
    SmartZipError::BackendUnavailable {
        backend: format!(
            "archive-router:{operation:?} (requirements: [{requirements}]; rejected: [{rejected}])"
        ),
    }
}

fn infer_format(requested: Option<ArchiveFormat>, path: &Path) -> Option<ArchiveFormat> {
    requested.or_else(|| format_from_extension(path))
}

fn ensure_empty_output_dir(path: &Path) -> Result<()> {
    std::fs::create_dir_all(path)
        .map_err(|source| SmartZipError::io(Some(path.to_path_buf()), source))?;
    let mut entries = std::fs::read_dir(path)
        .map_err(|source| SmartZipError::io(Some(path.to_path_buf()), source))?;
    if entries
        .next()
        .transpose()
        .map_err(|source| SmartZipError::io(Some(path.to_path_buf()), source))?
        .is_some()
    {
        return Err(SmartZipError::BackendProtocolError {
            backend: "router".into(),
            detail: format!(
                "routed extraction requires an empty staging directory: {}",
                path.display()
            ),
        });
    }
    Ok(())
}

fn clear_attempt_output(path: &Path) -> Result<()> {
    for entry in std::fs::read_dir(path)
        .map_err(|source| SmartZipError::io(Some(path.to_path_buf()), source))?
    {
        let entry = entry.map_err(|source| SmartZipError::io(Some(path.to_path_buf()), source))?;
        let entry_path = entry.path();
        let result = if entry_path.is_dir() {
            std::fs::remove_dir_all(&entry_path)
        } else {
            std::fs::remove_file(&entry_path)
        };
        result.map_err(|source| SmartZipError::io(Some(entry_path), source))?;
    }
    if std::fs::read_dir(path)
        .map_err(|source| SmartZipError::io(Some(path.to_path_buf()), source))?
        .next()
        .transpose()
        .map_err(|source| SmartZipError::io(Some(path.to_path_buf()), source))?
        .is_some()
    {
        return Err(SmartZipError::BackendProtocolError {
            backend: "archive-router-cleanup".into(),
            detail: format!("temporary output is not empty: {}", path.display()),
        });
    }
    Ok(())
}

pub fn format_from_extension(path: impl AsRef<Path>) -> Option<ArchiveFormat> {
    let extension = path
        .as_ref()
        .extension()
        .and_then(|extension| extension.to_str())?
        .to_ascii_lowercase();
    match extension.as_str() {
        "zip" => Some(ArchiveFormat::Zip),
        "7z" => Some(ArchiveFormat::SevenZip),
        "rar" => Some(ArchiveFormat::Rar),
        "tar" => Some(ArchiveFormat::Tar),
        "gz" | "tgz" => Some(ArchiveFormat::Gzip),
        "bz2" | "tbz2" => Some(ArchiveFormat::Bzip2),
        "xz" | "txz" => Some(ArchiveFormat::Xz),
        "zst" | "zstd" => Some(ArchiveFormat::Zstd),
        "cab" => Some(ArchiveFormat::Cab),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smartzip_core::{EncodingMode, TaskEvent, TaskEventKind, TaskEventSink};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    #[derive(Clone, Default)]
    struct RecordingSink(Arc<Mutex<Vec<TaskEvent>>>);

    impl TaskEventSink for RecordingSink {
        fn push(&self, event: TaskEvent) {
            self.0.lock().unwrap().push(event);
        }
    }

    #[derive(Clone)]
    struct FakeAdapter {
        id: String,
        calls: Arc<AtomicUsize>,
        failure: Option<FakeFailure>,
        partial: Option<&'static str>,
    }

    #[derive(Clone, Copy)]
    enum FakeFailure {
        UnsupportedCodec,
        WrongPassword,
        Corruption,
        Protocol,
    }

    impl FakeAdapter {
        fn new(id: &str, failure: Option<FakeFailure>) -> Self {
            Self {
                id: id.into(),
                calls: Arc::new(AtomicUsize::new(0)),
                failure,
                partial: None,
            }
        }

        fn with_partial(mut self, content: &'static str) -> Self {
            self.partial = Some(content);
            self
        }

        fn fail(&self, path: &Path) -> Result<()> {
            match self.failure {
                Some(FakeFailure::UnsupportedCodec) => Err(SmartZipError::UnsupportedCodec {
                    backend: self.id.clone(),
                    path: path.to_path_buf(),
                    codec: Some("zstd".into()),
                }),
                Some(FakeFailure::WrongPassword) => Err(SmartZipError::WrongPassword {
                    path: path.to_path_buf(),
                }),
                Some(FakeFailure::Corruption) => Err(SmartZipError::CorruptedArchive {
                    path: path.to_path_buf(),
                    detail: "broken".into(),
                }),
                Some(FakeFailure::Protocol) => Err(SmartZipError::BackendProtocolError {
                    backend: self.id.clone(),
                    detail: "bad framing".into(),
                }),
                None => Ok(()),
            }
        }
    }

    #[async_trait]
    impl ArchiveAdapter for FakeAdapter {
        fn id(&self) -> &str {
            &self.id
        }

        async fn probe(&self, path: &Path) -> Result<ArchiveProbe> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.fail(path)?;
            Ok(ArchiveProbe {
                path: path.to_path_buf(),
                format: Some(ArchiveFormat::SevenZip),
                encrypted: None,
                supported: true,
            })
        }

        async fn list(&self, request: ListRequest) -> Result<ArchiveListing> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.fail(&request.archive)?;
            Ok(ArchiveListing {
                format: request.format,
                entries: Vec::new(),
            })
        }

        async fn test(&self, request: TestRequest) -> Result<TestResult> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.fail(&request.archive)?;
            Ok(TestResult {
                ok: true,
                encrypted: None,
                ..TestResult::default()
            })
        }

        async fn extract(&self, request: ExtractArchiveRequest) -> Result<ExtractArchiveResult> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if let Some(content) = self.partial {
                std::fs::write(request.output_dir.join("partial.txt"), content).unwrap();
            }
            self.fail(&request.archive)?;
            std::fs::write(request.output_dir.join("success.txt"), &self.id).unwrap();
            Ok(ExtractArchiveResult {
                output_dir: request.output_dir,
            })
        }

        async fn compress(&self, request: CompressArchiveRequest) -> Result<CompressArchiveResult> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.fail(&request.output)?;
            Ok(CompressArchiveResult {
                output: request.output,
            })
        }

        fn capabilities(&self) -> smartzip_core::AdapterCapabilities {
            crate::router::seven_zip_capabilities()
        }
    }

    fn registration(adapter: FakeAdapter, priority: i32) -> AdapterRegistration {
        AdapterRegistration::from_adapter(adapter, priority)
    }

    fn extract_request(output_dir: PathBuf) -> ExtractArchiveRequest {
        ExtractArchiveRequest {
            archive: PathBuf::from("fixture.7z"),
            format: Some(ArchiveFormat::SevenZip),
            output_dir,
            password: Some("secret".into()),
            encoding: EncodingMode::Auto,
        }
    }

    #[test]
    fn route_plan_orders_candidates_and_explains_container_rejections() {
        let router = BackendRouter::from_adapters(vec![
            registration(FakeAdapter::new("7zz", None), 20),
            registration(FakeAdapter::new("7z", None), 10),
        ]);
        let plan = router.plan(
            ArchiveOperation::Extract,
            Some(&ArchiveFormat::Rar),
            ArchiveRequirements::default(),
        );
        assert_eq!(plan.candidates[0].adapter_id, "7zz");
        assert!(plan.rejected.is_empty());

        let rar_only =
            AdapterRegistration::new(FakeAdapter::new("rar", None), unrar_capabilities(), 20);
        let plan = BackendRouter::from_adapters(vec![rar_only]).plan(
            ArchiveOperation::Extract,
            Some(&ArchiveFormat::SevenZip),
            ArchiveRequirements::default(),
        );
        assert!(plan.candidates.is_empty());
        assert!(plan.rejected[0].reasons[0].contains("unsupported"));
    }

    #[test]
    fn compress_only_uses_compressible_containers() {
        let router = BackendRouter::from_adapters(vec![AdapterRegistration::new(
            FakeAdapter::new("7z", None),
            seven_zip_capabilities(),
            10,
        )]);
        let zip = router.plan(
            ArchiveOperation::Compress,
            Some(&ArchiveFormat::Zip),
            ArchiveRequirements::default(),
        );
        assert_eq!(zip.candidates.len(), 1);

        let rar = router.plan(
            ArchiveOperation::Compress,
            Some(&ArchiveFormat::Rar),
            ArchiveRequirements::default(),
        );
        assert!(rar.candidates.is_empty());
    }

    #[test]
    fn forced_adapter_bypasses_concrete_eligibility() {
        let router = BackendRouter::from_adapters(vec![AdapterRegistration::new(
            FakeAdapter::new("diagnostic", None),
            unrar_capabilities(),
            0,
        )])
        .with_forced_adapter("diagnostic");
        let plan = router.plan(
            ArchiveOperation::Extract,
            Some(&ArchiveFormat::SevenZip),
            ArchiveRequirements::default(),
        );
        assert_eq!(plan.candidates[0].adapter_id, "diagnostic");
        assert!(plan.rejected.is_empty());
    }

    #[tokio::test]
    async fn unsupported_codec_falls_back_and_cleans_partial_output() {
        let first = FakeAdapter::new("7zz", Some(FakeFailure::UnsupportedCodec))
            .with_partial("contamination");
        let second = FakeAdapter::new("7z", None);
        let router =
            BackendRouter::from_adapters(vec![registration(first, 20), registration(second, 10)]);
        let sink = RecordingSink::default();
        let context = router.begin_task(TaskId::new(), Arc::new(sink.clone()));
        let temp = tempfile::tempdir().unwrap();
        router
            .extract_with_context(
                extract_request(temp.path().to_path_buf()),
                std::sync::Arc::clone(&context),
            )
            .await
            .unwrap();
        assert!(!temp.path().join("partial.txt").exists());
        assert_eq!(
            std::fs::read_to_string(temp.path().join("success.txt")).unwrap(),
            "7z"
        );
        let events = sink.0.lock().unwrap().clone();
        assert!(events.iter().any(|event| matches!(
            &event.kind,
            TaskEventKind::Route(RouteEvent::BackendAttemptCleaned { adapter_id })
                if adapter_id == "7zz"
        )));
        assert!(!format!("{events:?}").contains("secret"));
    }

    #[tokio::test]
    async fn terminal_errors_never_fall_back() {
        for failure in [FakeFailure::WrongPassword, FakeFailure::Corruption] {
            let first = FakeAdapter::new("first", Some(failure));
            let second = FakeAdapter::new("second", None);
            let second_calls = second.calls.clone();
            let router = BackendRouter::from_adapters(vec![
                registration(first, 20),
                registration(second, 10),
            ]);
            let request = ListRequest {
                archive: PathBuf::from("fixture.7z"),
                format: Some(ArchiveFormat::SevenZip),
                password: Some("secret".into()),
                encoding: EncodingMode::Auto,
            };
            assert!(router.list(request).await.is_err());
            assert_eq!(second_calls.load(Ordering::SeqCst), 0);
        }
    }

    #[test]
    fn fallback_taxonomy_keeps_security_resource_and_user_errors_terminal() {
        let path = PathBuf::from("fixture.7z");
        let terminal = [
            SmartZipError::WrongPassword { path: path.clone() },
            SmartZipError::CorruptedArchive {
                path: path.clone(),
                detail: "broken".into(),
            },
            SmartZipError::UnsafeArchivePath {
                entry: "../escape".into(),
            },
            SmartZipError::io(
                Some(path.clone()),
                std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied"),
            ),
            SmartZipError::io(
                Some(path),
                std::io::Error::new(std::io::ErrorKind::StorageFull, "disk full"),
            ),
            SmartZipError::Cancelled,
            SmartZipError::BackendFailed {
                backend: "resource-limited".into(),
                exit_code: Some(8),
                stderr: "out of memory".into(),
            },
        ];
        assert!(terminal.iter().all(|error| !is_retryable(error)));
        assert!(is_retryable(&SmartZipError::BackendUnavailable {
            backend: "missing".into(),
        }));
    }

    #[tokio::test]
    async fn protocol_error_is_retryable_but_forced_route_is_not() {
        let first = FakeAdapter::new("first", Some(FakeFailure::Protocol));
        let second = FakeAdapter::new("second", None);
        let second_calls = second.calls.clone();
        let request = TestRequest {
            archive: PathBuf::from("fixture.7z"),
            format: Some(ArchiveFormat::SevenZip),
            password: None,
            encoding: EncodingMode::Auto,
        };
        let router = BackendRouter::from_adapters(vec![
            registration(first.clone(), 20),
            registration(second.clone(), 10),
        ]);
        assert!(router.test(request.clone()).await.unwrap().ok);
        assert_eq!(second_calls.load(Ordering::SeqCst), 1);

        let forced =
            BackendRouter::from_adapters(vec![registration(first, 20), registration(second, 10)])
                .with_forced_adapter("first");
        assert!(matches!(
            forced.test(request).await,
            Err(SmartZipError::BackendProtocolError { .. })
        ));
    }

    #[test]
    fn negative_cache_is_scoped_to_observed_codec_and_task() {
        let router = BackendRouter::from_adapters(vec![
            registration(FakeAdapter::new("7zz", None), 20),
            registration(FakeAdapter::new("7z", None), 10),
        ]);
        let context = TaskExecutionContext::detached();
        router.remember_retryable(
            &context,
            "7zz",
            ArchiveOperation::Extract,
            Some(ArchiveFormat::SevenZip),
            &SmartZipError::UnsupportedCodec {
                backend: "7zz".into(),
                path: PathBuf::from("one.7z"),
                codec: Some("zstd".into()),
            },
        );
        let plan = router.plan_with_context(
            ArchiveOperation::Extract,
            Some(&ArchiveFormat::SevenZip),
            ArchiveRequirements {
                codecs: vec!["zstd".into()],
                ..ArchiveRequirements::default()
            },
            Some(&context),
        );
        assert!(plan
            .rejected
            .iter()
            .any(|adapter| adapter.adapter_id == "7zz"));

        let other_codec = router.plan_with_context(
            ArchiveOperation::Extract,
            Some(&ArchiveFormat::SevenZip),
            ArchiveRequirements {
                codecs: vec!["lzma".into()],
                ..ArchiveRequirements::default()
            },
            Some(&context),
        );
        assert!(other_codec
            .candidates
            .iter()
            .any(|adapter| adapter.adapter_id == "7zz"));

        let reset_context = router.begin_task(TaskId::new(), Arc::new(RecordingSink::default()));
        let reset_plan = router.plan_with_context(
            ArchiveOperation::Extract,
            Some(&ArchiveFormat::SevenZip),
            ArchiveRequirements {
                codecs: vec!["zstd".into()],
                ..ArchiveRequirements::default()
            },
            Some(reset_context.as_ref()),
        );
        assert!(reset_plan
            .candidates
            .iter()
            .any(|adapter| adapter.adapter_id == "7zz"));
    }

    #[test]
    fn configured_adapter_survives_version_command_failure() {
        let config = BackendConfig {
            auto_discover: false,
            installations: vec![BackendInstallation {
                id: "configured-7z".into(),
                family: AdapterFamily::SevenZipCli,
                executable: PathBuf::from("/definitely/missing/7z"),
                declared_version: Some("24.09".into()),
                enabled: true,
                priority: 50,
            }],
        };
        let router = BackendRouter::from_config(&config).unwrap();
        assert!(router.adapter_ids().contains(&"configured-7z"));
        assert!(router
            .warnings()
            .iter()
            .any(|warning| warning.contains("configured adapter remains active")));
    }

    #[test]
    fn configured_paths_are_deduplicated() {
        let duplicate_path = PathBuf::from("/missing/shared-7z");
        let config = BackendConfig {
            auto_discover: false,
            installations: vec![
                BackendInstallation {
                    id: "first-7z".into(),
                    family: AdapterFamily::SevenZipCli,
                    executable: duplicate_path.clone(),
                    declared_version: None,
                    enabled: true,
                    priority: 0,
                },
                BackendInstallation {
                    id: "duplicate-7z".into(),
                    family: AdapterFamily::SevenZipCli,
                    executable: duplicate_path,
                    declared_version: None,
                    enabled: true,
                    priority: 0,
                },
            ],
        };
        let router = BackendRouter::from_config(&config).unwrap();
        assert!(router.adapter_ids().contains(&"first-7z"));
        assert!(!router.adapter_ids().contains(&"duplicate-7z"));
    }

    #[test]
    fn format_from_extension_covers_supported_aliases() {
        assert_eq!(format_from_extension("a.7z"), Some(ArchiveFormat::SevenZip));
        assert_eq!(format_from_extension("a.tgz"), Some(ArchiveFormat::Gzip));
        assert_eq!(
            format_from_extension("a.tar.zst"),
            Some(ArchiveFormat::Zstd)
        );
        assert_eq!(format_from_extension("a.unknown"), None);
    }

    #[test]
    fn sevenzip_is_final_fallback_for_non_rar_formats() {
        let router = BackendRouter::from_adapters(vec![
            AdapterRegistration::from_adapter(UnrarBackend::new(PathBuf::from("unrar")), 20),
            AdapterRegistration::from_adapter(SevenZipBackend::new(PathBuf::from("7z")), 10),
        ]);

        let zstd_plan = router.plan(
            ArchiveOperation::Extract,
            Some(&ArchiveFormat::Zstd),
            ArchiveRequirements::default(),
        );
        assert_eq!(zstd_plan.candidates.len(), 1);
        assert!(zstd_plan.candidates[0].adapter_id.starts_with("sevenzip:"));

        let rar_plan = router.plan(
            ArchiveOperation::Extract,
            Some(&ArchiveFormat::Rar),
            ArchiveRequirements::default(),
        );
        assert_eq!(rar_plan.candidates.len(), 2);
        assert!(rar_plan.candidates[0].adapter_id.starts_with("unrar:"));
        assert!(rar_plan.candidates[1].adapter_id.starts_with("sevenzip:"));
    }
}
