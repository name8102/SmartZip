use crate::backend::ArchiveAdapter;
use crate::types::*;
use async_trait::async_trait;
#[cfg(windows)]
use process_wrap::tokio::JobObject;
#[cfg(unix)]
use process_wrap::tokio::ProcessGroup;
use process_wrap::tokio::{CommandWrap, KillOnDrop};
use smartzip_core::{ArchiveFormat, Result, SmartZipError, TaskExecutionContext};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::AsyncReadExt;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SevenZipOperation {
    Extract,
    Test,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SevenZipDiagnosticSeverity {
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SevenZipEvent {
    Progress {
        operation: SevenZipOperation,
        percent: f32,
        item: Option<String>,
    },
    Diagnostic {
        severity: SevenZipDiagnosticSeverity,
        text: String,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SevenZipReport {
    pub warnings: Vec<String>,
    pub errors: Vec<String>,
    pub archive_type: Option<String>,
    pub physical_size: Option<u64>,
    pub encrypted: Option<bool>,
    pub files: Option<u64>,
    pub folders: Option<u64>,
    pub unpacked_size: Option<u64>,
    pub compressed_size: Option<u64>,
    pub elapsed_millis: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SevenZipExitStatus {
    Success,
    Warning,
    Fatal,
    CommandLineError,
    OutOfMemory,
    Cancelled,
    Unknown(Option<i32>),
}

impl SevenZipExitStatus {
    fn from_code(code: Option<i32>) -> Self {
        match code {
            Some(0) => Self::Success,
            Some(1) => Self::Warning,
            Some(2) => Self::Fatal,
            Some(7) => Self::CommandLineError,
            Some(8) => Self::OutOfMemory,
            Some(255) => Self::Cancelled,
            code => Self::Unknown(code),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SevenZipResult<T> {
    /// Present only for an unambiguous exit-0 completion.
    pub value: Option<T>,
    pub report: SevenZipReport,
    pub status: SevenZipExitStatus,
    pub stdout: String,
    pub stderr: String,
}

type Observer = Arc<dyn Fn(SevenZipEvent) + Send + Sync>;

#[derive(Debug, Clone)]
pub struct SevenZipLocator {
    bundled: Option<PathBuf>,
    candidates: Vec<String>,
}

impl Default for SevenZipLocator {
    fn default() -> Self {
        Self {
            bundled: None,
            candidates: vec!["7zz".into(), "7z".into()],
        }
    }
}

impl SevenZipLocator {
    pub fn bundled(path: PathBuf) -> Self {
        Self {
            bundled: Some(path),
            ..Default::default()
        }
    }

    pub fn locate(&self) -> Option<PathBuf> {
        self.locate_all().into_iter().next()
    }

    /// Find every independent installation, normalized and deduplicated by path.
    ///
    /// Uses the `which` crate for cross-platform executable lookup (checks
    /// `PATH`, `PATHEXT` on Windows, and executable permission) instead of a
    /// manual `split_paths` + `exists` traversal.
    pub fn locate_all(&self) -> Vec<PathBuf> {
        let mut paths = Vec::new();
        if let Some(path) = &self.bundled {
            if path.exists() {
                paths.push(normalize_executable_path(path));
            }
        }
        for candidate in &self.candidates {
            if let Ok(iter) = which::which_all(candidate) {
                for found in iter {
                    let found = normalize_executable_path(&found);
                    if !paths.contains(&found) {
                        paths.push(found);
                    }
                }
            }
        }
        paths
    }
}

fn normalize_executable_path(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

#[derive(Clone)]
pub struct SevenZipBackend {
    id: String,
    executable: PathBuf,
    observer: Option<Observer>,
}

impl std::fmt::Debug for SevenZipBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SevenZipBackend")
            .field("id", &self.id)
            .field("executable", &self.executable)
            .field("has_observer", &self.observer.is_some())
            .finish()
    }
}

impl SevenZipBackend {
    pub fn new(executable: PathBuf) -> Self {
        let executable = normalize_executable_path(&executable);
        let id = format!("sevenzip:{}", executable.display());
        Self {
            id,
            executable,
            observer: None,
        }
    }

    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.id = id.into();
        self
    }

    pub fn with_observer(
        mut self,
        observer: impl Fn(SevenZipEvent) + Send + Sync + 'static,
    ) -> Self {
        self.observer = Some(Arc::new(observer));
        self
    }

    pub fn locate(locator: &SevenZipLocator) -> Result<Self> {
        locator
            .locate()
            .map(Self::new)
            .ok_or_else(|| SmartZipError::BackendUnavailable {
                backend: "7zz".into(),
            })
    }

    pub fn executable(&self) -> &Path {
        &self.executable
    }

    fn map_start_error(&self, source: std::io::Error) -> SmartZipError {
        if source.kind() == std::io::ErrorKind::NotFound {
            SmartZipError::BackendUnavailable {
                backend: self.id.clone(),
            }
        } else {
            SmartZipError::io(Some(self.executable.clone()), source)
        }
    }

    async fn run(&self, args: &[String]) -> Result<BackendCommandOutput> {
        // Non-cancellable path used for probe/list. Delegates to the
        // cancellable implementation with a never-cancelled token.
        self.run_with_token(args, &CancellationToken::new()).await
    }

    async fn run_with_token(
        &self,
        args: &[String],
        token: &CancellationToken,
    ) -> Result<BackendCommandOutput> {
        let mut wrap = CommandWrap::with_new(&self.executable, |command| {
            command.args(args);
            command.stdin(Stdio::null());
            command.stdout(Stdio::piped());
            command.stderr(Stdio::piped());
        });
        #[cfg(unix)]
        wrap.wrap(ProcessGroup::leader());
        #[cfg(windows)]
        wrap.wrap(JobObject);
        wrap.wrap(KillOnDrop);

        let mut child = wrap
            .spawn()
            .map_err(|source| self.map_start_error(source))?;
        let stdout = child.stdout().take();
        let stderr = child.stderr().take();
        // Use the same reader infrastructure as streaming but without progress
        // observer; this lets us keep the child handle for kill on cancel.
        let stdout_task = stdout.map(|s| {
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut s = s;
                let _ = tokio::io::AsyncReadExt::read_to_end(&mut s, &mut buf).await;
                buf
            })
        });
        let stderr_task = stderr.map(|s| {
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut s = s;
                let _ = tokio::io::AsyncReadExt::read_to_end(&mut s, &mut buf).await;
                buf
            })
        });
        let status = tokio::select! {
            res = child.wait() => res.map_err(|source| SmartZipError::io(Some(self.executable.clone()), source))?,
            _ = token.cancelled() => {
                let _ = child.start_kill();
                let status = child.wait().await.map_err(|source| SmartZipError::io(Some(self.executable.clone()), source))?;
                if let Some(t) = stdout_task { t.abort(); let _ = t.await; }
                if let Some(t) = stderr_task { t.abort(); let _ = t.await; }
                let _ = status;
                return Err(SmartZipError::Cancelled);
            }
        };
        let stdout = if let Some(t) = stdout_task {
            t.await.unwrap_or_default()
        } else {
            Vec::new()
        };
        let stderr = if let Some(t) = stderr_task {
            t.await.unwrap_or_default()
        } else {
            Vec::new()
        };
        Ok(BackendCommandOutput {
            status: status.code(),
            stdout: String::from_utf8_lossy(&stdout).into_owned(),
            stderr: String::from_utf8_lossy(&stderr).into_owned(),
        })
    }

    async fn run_streaming_with_token(
        &self,
        args: &[String],
        operation: SevenZipOperation,
        token: &CancellationToken,
    ) -> Result<BackendCommandOutput> {
        let mut wrap = CommandWrap::with_new(&self.executable, |command| {
            command.args(args);
            command.stdin(Stdio::null());
            command.stdout(Stdio::piped());
            command.stderr(Stdio::piped());
        });
        #[cfg(unix)]
        wrap.wrap(ProcessGroup::leader());
        #[cfg(windows)]
        wrap.wrap(JobObject);
        wrap.wrap(KillOnDrop);

        let mut child = wrap
            .spawn()
            .map_err(|source| self.map_start_error(source))?;
        let stdout = child.stdout().take().ok_or_else(|| {
            SmartZipError::io(
                Some(self.executable.clone()),
                std::io::Error::other("7z child stdout pipe was unavailable"),
            )
        })?;
        let stderr = child.stderr().take().ok_or_else(|| {
            SmartZipError::io(
                Some(self.executable.clone()),
                std::io::Error::other("7z child stderr pipe was unavailable"),
            )
        })?;
        let observer = self.observer.clone();
        let stdout_task = tokio::spawn(read_stream(stdout, observer.clone(), Some(operation)));
        let stderr_task = tokio::spawn(read_stream(stderr, observer, None));
        // Wait for child or cancellation. On cancel we must:
        // 1. terminate the process group / job object,
        // 2. wait for the child to actually exit,
        // 3. drain the stdout/stderr readers,
        // 4. return Cancelled. The process tree is guaranteed stopped on
        //    return, so the caller can safely clean the attempt directory.
        let status = tokio::select! {
            res = child.wait() => res
                .map_err(|source| SmartZipError::io(Some(self.executable.clone()), source))?,
            _ = token.cancelled() => {
                let _ = child.start_kill();
                let status = child.wait().await
                    .map_err(|source| SmartZipError::io(Some(self.executable.clone()), source))?;
                // Pipes will get EOF after the group is killed; await
                // readers deterministically.
                stdout_task.abort();
                stderr_task.abort();
                let _ = stdout_task.await;
                let _ = stderr_task.await;
                let _ = status;
                return Err(SmartZipError::Cancelled);
            }
        };
        let stdout = stdout_task
            .await
            .map_err(|source| SmartZipError::BackendFailed {
                backend: "7zz".into(),
                exit_code: status.code(),
                stderr: source.to_string(),
            })?
            .map_err(|source| SmartZipError::io(None, source))?;
        let stderr = stderr_task
            .await
            .map_err(|source| SmartZipError::BackendFailed {
                backend: "7zz".into(),
                exit_code: status.code(),
                stderr: source.to_string(),
            })?
            .map_err(|source| SmartZipError::io(None, source))?;
        Ok(BackendCommandOutput {
            status: status.code(),
            stdout: String::from_utf8_lossy(&stdout).into_owned(),
            stderr: String::from_utf8_lossy(&stderr).into_owned(),
        })
    }

    fn encoding_arg(encoding: &smartzip_core::EncodingMode) -> Option<String> {
        match encoding {
            smartzip_core::EncodingMode::Override(s) => {
                let normalized = s.trim().replace('-', "_").to_ascii_lowercase();
                match normalized.as_str() {
                    "utf_8" | "utf8" => Some("-scsUTF-8".to_string()),
                    // p7zip accepts numeric code-page ids here; `CP936`-style values
                    // are rejected as unsupported charset names.
                    "gb18030" | "gbk" | "gb2312" => Some("-scs936".to_string()),
                    "big5" => Some("-scs950".to_string()),
                    "shift_jis" | "shiftjis" | "sjis" | "cp932" => Some("-scs932".to_string()),
                    "euc_kr" | "euckr" | "cp949" => Some("-scs949".to_string()),
                    "euc_jp" | "eucjp" => Some("-scs20932".to_string()),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    fn map_failure(&self, output: &BackendCommandOutput, path: &Path) -> SmartZipError {
        let combined = format!("{}\n{}", output.stdout, output.stderr);
        let lower = combined.to_lowercase();
        if output.status == Some(255) {
            return SmartZipError::Cancelled;
        }
        if lower.contains("wrong password") || lower.contains("can not open encrypted archive") {
            SmartZipError::WrongPassword {
                path: path.to_path_buf(),
            }
        } else if lower.contains("password is required") || lower.contains("enter password") {
            SmartZipError::PasswordRequired {
                path: path.to_path_buf(),
            }
        } else if lower.contains("crc failed")
            || lower.contains("data error")
            || lower.contains("unexpected end of data")
            || lower.contains("unexpected end of archive")
        {
            SmartZipError::CorruptedArchive {
                path: path.to_path_buf(),
                detail: combined,
            }
        } else if lower.contains("unsupported method") {
            SmartZipError::UnsupportedCodec {
                backend: self.id.clone(),
                path: path.to_path_buf(),
                codec: extract_unsupported_method(&combined),
            }
        } else if lower.contains("is not archive")
            || lower.contains("as archive")
            || lower.contains("unsupported archive")
        {
            SmartZipError::UnsupportedContainer {
                backend: self.id.clone(),
                path: path.to_path_buf(),
                container: None,
            }
        } else if lower.contains("no such file")
            || lower.contains("the system cannot find the file")
            || lower.contains("cannot find the file")
            || lower.contains("file not found")
        {
            SmartZipError::io(
                Some(path.to_path_buf()),
                std::io::Error::new(std::io::ErrorKind::NotFound, combined),
            )
        } else if lower.contains("permission denied") || lower.contains("access is denied") {
            SmartZipError::io(
                Some(path.to_path_buf()),
                std::io::Error::new(std::io::ErrorKind::PermissionDenied, combined),
            )
        } else {
            SmartZipError::BackendFailed {
                backend: self.id.clone(),
                exit_code: output.status,
                stderr: combined,
            }
        }
    }

    fn map_reported_failure<T>(&self, result: &SevenZipResult<T>, path: &Path) -> SmartZipError {
        let status = match result.status {
            SevenZipExitStatus::Success => Some(0),
            SevenZipExitStatus::Warning => Some(1),
            SevenZipExitStatus::Fatal => Some(2),
            SevenZipExitStatus::CommandLineError => Some(7),
            SevenZipExitStatus::OutOfMemory => Some(8),
            SevenZipExitStatus::Cancelled => Some(255),
            SevenZipExitStatus::Unknown(code) => code,
        };
        self.map_failure(
            &BackendCommandOutput {
                status,
                stdout: result.stdout.clone(),
                stderr: result.stderr.clone(),
            },
            path,
        )
    }

    pub async fn test_with_report(
        &self,
        request: TestRequest,
    ) -> Result<SevenZipResult<TestResult>> {
        self.test_with_report_and_token(request, &CancellationToken::new())
            .await
    }

    pub async fn test_with_report_and_token(
        &self,
        request: TestRequest,
        token: &CancellationToken,
    ) -> Result<SevenZipResult<TestResult>> {
        let mut args: Vec<String> = vec!["t".into(), "-bsp1".into()];
        if let Some(pw) = Self::password_arg(&request.password) {
            args.push(pw);
        }
        if let Some(enc) = Self::encoding_arg(&request.encoding) {
            args.push(enc);
        }
        args.push(request.archive.to_string_lossy().into_owned());
        let output = self
            .run_streaming_with_token(&args, SevenZipOperation::Test, token)
            .await?;
        let report = parse_report(&format!("{}\n{}", output.stdout, output.stderr));
        let status = SevenZipExitStatus::from_code(output.status);
        Ok(SevenZipResult {
            value: (status == SevenZipExitStatus::Success).then_some(TestResult {
                ok: true,
                encrypted: report.encrypted,
                ..TestResult::default()
            }),
            report,
            status,
            stdout: output.stdout,
            stderr: output.stderr,
        })
    }

    pub async fn extract_with_report(
        &self,
        request: ExtractArchiveRequest,
    ) -> Result<SevenZipResult<ExtractArchiveResult>> {
        self.extract_with_report_and_token(request, &CancellationToken::new())
            .await
    }

    pub async fn extract_with_report_and_token(
        &self,
        request: ExtractArchiveRequest,
        token: &CancellationToken,
    ) -> Result<SevenZipResult<ExtractArchiveResult>> {
        let mut args: Vec<String> = vec!["x".into(), "-y".into(), "-bsp1".into()];
        if let Some(pw) = Self::password_arg(&request.password) {
            args.push(pw);
        }
        if let Some(enc) = Self::encoding_arg(&request.encoding) {
            args.push(enc);
        }
        args.push(format!("-o{}", request.output_dir.display()));
        args.push(request.archive.to_string_lossy().into_owned());
        let output = self
            .run_streaming_with_token(&args, SevenZipOperation::Extract, token)
            .await?;
        let report = parse_report(&format!("{}\n{}", output.stdout, output.stderr));
        let status = SevenZipExitStatus::from_code(output.status);
        Ok(SevenZipResult {
            value: (status == SevenZipExitStatus::Success).then_some(ExtractArchiveResult {
                output_dir: request.output_dir,
            }),
            report,
            status,
            stdout: output.stdout,
            stderr: output.stderr,
        })
    }

    fn password_arg(password: &Option<String>) -> Option<String> {
        password.as_ref().map(|password| {
            if password.is_empty() {
                "-p\"\"".to_string()
            } else {
                format!("-p{password}")
            }
        })
    }
}

#[async_trait]
impl ArchiveAdapter for SevenZipBackend {
    fn id(&self) -> &str {
        &self.id
    }
    fn diagnostic_family(&self) -> Option<&'static str> {
        Some("7z")
    }

    async fn probe(&self, path: &Path) -> Result<ArchiveProbe> {
        let request = TestRequest {
            archive: path.to_path_buf(),
            format: None,
            password: Some(String::new()),
            encoding: smartzip_core::EncodingMode::Auto,
        };
        let result = self.test(request).await;
        let (supported, encrypted) = match result {
            Ok(result)
                if matches!(
                    result.diagnostics.failure,
                    Some(
                        crate::integrity::TestFailure::PasswordRequired
                            | crate::integrity::TestFailure::PasswordRejected
                            | crate::integrity::TestFailure::PasswordIndeterminate
                    )
                ) =>
            {
                (true, Some(true))
            }
            Ok(result) => (result.ok, result.encrypted),
            Err(SmartZipError::WrongPassword { .. })
            | Err(SmartZipError::PasswordRequired { .. }) => (true, Some(true)),
            Err(SmartZipError::UnsupportedContainer { .. }) => (false, None),
            Err(error) => return Err(error),
        };
        Ok(ArchiveProbe {
            path: path.to_path_buf(),
            format: None,
            encrypted,
            supported,
        })
    }

    async fn probe_with_context(
        &self,
        path: &Path,
        context: std::sync::Arc<TaskExecutionContext>,
    ) -> Result<ArchiveProbe> {
        let token = context.cancellation_token();
        let request = TestRequest {
            archive: path.to_path_buf(),
            format: None,
            password: Some(String::new()),
            encoding: smartzip_core::EncodingMode::Auto,
        };
        let result = self.test_with_report_and_token(request, &token).await;
        let (supported, encrypted) = match result {
            Ok(seven_result) => {
                if seven_result.value.is_some() {
                    (true, seven_result.report.encrypted)
                } else {
                    // Mirror the error classification in `test()` ->
                    // `map_reported_failure()`. A 7zz report with
                    // `value == None` is not an `Err`; we must map it
                    // explicitly to distinguish WrongPassword /
                    // PasswordRequired (supported) from UnsupportedContainer.
                    let mapped = self.map_reported_failure(&seven_result, path);
                    match mapped {
                        SmartZipError::WrongPassword { .. }
                        | SmartZipError::PasswordRequired { .. } => (true, Some(true)),
                        SmartZipError::UnsupportedContainer { .. } => (false, None),
                        other => return Err(other),
                    }
                }
            }
            Err(SmartZipError::WrongPassword { .. })
            | Err(SmartZipError::PasswordRequired { .. }) => (true, Some(true)),
            Err(SmartZipError::UnsupportedContainer { .. }) => (false, None),
            Err(error) => return Err(error),
        };
        Ok(ArchiveProbe {
            path: path.to_path_buf(),
            format: None,
            encrypted,
            supported,
        })
    }

    async fn list(&self, request: ListRequest) -> Result<ArchiveListing> {
        let mut args: Vec<String> = vec!["l".into(), "-slt".into()];
        if let Some(pw) = Self::password_arg(&request.password) {
            args.push(pw);
        }
        if let Some(enc) = Self::encoding_arg(&request.encoding) {
            args.push(enc);
        }
        args.push(request.archive.to_string_lossy().into_owned());
        let output = self.run(&args).await?;
        if output.status != Some(0) {
            return Err(self.map_failure(&output, &request.archive));
        }
        let report = parse_slt_archive_report(&output.stdout);
        Ok(ArchiveListing {
            format: report.archive_type.as_deref().map(parse_archive_format),
            entries: parse_entries(&output.stdout),
        })
    }

    async fn list_with_context(
        &self,
        request: ListRequest,
        context: std::sync::Arc<TaskExecutionContext>,
    ) -> Result<ArchiveListing> {
        let token = context.cancellation_token();
        let mut args: Vec<String> = vec!["l".into(), "-slt".into()];
        if let Some(pw) = Self::password_arg(&request.password) {
            args.push(pw);
        }
        if let Some(enc) = Self::encoding_arg(&request.encoding) {
            args.push(enc);
        }
        args.push(request.archive.to_string_lossy().into_owned());
        let output = self.run_with_token(&args, &token).await?;
        if output.status != Some(0) {
            return Err(self.map_failure(&output, &request.archive));
        }
        let report = parse_slt_archive_report(&output.stdout);
        Ok(ArchiveListing {
            format: report.archive_type.as_deref().map(parse_archive_format),
            entries: parse_entries(&output.stdout),
        })
    }

    async fn test(&self, request: TestRequest) -> Result<TestResult> {
        self.test_with_context(
            request,
            std::sync::Arc::new(TaskExecutionContext::detached()),
        )
        .await
    }

    async fn test_with_context(
        &self,
        request: TestRequest,
        context: std::sync::Arc<TaskExecutionContext>,
    ) -> Result<TestResult> {
        let mut args = vec!["t".into(), "-bd".into(), "-bb1".into(), "-sccUTF-8".into()];
        // In test mode an explicit bare -p supplies an empty credential. With
        // no switch 7z prompts on stdin and reports EOF as exit 255 (cancelled).
        // Arguments go straight to the process; do not add shell quote bytes.
        args.push(format!(
            "-p{}",
            request.password.as_deref().unwrap_or_default()
        ));
        if let Some(encoding) = Self::encoding_arg(&request.encoding) {
            args.push(encoding);
        }
        args.push("--".into());
        args.push(request.archive.to_string_lossy().into_owned());
        let (output, truncated) = crate::test_output::run(
            &self.executable,
            &self.id,
            &args,
            &context.cancellation_token(),
        )
        .await?;
        // Definitive unsupported-method errors retain normal router fallback.
        // Integrity/password results remain executed reports, even on exit 2.
        if output.status != Some(0)
            && format!("{}\n{}", output.stdout, output.stderr)
                .to_ascii_lowercase()
                .contains("unsupported method")
        {
            return Err(self.map_failure(&output, &request.archive));
        }
        Ok(crate::test_output::report(
            &self.id,
            "7z",
            output,
            truncated,
            request.password.as_deref(),
        ))
    }

    async fn extract(&self, request: ExtractArchiveRequest) -> Result<ExtractArchiveResult> {
        let path = request.archive.clone();
        let result = self.extract_with_report(request).await?;
        if result.value.is_none() {
            return Err(self.map_reported_failure(&result, &path));
        }
        match result.value {
            Some(value) => Ok(value),
            None => unreachable!("checked above"),
        }
    }

    async fn extract_with_context(
        &self,
        request: ExtractArchiveRequest,
        context: std::sync::Arc<TaskExecutionContext>,
    ) -> Result<ExtractArchiveResult> {
        let token = context.cancellation_token();
        let path = request.archive.clone();
        let result = self.extract_with_report_and_token(request, &token).await?;
        if result.value.is_none() {
            return Err(self.map_reported_failure(&result, &path));
        }
        match result.value {
            Some(value) => Ok(value),
            None => unreachable!("checked above"),
        }
    }

    async fn compress(&self, request: CompressArchiveRequest) -> Result<CompressArchiveResult> {
        let mut args: Vec<String> = vec!["a".into()];
        if let Some(pw) = Self::password_arg(&request.password) {
            args.push(pw);
        }
        args.push(request.output.to_string_lossy().into_owned());
        args.extend(
            request
                .inputs
                .iter()
                .map(|input| input.to_string_lossy().into_owned()),
        );
        let output = self.run(&args).await?;
        if output.status != Some(0) {
            return Err(self.map_failure(&output, &request.output));
        }
        Ok(CompressArchiveResult {
            output: request.output,
        })
    }

    async fn compress_with_context(
        &self,
        request: CompressArchiveRequest,
        context: std::sync::Arc<TaskExecutionContext>,
    ) -> Result<CompressArchiveResult> {
        let token = context.cancellation_token();
        let mut args: Vec<String> = vec!["a".into()];
        if let Some(pw) = Self::password_arg(&request.password) {
            args.push(pw);
        }
        args.push(request.output.to_string_lossy().into_owned());
        args.extend(
            request
                .inputs
                .iter()
                .map(|input| input.to_string_lossy().into_owned()),
        );
        let output = self.run_with_token(&args, &token).await?;
        if output.status != Some(0) {
            return Err(self.map_failure(&output, &request.output));
        }
        Ok(CompressArchiveResult {
            output: request.output,
        })
    }

    fn capabilities(&self) -> smartzip_core::AdapterCapabilities {
        crate::router::seven_zip_capabilities()
    }
}

fn extract_unsupported_method(output: &str) -> Option<String> {
    output.lines().find_map(|line| {
        let lower = line.to_ascii_lowercase();
        let index = lower.find("unsupported method")?;
        let value = line[index + "unsupported method".len()..]
            .trim_start_matches([' ', ':', '='])
            .trim();
        (!value.is_empty()
            && !value
                .chars()
                .any(|character| character.is_ascii_lowercase())
            && value.chars().all(|character| {
                character.is_ascii_uppercase()
                    || character.is_ascii_digit()
                    || matches!(character, '-' | '_' | ':')
            }))
        .then(|| value.to_ascii_lowercase())
    })
}

fn parse_entries(stdout: &str) -> Vec<ArchiveEntry> {
    let mut entries = Vec::new();
    let mut current_path: Option<PathBuf> = None;
    let mut current_size: Option<u64> = None;
    let mut current_packed_size: Option<u64> = None;
    let mut current_is_dir = false;
    let mut current_is_archive = false;

    for line in stdout.split_terminator('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if let Some(path) = line.strip_prefix("Path = ") {
            if let Some(path) = current_path.take() {
                if !current_is_archive {
                    entries.push(ArchiveEntry {
                        path,
                        raw_name: Vec::new(),
                        compressed_size: current_packed_size,
                        uncompressed_size: current_size,
                        is_dir: current_is_dir,
                    });
                }
            }
            current_path = Some(PathBuf::from(path));
            current_size = None;
            current_packed_size = None;
            current_is_dir = false;
            current_is_archive = false;
        } else if let Some(_type) = line.strip_prefix("Type = ") {
            current_is_archive = true;
        } else if let Some(size) = line.strip_prefix("Size = ") {
            current_size = size.parse().ok();
        } else if let Some(size) = line.strip_prefix("Packed Size = ") {
            current_packed_size = size.parse().ok();
        } else if let Some(attributes) = line.strip_prefix("Attributes = ") {
            current_is_dir = attributes.contains('D');
        }
    }

    if let Some(path) = current_path.take() {
        if !current_is_archive {
            entries.push(ArchiveEntry {
                path,
                raw_name: Vec::new(),
                compressed_size: current_packed_size,
                uncompressed_size: current_size,
                is_dir: current_is_dir,
            });
        }
    }

    entries
}

const MAX_RECORD: usize = 64 * 1024;

async fn read_stream<R: tokio::io::AsyncRead + Unpin>(
    mut reader: R,
    observer: Option<Observer>,
    operation: Option<SevenZipOperation>,
) -> std::io::Result<Vec<u8>> {
    let mut raw = Vec::new();
    let mut record = Vec::new();
    let mut oversized = false;
    let mut chunk = [0_u8; 4096];
    loop {
        let count = reader.read(&mut chunk).await?;
        if count == 0 {
            break;
        }
        raw.extend_from_slice(&chunk[..count]);
        for &byte in &chunk[..count] {
            if matches!(byte, b'\r' | b'\n' | 0x08) {
                if !oversized {
                    emit_record(&record, observer.as_ref(), operation);
                }
                record.clear();
                oversized = false;
            } else if record.len() < MAX_RECORD {
                record.push(byte);
            } else {
                oversized = true;
            }
        }
    }
    if !oversized {
        emit_record(&record, observer.as_ref(), operation);
    }
    Ok(raw)
}

fn emit_record(record: &[u8], observer: Option<&Observer>, operation: Option<SevenZipOperation>) {
    let Some(observer) = observer else {
        return;
    };
    let text = String::from_utf8_lossy(record);
    if let (Some(operation), Some((percent, item))) = (operation, parse_progress(&text)) {
        observer(SevenZipEvent::Progress {
            operation,
            percent,
            item,
        });
    } else if let Some(severity) = diagnostic_severity(&text) {
        observer(SevenZipEvent::Diagnostic {
            severity,
            text: text.trim().to_owned(),
        });
    }
}

fn parse_progress(line: &str) -> Option<(f32, Option<String>)> {
    let line = line.trim();
    let percent_end = line.find('%')?;
    let number = line[..percent_end].trim();
    if number.is_empty() || !number.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let percent = number.parse::<f32>().ok()?.clamp(0.0, 100.0);
    let mut remainder = line[percent_end + 1..].trim();
    if let Some((index, item)) = remainder.split_once(" - ") {
        if index.bytes().all(|byte| byte.is_ascii_digit()) {
            remainder = item.trim();
        }
    } else {
        remainder = remainder.trim_start_matches(['-', ' ']).trim();
    }
    if remainder.contains("Everything is Ok") || remainder.chars().any(char::is_control) {
        return None;
    }
    let item = (!remainder.is_empty()).then(|| remainder.to_owned());
    Some((percent, item))
}

fn diagnostic_severity(line: &str) -> Option<SevenZipDiagnosticSeverity> {
    let lower = line.trim().to_ascii_lowercase();
    if lower.starts_with("warning:") || lower.starts_with("warning ") {
        Some(SevenZipDiagnosticSeverity::Warning)
    } else if lower.starts_with("error:")
        || lower.starts_with("error ")
        || lower.contains("data error")
        || lower.contains("crc failed")
    {
        Some(SevenZipDiagnosticSeverity::Error)
    } else {
        None
    }
}

fn parse_report(output: &str) -> SevenZipReport {
    let mut report = SevenZipReport::default();
    for line in output
        .split(['\r', '\n', '\u{8}'])
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        match diagnostic_severity(line) {
            Some(SevenZipDiagnosticSeverity::Warning) => report.warnings.push(line.to_owned()),
            Some(SevenZipDiagnosticSeverity::Error) => report.errors.push(line.to_owned()),
            None => {}
        }
        let Some((key, value)) = line
            .split_once(" = ")
            .or_else(|| line.split_once(':').map(|(key, value)| (key, value.trim())))
        else {
            continue;
        };
        match key {
            "Type" => report.archive_type = Some(value.to_owned()),
            "Physical Size" => report.physical_size = value.parse().ok(),
            "Encrypted" => {
                report.encrypted = match value {
                    "+" | "1" | "true" => Some(true),
                    "-" | "0" | "false" => Some(false),
                    _ => None,
                }
            }
            "Files" => report.files = value.parse().ok(),
            "Folders" => report.folders = value.parse().ok(),
            "Size" => report.unpacked_size = value.parse().ok(),
            "Compressed" | "Packed Size" => report.compressed_size = value.parse().ok(),
            "Elapsed Time" | "Global Time" => report.elapsed_millis = parse_elapsed_millis(value),
            _ => {}
        }
    }
    report
}

fn parse_slt_archive_report(output: &str) -> SevenZipReport {
    let mut record = String::new();
    for line in output.split_terminator('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.is_empty() {
            if record.lines().any(|line| line.starts_with("Type = ")) {
                return parse_report(&record);
            }
            record.clear();
        } else {
            if !record.is_empty() {
                record.push('\n');
            }
            record.push_str(line);
        }
    }
    if record.lines().any(|line| line.starts_with("Type = ")) {
        parse_report(&record)
    } else {
        SevenZipReport::default()
    }
}

fn parse_elapsed_millis(value: &str) -> Option<u64> {
    let seconds = value.trim_end_matches(" sec").parse::<f64>().ok()?;
    (seconds.is_finite() && seconds >= 0.0).then(|| (seconds * 1000.0).round() as u64)
}

fn parse_archive_format(value: &str) -> ArchiveFormat {
    match value.to_ascii_lowercase().as_str() {
        "zip" => ArchiveFormat::Zip,
        "7z" => ArchiveFormat::SevenZip,
        "rar" => ArchiveFormat::Rar,
        "tar" => ArchiveFormat::Tar,
        "gzip" | "gz" => ArchiveFormat::Gzip,
        "bzip2" | "bz2" => ArchiveFormat::Bzip2,
        "xz" => ArchiveFormat::Xz,
        "zstd" | "zst" => ArchiveFormat::Zstd,
        "cab" => ArchiveFormat::Cab,
        other => ArchiveFormat::Unknown(other.to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn fake_executable(script: &str) -> (tempfile::TempDir, PathBuf) {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("fake-7z");
        std::fs::write(&path, format!("#!/bin/sh\nset -eu\n{script}\n")).unwrap();
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&path, permissions).unwrap();
        (root, path)
    }

    #[test]
    fn locator_finds_bundled_path() {
        let path = std::env::current_exe().unwrap();
        let locator = SevenZipLocator::bundled(path.clone());
        assert_eq!(locator.locate(), Some(path));
    }

    #[test]
    fn parses_slt_entries() {
        let stdout = "Path = archive.zip\nType = zip\n\nPath = file.txt\nSize = 42\nPacked Size = 21\nAttributes = A\n\nPath = dir\nSize = 0\nAttributes = D\n";
        let entries = parse_entries(stdout);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].path, PathBuf::from("file.txt"));
        assert_eq!(entries[0].uncompressed_size, Some(42));
        assert_eq!(entries[0].compressed_size, Some(21));
        assert!(entries[1].is_dir);
    }

    #[test]
    fn parses_only_well_formed_progress() {
        assert_eq!(
            parse_progress(" 42% - 目录/file name.txt "),
            Some((42.0, Some("目录/file name.txt".into())))
        );
        assert_eq!(parse_progress("123%"), Some((100.0, None)));
        assert_eq!(
            parse_progress("42% 1 - modern 26.01/路径.txt"),
            Some((42.0, Some("modern 26.01/路径.txt".into())))
        );
        for invalid in [
            "Everything is Ok",
            "-1% file",
            "1.5% file",
            "% file",
            "warning: 5%",
        ] {
            assert_eq!(parse_progress(invalid), None, "{invalid}");
        }
    }

    #[tokio::test]
    async fn framing_handles_split_utf8_and_final_record() {
        let bytes = "10% - 你好.txt\r55% - second file\n100% - done".as_bytes();
        for split in 0..=bytes.len() {
            let (mut writer, reader) = tokio::io::duplex(128);
            let events = Arc::new(std::sync::Mutex::new(Vec::new()));
            let captured = Arc::clone(&events);
            let observer: Observer = Arc::new(move |event| captured.lock().unwrap().push(event));
            let task = tokio::spawn(read_stream(
                reader,
                Some(observer),
                Some(SevenZipOperation::Test),
            ));
            tokio::io::AsyncWriteExt::write_all(&mut writer, &bytes[..split])
                .await
                .unwrap();
            tokio::io::AsyncWriteExt::write_all(&mut writer, &bytes[split..])
                .await
                .unwrap();
            drop(writer);
            task.await.unwrap().unwrap();
            assert_eq!(events.lock().unwrap().len(), 3, "split {split}");
        }
    }

    #[tokio::test]
    async fn framing_handles_p7zip_backspace_overwrites_without_garbage() {
        let bytes = b"  0M Scan foo\x08\x08\x08  0% 1 - first.txt\x08\x08 42% 2 - second file.txt\x08100%\x08Everything is Ok";
        let (mut writer, reader) = tokio::io::duplex(256);
        let events = Arc::new(std::sync::Mutex::new(Vec::new()));
        let captured = Arc::clone(&events);
        let observer: Observer = Arc::new(move |event| captured.lock().unwrap().push(event));
        let task = tokio::spawn(read_stream(
            reader,
            Some(observer),
            Some(SevenZipOperation::Extract),
        ));
        tokio::io::AsyncWriteExt::write_all(&mut writer, bytes)
            .await
            .unwrap();
        drop(writer);
        task.await.unwrap().unwrap();
        let events = events.lock().unwrap();
        assert!(events.iter().any(|event| matches!(event, SevenZipEvent::Progress { percent, item: Some(item), .. } if *percent == 42.0 && item == "second file.txt")));
        assert!(!events
            .iter()
            .any(|event| format!("{event:?}").contains("Everything is Ok")));
    }

    #[tokio::test]
    async fn oversized_records_are_discarded_instead_of_emitting_prefixes() {
        let mut bytes = b"42% - ".to_vec();
        bytes.resize(MAX_RECORD + 10, b'x');
        bytes.push(b'\n');
        let (mut writer, reader) = tokio::io::duplex(bytes.len() + 1);
        let events = Arc::new(std::sync::Mutex::new(Vec::new()));
        let captured = Arc::clone(&events);
        let observer: Observer = Arc::new(move |event| captured.lock().unwrap().push(event));
        let task = tokio::spawn(read_stream(
            reader,
            Some(observer),
            Some(SevenZipOperation::Test),
        ));
        tokio::io::AsyncWriteExt::write_all(&mut writer, &bytes)
            .await
            .unwrap();
        drop(writer);
        task.await.unwrap().unwrap();
        assert!(events.lock().unwrap().is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn fake_child_delivers_before_completion_drains_both_pipes_and_flushes_tail() {
        let temp = tempfile::tempdir().unwrap();
        let release = temp.path().join("observer-released-child");
        let script = format!(
            "printf '%s\\r' '25% 1 - early.txt'\nwaits=0\nwhile [ ! -f '{}' ] && [ $waits -lt 100 ]; do sleep 0.01; waits=$((waits + 1)); done\n[ -f '{}' ] || exit 99\ni=0\nwhile [ $i -lt 6000 ]; do printf 'stdout-padding-%04d\\n' \"$i\"; printf 'stderr-padding-%04d\\n' \"$i\" >&2; i=$((i + 1)); done\nprintf '%s\\rFiles: 2\\nGlobal Time = 0.010 sec\\n' '100% 2 - final.txt'\nprintf 'WARNING: final diagnostic' >&2",
            release.display(),
            release.display()
        );
        let (_root, executable) = fake_executable(&script);
        let release_from_observer = release.clone();
        let events = Arc::new(std::sync::Mutex::new(Vec::new()));
        let captured = Arc::clone(&events);
        let backend = SevenZipBackend::new(executable).with_observer(move |event| {
            if matches!(event, SevenZipEvent::Progress { percent, .. } if percent == 25.0) {
                std::fs::write(&release_from_observer, b"go").unwrap();
            }
            captured.lock().unwrap().push(event);
        });
        let request = TestRequest {
            archive: PathBuf::from("fixture.7z"),
            format: None,
            password: None,
            encoding: smartzip_core::EncodingMode::Auto,
        };
        let result = backend.test_with_report(request).await.unwrap();
        assert_eq!(result.status, SevenZipExitStatus::Success);
        assert!(result.value.is_some());
        assert_eq!(result.report.files, Some(2));
        assert_eq!(result.report.elapsed_millis, Some(10));
        assert!(result.stderr.ends_with("WARNING: final diagnostic"));
        assert!(result
            .report
            .warnings
            .iter()
            .any(|line| line == "WARNING: final diagnostic"));
        assert!(events.lock().unwrap().iter().any(|event| matches!(event, SevenZipEvent::Progress { percent, item: Some(item), .. } if *percent == 100.0 && item == "final.txt")));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn warning_exit_preserves_report_and_raw_diagnostics_without_success_value() {
        let (_root, executable) = fake_executable(
            "printf '%s\\rFiles: 1\\n' '50% 1 - partial.txt'\nprintf 'WARNING: file could not be opened' >&2\nexit 1",
        );
        let backend = SevenZipBackend::new(executable);
        let result = backend
            .test_with_report(TestRequest {
                archive: PathBuf::from("fixture.7z"),
                format: None,
                password: None,
                encoding: smartzip_core::EncodingMode::Auto,
            })
            .await
            .unwrap();
        assert_eq!(result.status, SevenZipExitStatus::Warning);
        assert!(result.value.is_none());
        assert_eq!(result.report.files, Some(1));
        assert!(result.stderr.contains("file could not be opened"));
        assert_eq!(
            result.report.warnings,
            ["WARNING: file could not be opened"]
        );
    }

    #[test]
    fn parses_report_without_fabricating_missing_fields() {
        let report = parse_report("Type = 7z\nPhysical Size = 120\nEncrypted = +\nFiles = 3\nFolders = 1\nSize = 400\nCompressed = 120\nElapsed Time = 0.125 sec\nWARNING: one file skipped\n");
        assert_eq!(report.archive_type.as_deref(), Some("7z"));
        assert_eq!(report.physical_size, Some(120));
        assert_eq!(report.encrypted, Some(true));
        assert_eq!(report.files, Some(3));
        assert_eq!(report.elapsed_millis, Some(125));
        assert_eq!(report.warnings, ["WARNING: one file skipped"]);
        assert!(report.errors.is_empty());
    }

    #[test]
    fn parses_colon_summaries_and_global_time() {
        let report = parse_report(
            "Size: 400\nCompressed: 120\nFiles: 3\nFolders: 2\nGlobal Time = 0.250 sec\n",
        );
        assert_eq!(report.unpacked_size, Some(400));
        assert_eq!(report.compressed_size, Some(120));
        assert_eq!(report.files, Some(3));
        assert_eq!(report.folders, Some(2));
        assert_eq!(report.elapsed_millis, Some(250));
    }

    #[test]
    fn slt_entry_metadata_does_not_override_archive_header() {
        let output = "Path = archive.7z\nType = 7z\nPhysical Size = 100\nEncrypted = -\n\nPath = secret.txt\nSize = 200\nPacked Size = 50\nEncrypted = +\n";
        let report = parse_slt_archive_report(output);
        assert_eq!(report.archive_type.as_deref(), Some("7z"));
        assert_eq!(report.physical_size, Some(100));
        assert_eq!(report.encrypted, Some(false));
    }

    #[test]
    fn parses_crlf_slt_without_trimming_legitimate_path_spaces() {
        let output = "Path = archive.7z\r\nType = 7z\r\nPhysical Size = 100\r\nEncrypted = -\r\n\r\nPath = name with trailing space \r\nSize = 200\r\nPacked Size = 50\r\nAttributes = A\r\nEncrypted = +\r\n\r\nPath = folder\r\nSize = 0\r\nAttributes = D\r\n";
        let entries = parse_entries(output);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].path, PathBuf::from("name with trailing space "));
        assert_eq!(entries[0].uncompressed_size, Some(200));
        assert_eq!(entries[0].compressed_size, Some(50));
        assert!(!entries[0].is_dir);
        assert_eq!(entries[1].path, PathBuf::from("folder"));
        assert!(entries[1].is_dir);

        let report = parse_slt_archive_report(output);
        assert_eq!(report.archive_type.as_deref(), Some("7z"));
        assert_eq!(report.physical_size, Some(100));
        assert_eq!(report.encrypted, Some(false));
    }

    #[test]
    fn classifies_specific_failures_before_generic_statuses() {
        let backend = SevenZipBackend::new("7z".into());
        let path = Path::new("archive.7z");
        let output = |status, text: &str| BackendCommandOutput {
            status: Some(status),
            stdout: String::new(),
            stderr: text.into(),
        };
        assert!(matches!(
            backend.map_failure(&output(2, "ERROR: Wrong password"), path),
            SmartZipError::WrongPassword { .. }
        ));
        for (code, expected) in [
            (0, SevenZipExitStatus::Success),
            (1, SevenZipExitStatus::Warning),
            (2, SevenZipExitStatus::Fatal),
            (7, SevenZipExitStatus::CommandLineError),
            (8, SevenZipExitStatus::OutOfMemory),
            (255, SevenZipExitStatus::Cancelled),
        ] {
            assert_eq!(SevenZipExitStatus::from_code(Some(code)), expected);
        }
        assert!(matches!(
            backend.map_failure(&output(2, "ERROR: Password is required"), path),
            SmartZipError::PasswordRequired { .. }
        ));
        assert!(matches!(
            backend.map_failure(&output(2, "ERROR: CRC Failed"), path),
            SmartZipError::CorruptedArchive { .. }
        ));
        assert!(matches!(
            backend.map_failure(&output(2, "ERROR: Unsupported Method : ZSTD"), path),
            SmartZipError::UnsupportedCodec { codec: Some(ref codec), .. } if codec == "zstd"
        ));
        assert!(matches!(
            backend.map_failure(&output(2, "ERROR: Unsupported Method : payload.txt"), path),
            SmartZipError::UnsupportedCodec { codec: None, .. }
        ));
        assert!(matches!(
            backend.map_failure(&output(255, "Break signaled"), path),
            SmartZipError::Cancelled
        ));
        assert!(matches!(
            backend.map_failure(&output(7, "Command Line Error"), path),
            SmartZipError::BackendFailed {
                exit_code: Some(7),
                ..
            }
        ));
        assert!(matches!(
            backend.map_failure(&output(8, "Not enough memory"), path),
            SmartZipError::BackendFailed {
                exit_code: Some(8),
                ..
            }
        ));
        match backend.map_failure(&output(2, "ERROR: No such file or directory"), path) {
            SmartZipError::Io {
                path: error_path,
                source,
            } => {
                assert_eq!(error_path.as_deref(), Some(path));
                assert_eq!(source.kind(), std::io::ErrorKind::NotFound);
                assert!(source.to_string().contains("No such file"));
            }
            other => panic!("expected not-found I/O error, got {other:?}"),
        }
        match backend.map_failure(&output(2, "ERROR: Access is denied"), path) {
            SmartZipError::Io {
                path: error_path,
                source,
            } => {
                assert_eq!(error_path.as_deref(), Some(path));
                assert_eq!(source.kind(), std::io::ErrorKind::PermissionDenied);
                assert!(source.to_string().contains("Access is denied"));
            }
            other => panic!("expected permission I/O error, got {other:?}"),
        }
        assert!(matches!(
            backend.map_failure(&output(2, "ERROR: Can not open file as archive"), path),
            SmartZipError::UnsupportedContainer { .. }
        ));
    }

    #[test]
    fn empty_password_is_passed_explicitly() {
        assert_eq!(
            SevenZipBackend::password_arg(&Some(String::new())),
            Some("-p\"\"".into())
        );
    }

    #[test]
    fn capabilities_declare_zstd_extract_support() {
        let backend = SevenZipBackend::new(PathBuf::from("7z"));
        let capabilities = backend.capabilities();
        assert!(capabilities.supports(
            smartzip_core::ArchiveOperation::Extract,
            Some(&ArchiveFormat::Zstd),
        ));
    }

    #[test]
    fn parses_zstd_archive_type() {
        assert_eq!(parse_archive_format("zstd"), ArchiveFormat::Zstd);
    }

    #[test]
    fn maps_supported_encoding_overrides_to_code_pages() {
        use smartzip_core::EncodingMode;

        assert_eq!(
            SevenZipBackend::encoding_arg(&EncodingMode::Override("gbk".into())),
            Some("-scs936".into())
        );
        assert_eq!(
            SevenZipBackend::encoding_arg(&EncodingMode::Override("EUC-KR".into())),
            Some("-scs949".into())
        );
        assert_eq!(
            SevenZipBackend::encoding_arg(&EncodingMode::Override("Shift_JIS".into())),
            Some("-scs932".into())
        );
    }

    #[tokio::test]
    async fn probe_handles_encrypted_archives_without_prompting() {
        let root = std::env::temp_dir().join(format!("smartzip-probe-{}", std::process::id()));
        let archive = root.join("secret.7z");
        let file = root.join("hello.txt");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(&file, b"hello").unwrap();

        let status = std::process::Command::new("7z")
            .arg("a")
            .arg("-psecret")
            .arg(&archive)
            .arg(&file)
            .current_dir(&root)
            .status()
            .unwrap();
        assert!(status.success(), "7z must be available in PATH");

        let backend =
            SevenZipBackend::locate(&SevenZipLocator::default()).expect("7z/7zz must be available");
        let probe = backend.probe(&archive).await.unwrap();

        assert!(probe.supported);
        assert_eq!(probe.encrypted, Some(true));
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn probe_with_context_encrypted_archive_is_supported() {
        let root = std::env::temp_dir().join(format!("smartzip-probe-ctx-{}", std::process::id()));
        let archive = root.join("secret2.7z");
        let file = root.join("hello2.txt");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(&file, b"hello2").unwrap();
        let status = std::process::Command::new("7z")
            .arg("a")
            .arg("-psecret2")
            .arg(&archive)
            .arg(&file)
            .current_dir(&root)
            .status()
            .unwrap();
        assert!(status.success(), "7z must be available");
        let backend =
            SevenZipBackend::locate(&SevenZipLocator::default()).expect("7z/7zz must be available");
        let ctx = std::sync::Arc::new(smartzip_core::TaskExecutionContext::detached());
        let probe = backend.probe_with_context(&archive, ctx).await.unwrap();
        assert!(
            probe.supported,
            "probe_with_context should report supported for encrypted archive"
        );
        assert_eq!(probe.encrypted, Some(true));
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn probe_with_context_invalid_archive_is_unsupported() {
        let temp = tempfile::tempdir().unwrap();
        let bogus = temp.path().join("bogus.7z");
        std::fs::write(&bogus, b"not an archive at all").unwrap();
        let backend = SevenZipBackend::new(std::path::PathBuf::from("7z"));
        let ctx = std::sync::Arc::new(smartzip_core::TaskExecutionContext::detached());
        let probe = backend.probe_with_context(&bogus, ctx).await.unwrap();
        assert!(
            !probe.supported,
            "probe_with_context should report unsupported for invalid archive"
        );
        assert_eq!(probe.encrypted, None);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cancellation_kills_process_group_and_stops_writing() {
        let temp = tempfile::tempdir().unwrap();
        let output_dir = temp.path().join("output");
        std::fs::create_dir_all(&output_dir).unwrap();
        let archive = temp.path().join("dummy.7z");
        std::fs::write(&archive, b"dummy").unwrap();
        // Fake 7z that spawns a descendant and writes continuously to the
        // known output directory. The script is intentionally long-running
        // so we can cancel it. The output path is hardcoded to avoid
        // parsing -o (which would be fragile in the test).
        let script = format!(
            r#"
output="{}"
mkdir -p "$output"
# descendant: a sleep that should be killed with the process group
sh -c 'sleep 30' &
echo $! > "$output/descendant.pid"
# background writer that would continue if not killed
(
  i=0
  while [ $i -lt 1000 ]; do
    echo "data $i" >> "$output/output.txt"
    sleep 0.05
    i=$((i+1))
  done
) &
writer_pid=$!
wait $writer_pid 2>/dev/null || true
"#,
            output_dir.display()
        );
        let (_tmp, executable) = fake_executable(&script);
        let backend = SevenZipBackend::new(executable);

        let ctx = std::sync::Arc::new(smartzip_core::TaskExecutionContext::detached());
        let token = ctx.cancellation_token();
        let request = ExtractArchiveRequest {
            archive: archive.clone(),
            format: Some(ArchiveFormat::SevenZip),
            output_dir: output_dir.clone(),
            password: None,
            encoding: smartzip_core::EncodingMode::Auto,
        };
        let backend_clone = backend.clone();
        let ctx_clone = std::sync::Arc::clone(&ctx);
        let fut = tokio::spawn(async move {
            backend_clone
                .extract_with_context(request, std::sync::Arc::clone(&ctx_clone))
                .await
        });
        // Let the child start and write a bit; wait up to 2s for the file to appear.
        let mut size_before = 0;
        for _ in 0..20 {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            size_before = std::fs::metadata(output_dir.join("output.txt"))
                .map(|m| m.len())
                .unwrap_or(0);
            if size_before > 0 {
                break;
            }
        }
        assert!(size_before > 0, "child should have written before cancel");
        token.cancel();
        let result = fut.await.unwrap();
        assert!(
            matches!(result, Err(SmartZipError::Cancelled)),
            "expected Cancelled, got {result:?}"
        );
        // After cancellation the backend guarantees the process tree is
        // stopped, so the file must not grow and the descendant must be gone.
        let size_after = std::fs::metadata(output_dir.join("output.txt"))
            .map(|m| m.len())
            .unwrap_or(0);
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        let size_later = std::fs::metadata(output_dir.join("output.txt"))
            .map(|m| m.len())
            .unwrap_or(0);
        assert_eq!(
            size_after, size_later,
            "file should not grow after cancellation"
        );
        // On Unix the process group kill should have terminated the sleep
        // descendant. Verify via kill -0 -> ESRCH.
        #[cfg(unix)]
        {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            let pid_str = std::fs::read_to_string(output_dir.join("descendant.pid"))
                .expect("descendant.pid should have been written by fake 7z");
            let pid = pid_str
                .trim()
                .parse::<i32>()
                .expect("descendant.pid should contain a valid pid");
            let status = std::process::Command::new("kill")
                .arg("-0")
                .arg(pid.to_string())
                .status()
                .unwrap();
            assert!(
                !status.success(),
                "descendant pid {pid} should have been killed by process group; still alive"
            );
        }
        // Deterministic cleanup: the attempt dir (here output_dir) must be
        // removable immediately after Cancelled.
        assert!(
            std::fs::remove_dir_all(&output_dir).is_ok(),
            "output dir should be removable after cancellation"
        );
    }
}
