//! Backend call wrappers and panic mapping.

use futures_util::FutureExt;
use std::any::Any;
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::path::Path;

pub(crate) fn map_detect_error(
    error: smartzip_core::SmartZipError,
    path: &Path,
) -> smartzip_core::SmartZipError {
    match error {
        smartzip_core::SmartZipError::UnsupportedFormat { .. } => {
            smartzip_core::SmartZipError::UnsupportedFormat {
                path: path.to_path_buf(),
                format: None,
            }
        }
        other => other,
    }
}

pub(crate) fn confidence_score(confidence: smartzip_scanner::Confidence) -> f32 {
    match confidence {
        smartzip_scanner::Confidence::Low => 0.33,
        smartzip_scanner::Confidence::Medium => 0.66,
        smartzip_scanner::Confidence::High => 1.0,
    }
}

pub(crate) async fn backend_call<T, F>(
    backend: &str,
    action: &str,
    path: &Path,
    future: F,
) -> smartzip_core::Result<T>
where
    F: Future<Output = smartzip_core::Result<T>>,
{
    match AssertUnwindSafe(future).catch_unwind().await {
        Ok(result) => result,
        Err(panic) => Err(smartzip_core::SmartZipError::BackendFailed {
            backend: backend.to_string(),
            exit_code: None,
            stderr: format!(
                "panic while {action} {}: {}",
                path.display(),
                panic_message(panic)
            ),
        }),
    }
}

pub(crate) fn panic_message(panic: Box<dyn Any + Send>) -> String {
    if let Some(message) = panic.downcast_ref::<&str>() {
        (*message).to_string()
    } else if let Some(message) = panic.downcast_ref::<String>() {
        message.clone()
    } else {
        "unknown panic payload".to_string()
    }
}
