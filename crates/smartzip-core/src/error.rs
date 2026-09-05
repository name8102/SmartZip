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

    #[error("backend {backend} does not support container {container:?} for {path}")]
    UnsupportedContainer {
        backend: String,
        path: PathBuf,
        container: Option<String>,
    },

    #[error("backend {backend} does not support codec {codec:?} for {path}")]
    UnsupportedCodec {
        backend: String,
        path: PathBuf,
        codec: Option<String>,
    },

    #[error("password required for {path}")]
    PasswordRequired { path: PathBuf },

    #[error("wrong password for {path}")]
    WrongPassword { path: PathBuf },

    #[error(
        "password attempts exhausted for {path:?} after {attempts} password attempt(s); last diagnostic: {diagnostic}"
    )]
    PasswordAttemptsExhausted {
        path: PathBuf,
        attempts: usize,
        diagnostic: String,
    },

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

    #[error("archive backend {backend} protocol error: {detail}")]
    BackendProtocolError { backend: String, detail: String },

    #[error("ambiguous embedded archives at {path:?}: {count} findings")]
    EmbeddedArchiveAmbiguous { path: PathBuf, count: usize },

    #[error("embedded archive at {path:?} offset {offset} not extractable: {detail}")]
    EmbeddedArchiveDetectedButNotExtractable {
        path: PathBuf,
        offset: u64,
        detail: String,
    },

    #[error("failed to carve embedded archive at {path:?} offset {offset}: {detail}")]
    EmbeddedArchiveCarveFailed {
        path: PathBuf,
        offset: u64,
        detail: String,
    },

    #[error("large file scan requires confirmation: {path:?} ({file_size} bytes, threshold {threshold})")]
    LargeEmbeddedScanRequiresConfirmation {
        path: PathBuf,
        file_size: u64,
        threshold: u64,
    },

    #[error("extraction resource limit: {detail}")]
    ResourceLimit { detail: String },

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
