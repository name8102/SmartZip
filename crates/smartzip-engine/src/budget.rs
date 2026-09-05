//! Dynamic limits for the managed extraction tree. Polling bounds resource use
//! at checkpoints, not to the last byte a subprocess can write between checks.
use smartzip_core::{Result, SmartZipError, TaskExecutionContext};
use std::path::Path;
use std::sync::Arc;

pub use smartzip_config::ExtractionLimits;
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct Usage {
    pub files: u64,
    pub bytes: u64,
}

pub(crate) fn exceeded(detail: impl Into<String>) -> SmartZipError {
    SmartZipError::ResourceLimit {
        detail: detail.into(),
    }
}

pub(crate) fn inspect(path: &Path, limits: &ExtractionLimits, previous: Usage) -> Result<Usage> {
    let mut usage = previous;
    // Streaming traversal; no vector of all files and no following symlinks.
    for entry in walkdir::WalkDir::new(path).follow_links(false) {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error)
                if error
                    .io_error()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
            {
                continue
            }
            Err(error) => {
                return Err(SmartZipError::io(
                    Some(path.into()),
                    std::io::Error::other(error),
                ))
            }
        };
        if entry.depth() == 0 && entry.file_type().is_dir() {
            continue;
        }
        let metadata = match std::fs::symlink_metadata(entry.path()) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(SmartZipError::io(Some(entry.path().into()), error)),
        };
        if !metadata.is_file() && !metadata.is_dir() {
            return Err(SmartZipError::UnsafeArchivePath {
                entry: entry.path().display().to_string(),
            });
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if metadata.is_file() && metadata.nlink() > 1 {
                return Err(SmartZipError::UnsafeArchivePath {
                    entry: entry.path().display().to_string(),
                });
            }
        }
        usage.files = usage.files.saturating_add(1);
        if metadata.is_file() {
            usage.bytes = usage.bytes.saturating_add(metadata.len());
        }
        if usage.files > limits.max_files {
            return Err(exceeded(format!(
                "output entry limit {} exceeded",
                limits.max_files
            )));
        }
        if usage.bytes > limits.max_output_bytes {
            return Err(exceeded(format!(
                "output byte limit {} exceeded",
                limits.max_output_bytes
            )));
        }
    }
    if free_bytes(path)? < limits.min_free_bytes {
        return Err(exceeded(format!(
            "free disk space fell below {} bytes",
            limits.min_free_bytes
        )));
    }
    Ok(usage)
}

fn free_bytes(path: &Path) -> Result<u64> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let path = std::ffi::CString::new(path.as_os_str().as_bytes())
            .map_err(|e| SmartZipError::io(Some(path.into()), std::io::Error::other(e)))?;
        let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
        // SAFETY: path is a live terminated string and stat is writable.
        if unsafe { libc::statvfs(path.as_ptr(), stat.as_mut_ptr()) } != 0 {
            return Err(SmartZipError::io(None, std::io::Error::last_os_error()));
        }
        // SAFETY: successful statvfs initialized the structure.
        let stat = unsafe { stat.assume_init() };
        Ok((stat.f_bavail as u64).saturating_mul(stat.f_frsize as u64))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Err(exceeded(
            "disk budget monitoring is unsupported on this platform",
        ))
    }
}

pub(crate) async fn monitor<T>(
    path: &Path,
    limits: &ExtractionLimits,
    previous: Usage,
    context: Arc<TaskExecutionContext>,
    operation: impl std::future::Future<Output = Result<T>>,
) -> Result<T> {
    inspect(path, limits, previous)?;
    let mut operation = std::pin::pin!(operation);
    let mut interval = tokio::time::interval(std::time::Duration::from_millis(50));
    loop {
        tokio::select! {
            result = &mut operation => {
                let value = result?;
                if context.is_cancelled() { return Err(SmartZipError::Cancelled); }
                inspect(path, limits, previous)?;
                return Ok(value);
            }
            _ = interval.tick() => {
                if let Err(error) = inspect(path, limits, previous) {
                    context.cancel();
                    // Do not drop the future: adapters must terminate and reap
                    // their processes before staging can safely be removed.
                    let _ = operation.await;
                    return Err(error);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn growing_output_stops_backend_and_fails_budget() {
        let root = tempfile::tempdir().unwrap();
        let context = Arc::new(TaskExecutionContext::detached());
        let token = context.cancellation_token();
        let limits = ExtractionLimits {
            max_output_bytes: 8,
            min_free_bytes: 0,
            ..Default::default()
        };
        let stopped = std::cell::Cell::new(false);
        let result = monitor(root.path(), &limits, Usage::default(), context, async {
            std::fs::write(root.path().join("bomb"), [0; 9]).unwrap();
            token.cancelled().await;
            stopped.set(true);
            Err::<(), _>(SmartZipError::Cancelled)
        })
        .await;
        assert!(matches!(result, Err(SmartZipError::ResourceLimit { .. })));
        assert!(stopped.get());
    }
    #[test]
    fn cumulative_count_and_bytes_are_enforced() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("file"), [0; 5]).unwrap();
        let limits = ExtractionLimits {
            max_files: 1,
            max_output_bytes: 8,
            min_free_bytes: 0,
            ..Default::default()
        };
        assert!(inspect(root.path(), &limits, Usage { files: 1, bytes: 0 }).is_err());
        assert!(inspect(root.path(), &limits, Usage { files: 0, bytes: 4 }).is_err());
    }
}
