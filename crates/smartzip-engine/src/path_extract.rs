//! Own managed output names while prepared backends produce entry content.
use crate::path_policy::{plan_paths, plan_paths_forced, PathEntry};
use smartzip_archive::{
    ArchiveExecutor, ExtractArchiveRequest, ExtractArchiveResult, ExtractionManifest,
    ExtractionNameSource, ManagedSink,
};
use smartzip_core::path_policy::*;
use smartzip_core::{ArchiveFacts, Result, SmartZipError, TaskEventKind, TaskExecutionContext};
use smartzip_platform::path_policy::{probe_target, ControlledRoot};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};
use unicode_normalization::UnicodeNormalization;

pub(crate) async fn extract<B: ArchiveExecutor>(
    backend: &B,
    request: ExtractArchiveRequest,
    facts: &ArchiveFacts,
    mode: PathMode,
    budget: crate::budget::WriterBudget,
    context: Arc<TaskExecutionContext>,
    report_slot: &std::cell::RefCell<Option<PathMappingReport>>,
    metadata_slot: &std::cell::RefCell<Option<DeferredDirectoryMetadata>>,
) -> Result<ExtractArchiveResult> {
    let parent = request
        .output_dir
        .parent()
        .unwrap_or_else(|| Path::new("."));
    let policy = probe_target(parent, mode)?;
    let Some(session) = backend
        .prepare_extraction_with_facts_and_context(request.clone(), facts, context.clone())
        .await?
    else {
        return backend
            .extract_with_facts_and_context(request, facts, context)
            .await;
    };
    let manifest = session.manifest().clone();
    let entries: Vec<_> = manifest
        .entries
        .iter()
        .map(|e| PathEntry {
            id: e.id,
            source: e.display_name.clone(),
            raw_name: e.raw_name.clone(),
            source_kind: match e.source_kind {
                ExtractionNameSource::ZipCentralDirectory => SourceNameKind::ZipCentralDirectory,
                ExtractionNameSource::ZipUnicodeExtra => SourceNameKind::ZipUnicodeExtra,
                ExtractionNameSource::BackendText => SourceNameKind::BackendText,
            },
            is_dir: e.is_dir,
        })
        .collect();
    let mut report = plan_paths(&entries, &policy)?;
    report.manifest_digest = manifest.digest.clone();
    report.archive_identity = manifest.source_identity.clone();
    report.adapter_id = manifest.adapter_id.clone();
    report.archive_path = request.archive.to_string_lossy().into_owned();
    report.refresh_digest();
    check_path_lengths(&request.output_dir, &report, PathStage::Preflight)?;
    *report_slot.borrow_mut() = Some(report.clone());
    budget.update(crate::budget::Usage {
        files: expected_tree(&report)?.len() as u64,
        bytes: 0,
    })?;
    context.push_event(TaskEventKind::PathMappingPlanned {
        report_id: report.digest.clone(),
        renamed_count: report.changed_count(),
    });
    session.verify_source().await?;
    let root = ControlledRoot::open(&request.output_dir)?;
    let verified = freeze_namespace(&root, &entries, &mut report, context.clone());
    *report_slot.borrow_mut() = Some(report.clone());
    verified?;
    report.tentative = false;
    let mut output_verified = false;
    let bulk_metadata_safe = manifest
        .entries
        .iter()
        .filter(|entry| entry.is_dir)
        .all(|entry| {
            entry
                .metadata
                .unix_mode
                .is_none_or(|mode| mode & 0o700 == 0o700)
        });
    let result =
        if report.changed_count() == 0 && session.supports_bulk_original() && bulk_metadata_safe {
            session.execute_bulk(context.clone()).await?
        } else if session.supports_managed() {
            let sink = Arc::new(OutputSink::new(
                root,
                &report,
                &manifest,
                budget,
                context.clone(),
            )?);
            let result = session.execute(sink.clone(), context.clone()).await;
            if let Some(error) = sink.failure() {
                return Err(error);
            }
            let result = result?;
            sink.finish()?;
            verify_output(&request.output_dir, &report, &manifest)?;
            output_verified = true;
            let sink = Arc::try_unwrap(sink).map_err(|_| SmartZipError::BackendProtocolError {
                backend: manifest.adapter_id.clone(),
                detail: "backend retained output sink after completion".into(),
            })?;
            *metadata_slot.borrow_mut() = Some(DeferredDirectoryMetadata {
                root: sink.root,
                directories: sink.directories,
                applied: false,
            });
            result
        } else {
            if report.changed_count() != 0 {
                let mut d = PathDiagnostic::new(
                    PathConstraintReason::PathRemapUnsupported,
                    PathStage::Preflight,
                    "selected backend cannot provide indexed content for the required mapping",
                );
                d.policy = Some(policy);
                return Err(d.error());
            }
            let result = backend
                .extract_with_facts_and_context(request.clone(), facts, context.clone())
                .await?;
            session.verify_source().await?;
            report.tentative = false;
            result
        };
    if !output_verified {
        verify_output(&request.output_dir, &report, &manifest)?;
    }
    *report_slot.borrow_mut() = Some(report);
    Ok(result)
}

