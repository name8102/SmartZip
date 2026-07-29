use crate::backend::{ArchiveAdapter, ArchiveExecutor};
use crate::native_zip::NativeZipBackend;
use crate::sevenzz::{SevenZipBackend, SevenZipLocator};
use crate::types::*;
use crate::unrar::{UnrarBackend, UnrarLocator};
use async_trait::async_trait;
use smartzip_config::{AdapterFamily, BackendConfig, BackendInstallation};
use smartzip_core::{
    ArchiveFacts, ArchiveFormat, ArchiveOperation, ArchiveRequirement, ArchiveRequirements,
    BackendCapabilityProfile, CapabilityId, CapabilityRule, NegativeCapabilityKey, RejectedAdapter,
    RequirementClass, Result, RouteCandidate, RouteEvent, RoutePlan, SmartZipError, SupportState,
    TaskEventSink, TaskExecutionContext, TaskId,
};
use std::collections::HashSet;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;

#[derive(Clone)]
pub struct AdapterRegistration {
    pub adapter: Arc<dyn ArchiveAdapter>,
    pub profile: BackendCapabilityProfile,
    pub priority: i32,
}

impl std::fmt::Debug for AdapterRegistration {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AdapterRegistration")
            .field("adapter_id", &self.adapter.id())
            .field("profile", &self.profile)
            .field("priority", &self.priority)
            .finish()
    }
}

impl AdapterRegistration {
    pub fn new(
        adapter: impl ArchiveAdapter + 'static,
        profile: BackendCapabilityProfile,
        priority: i32,
    ) -> Self {
        Self {
            adapter: Arc::new(adapter),
            profile,
            priority,
        }
    }

    pub fn from_adapter(adapter: impl ArchiveAdapter + 'static, priority: i32) -> Self {
        let profile = adapter.profile();
        Self::new(adapter, profile, priority)
    }
}

#[derive(Debug, Clone)]
pub struct BackendRouter {
    adapters: Vec<AdapterRegistration>,
    forced_adapter: Option<String>,
    warnings: Vec<String>,
}

impl BackendRouter {
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
            .filter(|entry| !matches!(entry.family, AdapterFamily::NativeZip))
            .map(|entry| resolve_executable(&entry.executable))
            .collect();
        let mut registered_paths = HashSet::new();
        let mut has_native_zip = false;
        let mut warnings = Vec::new();

        for installation in config.installations.iter().filter(|entry| entry.enabled) {
            let executable = resolve_executable(&installation.executable);
            if !matches!(installation.family, AdapterFamily::NativeZip) {
                if !registered_paths.insert(executable.clone()) {
                    warnings.push(format!(
                        "backend {} duplicates an earlier configured executable and was ignored",
                        installation.id
                    ));
                    continue;
                }
                check_declared_version(installation, &mut warnings);
            }
            let configured_profile = config.profile_for(installation).map_err(|detail| {
                SmartZipError::BackendProtocolError {
                    backend: installation.id.clone(),
                    detail,
                }
            })?;
            let registration = match &installation.family {
                AdapterFamily::NativeZip if has_native_zip => {
                    warnings.push(format!(
                        "backend {} duplicates the configured native ZIP adapter and was ignored",
                        installation.id
                    ));
                    continue;
                }
                AdapterFamily::NativeZip => {
                    has_native_zip = true;
                    configured_registration(
                        NativeZipBackend::new().with_id(installation.id.clone()),
                        installation,
                        &configured_profile,
                    )?
                }
                AdapterFamily::SevenZipCli => configured_registration(
                    SevenZipBackend::new(executable).with_id(installation.id.clone()),
                    installation,
                    &configured_profile,
                )?,
                AdapterFamily::UnrarCli => configured_registration(
                    UnrarBackend::new(executable).with_id(installation.id.clone()),
                    installation,
                    &configured_profile,
                )?,
                AdapterFamily::Custom(family) => {
                    warnings.push(format!(
                        "backend {} uses unavailable adapter family {family}",
                        installation.id
                    ));
                    continue;
                }
            };
            adapters.push(registration);
        }

