use crate::layout::{
    LayoutPlan, LayoutPlanKind, LayoutRequest, OutputLayoutPolicy, PlanSource, SingleRootNamePolicy,
};
use smartzip_core::{Result, SmartZipError};
use std::future::Future;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitPolicy {
    FailIfExists,
    Overwrite,
    Rename,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaterializeRequest {
    pub output_dir: PathBuf,
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

    pub async fn materialize<F, Fut>(
        &self,
        request: MaterializeRequest,
        extract_into: F,
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
        })?;

        let temp = tempfile::Builder::new()
            .prefix(".smartzip-")
            .tempdir_in(&parent)
            .map_err(|source| MaterializeFailure {
                error: SmartZipError::io(Some(parent.clone()), source),
                preserved_temp_dir: None,
            })?;
        let temp_path = temp.path().to_path_buf();

        if let Err(error) = extract_into(temp_path.clone()).await {
            if self.preserve_temp_on_failure {
                let preserved = temp.keep();
                return Err(MaterializeFailure {
                    error,
                    preserved_temp_dir: Some(preserved),
                });
            }
            return Err(MaterializeFailure {
                error,
                preserved_temp_dir: None,
            });
        }

        let archive_stem = request.archive_stem.unwrap_or_else(|| {
            crate::name_score::archive_display_stem(&request.output_dir)
        });

        let shape = crate::layout::scan_visible_top_level(&temp_path);
        let layout_plan = crate::layout::plan_layout(&LayoutRequest {
            shape,
            archive_path: request.output_dir.clone(),
            archive_stem,
            output_root: parent.clone(),
            layout_policy: request.layout_policy,
            single_root_name_policy: request.single_root_name_policy,
        });

        let committed_temp_path = temp.keep();
        match &layout_plan.kind {
            LayoutPlanKind::CommitWholeTempAsArchiveDir { .. }
            | LayoutPlanKind::RawArchiveDir { .. } => {
                let commit_target =
                    resolve_commit_target(&layout_plan.target, request.commit_policy).map_err(
                        |error| MaterializeFailure {
                            error,
                            preserved_temp_dir: None,
                        },
                    )?;
                if commit_target.exists() {
                    remove_existing_output(&commit_target).map_err(|error| MaterializeFailure {
                        error,
                        preserved_temp_dir: None,
                    })?;
                }
                match std::fs::rename(&committed_temp_path, &commit_target) {
                    Ok(_) => Ok(MaterializeResult {
                        output_dir: commit_target,
                        layout_plan: Some(layout_plan),
                    }),
                    Err(error) => {
                        let _ = std::fs::remove_dir_all(&committed_temp_path);
                        Err(MaterializeFailure {
                            error: SmartZipError::io(Some(commit_target), error),
                            preserved_temp_dir: None,
                        })
                    }
                }
            }
            LayoutPlanKind::CommitSingleDirContentsAsArchiveName => {
                let PlanSource::SingleDirContents(dir_path) = &layout_plan.source else {
                    unreachable!()
                };
                let commit_target =
                    resolve_commit_target(&layout_plan.target, request.commit_policy).map_err(
                        |error| MaterializeFailure {
                            error,
                            preserved_temp_dir: None,
                        },
                    )?;
                if commit_target.exists() {
                    remove_existing_output(&commit_target).map_err(|error| MaterializeFailure {
                        error,
                        preserved_temp_dir: None,
                    })?;
                }
                std::fs::create_dir_all(&commit_target).map_err(|source| MaterializeFailure {
                    error: SmartZipError::io(Some(commit_target.clone()), source),
                    preserved_temp_dir: None,
                })?;
                recursive_move_contents(dir_path, &commit_target).map_err(|source| {
                    MaterializeFailure {
                        error: SmartZipError::io(Some(commit_target.clone()), source),
                        preserved_temp_dir: None,
                    }
                })?;
                let _ = std::fs::remove_dir_all(&committed_temp_path);
                Ok(MaterializeResult {
                    output_dir: commit_target,
                    layout_plan: Some(layout_plan),
                })
            }
            LayoutPlanKind::CommitSingleDirAsInnerName => {
                let PlanSource::SingleDir(dir_path) = &layout_plan.source else {
                    unreachable!()
                };
                let commit_target = request.output_dir.join(dir_path.file_name().unwrap());
                std::fs::create_dir_all(&commit_target).map_err(|source| MaterializeFailure {
                    error: SmartZipError::io(Some(commit_target.clone()), source),
                    preserved_temp_dir: None,
                })?;
                recursive_move_contents(dir_path, &commit_target).map_err(|source| {
                    MaterializeFailure {
                        error: SmartZipError::io(Some(commit_target.clone()), source),
                        preserved_temp_dir: None,
                    }
                })?;
                let _ = std::fs::remove_dir_all(&committed_temp_path);
                Ok(MaterializeResult {
                    output_dir: commit_target,
                    layout_plan: Some(layout_plan),
                })
            }
            LayoutPlanKind::CommitSingleFileAsArchiveName => {
                let PlanSource::SingleFile(file_path) = &layout_plan.source else {
                    unreachable!()
                };
                let commit_target =
                    resolve_commit_target(&layout_plan.target, request.commit_policy).map_err(
                        |error| MaterializeFailure {
                            error,
                            preserved_temp_dir: None,
                        },
                    )?;
                if commit_target.exists() {
                    remove_existing_output(&commit_target).map_err(|error| MaterializeFailure {
                        error,
                        preserved_temp_dir: None,
                    })?;
                }
                std::fs::rename(file_path, &commit_target).map_err(|source| {
                    MaterializeFailure {
                        error: SmartZipError::io(Some(commit_target.clone()), source),
                        preserved_temp_dir: None,
                    }
                })?;
                let _ = std::fs::remove_dir_all(&committed_temp_path);
                Ok(MaterializeResult {
                    output_dir: commit_target,
                    layout_plan: Some(layout_plan),
                })
            }
            LayoutPlanKind::CommitSingleFileAsInnerName => {
                let PlanSource::SingleFile(file_path) = &layout_plan.source else {
                    unreachable!()
                };
                let commit_target = &request.output_dir;
                std::fs::create_dir_all(commit_target).map_err(|source| MaterializeFailure {
                    error: SmartZipError::io(Some(commit_target.clone()), source),
                    preserved_temp_dir: None,
                })?;
                let target_file = commit_target.join(file_path.file_name().unwrap());
                std::fs::rename(file_path, &target_file).map_err(|source| MaterializeFailure {
                    error: SmartZipError::io(Some(target_file), source),
                    preserved_temp_dir: None,
                })?;
                let _ = std::fs::remove_dir_all(&committed_temp_path);
                Ok(MaterializeResult {
                    output_dir: commit_target.clone(),
                    layout_plan: Some(layout_plan),
                })
            }
            LayoutPlanKind::PreserveBothSingleDir => {
                let PlanSource::SingleDir(dir_path) = &layout_plan.source else {
                    unreachable!()
                };
                let commit_target =
                    resolve_commit_target(&layout_plan.target, request.commit_policy).map_err(
                        |error| MaterializeFailure {
                            error,
                            preserved_temp_dir: None,
                        },
                    )?;
                if commit_target.exists() {
                    remove_existing_output(&commit_target).map_err(|error| MaterializeFailure {
                        error,
                        preserved_temp_dir: None,
                    })?;
                }
                std::fs::create_dir_all(&commit_target).map_err(|source| MaterializeFailure {
                    error: SmartZipError::io(Some(commit_target.clone()), source),
                    preserved_temp_dir: None,
                })?;
                let inner_target = commit_target.join(dir_path.file_name().unwrap());
                std::fs::rename(dir_path, &inner_target).map_err(|source| MaterializeFailure {
                    error: SmartZipError::io(Some(inner_target), source),
                    preserved_temp_dir: None,
                })?;
                let _ = std::fs::remove_dir_all(&committed_temp_path);
                Ok(MaterializeResult {
                    output_dir: commit_target,
                    layout_plan: Some(layout_plan),
                })
            }
            LayoutPlanKind::PreserveBothSingleFile => {
                let PlanSource::SingleFile(file_path) = &layout_plan.source else {
                    unreachable!()
                };
                let commit_target =
                    resolve_commit_target(&layout_plan.target, request.commit_policy).map_err(
                        |error| MaterializeFailure {
                            error,
                            preserved_temp_dir: None,
                        },
                    )?;
                if commit_target.exists() {
                    remove_existing_output(&commit_target).map_err(|error| MaterializeFailure {
                        error,
                        preserved_temp_dir: None,
                    })?;
                }
                std::fs::create_dir_all(&commit_target).map_err(|source| MaterializeFailure {
                    error: SmartZipError::io(Some(commit_target.clone()), source),
                    preserved_temp_dir: None,
                })?;
                let inner_target = commit_target.join(file_path.file_name().unwrap());
                std::fs::rename(file_path, &inner_target).map_err(|source| MaterializeFailure {
                    error: SmartZipError::io(Some(inner_target), source),
                    preserved_temp_dir: None,
                })?;
                let _ = std::fs::remove_dir_all(&committed_temp_path);
                Ok(MaterializeResult {
                    output_dir: commit_target,
                    layout_plan: Some(layout_plan),
                })
            }
            LayoutPlanKind::Empty => {
                let _ = std::fs::remove_dir_all(&committed_temp_path);
                Ok(MaterializeResult {
                    output_dir: request.output_dir,
                    layout_plan: Some(layout_plan),
                })
            }
        }
    }
}