fn original_node_for(report: &PathMappingReport, mapped: &str) -> Option<String> {
    for entry in &report.entries {
        if entry.is_dir && entry.staging_relative.is_empty() {
            continue;
        }
        let output: Vec<_> = entry.staging_relative.split('/').collect();
        let original = crate::path_policy::logical_components(&entry.source).ok()?;
        for depth in 1..=output.len() {
            if output[..depth].join("/") == mapped {
                return Some(original[..depth].join("/"));
            }
        }
    }
    None
}

fn freeze_namespace(
    root: &ControlledRoot,
    entries: &[PathEntry],
    report: &mut PathMappingReport,
    context: Arc<TaskExecutionContext>,
) -> Result<()> {
    let initial_limit = report.policy.component.limit;
    let mut policy = report.policy.clone();
    let mut forced = BTreeSet::new();
    let mut length_retry = 0usize;
    let mut alias_retry = 0usize;
    loop {
        match root.verify_namespace_with_cancellation(report, || context.is_cancelled()) {
            Ok(()) => return Ok(()),
            Err(error) => {
                let SmartZipError::PathConstraint { diagnostic, .. } = &error else {
                    return Err(error);
                };
                let reason = diagnostic.reason;
                match reason {
                    PathConstraintReason::NameTooLong if length_retry < 4 => {
                        // Only platform ENAMETOOLONG evidence authorizes shortening.
                        let confirmed = if cfg!(unix) {
                            diagnostic.os_error_code == Some(libc::ENAMETOOLONG)
                        } else {
                            diagnostic.os_error_code == Some(206)
                        };
                        if !confirmed {
                            return Err(error);
                        }
                        policy.component.limit = [
                            initial_limit.saturating_mul(3) / 4,
                            initial_limit / 2,
                            initial_limit / 4,
                            16,
                        ][length_retry];
                        policy.component.source = "exclusive creation ENAMETOOLONG fallback".into();
                        policy.component.confidence = PolicyConfidence::Conservative;
                        length_retry += 1;
                    }
                    PathConstraintReason::NameCollision if alias_retry < 4 => {
                        let Some(a) = diagnostic
                            .candidate
                            .as_deref()
                            .and_then(|p| original_node_for(report, p))
                        else {
                            return Err(error);
                        };
                        let Some(b) = diagnostic
                            .actual_mapping
                            .as_deref()
                            .and_then(|p| original_node_for(report, p))
                        else {
                            return Err(error);
                        };
                        forced.insert(a);
                        forced.insert(b);
                        alias_retry += 1;
                    }
                    _ => return Err(error),
                }
                root.clear_verified_namespace()?;
                let mut replanned = plan_paths_forced(entries, &policy, &forced)?;
                replanned.manifest_digest = report.manifest_digest.clone();
                replanned.archive_identity = report.archive_identity.clone();
                replanned.adapter_id = report.adapter_id.clone();
                replanned.archive_path = report.archive_path.clone();
                for entry in &mut replanned.entries {
                    if report
                        .entries
                        .iter()
                        .find(|old| old.id == entry.id)
                        .is_some_and(|old| old.staging_relative != entry.staging_relative)
                    {
                        entry.reasons.push(PathMappingReason::CreationFallback);
                    }
                }
                replanned.refresh_digest();
                context.push_event(TaskEventKind::PathMappingFallback {
                    report_id: replanned.digest.clone(),
                    reason: reason.as_str().into(),
                });
                *report = replanned;
            }
        }
    }
}

