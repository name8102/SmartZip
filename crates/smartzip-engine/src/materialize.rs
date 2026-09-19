use crate::layout::{
    LayoutPlan, LayoutPlanKind, LayoutRequest, OutputLayoutPolicy, PlanSource, SingleRootNamePolicy,
};
use smartzip_core::{Result, SmartZipError};
use std::future::Future;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitPolicy {
    FailIfExists,
    Overwrite,
    Rename,
}

/// Strategy for handling output path collisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CollisionAction {
    Skip,
    Overwrite,
    Rename,
}

#[derive(Debug, Clone)]
pub(crate) struct CollisionRequest {
    pub(crate) archive_path: PathBuf,
    pub(crate) target_path: PathBuf,
    version: TargetVersion,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TargetVersion {
    len: u64,
    modified: Option<std::time::SystemTime>,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
}

impl TargetVersion {
    fn read(path: &Path) -> std::io::Result<Self> {
        let metadata = std::fs::symlink_metadata(path)?;
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;
        Ok(Self {
            len: metadata.len(),
            modified: metadata.modified().ok(),
            #[cfg(unix)]
            device: metadata.dev(),
            #[cfg(unix)]
            inode: metadata.ino(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaterializeRequest {
    pub output_dir: PathBuf,
    pub archive_path: PathBuf,
    pub commit_policy: CommitPolicy,
    pub archive_stem: Option<String>,
    pub layout_policy: OutputLayoutPolicy,
    pub single_root_name_policy: SingleRootNamePolicy,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MaterializeResult {
    pub output_dir: PathBuf,
    pub layout_plan: LayoutPlan,
}

#[derive(Debug)]
pub struct MaterializeFailure {
    pub error: SmartZipError,
    pub preserved_temp_dir: Option<PathBuf>,
    pub kind: MaterializeFailureKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaterializeFailureKind {
    ExtractFailed,
    CommitFailed,
    CollisionSkipped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutputMaterializer {
    preserve_temp_on_failure: bool,
}

/// A successful extraction whose output is still private and uncommitted.
/// The staging directory remains owned until commit or drop.
#[derive(Debug)]
pub(crate) struct StagedOutput {
    request: MaterializeRequest,
    temp: tempfile::TempDir,
    layout_plan: LayoutPlan,
    preserve_temp_on_failure: bool,
}

#[derive(Debug)]
pub(crate) struct PreparedCommit {
    request: MaterializeRequest,
    temp: tempfile::TempDir,
    layout_plan: LayoutPlan,
    commit_policy: CommitPolicy,
    intent: Option<crate::CommitIntent>,
    preserve_temp_on_failure: bool,
}

#[derive(Debug)]
pub(crate) struct PublishedOutput {
    result: MaterializeResult,
    intent: Option<crate::CommitIntent>,
}

impl OutputMaterializer {
    pub fn new(preserve_temp_on_failure: bool) -> Self {
        Self {
            preserve_temp_on_failure,
        }
    }

    pub(crate) async fn prepare<F, Fut>(
        &self,
        request: MaterializeRequest,
        extract_into: F,
    ) -> std::result::Result<StagedOutput, MaterializeFailure>
    where
        F: FnOnce(PathBuf) -> Fut,
        Fut: Future<Output = Result<()>>,
    {
        let parent = request
            .output_dir
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        std::fs::create_dir_all(&parent).map_err(|source| MaterializeFailure {
            error: SmartZipError::io(Some(parent.clone()), source),
            preserved_temp_dir: None,
            kind: MaterializeFailureKind::CommitFailed,
        })?;

        let temp = tempfile::Builder::new()
            .prefix(".smartzip-")
            .tempdir_in(&parent)
            .map_err(|source| MaterializeFailure {
                error: SmartZipError::io(Some(parent.clone()), source),
                preserved_temp_dir: None,
                kind: MaterializeFailureKind::CommitFailed,
            })?;
        let temp_path = temp.path().to_path_buf();

        if let Err(error) = extract_into(temp_path.clone()).await {
            if self.preserve_temp_on_failure {
                let preserved = temp.keep();
                return Err(MaterializeFailure {
                    error,
                    preserved_temp_dir: Some(preserved),
                    kind: MaterializeFailureKind::ExtractFailed,
                });
            }
            let preserved_temp_dir = cleanup_staging(temp);
            let error = match &preserved_temp_dir {
                Some(path) => SmartZipError::io(
                    Some(path.clone()),
                    std::io::Error::other(format!("{error}; temporary output cleanup failed")),
                ),
                None => error,
            };
            return Err(MaterializeFailure {
                error,
                preserved_temp_dir,
                kind: MaterializeFailureKind::ExtractFailed,
            });
        }

        let archive_stem = request
            .archive_stem
            .clone()
            .unwrap_or_else(|| crate::name_score::archive_display_stem(&request.output_dir));

        let shape = crate::layout::scan_visible_top_level(&temp_path);
        let layout_plan = crate::layout::plan_layout(&LayoutRequest {
            shape,
            archive_path: request.output_dir.clone(),
            archive_stem,
            output_root: parent.clone(),
            layout_policy: request.layout_policy,
            single_root_name_policy: request.single_root_name_policy,
        });

        Ok(StagedOutput {
            request,
            temp,
            layout_plan,
            preserve_temp_on_failure: self.preserve_temp_on_failure,
        })
    }
}

impl StagedOutput {
    pub(crate) fn collision_request(
        &self,
    ) -> std::result::Result<Option<CollisionRequest>, MaterializeFailure> {
        if self.request.commit_policy != CommitPolicy::FailIfExists {
            return Ok(None);
        }
        match TargetVersion::read(&self.layout_plan.target) {
            Ok(version) => Ok(Some(CollisionRequest {
                archive_path: self.request.archive_path.clone(),
                target_path: self.layout_plan.target.clone(),
                version,
            })),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
            Err(error) => Err(commit_failure(SmartZipError::io(
                Some(self.layout_plan.target.clone()),
                error,
            ))),
        }
    }

    pub(crate) fn prepare_commit(
        self,
        collision_decision: Option<(CollisionRequest, CollisionAction)>,
        success: crate::CommitSuccessFacts,
    ) -> std::result::Result<PreparedCommit, MaterializeFailure> {
        let Self {
            request,
            temp,
            layout_plan,
            preserve_temp_on_failure,
        } = self;

        // Empty extraction has no filesystem publication to reconcile.
        if matches!(layout_plan.kind, LayoutPlanKind::Empty) {
            return Ok(PreparedCommit {
                request,
                temp,
                layout_plan,
                commit_policy: CommitPolicy::FailIfExists,
                intent: None,
                preserve_temp_on_failure,
            });
        }

        let mut commit_policy = request.commit_policy;
        if path_present(&layout_plan.target) && commit_policy == CommitPolicy::FailIfExists {
            if let Some((decision, action)) = collision_decision {
                let current_version =
                    TargetVersion::read(&layout_plan.target).map_err(|error| {
                        commit_failure(SmartZipError::io(Some(layout_plan.target.clone()), error))
                    })?;
                if decision.target_path != layout_plan.target || current_version != decision.version
                {
                    return Err(commit_failure(SmartZipError::io(
                        Some(layout_plan.target),
                        std::io::Error::new(
                            ErrorKind::AlreadyExists,
                            "output changed after decision",
                        ),
                    )));
                }
                match action {
                    CollisionAction::Skip => {
                        let _ = std::fs::remove_dir_all(temp.path());
                        return Err(MaterializeFailure {
                            error: SmartZipError::io(
                                Some(layout_plan.target.clone()),
                                std::io::Error::new(
                                    ErrorKind::AlreadyExists,
                                    format!(
                                        "output path already exists: {}",
                                        layout_plan.target.display()
                                    ),
                                ),
                            ),
                            preserved_temp_dir: None,
                            kind: MaterializeFailureKind::CollisionSkipped,
                        });
                    }
                    CollisionAction::Overwrite => {
                        commit_policy = CommitPolicy::Overwrite;
                    }
                    CollisionAction::Rename => {
                        commit_policy = CommitPolicy::Rename;
                    }
                }
            } else {
                let _ = std::fs::remove_dir_all(temp.path());
                return Err(MaterializeFailure {
                    error: SmartZipError::io(
                        Some(layout_plan.target.clone()),
                        std::io::Error::new(
                            ErrorKind::AlreadyExists,
                            format!(
                                "output path already exists: {}",
                                layout_plan.target.display()
                            ),
                        ),
                    ),
                    preserved_temp_dir: None,
                    kind: MaterializeFailureKind::CommitFailed,
                });
            }
        }

        // Every layout is already a complete file or directory inside staging.
        // Never construct a partially populated final directory.
        let source = match (&layout_plan.kind, &layout_plan.source) {
            (LayoutPlanKind::PreserveBothSingleDir | LayoutPlanKind::PreserveBothSingleFile, _) => {
                temp.path()
            }
            (_, PlanSource::WholeTempDir) => temp.path(),
            (
                _,
                PlanSource::SingleDir(path)
                | PlanSource::SingleDirContents(path)
                | PlanSource::SingleFile(path),
            ) => path,
        };
        let commit_target =
            resolve_commit_target(&layout_plan.target, commit_policy).map_err(commit_failure)?;
        let commit_id = smartzip_core::AttemptId::new().to_string();
        let parent = commit_target
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();
        let target_before = match crate::ArtifactIdentity::capture(&commit_target) {
            Ok(identity) => Some(identity),
            Err(error) if error.kind() == ErrorKind::NotFound => None,
            Err(error) => {
                return Err(commit_failure(SmartZipError::io(
                    Some(commit_target.clone()),
                    error,
                )))
            }
        };
        let backup_path = (commit_policy == CommitPolicy::Overwrite && target_before.is_some())
            .then(|| parent.join(format!(".smartzip-backup-{commit_id}")));
        let intent = crate::CommitIntent {
            commit_id: commit_id.clone(),
            staging_path: temp.path().to_path_buf(),
            source_path: source.to_path_buf(),
            target_path: commit_target,
            marker_path: parent.join(format!(".smartzip-commit-{commit_id}")),
            backup_path,
            source_identity: crate::ArtifactIdentity::capture(source).map_err(|error| {
                commit_failure(SmartZipError::io(Some(source.to_path_buf()), error))
            })?,
            target_before,
            output_files: 0,
            output_bytes: 0,
            success,
        };
        Ok(PreparedCommit {
            request,
            temp,
            layout_plan,
            commit_policy,
            intent: Some(intent),
            preserve_temp_on_failure,
        })
    }

    #[cfg(test)]
    fn commit(
        self,
        collision_decision: Option<(CollisionRequest, CollisionAction)>,
    ) -> std::result::Result<MaterializeResult, MaterializeFailure> {
        self.prepare_commit(collision_decision, crate::CommitSuccessFacts::default())?
            .commit()
            .map(PublishedOutput::finalize)
    }
}

impl PreparedCommit {
    pub(crate) fn set_output_usage(&mut self, usage: crate::budget::Usage) {
        if let Some(intent) = &mut self.intent {
            intent.output_files = usage.files;
            intent.output_bytes = usage.bytes;
        }
    }

    pub(crate) fn intent(&self) -> Option<&crate::CommitIntent> {
        self.intent.as_ref()
    }

    pub(crate) fn commit(self) -> std::result::Result<PublishedOutput, MaterializeFailure> {
        let Self {
            request,
            temp,
            layout_plan,
            commit_policy,
            intent,
            preserve_temp_on_failure,
        } = self;
        let Some(intent) = intent else {
            let _ = std::fs::remove_dir_all(temp.path());
            return Ok(PublishedOutput {
                result: MaterializeResult {
                    output_dir: request.output_dir,
                    layout_plan,
                },
                intent: None,
            });
        };

        let marker_result = (|| -> std::io::Result<()> {
            use std::io::Write;
            let mut marker = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&intent.marker_path)?;
            marker.write_all(intent.commit_id.as_bytes())?;
            marker.sync_all()
        })();
        if let Err(error) = marker_result {
            return Err(commit_failure(SmartZipError::io(
                Some(intent.marker_path.clone()),
                error,
            )));
        }

        let mut layout_plan = layout_plan.clone();
        match commit_output_recoverable(&intent, commit_policy) {
            Ok(residual_backup) => {
                if let Some(path) = cleanup_staging(temp) {
                    layout_plan
                        .warnings
                        .push(format!("temporary output retained at {}", path.display()));
                }
                if let Some(path) = residual_backup {
                    layout_plan
                        .warnings
                        .push(format!("old output backup retained at {}", path.display()));
                }
                Ok(PublishedOutput {
                    result: MaterializeResult {
                        output_dir: intent.target_path.clone(),
                        layout_plan,
                    },
                    intent: Some(intent),
                })
            }
            Err(mut failure) => {
                if failure.preserved_temp_dir.is_some() {
                    let staging = temp.keep();
                    failure.error = SmartZipError::io(
                        Some(staging.clone()),
                        std::io::Error::other(format!(
                            "{}; commit artifacts retained for recovery",
                            failure.error
                        )),
                    );
                    failure.preserved_temp_dir = Some(staging);
                    return Err(failure);
                }
                let _ = std::fs::remove_file(&intent.marker_path);
                if preserve_temp_on_failure && failure.preserved_temp_dir.is_none() {
                    failure.preserved_temp_dir = Some(temp.keep());
                } else if let Some(path) = cleanup_staging(temp) {
                    if failure.preserved_temp_dir.is_none() {
                        failure.preserved_temp_dir = Some(path);
                    } else {
                        failure.error = SmartZipError::io(
                            Some(path),
                            std::io::Error::other(format!(
                                "{}; temporary output cleanup also failed",
                                failure.error
                            )),
                        );
                    }
                }
                Err(failure)
            }
        }
    }
}

impl PublishedOutput {
    pub(crate) fn intent(&self) -> Option<&crate::CommitIntent> {
        self.intent.as_ref()
    }

    pub(crate) fn finalize(mut self) -> MaterializeResult {
        if let Some(intent) = &self.intent {
            if let Some(backup) = &intent.backup_path {
                if path_present(backup) && std::fs::remove_dir_all(backup).is_err() {
                    self.result.layout_plan.warnings.push(format!(
                        "old output backup retained at {}",
                        backup.display()
                    ));
                }
            }
            if path_present(&intent.marker_path)
                && std::fs::remove_file(&intent.marker_path).is_err()
            {
                self.result.layout_plan.warnings.push(format!(
                    "commit marker retained at {}",
                    intent.marker_path.display()
                ));
            }
        }
        self.result
    }
}

fn cleanup_staging(temp: tempfile::TempDir) -> Option<PathBuf> {
    let path = temp.path().to_path_buf();
    if !path_present(&path) {
        return None;
    }
    temp.close().err().map(|_| path)
}

impl Default for OutputMaterializer {
    fn default() -> Self {
        Self::new(false)
    }
}

fn resolve_commit_target(output_dir: &Path, policy: CommitPolicy) -> Result<PathBuf> {
    match policy {
        CommitPolicy::FailIfExists if path_present(output_dir) => Err(SmartZipError::io(
            Some(output_dir.to_path_buf()),
            std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                format!("output path already exists: {}", output_dir.display()),
            ),
        )),
        CommitPolicy::FailIfExists | CommitPolicy::Overwrite => Ok(output_dir.to_path_buf()),
        CommitPolicy::Rename => {
            let parent = output_dir.parent().unwrap_or_else(|| Path::new("."));
            let file_name = output_dir
                .file_name()
                .unwrap_or_else(|| std::ffi::OsStr::new("archive"));
            Ok(find_non_colliding_name(parent, file_name))
        }
    }
}

fn commit_failure(error: SmartZipError) -> MaterializeFailure {
    MaterializeFailure {
        error,
        preserved_temp_dir: None,
        kind: MaterializeFailureKind::CommitFailed,
    }
}

fn path_present(path: &Path) -> bool {
    // Includes dangling links; other errors must fail closed during rename.
    std::fs::symlink_metadata(path).is_ok()
}

#[cfg(test)]
fn commit_output(
    source: &Path,
    target: &Path,
    policy: CommitPolicy,
    mut rename: impl FnMut(&Path, &Path) -> std::io::Result<()>,
) -> std::result::Result<Option<PathBuf>, MaterializeFailure> {
    let parent = target.parent().unwrap_or_else(|| Path::new("."));
    let mut backup = None;
    if policy == CommitPolicy::Overwrite && path_present(target) {
        let dir = tempfile::Builder::new()
            .prefix(".smartzip-backup-")
            .tempdir_in(parent)
            .map_err(|e| commit_failure(SmartZipError::io(Some(parent.into()), e)))?;
        let old = dir.path().join("original");
        rename(target, &old)
            .map_err(|e| commit_failure(SmartZipError::io(Some(target.into()), e)))?;
        backup = Some(dir);
    }
    if let Err(error) = rename(source, target) {
        if let Some(dir) = backup {
            if let Err(restore_error) = rename(&dir.path().join("original"), target) {
                // A concurrent target must never be overwritten by rollback.
                let retained = dir.keep();
                return Err(MaterializeFailure {
                    error: SmartZipError::io(Some(target.into()), std::io::Error::other(format!(
                        "commit failed: {error}; restore failed: {restore_error}; old output retained at {}",
                        retained.join("original").display()
                    ))),
                    preserved_temp_dir: Some(retained), kind: MaterializeFailureKind::CommitFailed,
                });
            }
        }
        return Err(commit_failure(SmartZipError::io(
            Some(target.into()),
            error,
        )));
    }
    // Cleanup failure cannot undo a successful commit. Retain and report the
    // recovery directory instead of silently leaking it.
    if let Some(dir) = backup {
        let path = dir.keep();
        if std::fs::remove_dir_all(&path).is_err() {
            return Ok(Some(path));
        }
    }
    Ok(None)
}

fn commit_output_recoverable(
    intent: &crate::CommitIntent,
    policy: CommitPolicy,
) -> std::result::Result<Option<PathBuf>, MaterializeFailure> {
    let target_unchanged = match &intent.target_before {
        Some(identity) => identity
            .matches_version(&intent.target_path)
            .map_err(|error| {
                commit_failure(SmartZipError::io(Some(intent.target_path.clone()), error))
            })?,
        None => matches!(
            std::fs::symlink_metadata(&intent.target_path),
            Err(error) if error.kind() == ErrorKind::NotFound
        ),
    };
    if !target_unchanged {
        return Err(commit_failure(SmartZipError::io(
            Some(intent.target_path.clone()),
            std::io::Error::new(ErrorKind::AlreadyExists, "output changed before commit"),
        )));
    }
    let mut backup = None;
    if let (CommitPolicy::Overwrite, Some(expected_target)) =
        (policy, intent.target_before.as_ref())
    {
        let backup_path = intent.backup_path.as_ref().ok_or_else(|| {
            commit_failure(SmartZipError::io(
                Some(intent.target_path.clone()),
                std::io::Error::other("overwrite commit has no backup path"),
            ))
        })?;
        std::fs::create_dir(backup_path)
            .map_err(|error| commit_failure(SmartZipError::io(Some(backup_path.clone()), error)))?;
        let old = backup_path.join("original");
        if let Err(error) = rename_no_replace(&intent.target_path, &old) {
            let _ = std::fs::remove_dir(backup_path);
            return Err(commit_failure(SmartZipError::io(
                Some(intent.target_path.clone()),
                error,
            )));
        }
        let moved_expected_target = expected_target.matches_version(&old);
        if !matches!(moved_expected_target, Ok(true)) {
            if let Err(restore_error) = rename_no_replace(&old, &intent.target_path) {
                return Err(MaterializeFailure {
                    error: SmartZipError::io(
                        Some(intent.target_path.clone()),
                        std::io::Error::other(format!(
                            "output changed during commit; restore failed: {restore_error}; changed output retained at {}",
                            old.display()
                        )),
                    ),
                    preserved_temp_dir: Some(backup_path.clone()),
                    kind: MaterializeFailureKind::CommitFailed,
                });
            }
            let _ = std::fs::remove_dir(backup_path);
            let error = match moved_expected_target {
                Ok(false) => {
                    std::io::Error::new(ErrorKind::AlreadyExists, "output changed during commit")
                }
                Err(error) => error,
                Ok(true) => unreachable!(),
            };
            return Err(commit_failure(SmartZipError::io(
                Some(intent.target_path.clone()),
                error,
            )));
        }
        backup = Some(backup_path.clone());
    }

    if let Err(error) = rename_no_replace(&intent.source_path, &intent.target_path) {
        if let Some(backup_path) = &backup {
            let old = backup_path.join("original");
            if let Err(restore_error) = rename_no_replace(&old, &intent.target_path) {
                return Err(MaterializeFailure {
                    error: SmartZipError::io(
                        Some(intent.target_path.clone()),
                        std::io::Error::other(format!(
                            "commit failed: {error}; restore failed: {restore_error}; old output retained at {}",
                            old.display()
                        )),
                    ),
                    preserved_temp_dir: Some(backup_path.clone()),
                    kind: MaterializeFailureKind::CommitFailed,
                });
            }
            let _ = std::fs::remove_dir(backup_path);
        }
        return Err(commit_failure(SmartZipError::io(
            Some(intent.target_path.clone()),
            error,
        )));
    }
    Ok(backup)
}

/// Atomically refuse an occupied destination, including a dangling symlink.
pub(crate) fn rename_no_replace(from: &Path, to: &Path) -> std::io::Result<()> {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        use std::os::unix::ffi::OsStrExt;
        let from = std::ffi::CString::new(from.as_os_str().as_bytes())?;
        let to = std::ffi::CString::new(to.as_os_str().as_bytes())?;
        // SAFETY: both C strings remain alive for the syscall; no borrowed
        // descriptor is closed. Unsupported filesystems fail without clobbering.
        #[cfg(target_os = "linux")]
        let result = unsafe {
            libc::renameat2(
                libc::AT_FDCWD,
                from.as_ptr(),
                libc::AT_FDCWD,
                to.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        };
        #[cfg(target_os = "macos")]
        let result = unsafe { libc::renamex_np(from.as_ptr(), to.as_ptr(), libc::RENAME_EXCL) };
        if result == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = (from, to);
        Err(std::io::Error::new(
            ErrorKind::Unsupported,
            "atomic no-replace commit requires Linux or macOS",
        ))
    }
}

fn find_non_colliding_name(parent: &Path, name: &std::ffi::OsStr) -> PathBuf {
    let base = parent.join(name);
    if !path_present(&base) {
        return base;
    }
    let name_str = name.to_string_lossy();
    for n in 1..1000u32 {
        let alt = parent.join(format!("{name_str}_collided_{n}"));
        if !path_present(&alt) {
            return alt;
        }
    }
    parent.join(format!("{name_str}_{}", std::process::id()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commit_failure_restores_old_tree() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("target");
        let source = root.path().join("new");
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("old"), b"old").unwrap();
        std::fs::create_dir(&source).unwrap();
        let mut step = 0;
        let result = commit_output(&source, &target, CommitPolicy::Overwrite, |a, b| {
            step += 1;
            if step == 2 {
                return Err(std::io::Error::new(
                    ErrorKind::PermissionDenied,
                    "injected commit failure",
                ));
            }
            rename_no_replace(a, b)
        });
        assert!(result.is_err());
        assert_eq!(std::fs::read(target.join("old")).unwrap(), b"old");
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 2);
    }

    #[test]
    fn concurrent_target_is_kept_and_old_backup_is_reported() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("target");
        let source = root.path().join("new");
        std::fs::write(&target, b"old").unwrap();
        std::fs::write(&source, b"new").unwrap();
        let mut step = 0;
        let failure = commit_output(&source, &target, CommitPolicy::Overwrite, |a, b| {
            step += 1;
            if step == 2 {
                std::fs::write(&target, b"concurrent").unwrap();
            }
            rename_no_replace(a, b)
        })
        .unwrap_err();
        assert_eq!(std::fs::read(&target).unwrap(), b"concurrent");
        assert_eq!(
            std::fs::read(failure.preserved_temp_dir.unwrap().join("original")).unwrap(),
            b"old"
        );
        assert_eq!(std::fs::read(source).unwrap(), b"new");
    }

    #[cfg(unix)]
    #[test]
    fn dangling_link_is_an_occupied_target() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("target");
        let source = root.path().join("new");
        std::os::unix::fs::symlink("missing", &target).unwrap();
        std::fs::write(&source, b"new").unwrap();
        assert!(commit_output(
            &source,
            &target,
            CommitPolicy::FailIfExists,
            rename_no_replace
        )
        .is_err());
        assert_eq!(
            std::fs::read_link(target).unwrap(),
            PathBuf::from("missing")
        );
    }

    #[tokio::test]
    async fn commits_temp_output_after_success() {
        let root = tempfile::tempdir().unwrap();
        let output = root.path().join("archive-d0");

        let result = OutputMaterializer::default()
            .prepare(
                MaterializeRequest {
                    output_dir: output.clone(),
                    archive_path: output.clone(),
                    commit_policy: CommitPolicy::FailIfExists,
                    archive_stem: None,
                    layout_policy: OutputLayoutPolicy::default(),
                    single_root_name_policy: SingleRootNamePolicy::default(),
                },
                |temp_dir| async move {
                    std::fs::write(temp_dir.join("hello.txt"), b"hello")
                        .map_err(|source| SmartZipError::io(Some(temp_dir), source))
                },
            )
            .await
            .unwrap()
            .commit(None)
            .unwrap();

        let plan = &result.layout_plan;
        assert_eq!(
            plan.kind,
            LayoutPlanKind::CommitWholeTempAsArchiveDir {
                name: "archive-d0".to_string()
            }
        );
        assert_eq!(result.output_dir, output);
        assert_eq!(std::fs::read(output.join("hello.txt")).unwrap(), b"hello");
    }

    #[tokio::test]
    async fn uncommitted_stage_keeps_output_private_and_cleans_on_drop() {
        let root = tempfile::tempdir().unwrap();
        let output = root.path().join("archive");
        let staged = OutputMaterializer::default()
            .prepare(
                MaterializeRequest {
                    output_dir: output.clone(),
                    archive_path: output.clone(),
                    commit_policy: CommitPolicy::FailIfExists,
                    archive_stem: None,
                    layout_policy: OutputLayoutPolicy::default(),
                    single_root_name_policy: SingleRootNamePolicy::default(),
                },
                |temp_dir| async move {
                    std::fs::write(temp_dir.join("file"), b"data")
                        .map_err(|source| SmartZipError::io(Some(temp_dir), source))
                },
            )
            .await
            .unwrap();
        let staging_path = staged.temp.path().to_path_buf();
        assert_eq!(std::fs::read(staging_path.join("file")).unwrap(), b"data");
        assert!(!output.exists());
        drop(staged);
        assert!(!staging_path.exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn collision_reply_cannot_overwrite_a_replaced_target() {
        let root = tempfile::tempdir().unwrap();
        let output = root.path().join("archive");
        std::fs::create_dir(&output).unwrap();
        let staged = OutputMaterializer::default()
            .prepare(
                MaterializeRequest {
                    output_dir: output.clone(),
                    archive_path: output.clone(),
                    commit_policy: CommitPolicy::FailIfExists,
                    archive_stem: None,
                    layout_policy: OutputLayoutPolicy::default(),
                    single_root_name_policy: SingleRootNamePolicy::default(),
                },
                |temp_dir| async move {
                    std::fs::write(temp_dir.join("file"), b"new")
                        .map_err(|source| SmartZipError::io(Some(temp_dir), source))
                },
            )
            .await
            .unwrap();
        let decision = staged.collision_request().unwrap().unwrap();
        std::fs::rename(&output, root.path().join("old-target")).unwrap();
        std::fs::create_dir(&output).unwrap();
        std::fs::write(output.join("other"), b"other").unwrap();
        let failure = staged
            .commit(Some((decision, CollisionAction::Overwrite)))
            .unwrap_err();
        assert_eq!(failure.kind, MaterializeFailureKind::CommitFailed);
        assert_eq!(std::fs::read(output.join("other")).unwrap(), b"other");
    }

    #[tokio::test]
    async fn prepared_overwrite_rechecks_target_before_rename() {
        let root = tempfile::tempdir().unwrap();
        let output = root.path().join("archive");
        std::fs::create_dir(&output).unwrap();
        std::fs::write(output.join("old"), b"old").unwrap();
        let staged = OutputMaterializer::default()
            .prepare(
                MaterializeRequest {
                    output_dir: output.clone(),
                    archive_path: output.clone(),
                    commit_policy: CommitPolicy::FailIfExists,
                    archive_stem: None,
                    layout_policy: OutputLayoutPolicy::default(),
                    single_root_name_policy: SingleRootNamePolicy::default(),
                },
                |temp_dir| async move {
                    std::fs::write(temp_dir.join("new-a"), b"new")
                        .map_err(|source| SmartZipError::io(Some(temp_dir.clone()), source))?;
                    std::fs::write(temp_dir.join("new-b"), b"new")
                        .map_err(|source| SmartZipError::io(Some(temp_dir), source))
                },
            )
            .await
            .unwrap();
        let decision = staged.collision_request().unwrap().unwrap();
        let prepared = staged
            .prepare_commit(
                Some((decision, CollisionAction::Overwrite)),
                crate::CommitSuccessFacts::default(),
            )
            .unwrap();

        std::fs::rename(&output, root.path().join("old-target")).unwrap();
        std::fs::create_dir(&output).unwrap();
        std::fs::write(output.join("concurrent"), b"concurrent").unwrap();

        let failure = prepared.commit().unwrap_err();
        assert_eq!(failure.kind, MaterializeFailureKind::CommitFailed);
        assert_eq!(
            std::fs::read(output.join("concurrent")).unwrap(),
            b"concurrent"
        );
    }

    #[tokio::test]
    async fn overwrite_removes_existing_output_only_after_success() {
        let root = tempfile::tempdir().unwrap();
        let output = root.path().join("archive-d0");
        std::fs::create_dir_all(&output).unwrap();
        std::fs::write(output.join("old.txt"), b"old").unwrap();

        let result = OutputMaterializer::default()
            .prepare(
                MaterializeRequest {
                    output_dir: output.clone(),
                    archive_path: output.clone(),
                    commit_policy: CommitPolicy::Overwrite,
                    archive_stem: None,
                    layout_policy: OutputLayoutPolicy::default(),
                    single_root_name_policy: SingleRootNamePolicy::default(),
                },
                |temp_dir| async move {
                    std::fs::write(temp_dir.join("new.txt"), b"new")
                        .map_err(|source| SmartZipError::io(Some(temp_dir.clone()), source))?;
                    std::fs::write(temp_dir.join("also.txt"), b"also")
                        .map_err(|source| SmartZipError::io(Some(temp_dir), source))
                },
            )
            .await
            .unwrap()
            .commit(None)
            .unwrap();

        let plan = &result.layout_plan;
        assert_eq!(
            plan.kind,
            LayoutPlanKind::CommitWholeTempAsArchiveDir {
                name: "archive-d0".to_string()
            }
        );
        assert!(!output.join("old.txt").exists());
        assert_eq!(std::fs::read(output.join("new.txt")).unwrap(), b"new");
        assert_eq!(std::fs::read(output.join("also.txt")).unwrap(), b"also");
    }

    #[tokio::test]
    async fn failed_overwrite_keeps_existing_output() {
        let root = tempfile::tempdir().unwrap();
        let output = root.path().join("archive-d0");
        std::fs::create_dir_all(&output).unwrap();
        std::fs::write(output.join("old.txt"), b"old").unwrap();

        let result = OutputMaterializer::default()
            .prepare(
                MaterializeRequest {
                    output_dir: output.clone(),
                    archive_path: output.clone(),
                    commit_policy: CommitPolicy::Overwrite,
                    archive_stem: None,
                    layout_policy: OutputLayoutPolicy::default(),
                    single_root_name_policy: SingleRootNamePolicy::default(),
                },
                |_temp_dir| async {
                    Err(SmartZipError::BackendFailed {
                        backend: "fake".into(),
                        exit_code: None,
                        stderr: "failed".into(),
                    })
                },
            )
            .await;

        assert!(result.is_err());
        assert_eq!(std::fs::read(output.join("old.txt")).unwrap(), b"old");
    }

    #[tokio::test]
    async fn development_mode_preserves_temp_output_after_failure() {
        let root = tempfile::tempdir().unwrap();
        let output = root.path().join("archive-d0");

        let result = OutputMaterializer::new(true)
            .prepare(
                MaterializeRequest {
                    output_dir: output.clone(),
                    archive_path: output.clone(),
                    commit_policy: CommitPolicy::FailIfExists,
                    archive_stem: None,
                    layout_policy: OutputLayoutPolicy::default(),
                    single_root_name_policy: SingleRootNamePolicy::default(),
                },
                |temp_dir| async move {
                    std::fs::write(temp_dir.join("partial.txt"), b"partial")
                        .map_err(|source| SmartZipError::io(Some(temp_dir.clone()), source))?;
                    Err(SmartZipError::BackendFailed {
                        backend: "fake".into(),
                        exit_code: None,
                        stderr: "failed".into(),
                    })
                },
            )
            .await
            .unwrap_err();

        let preserved = result.preserved_temp_dir.unwrap();
        assert!(preserved.join("partial.txt").exists());
        assert!(!output.exists());
        std::fs::remove_dir_all(preserved).unwrap();
    }

    #[tokio::test]
    async fn materialize_flattens_single_generic_inner_dir() {
        let root = tempfile::tempdir().unwrap();
        let output = root.path().join("my-archive");

        let result = OutputMaterializer::default()
            .prepare(
                MaterializeRequest {
                    output_dir: output.clone(),
                    archive_path: output.clone(),
                    commit_policy: CommitPolicy::FailIfExists,
                    archive_stem: Some("my-archive".to_string()),
                    layout_policy: OutputLayoutPolicy::Smart,
                    single_root_name_policy: SingleRootNamePolicy::default(),
                },
                |temp_dir| async move {
                    let inner = temp_dir.join("files");
                    std::fs::create_dir_all(&inner)
                        .map_err(|source| SmartZipError::io(Some(temp_dir.clone()), source))?;
                    std::fs::write(inner.join("a.txt"), b"alpha")
                        .map_err(|source| SmartZipError::io(Some(temp_dir), source))
                },
            )
            .await
            .unwrap()
            .commit(None)
            .unwrap();

        let plan = &result.layout_plan;
        assert_eq!(
            plan.kind,
            LayoutPlanKind::CommitSingleDirContentsAsArchiveName
        );
        assert_eq!(result.output_dir, plan.target);
        assert!(result.output_dir.join("a.txt").exists());
        assert_eq!(
            std::fs::read(result.output_dir.join("a.txt")).unwrap(),
            b"alpha"
        );
    }

    #[tokio::test]
    async fn materialize_single_dir_as_inner_name_outputs_inner_dir_not_archive_dir() {
        let root = tempfile::tempdir().unwrap();
        let output = root.path().join("output");

        let result = OutputMaterializer::default()
            .prepare(
                MaterializeRequest {
                    output_dir: output.clone(),
                    archive_path: output.clone(),
                    commit_policy: CommitPolicy::FailIfExists,
                    archive_stem: Some("archive".to_string()),
                    layout_policy: OutputLayoutPolicy::Smart,
                    single_root_name_policy: SingleRootNamePolicy::PreferInnerName,
                },
                |temp_dir| async move {
                    let inner = temp_dir.join("single_dir");
                    std::fs::create_dir_all(&inner)
                        .map_err(|source| SmartZipError::io(Some(temp_dir.clone()), source))?;
                    std::fs::write(inner.join("file.txt"), b"hello")
                        .map_err(|source| SmartZipError::io(Some(temp_dir), source))
                },
            )
            .await
            .unwrap()
            .commit(None)
            .unwrap();

        let plan = &result.layout_plan;
        assert_eq!(plan.kind, LayoutPlanKind::CommitSingleDirAsInnerName);
        assert_eq!(result.output_dir, plan.target);
        assert!(plan.target.exists());
        assert!(plan.target.join("file.txt").exists());
        assert_eq!(
            std::fs::read(plan.target.join("file.txt")).unwrap(),
            b"hello"
        );
    }

    #[tokio::test]
    async fn materialize_single_file_as_inner_name_outputs_file_at_root() {
        let root = tempfile::tempdir().unwrap();
        let output = root.path().join("output");

        let result = OutputMaterializer::default()
            .prepare(
                MaterializeRequest {
                    output_dir: output.clone(),
                    archive_path: output.clone(),
                    commit_policy: CommitPolicy::FailIfExists,
                    archive_stem: Some("archive".to_string()),
                    layout_policy: OutputLayoutPolicy::Smart,
                    single_root_name_policy: SingleRootNamePolicy::PreferInnerName,
                },
                |temp_dir| async move {
                    std::fs::write(temp_dir.join("doc.pdf"), b"pdf-content")
                        .map_err(|source| SmartZipError::io(Some(temp_dir), source))
                },
            )
            .await
            .unwrap()
            .commit(None)
            .unwrap();

        let plan = &result.layout_plan;
        assert_eq!(plan.kind, LayoutPlanKind::CommitSingleFileAsInnerName);
        assert_eq!(result.output_dir, plan.target);
        assert!(plan.target.exists());
        assert!(plan.target.is_file());
        assert_eq!(std::fs::read(&plan.target).unwrap(), b"pdf-content");
    }

    #[tokio::test]
    async fn materialize_single_dir_collapse_outputs_to_layout_target() {
        let root = tempfile::tempdir().unwrap();
        let output = root.path().join("output");
        std::fs::create_dir_all(&output).unwrap();

        let _result = OutputMaterializer::default()
            .prepare(
                MaterializeRequest {
                    output_dir: output.join("download"),
                    archive_path: output.clone(),
                    commit_policy: CommitPolicy::FailIfExists,
                    archive_stem: Some("download".to_string()),
                    layout_policy: OutputLayoutPolicy::Conservative,
                    single_root_name_policy: SingleRootNamePolicy::PreferInnerName,
                },
                |temp_dir| async move {
                    let inner = temp_dir.join("ProjectName");
                    std::fs::create_dir_all(&inner)
                        .map_err(|e| SmartZipError::io(Some(temp_dir), e))?;
                    std::fs::write(inner.join("file.txt"), b"content")
                        .map_err(|e| SmartZipError::io(Some(inner), e))
                },
            )
            .await
            .unwrap()
            .commit(None)
            .unwrap();

        // Should be output/ProjectName/file.txt, NOT output/download/ProjectName/file.txt
        assert!(output.join("ProjectName").exists());
        assert!(output.join("ProjectName").join("file.txt").exists());
        assert!(!output.join("download").join("ProjectName").exists());
    }
}