impl Default for OutputMaterializer {
    fn default() -> Self {
        Self::new(false)
    }
}

fn resolve_commit_target(output_dir: &Path, policy: CommitPolicy) -> Result<PathBuf> {
    match policy {
        CommitPolicy::FailIfExists if output_dir.exists() => Err(SmartZipError::io(
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

fn remove_existing_output(path: &Path) -> Result<()> {
    if path.is_dir() {
        std::fs::remove_dir_all(path)
            .map_err(|source| SmartZipError::io(Some(path.to_path_buf()), source))
    } else {
        std::fs::remove_file(path)
            .map_err(|source| SmartZipError::io(Some(path.to_path_buf()), source))
    }
}

fn find_non_colliding_name(parent: &Path, name: &std::ffi::OsStr) -> PathBuf {
    let base = parent.join(name);
    if !base.exists() {
        return base;
    }
    let name_str = name.to_string_lossy();
    for n in 1..1000u32 {
        let alt = parent.join(format!("{name_str}_collided_{n}"));
        if !alt.exists() {
            return alt;
        }
    }
    parent.join(format!("{name_str}_{}", std::process::id()))
}

fn recursive_move_contents(from: &Path, to: &Path) -> std::io::Result<()> {
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let source = entry.path();
        let dest = to.join(entry.file_name());
        if source.is_dir() {
            std::fs::create_dir_all(&dest)?;
            recursive_move_contents(&source, &dest)?;
        } else {
            std::fs::rename(&source, &dest)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn commits_temp_output_after_success() {
        let root = tempfile::tempdir().unwrap();
        let output = root.path().join("archive-d0");

        let result = OutputMaterializer::default()
            .materialize(
                MaterializeRequest {
                    output_dir: output.clone(),
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
            .unwrap();

        let plan = result.layout_plan.as_ref().unwrap();
        assert_eq!(plan.kind, LayoutPlanKind::CommitWholeTempAsArchiveDir { name: "archive-d0".to_string() });
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
            .materialize(
                MaterializeRequest {
                    output_dir: output.clone(),
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
            .materialize(
                MaterializeRequest {
                    output_dir: output.clone(),
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
            .unwrap();

        let plan = result.layout_plan.as_ref().unwrap();
        assert_eq!(plan.kind, LayoutPlanKind::CommitSingleDirContentsAsArchiveName);
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
            .unwrap();

        let plan = result.layout_plan.as_ref().unwrap();
        assert_eq!(plan.kind, LayoutPlanKind::CommitSingleDirAsInnerName);
        assert!(output.join("single_dir").exists());
        assert!(output.join("single_dir/file.txt").exists());
        assert_eq!(
            std::fs::read(output.join("single_dir/file.txt")).unwrap(),
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
            .unwrap();

        let plan = result.layout_plan.as_ref().unwrap();
        assert_eq!(plan.kind, LayoutPlanKind::CommitSingleFileAsInnerName);
        assert!(output.join("doc.pdf").exists());
        assert_eq!(
            std::fs::read(output.join("doc.pdf")).unwrap(),
            b"pdf-content"
        );
    }
}