pub(crate) fn check_path_lengths(
    root: &Path,
    report: &PathMappingReport,
    stage: PathStage,
) -> Result<()> {
    let Some(limit) = report.policy.full_path_limit else {
        return Ok(());
    };
    for entry in &report.entries {
        let path = root.join(&entry.staging_relative);
        let value = path.to_string_lossy();
        let measured = if cfg!(windows) {
            value.encode_utf16().count() + 16
        } else {
            value.len() + 1
        };
        if measured > limit {
            let mut d = PathDiagnostic::new(
                PathConstraintReason::PathTooLong,
                stage,
                "complete extraction/scan/recovery path exceeds the supported API budget",
            );
            d.scope = "path".into();
            d.entry_id = Some(entry.id);
            d.display_name = Some(entry.display_name.clone());
            d.policy = Some(report.policy.clone());
            d.measured = Some(measured);
            d.limit = Some(limit);
            d.candidate = Some(value.into_owned());
            return Err(d.error());
        }
    }
    Ok(())
}

/// Keep the tracked root alive until all scans and layout checks have finished.
pub(crate) struct DeferredDirectoryMetadata {
    root: ControlledRoot,
    directories: Vec<(String, Option<u32>, Option<i64>)>,
    applied: bool,
}
impl std::fmt::Debug for DeferredDirectoryMetadata {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeferredDirectoryMetadata")
            .field("directories", &self.directories.len())
            .field("applied", &self.applied)
            .finish()
    }
}
impl DeferredDirectoryMetadata {
    pub(crate) fn apply_staging(&mut self) -> Result<()> {
        self.applied = true;
        for (path, mode, time) in &self.directories {
            self.root
                .apply_metadata(path, mode.map(|mode| mode | 0o700), *time)?;
        }
        Ok(())
    }
    pub(crate) fn apply_final_permissions(&mut self) -> Result<()> {
        for (path, mode, _) in &self.directories {
            self.root.apply_metadata(path, *mode, None)?;
        }
        Ok(())
    }
    pub(crate) fn restore(&mut self) -> Result<()> {
        if !self.applied {
            return Ok(());
        }
        self.root.restore_directory_access("")?;
        for (path, _, _) in self.directories.iter().rev() {
            if !path.is_empty() {
                self.root.restore_directory_access(path)?;
            }
        }
        self.applied = false;
        Ok(())
    }
    pub(crate) fn disarm(&mut self) {
        self.applied = false;
    }
}
impl Drop for DeferredDirectoryMetadata {
    fn drop(&mut self) {
        if let Err(error) = self.restore() {
            eprintln!("SmartZip staging access restoration failed: {error}");
        }
    }
}

