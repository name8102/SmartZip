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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaterializeResult {
    pub output_dir: PathBuf,
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

        let output_dir = resolve_commit_target(&request.output_dir, request.commit_policy)
            .map_err(|error| MaterializeFailure {
                error,
                preserved_temp_dir: None,
            })?;
        if output_dir.exists() {
            remove_existing_output(&output_dir).map_err(|error| MaterializeFailure {
                error,
                preserved_temp_dir: None,
            })?;
        }

        let committed_temp_path = temp.keep();
        match std::fs::rename(&committed_temp_path, &output_dir) {
            Ok(_) => Ok(MaterializeResult { output_dir }),
            Err(error) => {
                let _ = std::fs::remove_dir_all(&committed_temp_path);
                Err(MaterializeFailure {
                    error: SmartZipError::io(Some(output_dir), error),
                    preserved_temp_dir: None,
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
                },
                |temp_dir| async move {
                    std::fs::write(temp_dir.join("hello.txt"), b"hello")
                        .map_err(|source| SmartZipError::io(Some(temp_dir), source))
                },
            )
            .await
            .unwrap();

        assert_eq!(result.output_dir, output);
        assert_eq!(std::fs::read(output.join("hello.txt")).unwrap(), b"hello");
    }

    #[tokio::test]
    async fn overwrite_removes_existing_output_only_after_success() {
        let root = tempfile::tempdir().unwrap();
        let output = root.path().join("archive-d0");
        std::fs::create_dir_all(&output).unwrap();
        std::fs::write(output.join("old.txt"), b"old").unwrap();

        OutputMaterializer::default()
            .materialize(
                MaterializeRequest {
                    output_dir: output.clone(),
                    commit_policy: CommitPolicy::Overwrite,
                },
                |temp_dir| async move {
                    std::fs::write(temp_dir.join("new.txt"), b"new")
                        .map_err(|source| SmartZipError::io(Some(temp_dir), source))
                },
            )
            .await
            .unwrap();

        assert!(!output.join("old.txt").exists());
        assert_eq!(std::fs::read(output.join("new.txt")).unwrap(), b"new");
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
}
