use crate::layout::{
    LayoutPlan, LayoutPlanKind, LayoutRequest, OutputLayoutPolicy, PlanSource, SingleRootNamePolicy,
};
use smartzip_core::{Result, SmartZipError};
use std::future::Future;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::pin::Pin;

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

/// Async callback that resolves a collision at `target_path`.
/// Returns the action to take. Called after layout planning, before commit.
pub type CollisionResolver<'a> = Box<
    dyn Fn(
            PathBuf,
            PathBuf,
            LayoutPlan,
        ) -> Pin<Box<dyn Future<Output = CollisionAction> + Send + 'a>>
        + Send
        + Sync
        + 'a,
>;

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
pub struct MaterializeResult {
    pub output_dir: PathBuf,
    pub layout_plan: Option<LayoutPlan>,
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

impl OutputMaterializer {
    pub fn new(preserve_temp_on_failure: bool) -> Self {
        Self {
            preserve_temp_on_failure,
        }
    }

    pub async fn materialize<'r, 'a, F, Fut>(
        &self,
        request: MaterializeRequest,
        extract_into: F,
        collision_resolver: Option<&'r CollisionResolver<'a>>,
    ) -> std::result::Result<MaterializeResult, MaterializeFailure>
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
            return Err(MaterializeFailure {
                error,
                preserved_temp_dir,
                kind: MaterializeFailureKind::ExtractFailed,
            });
        }

        let archive_stem = request
            .archive_stem
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

        // Empty extraction: nothing to commit, no collision possible.
        if matches!(layout_plan.kind, LayoutPlanKind::Empty) {
            let _ = std::fs::remove_dir_all(temp.path());
            return Ok(MaterializeResult {
                output_dir: request.output_dir,
                layout_plan: Some(layout_plan),
            });
        }

        let mut commit_policy = request.commit_policy;
        if path_present(&layout_plan.target) && commit_policy == CommitPolicy::FailIfExists {
            if let Some(resolver) = collision_resolver {
                let action = resolver(
                    request.archive_path.clone(),
                    layout_plan.target.clone(),
                    layout_plan.clone(),
                )
                .await;
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
        let mut layout_plan = layout_plan.clone();
        match commit_output(source, &commit_target, commit_policy, rename_no_replace) {
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
                Ok(MaterializeResult {
                    output_dir: commit_target,
                    layout_plan: Some(layout_plan),
                })
            }
            Err(mut failure) => {
                if self.preserve_temp_on_failure && failure.preserved_temp_dir.is_none() {
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

/// Atomically refuse an occupied destination, including a dangling symlink.
fn rename_no_replace(from: &Path, to: &Path) -> std::io::Result<()> {
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
            .materialize(
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
                None,
            )
            .await
            .unwrap();

        let plan = result.layout_plan.as_ref().unwrap();
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
    async fn overwrite_removes_existing_output_only_after_success() {
        let root = tempfile::tempdir().unwrap();
        let output = root.path().join("archive-d0");
        std::fs::create_dir_all(&output).unwrap();
        std::fs::write(output.join("old.txt"), b"old").unwrap();

        let result = OutputMaterializer::default()
            .materialize(
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
                None,
            )
            .await
            .unwrap();

        let plan = result.layout_plan.as_ref().unwrap();
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
            .materialize(
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
                None,
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
            .materialize(
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
                None,
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
            .materialize(
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
                None,
            )
            .await
            .unwrap();

        let plan = result.layout_plan.as_ref().unwrap();
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
            .materialize(
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
                None,
            )
            .await
            .unwrap();

        let plan = result.layout_plan.as_ref().unwrap();
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
            .materialize(
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
                None,
            )
            .await
            .unwrap();

        let plan = result.layout_plan.as_ref().unwrap();
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
            .materialize(
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
                None,
            )
            .await
            .unwrap();

        // Should be output/ProjectName/file.txt, NOT output/download/ProjectName/file.txt
        assert!(output.join("ProjectName").exists());
        assert!(output.join("ProjectName").join("file.txt").exists());
        assert!(!output.join("download").join("ProjectName").exists());
    }
}