struct SinkState {
    opened: BTreeSet<u64>,
    completed: BTreeSet<u64>,
    bytes: u64,
    failure: Option<SmartZipError>,
}
struct OutputSink {
    root: ControlledRoot,
    paths: BTreeMap<u64, (String, u64, Option<u32>, Option<i64>)>,
    state: Arc<Mutex<SinkState>>,
    budget: Arc<crate::budget::WriterBudget>,
    entry_count: u64,
    context: Arc<TaskExecutionContext>,
    directories: Vec<(String, Option<u32>, Option<i64>)>,
}
impl OutputSink {
    fn new(
        root: ControlledRoot,
        report: &PathMappingReport,
        manifest: &ExtractionManifest,
        budget: crate::budget::WriterBudget,
        context: Arc<TaskExecutionContext>,
    ) -> Result<Self> {
        let metadata: BTreeMap<_, _> = manifest.entries.iter().map(|e| (e.id, e)).collect();
        let paths = report
            .entries
            .iter()
            .filter(|e| !e.is_dir)
            .map(|e| {
                let m = metadata[&e.id];
                (
                    e.id,
                    (
                        e.staging_relative.clone(),
                        m.size,
                        m.metadata.unix_mode,
                        m.metadata.modified_unix_seconds,
                    ),
                )
            })
            .collect();
        let entry_count = expected_tree(report)?.len() as u64;
        budget.update(crate::budget::Usage {
            files: entry_count,
            bytes: 0,
        })?;
        let mut directories: Vec<_> = report
            .entries
            .iter()
            .filter(|e| e.is_dir)
            .map(|e| {
                let m = metadata[&e.id];
                (
                    e.staging_relative.clone(),
                    m.metadata.unix_mode,
                    m.metadata.modified_unix_seconds,
                )
            })
            .collect();
        directories.sort_by(|a, b| {
            b.0.split('/')
                .count()
                .cmp(&a.0.split('/').count())
                .then(b.0.cmp(&a.0))
        });
        directories.dedup_by(|a, b| a.0 == b.0);
        Ok(Self {
            root,
            paths,
            state: Arc::new(Mutex::new(SinkState {
                opened: BTreeSet::new(),
                completed: BTreeSet::new(),
                bytes: 0,
                failure: None,
            })),
            budget: Arc::new(budget),
            entry_count,
            context,
            directories,
        })
    }
    fn failure(&self) -> Option<SmartZipError> {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .failure
            .take()
    }
    fn finish(&self) -> Result<()> {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.completed.len() != self.paths.len() {
            return Err(SmartZipError::BackendProtocolError {
                backend: "managed-writer".into(),
                detail: "not every manifest file completed and closed".into(),
            });
        }
        Ok(())
    }
}
impl ManagedSink for OutputSink {
    fn open_file(&self, id: u64) -> Result<Box<dyn Write + Send>> {
        let (path, expected_size, mode, modified) =
            self.paths
                .get(&id)
                .ok_or_else(|| SmartZipError::BackendProtocolError {
                    backend: "managed-writer".into(),
                    detail: format!("unknown or directory entry id {id}"),
                })?;
        if !self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .opened
            .insert(id)
        {
            return Err(PathDiagnostic::new(
                PathConstraintReason::NameCollision,
                PathStage::Create,
                "backend opened the same member twice",
            )
            .error());
        }
        let file = self.root.create_file(path)?;
        Ok(Box::new(OutputWriter {
            file,
            id,
            written: 0,
            expected_size: *expected_size,
            mode: *mode,
            modified: *modified,
            state: self.state.clone(),
            budget: self.budget.clone(),
            entry_count: self.entry_count,
            context: self.context.clone(),
        }))
    }
}
struct OutputWriter {
    file: std::fs::File,
    id: u64,
    written: u64,
    expected_size: u64,
    mode: Option<u32>,
    modified: Option<i64>,
    state: Arc<Mutex<SinkState>>,
    budget: Arc<crate::budget::WriterBudget>,
    entry_count: u64,
    context: Arc<TaskExecutionContext>,
}
impl Write for OutputWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.context.is_cancelled() {
            self.state.lock().unwrap_or_else(|e| e.into_inner()).failure =
                Some(SmartZipError::Cancelled);
            // write_all retries Interrupted, so cancellation must be terminal.
            return Err(io::Error::other("extraction cancelled"));
        }
        if self.written.saturating_add(bytes.len() as u64) > self.expected_size {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "member produced more bytes than the manifest",
            ));
        }
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let bytes_after = state.bytes.saturating_add(bytes.len() as u64);
        if let Err(error) = self.budget.update(crate::budget::Usage {
            files: self.entry_count,
            bytes: bytes_after,
        }) {
            let detail = error.to_string();
            state.failure = Some(error);
            return Err(io::Error::other(detail));
        }
        let n = self.file.write(bytes)?;
        self.written += n as u64;
        state.bytes += n as u64;
        Ok(n)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}