        if !has_native_zip {
            adapters.push(AdapterRegistration::from_adapter(
                NativeZipBackend::new(),
                -10,
            ));
        }
        if config.auto_discover {
            if let Some(executable) = UnrarLocator::default().locate() {
                if !explicit_paths.contains(&resolve_executable(&executable)) {
                    adapters.push(discovered_registration(
                        UnrarBackend::new(executable.clone()),
                        "unrar-cli",
                        &executable,
                        config,
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
                        config,
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
        intent
            .items
            .extend(ArchiveRequirements::from_facts(facts).items);
        self.plan(
            operation,
            facts.container.as_ref().map(|fact| &fact.value),
            intent,
        )
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

        for registration in &self.adapters {
            let adapter_id = registration.adapter.id();
            let mut reasons = Vec::new();
            if let Some(forced) = self.forced_adapter.as_deref() {
                if forced == adapter_id {
                    candidates.push((
                        RouteCandidate {
                            adapter_id: adapter_id.to_owned(),
                            priority: registration.priority,
                            notes: vec![
                                "forced for diagnostics; compatibility filters bypassed".into()
                            ],
                        },
                        0,
                        0,
                    ));
                    continue;
                }
                reasons.push(format!("forced adapter is {forced}"));
            }

            let negative = NegativeCapabilityKey {
                adapter_id: adapter_id.to_owned(),
                operation,
                container: container.cloned(),
                capability: None,
            };
            if let Some(context) = context {
                if let Some(reason) = context.rejection(&negative) {
                    reasons.push(format!("task-local incompatibility: {reason}"));
                }
                for requirement in &requirements.items {
                    let feature_negative = NegativeCapabilityKey {
                        adapter_id: adapter_id.to_owned(),
                        operation,
                        container: container.cloned(),
                        capability: Some(requirement.capability.clone()),
                    };
                    if let Some(reason) = context.rejection(&feature_negative) {
                        reasons.push(format!(
                            "task-local incompatibility for {}: {reason}",
                            requirement.capability
                        ));
                    }
                }
            }

            let operation_capability = capability_id("operation", operation_name(operation));
            reject_unsupported(
                &registration.profile,
                &operation_capability,
                operation,
                container,
                "operation",
                &mut reasons,
            );
            if let Some(container) = container {
                let container_capability = capability_id("container", container.as_str());
                reject_unsupported(
                    &registration.profile,
                    &container_capability,
                    operation,
                    Some(container),
                    "container",
                    &mut reasons,
                );
            }

            let mut known_supported = 0;
            let mut preferred = 0;
            let mut notes = Vec::new();
            for requirement in &requirements.items {
                let support =
                    registration
                        .profile
                        .support(&requirement.capability, operation, container);
                match (&requirement.class, support) {
                    (RequirementClass::Required, SupportState::Unsupported) => {
                        reasons.push(format!(
                            "{} is unsupported ({})",
                            requirement.capability, requirement.reason
                        ))
                    }
                    (RequirementClass::Required, SupportState::Conditional { conditions })
                        if !conditions
                            .iter()
                            .all(|condition| requirement.conditions.contains(condition)) =>
                    {
                        reasons.push(format!(
                            "{} requires conditions: {}",
                            requirement.capability,
                            conditions.join(", ")
                        ));
                    }
                    (RequirementClass::Required, SupportState::Supported) => known_supported += 1,
                    (RequirementClass::Preferred, SupportState::Supported) => preferred += 1,
                    (RequirementClass::Required, SupportState::Unknown) => {
                        notes.push(format!("{} support is unknown", requirement.capability))
                    }
                    _ => {}
                }
            }

            if reasons.is_empty() {
                candidates.push((
                    RouteCandidate {
                        adapter_id: adapter_id.to_owned(),
                        priority: registration.priority,
                        notes,
                    },
                    known_supported,
                    preferred,
                ));
            } else {
                rejected.push(RejectedAdapter {
                    adapter_id: adapter_id.to_owned(),
                    reasons,
                });
            }
        }

        candidates.sort_by(
            |(left, left_supported, left_preferred), (right, right_supported, right_preferred)| {
                right_supported
                    .cmp(left_supported)
                    .then_with(|| right_preferred.cmp(left_preferred))
                    .then_with(|| right.priority.cmp(&left.priority))
                    .then_with(|| left.adapter_id.cmp(&right.adapter_id))
            },
        );
        let plan = RoutePlan {
            operation,
            requirements,
            candidates: candidates
                .into_iter()
                .map(|(candidate, _, _)| candidate)
                .collect(),
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
        let (reason, capability) = match error {
            SmartZipError::UnsupportedContainer { .. } => ("unsupported container", None),
            SmartZipError::UnsupportedCodec {
                codec: Some(codec), ..
            } => {
                let identifier = if codec.contains(':') {
                    CapabilityId::new(codec.clone())
                } else {
                    CapabilityId::new(format!("codec:{codec}"))
                };
                let Ok(identifier) = identifier else {
                    return;
                };
                ("unsupported codec", Some(identifier))
            }
            // A codec observation without an exact feature is not safe to cache.
            SmartZipError::UnsupportedCodec { codec: None, .. } => return,
            _ => return,
        };
        context.record_rejection(
            NegativeCapabilityKey {
                adapter_id: adapter_id.to_owned(),
                operation,
                container,
                capability,
            },
            reason,
        );
    }

    async fn route<T, F>(
        &self,
        context: &TaskExecutionContext,
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
        let plan =
            self.plan_with_context(operation, container.as_ref(), requirements, Some(context));
        let mut attempted = Vec::new();
        let mut last_error = None;
        for candidate in &plan.candidates {
            let registration = self
                .registration(&candidate.adapter_id)
                .expect("planned adapter must remain registered");
            attempted.push(candidate.adapter_id.clone());
            self.emit(
                context,
                RouteEvent::BackendAttemptStarted {
                    adapter_id: candidate.adapter_id.clone(),
                },
            );
            match invoke(registration.adapter.as_ref()).await {
                Ok(value) => {
                    self.emit(
                        context,
                        RouteEvent::BackendSelected {
                            adapter_id: candidate.adapter_id.clone(),
                        },
                    );
                    return Ok(value);
                }
                Err(error) => {
                    self.emit(
                        context,
                        RouteEvent::BackendAttemptFailed {
                            adapter_id: candidate.adapter_id.clone(),
                            class: error_class(&error).into(),
                        },
                    );
                    if !is_retryable(&error) || self.forced_adapter.is_some() {
                        return Err(error);
                    }
                    self.remember_retryable(
                        context,
                        &candidate.adapter_id,
                        operation,
                        container.clone(),
                        &error,
                    );
                    last_error = Some(error);
                }
            }
        }
        self.emit(context, RouteEvent::RouteExhausted { attempted });
        Err(last_error.unwrap_or_else(|| no_compatible_backend(operation, &plan)))
    }

    async fn extract_with_facts_in_context(
        &self,
        request: ExtractArchiveRequest,
        facts: &ArchiveFacts,
        context: &TaskExecutionContext,
    ) -> Result<ExtractArchiveResult> {
        let container = facts
            .container
            .as_ref()
            .map(|fact| fact.value.clone())
            .or_else(|| infer_format(request.format.clone(), &request.archive));
        let mut requirements = request_requirements(
            ArchiveOperation::Extract,
            container.as_ref(),
            request.password.as_deref(),
            Some(&request.encoding),
        );
        requirements
            .items
            .extend(ArchiveRequirements::from_facts(facts).items);
        self.extract_isolated_planned(request, container, requirements, context)
            .await
    }

    async fn extract_isolated(
        &self,
        request: ExtractArchiveRequest,
        context: &TaskExecutionContext,
    ) -> Result<ExtractArchiveResult> {
        let container = infer_format(request.format.clone(), &request.archive);
        let requirements = request_requirements(
            ArchiveOperation::Extract,
            container.as_ref(),
            request.password.as_deref(),
            Some(&request.encoding),
        );
        self.extract_isolated_planned(request, container, requirements, context)
            .await
    }

    async fn extract_isolated_planned(
        &self,
        request: ExtractArchiveRequest,
        container: Option<ArchiveFormat>,
        requirements: ArchiveRequirements,
        context: &TaskExecutionContext,
    ) -> Result<ExtractArchiveResult> {
        let plan = self.plan_with_context(
            ArchiveOperation::Extract,
            container.as_ref(),
            requirements,
            Some(context),
        );
        let mut attempted = Vec::new();
        let mut last_error = None;
        // OutputMaterializer owns this directory. The router only clears its
        // contents between adapter attempts; it never creates nested staging.
        ensure_empty_output_dir(&request.output_dir)?;

        for candidate in &plan.candidates {
            let registration = self
                .registration(&candidate.adapter_id)
                .expect("planned adapter must remain registered");
            attempted.push(candidate.adapter_id.clone());
            self.emit(
                context,
                RouteEvent::BackendAttemptStarted {
                    adapter_id: candidate.adapter_id.clone(),
                },
            );

            match registration.adapter.extract(request.clone()).await {
                Ok(_) => {
                    self.emit(
                        context,
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
                        context,
                        RouteEvent::BackendAttemptFailed {
                            adapter_id: candidate.adapter_id.clone(),
                            class: error_class(&error).into(),
                        },
                    );
                    clear_output_dir(&request.output_dir)?;
                    self.emit(
                        context,
                        RouteEvent::BackendAttemptCleaned {
                            adapter_id: candidate.adapter_id.clone(),
                        },
                    );
                    if !is_retryable(&error) || self.forced_adapter.is_some() {
                        return Err(error);
                    }
                    self.remember_retryable(
                        context,
                        &candidate.adapter_id,
                        ArchiveOperation::Extract,
                        container.clone(),
                        &error,
                    );
                    last_error = Some(error);
                }
            }
        }
        self.emit(context, RouteEvent::RouteExhausted { attempted });
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
        let context = TaskExecutionContext::detached();
        self.probe_with_context(path, &context).await
    }

    async fn probe_with_context(
        &self,
        path: &Path,
        context: &TaskExecutionContext,
    ) -> Result<ArchiveProbe> {
        let container = format_from_extension(path);
        let requirements = base_requirements(ArchiveOperation::Probe, container.as_ref());
        let path = path.to_path_buf();
        self.route(
            context,
            ArchiveOperation::Probe,
            container,
            requirements,
            |adapter| {
                let path = path.clone();
                Box::pin(async move {
                    let probe = adapter.probe(&path).await?;
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
        let context = TaskExecutionContext::detached();
        self.list_with_context(request, &context).await
    }

    async fn list_with_context(
        &self,
        request: ListRequest,
        context: &TaskExecutionContext,
    ) -> Result<ArchiveListing> {
        let container = infer_format(request.format.clone(), &request.archive);
        let requirements = request_requirements(
            ArchiveOperation::List,
            container.as_ref(),
            request.password.as_deref(),
            Some(&request.encoding),
        );
        self.route(
            context,
            ArchiveOperation::List,
            container,
            requirements,
            |adapter| {
                let request = request.clone();
                Box::pin(async move { adapter.list(request).await })
            },
        )
        .await
    }

    async fn test(&self, request: TestRequest) -> Result<TestResult> {
        let context = TaskExecutionContext::detached();
        self.test_with_context(request, &context).await
    }

    async fn test_with_context(
        &self,
        request: TestRequest,
        context: &TaskExecutionContext,
    ) -> Result<TestResult> {
        let container = infer_format(request.format.clone(), &request.archive);
        let requirements = request_requirements(
            ArchiveOperation::Test,
            container.as_ref(),
            request.password.as_deref(),
            Some(&request.encoding),
        );
        self.route(
            context,
            ArchiveOperation::Test,
            container,
            requirements,
            |adapter| {
                let request = request.clone();
                Box::pin(async move { adapter.test(request).await })
            },
        )
        .await
    }

    async fn extract(&self, request: ExtractArchiveRequest) -> Result<ExtractArchiveResult> {
        let context = TaskExecutionContext::detached();
        self.extract_with_context(request, &context).await
    }

    async fn extract_with_context(
        &self,
        request: ExtractArchiveRequest,
        context: &TaskExecutionContext,
    ) -> Result<ExtractArchiveResult> {
        self.extract_isolated(request, context).await
    }

    async fn extract_with_facts(
        &self,
        request: ExtractArchiveRequest,
        facts: &ArchiveFacts,
    ) -> Result<ExtractArchiveResult> {
        let context = TaskExecutionContext::detached();
        self.extract_with_facts_and_context(request, facts, &context)
            .await
    }

    async fn extract_with_facts_and_context(
        &self,
        request: ExtractArchiveRequest,
        facts: &ArchiveFacts,
        context: &TaskExecutionContext,
    ) -> Result<ExtractArchiveResult> {
        self.extract_with_facts_in_context(request, facts, context)
            .await
    }

    async fn compress(&self, request: CompressArchiveRequest) -> Result<CompressArchiveResult> {
        let context = TaskExecutionContext::detached();
        self.compress_with_context(request, &context).await
    }

    async fn compress_with_context(
        &self,
        request: CompressArchiveRequest,
        context: &TaskExecutionContext,
    ) -> Result<CompressArchiveResult> {
        let container = Some(request.format.clone());
        let requirements = request_requirements(
            ArchiveOperation::Compress,
            container.as_ref(),
            request.password.as_deref(),
            None,
        );
        self.route(
            context,
            ArchiveOperation::Compress,
            container,
            requirements,
            |adapter| {
                let request = request.clone();
                Box::pin(async move { adapter.compress(request).await })
            },
        )
        .await
    }
}

fn configured_registration<A: ArchiveAdapter + 'static>(
    adapter: A,
    installation: &BackendInstallation,
    configured_profile: &BackendCapabilityProfile,
) -> Result<AdapterRegistration> {
    let mut profile = adapter.profile();
    // `configured_profile` is already family -> version -> installation ordered.
    // Appending preserves that layer precedence after the built-in family baseline.
    profile
        .rules
        .extend(configured_profile.rules.iter().cloned().map(|mut rule| {
            rule.precedence = rule.precedence.saturating_add(1);
            rule
        }));
    Ok(AdapterRegistration::new(
        adapter,
        profile,
        installation.priority,
    ))
}

fn discovered_registration<A: ArchiveAdapter + 'static>(
    adapter: A,
    family_key: &str,
    executable: &Path,
    config: &BackendConfig,
    priority: i32,
    warnings: &mut Vec<String>,
) -> AdapterRegistration {
    let mut profile = adapter.profile();
    if let Some(family) = config.family_profiles.get(family_key) {
        profile
            .rules
            .extend(family.rules.iter().cloned().map(|mut rule| {
                rule.precedence = 1;
                rule
            }));
    }
    match identify_version(executable, family_key) {
        Ok(version) => {
            let key = format!("{family_key}@{version}");
            if let Some(version_profile) = config.version_profiles.get(&key) {
                profile
                    .rules
                    .extend(version_profile.rules.iter().cloned().map(|mut rule| {
                        rule.precedence = 2;
                        rule
                    }));
            }
        }
        Err(error) => warnings.push(format!(
            "auto-discovered backend {} version command failed ({error}); family profile remains active",
            executable.display()
        )),
    }
    AdapterRegistration::new(adapter, profile, priority)
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
        if let Some(search_path) = std::env::var_os("PATH") {
            if let Some(found) = std::env::split_paths(&search_path)
                .map(|directory| directory.join(path))
                .find(|candidate| candidate.exists())
            {
                return std::fs::canonicalize(&found).unwrap_or(found);
            }
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
            "backend {} version command failed; configured profile remains active",
            installation.id
        )),
        Err(error) => warnings.push(format!(
            "backend {} version command failed ({error}); configured profile remains active",
            installation.id
        )),
    }
}

fn reject_unsupported(
    profile: &BackendCapabilityProfile,
    capability: &CapabilityId,
    operation: ArchiveOperation,
    container: Option<&ArchiveFormat>,
    kind: &str,
    reasons: &mut Vec<String>,
) {
    if matches!(
        profile.support(capability, operation, container),
        SupportState::Unsupported
    ) {
        reasons.push(format!("{kind} {} is unsupported", capability));
    }
}

fn base_requirements(
    operation: ArchiveOperation,
    container: Option<&ArchiveFormat>,
) -> ArchiveRequirements {
    let mut items = vec![ArchiveRequirement {
        capability: capability_id("operation", operation_name(operation)),
        class: RequirementClass::Required,
        conditions: Vec::new(),
        reason: "requested operation".into(),
    }];
    if let Some(container) = container {
        items.push(ArchiveRequirement {
            capability: capability_id("container", container.as_str()),
            class: RequirementClass::Required,
            conditions: Vec::new(),
            reason: "archive container".into(),
        });
    }
    ArchiveRequirements { items }
}

fn request_requirements(
    operation: ArchiveOperation,
    container: Option<&ArchiveFormat>,
    password: Option<&str>,
    encoding: Option<&smartzip_core::EncodingMode>,
) -> ArchiveRequirements {
    let mut requirements = base_requirements(operation, container);
    if password.is_some_and(|password| !password.is_empty()) {
        requirements.items.push(ArchiveRequirement {
            capability: capability_id("password", "provided"),
            class: RequirementClass::Required,
            conditions: Vec::new(),
            reason: "caller supplied a password".into(),
        });
    }
    if matches!(encoding, Some(smartzip_core::EncodingMode::Override(_))) {
        requirements.items.push(ArchiveRequirement {
            capability: capability_id("metadata", "charset-override"),
            class: RequirementClass::Required,
            conditions: Vec::new(),
            reason: "caller requested a filename charset override".into(),
        });
    }
    requirements
}

pub(crate) fn builtin_profile(
    can_extract: &[ArchiveFormat],
    can_compress: &[ArchiveFormat],
    supports_passwords: bool,
    supports_listing: bool,
    supports_test: bool,
) -> BackendCapabilityProfile {
    let mut rules = Vec::new();
    rules.push(global_rule(
        capability_id("operation", "probe"),
        ArchiveOperation::Probe,
        !can_extract.is_empty(),
    ));
    rules.push(global_rule(
        capability_id("operation", "list"),
        ArchiveOperation::List,
        supports_listing,
    ));
    rules.push(global_rule(
        capability_id("operation", "test"),
        ArchiveOperation::Test,
        supports_test,
    ));
    rules.push(global_rule(
        capability_id("operation", "extract"),
        ArchiveOperation::Extract,
        !can_extract.is_empty(),
    ));
    rules.push(global_rule(
        capability_id("operation", "compress"),
        ArchiveOperation::Compress,
        !can_compress.is_empty(),
    ));
    for operation in [
        ArchiveOperation::List,
        ArchiveOperation::Test,
        ArchiveOperation::Extract,
    ] {
        rules.push(global_rule(
            capability_id("password", "provided"),
            operation,
            supports_passwords,
        ));
    }
    for format in can_extract {
        rules.push(container_rule(format.clone(), ArchiveOperation::Probe));
        rules.push(container_rule(format.clone(), ArchiveOperation::List));
        rules.push(container_rule(format.clone(), ArchiveOperation::Test));
        rules.push(container_rule(format.clone(), ArchiveOperation::Extract));
    }
    for format in can_compress {
        rules.push(container_rule(format.clone(), ArchiveOperation::Compress));
    }
    BackendCapabilityProfile { rules }
}

fn global_rule(
    capability: CapabilityId,
    operation: ArchiveOperation,
    supported: bool,
) -> CapabilityRule {
    CapabilityRule {
        capability,
        precedence: 0,
        operations: vec![operation],
        containers: Vec::new(),
        support: if supported {
            SupportState::Supported
        } else {
            SupportState::Unsupported
        },
        evidence: Some("adapter family baseline".into()),
    }
}

fn container_rule(format: ArchiveFormat, operation: ArchiveOperation) -> CapabilityRule {
    CapabilityRule {
        capability: capability_id("container", format.as_str()),
        precedence: 0,
        operations: vec![operation],
        containers: vec![format],
        support: SupportState::Supported,
        evidence: Some("adapter family baseline".into()),
    }
}

fn capability_id(namespace: &str, name: &str) -> CapabilityId {
    CapabilityId::new(format!("{namespace}:{name}"))
        .expect("built-in capability identifiers are valid")
}

fn operation_name(operation: ArchiveOperation) -> &'static str {
    match operation {
        ArchiveOperation::Probe => "probe",
        ArchiveOperation::List => "list",
        ArchiveOperation::Test => "test",
        ArchiveOperation::Extract => "extract",
        ArchiveOperation::Compress => "compress",
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
    let requirements = plan
        .requirements
        .items
        .iter()
        .map(|requirement| requirement.capability.as_str())
        .collect::<Vec<_>>()
        .join(", ");
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

fn clear_output_dir(path: &Path) -> Result<()> {
    for entry in std::fs::read_dir(path)
        .map_err(|source| SmartZipError::io(Some(path.to_path_buf()), source))?
    {
        let entry = entry.map_err(|source| SmartZipError::io(Some(path.to_path_buf()), source))?;
        let entry_path = entry.path();
        if entry_path.is_dir() {
            std::fs::remove_dir_all(&entry_path)
                .map_err(|source| SmartZipError::io(Some(entry_path), source))?;
        } else {
            std::fs::remove_file(&entry_path)
                .map_err(|source| SmartZipError::io(Some(entry_path), source))?;
        }
    }
    ensure_empty_output_dir(path)
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
        "cab" => Some(ArchiveFormat::Cab),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smartzip_core::{CompressionLevel, EncodingMode, TaskEvent, TaskEventKind, TaskEventSink};
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

        fn profile(&self) -> smartzip_core::BackendCapabilityProfile {
            crate::router::builtin_profile(
                &[ArchiveFormat::SevenZip],
                &[ArchiveFormat::SevenZip],
                true,
                true,
                true,
            )
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
    fn route_plan_is_stable_and_explains_rejections() {
        let incapable = FakeAdapter::new("7zz", None);
        let capable = FakeAdapter::new("7z", None);
        let mut unsupported = incapable.profile();
        unsupported.rules.push(CapabilityRule {
            capability: capability_id("codec", "zstd"),
            precedence: 0,
            operations: vec![ArchiveOperation::Extract],
            containers: vec![ArchiveFormat::SevenZip],
            support: SupportState::Unsupported,
            evidence: Some("configured".into()),
        });
        let router = BackendRouter::from_adapters(vec![
            AdapterRegistration::new(incapable, unsupported, 20),
            registration(capable, 10),
        ]);
        let facts = ArchiveFacts {
            container: Some(smartzip_core::ArchiveFact {
                value: ArchiveFormat::SevenZip,
                source: "extension".into(),
            }),
            codecs: vec![smartzip_core::ArchiveFact {
                value: capability_id("codec", "zstd"),
                source: "archive header".into(),
            }],
            ..ArchiveFacts::default()
        };
        let plan = router.plan_for_facts(
            ArchiveOperation::Extract,
            &facts,
            ArchiveRequirements::default(),
        );
        assert_eq!(plan.candidates[0].adapter_id, "7z");
        assert_eq!(plan.rejected[0].adapter_id, "7zz");
        assert!(plan.rejected[0].reasons[0].contains("codec:zstd"));
    }

    #[test]
    fn forced_adapter_bypasses_profile_rejection() {
        let adapter = FakeAdapter::new("diagnostic", None);
        let mut profile = adapter.profile();
        profile.rules.push(CapabilityRule {
            capability: capability_id("codec", "zstd"),
            precedence: 1,
            operations: vec![ArchiveOperation::Extract],
            containers: vec![ArchiveFormat::SevenZip],
            support: SupportState::Unsupported,
            evidence: Some("configured".into()),
        });
        let router =
            BackendRouter::from_adapters(vec![AdapterRegistration::new(adapter, profile, 0)])
                .with_forced_adapter("diagnostic");
        let plan = router.plan(
            ArchiveOperation::Extract,
            Some(&ArchiveFormat::SevenZip),
            ArchiveRequirements {
                items: vec![ArchiveRequirement {
                    capability: capability_id("codec", "zstd"),
                    class: RequirementClass::Required,
                    conditions: Vec::new(),
                    reason: "archive method".into(),
                }],
            },
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
            .extract_with_context(extract_request(temp.path().to_path_buf()), context.as_ref())
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
    fn negative_cache_skips_adapter_only_for_matching_codec() {
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
        let zstd = ArchiveRequirements {
            items: vec![ArchiveRequirement {
                capability: capability_id("codec", "zstd"),
                class: RequirementClass::Required,
                conditions: Vec::new(),
                reason: "archive method".into(),
            }],
        };
        let plan = router.plan_with_context(
            ArchiveOperation::Extract,
            Some(&ArchiveFormat::SevenZip),
            zstd.clone(),
            Some(&context),
        );
        assert!(plan
            .rejected
            .iter()
            .any(|adapter| adapter.adapter_id == "7zz"));

        let reset_context = router.begin_task(TaskId::new(), Arc::new(RecordingSink::default()));
        let reset_plan = router.plan_with_context(
            ArchiveOperation::Extract,
            Some(&ArchiveFormat::SevenZip),
            zstd,
            Some(reset_context.as_ref()),
        );
        assert!(reset_plan
            .candidates
            .iter()
            .any(|adapter| adapter.adapter_id == "7zz"));

        let lzma = ArchiveRequirements {
            items: vec![ArchiveRequirement {
                capability: capability_id("codec", "lzma"),
                class: RequirementClass::Required,
                conditions: Vec::new(),
                reason: "archive method".into(),
            }],
        };
        let plan = router.plan(
            ArchiveOperation::Extract,
            Some(&ArchiveFormat::SevenZip),
            lzma,
        );
        assert!(plan
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
                profile: BackendCapabilityProfile::default(),
            }],
            family_profiles: Default::default(),
            version_profiles: Default::default(),
        };
        let router = BackendRouter::from_config(&config).unwrap();
        assert!(router.adapter_ids().contains(&"configured-7z"));
        assert!(router
            .warnings()
            .iter()
            .any(|warning| warning.contains("configured profile remains active")));
    }

    #[test]
    fn configured_paths_are_deduplicated_and_native_id_is_preserved() {
        let duplicate_path = PathBuf::from("/missing/shared-7z");
        let config = BackendConfig {
            auto_discover: false,
            installations: vec![
                BackendInstallation {
                    id: "configured-native".into(),
                    family: AdapterFamily::NativeZip,
                    executable: PathBuf::new(),
                    declared_version: None,
                    enabled: true,
                    priority: 0,
                    profile: BackendCapabilityProfile::default(),
                },
                BackendInstallation {
                    id: "first-7z".into(),
                    family: AdapterFamily::SevenZipCli,
                    executable: duplicate_path.clone(),
                    declared_version: None,
                    enabled: true,
                    priority: 0,
                    profile: BackendCapabilityProfile::default(),
                },
                BackendInstallation {
                    id: "duplicate-7z".into(),
                    family: AdapterFamily::SevenZipCli,
                    executable: duplicate_path,
                    declared_version: None,
                    enabled: true,
                    priority: 0,
                    profile: BackendCapabilityProfile::default(),
                },
            ],
            family_profiles: Default::default(),
            version_profiles: Default::default(),
        };
        let router = BackendRouter::from_config(&config).unwrap();
        assert!(router.adapter_ids().contains(&"configured-native"));
        assert!(router.adapter_ids().contains(&"first-7z"));
        assert!(!router.adapter_ids().contains(&"duplicate-7z"));
    }

    #[test]
    fn format_from_extension_covers_supported_aliases() {
        assert_eq!(format_from_extension("a.7z"), Some(ArchiveFormat::SevenZip));
        assert_eq!(format_from_extension("a.tgz"), Some(ArchiveFormat::Gzip));
        assert_eq!(format_from_extension("a.unknown"), None);
    }

    #[allow(dead_code)]
    fn compression_request() -> CompressArchiveRequest {
        CompressArchiveRequest {
            inputs: vec![PathBuf::from("input")],
            output: PathBuf::from("archive.7z"),
            format: ArchiveFormat::SevenZip,
            level: CompressionLevel::Balanced,
            password: None,
        }
    }
}
