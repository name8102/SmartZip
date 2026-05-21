use std::path::PathBuf;

/// Convenient result alias for SmartZip core operations.
pub type Result<T> = std::result::Result<T, SmartZipError>;

/// Domain errors shared by the CLI, GUI, and backend crates.
#[derive(Debug, thiserror::Error)]
pub enum SmartZipError {
    #[error("I/O error at {path:?}: {source}")]
    Io {
        path: Option<PathBuf>,
        #[source]
        source: std::io::Error,
    },

    #[error("unsupported archive format for {path}")]
    UnsupportedFormat {
        path: PathBuf,
        format: Option<String>,
    },

    #[error("password required for {path}")]
    PasswordRequired { path: PathBuf },

    #[error("wrong password for {path}")]
    WrongPassword { path: PathBuf },

    #[error("corrupted archive {path}: {detail}")]
    CorruptedArchive { path: PathBuf, detail: String },

    #[error("unsafe archive entry path: {entry}")]
    UnsafeArchivePath { entry: String },

    #[error("archive backend unavailable: {backend}")]
    BackendUnavailable { backend: String },

    #[error("archive backend {backend} failed with exit code {exit_code:?}: {stderr}")]
    BackendFailed {
        backend: String,
        exit_code: Option<i32>,
        stderr: String,
    },

    #[error("operation cancelled")]
    Cancelled,
}

impl SmartZipError {
    pub fn io(path: impl Into<Option<PathBuf>>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }
}