impl Drop for OutputWriter {
    fn drop(&mut self) {
        if self.written != self.expected_size {
            return;
        }
        let result = smartzip_platform::path_policy::apply_file_metadata(
            &self.file,
            self.mode,
            self.modified,
        )
        .and_then(|_| self.file.sync_all().map_err(|e| SmartZipError::io(None, e)));
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        match result {
            Ok(()) => {
                state.completed.insert(self.id);
            }
            Err(error) => {
                state.failure = Some(error);
            }
        }
    }
}
pub(crate) fn expected_tree(report: &PathMappingReport) -> Result<BTreeMap<String, bool>> {
    let mut tree = BTreeMap::new();
    for e in &report.entries {
        if e.is_dir && e.staging_relative.is_empty() {
            continue;
        }
        let parts = crate::path_policy::logical_components(&e.staging_relative)?;
        for n in 1..parts.len() {
            tree.insert(parts[..n].join("/"), true);
        }
        tree.insert(e.staging_relative.clone(), e.is_dir);
    }
    Ok(tree)
}
fn output_key(path: &str, policy: &TargetPathPolicy) -> String {
    let name = if policy.comparison.normalization_sensitive {
        path.into()
    } else {
        path.nfc().collect::<String>()
    };
    if policy.comparison.case_sensitive {
        name
    } else {
        name.to_lowercase()
    }
}
fn verify_output(
    root: &Path,
    report: &PathMappingReport,
    manifest: &ExtractionManifest,
) -> Result<()> {
    let mut expected: BTreeMap<_, _> = expected_tree(report)?
        .into_iter()
        .map(|(p, k)| (output_key(&p, &report.policy), k))
        .collect();
    let sizes: BTreeMap<_, _> = manifest
        .entries
        .iter()
        .filter(|e| !e.is_dir)
        .map(|e| (e.id, e.size))
        .collect();
    let file_sizes: BTreeMap<_, _> = report
        .entries
        .iter()
        .filter(|e| !e.is_dir)
        .map(|e| {
            (
                output_key(&e.staging_relative, &report.policy),
                sizes[&e.id],
            )
        })
        .collect();
    for entry in walkdir::WalkDir::new(root).follow_links(false).min_depth(1) {
        let entry = entry.map_err(|e| SmartZipError::io(Some(root.into()), io::Error::other(e)))?;
        let relative = entry
            .path()
            .strip_prefix(root)
            .map_err(|e| SmartZipError::io(Some(root.into()), io::Error::other(e)))?
            .to_str()
            .ok_or_else(|| {
                PathDiagnostic::new(
                    PathConstraintReason::InvalidName,
                    PathStage::Create,
                    "backend created non-UTF8 output",
                )
                .error()
            })?
            .replace('\\', "/");
        let key = output_key(&relative, &report.policy);
        if expected.remove(&key) != Some(entry.file_type().is_dir())
            || entry.file_type().is_symlink()
        {
            return Err(PathDiagnostic::new(
                PathConstraintReason::NameCollision,
                PathStage::Create,
                format!("unexpected output object: {relative}"),
            )
            .error());
        }
        if entry.file_type().is_file() {
            let actual = entry
                .metadata()
                .map_err(|e| SmartZipError::io(Some(entry.path().into()), io::Error::other(e)))?
                .len();
            if Some(&actual) != file_sizes.get(&key) {
                return Err(SmartZipError::BackendProtocolError {
                    backend: manifest.adapter_id.clone(),
                    detail: format!("member {relative} size differs from complete manifest"),
                });
            }
        }
    }
    if !expected.is_empty() {
        return Err(SmartZipError::BackendProtocolError {
            backend: manifest.adapter_id.clone(),
            detail: "backend omitted manifest entries".into(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancelled_writer_returns_terminal_error_and_preserves_cancellation() {
        let root = tempfile::tempdir().unwrap();
        let file = std::fs::File::create(root.path().join("member")).unwrap();
        let token = tokio_util::sync::CancellationToken::new();
        token.cancel();
        let context = Arc::new(TaskExecutionContext::detached().with_cancellation(token));
        let state = Arc::new(Mutex::new(SinkState {
            opened: BTreeSet::new(),
            completed: BTreeSet::new(),
            bytes: 0,
            failure: None,
        }));
        let budget = Arc::new(crate::budget::TaskBudget::default());
        let reservation = budget.reserve_attempt();
        let mut writer = OutputWriter {
            file,
            id: 0,
            written: 0,
            expected_size: 1,
            mode: None,
            modified: None,
            state: state.clone(),
            budget: Arc::new(
                reservation.writer_budget(&crate::budget::ExtractionLimits::default()),
            ),
            entry_count: 1,
            context,
        };
        let error = writer.write(b"x").unwrap_err();
        assert_ne!(error.kind(), io::ErrorKind::Interrupted);
        assert_eq!(writer.written, 0);
        assert!(matches!(
            state.lock().unwrap().failure,
            Some(SmartZipError::Cancelled)
        ));
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn commit_failure_restores_restrictive_staging_access_before_cleanup() {
        use crate::layout::{OutputLayoutPolicy, SingleRootNamePolicy};
        use crate::materialize::{CommitPolicy, MaterializeRequest, OutputMaterializer};
        let workspace = tempfile::tempdir().unwrap();
        let output = workspace.path().join("archive");
        std::fs::create_dir(&output).unwrap();
        std::fs::write(output.join("keep"), b"old user data").unwrap();
        let metadata = std::cell::RefCell::new(None);
        let staged = OutputMaterializer::new(false)
            .prepare(
                MaterializeRequest {
                    output_dir: output.clone(),
                    archive_path: workspace.path().join("archive.zip"),
                    archive_stem: Some("archive".into()),
                    commit_policy: CommitPolicy::Overwrite,
                    layout_policy: OutputLayoutPolicy::Raw,
                    single_root_name_policy: SingleRootNamePolicy::Auto,
                },
                |path| {
                    let metadata = &metadata;
                    async move {
                        let root = ControlledRoot::open(&path)?;
                        root.create_dir("private")?;
                        root.create_file("private/file")?
                            .write_all(b"new content")
                            .unwrap();
                        *metadata.borrow_mut() = Some(DeferredDirectoryMetadata {
                            root,
                            directories: vec![
                                ("private".into(), Some(0o400), None),
                                ("".into(), Some(0), None),
                            ],
                            applied: false,
                        });
                        Ok(())
                    }
                },
            )
            .await
            .unwrap()
            .with_directory_metadata(metadata.borrow_mut().take());
        let prepared = staged
            .prepare_commit(None, crate::CommitSuccessFacts::default())
            .unwrap();
        let intent = prepared.intent().unwrap().clone();
        std::fs::create_dir(intent.backup_path.as_ref().unwrap()).unwrap();
        assert!(prepared.commit_with_recovery(false).is_err());
        assert!(!intent.staging_path.exists());
        assert_eq!(
            std::fs::read(output.join("keep")).unwrap(),
            b"old user data"
        );
    }
}
