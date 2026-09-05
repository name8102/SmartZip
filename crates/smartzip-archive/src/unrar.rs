use crate::backend::ArchiveAdapter;
use crate::test_output::collect_bounded_output;
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
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone)]
pub struct UnrarLocator {
    bundled: Option<PathBuf>,
    candidates: Vec<String>,
}

impl Default for UnrarLocator {
    fn default() -> Self {
        Self {
            bundled: None,
            candidates: vec!["unrar".into()],
        }
    }
}

impl UnrarLocator {
    pub fn bundled(path: PathBuf) -> Self {
        Self {
            bundled: Some(path),
            ..Default::default()
        }
    }

    pub fn locate(&self) -> Option<PathBuf> {
        locate_executable(self.bundled.as_ref(), &self.candidates)
    }
}

#[derive(Debug, Clone)]
pub struct UnrarBackend {
    id: String,
    executable: PathBuf,
}

impl UnrarBackend {
    pub fn new(executable: PathBuf) -> Self {
        let id = format!("unrar:{}", executable.display());
        Self { id, executable }
    }

    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.id = id.into();
        self
    }

    pub fn locate(locator: &UnrarLocator) -> Result<Self> {
        locator
            .locate()
            .map(Self::new)
            .ok_or_else(|| SmartZipError::BackendUnavailable {
                backend: "unrar".into(),
            })
    }

    pub fn executable(&self) -> &Path {
        &self.executable
    }

    async fn validate_before_extract(
        &self,
        archive: &Path,
        password: &Option<String>,
        token: &CancellationToken,
    ) -> Result<Option<bool>> {
        let args = vec![
            "lt".into(),
            "-cfg-".into(),
            "-c-".into(),
            Self::password_arg(password),
            "--".into(),
            archive.to_string_lossy().into_owned(),
        ];
        let output = self.run_with_token(&args, token).await?;
        if output.status != Some(0) {
            return Err(crate::test_output::password_error(
                &output,
                "unrar",
                password.as_deref(),
                archive,
            )
            .unwrap_or_else(|| self.map_failure(&output, archive)));
        }
        validate_extraction_listing(&output.stdout)?;
        let encrypted = output.stdout.lines().any(|line| {
            line.trim_start()
                .strip_prefix("Flags:")
                .is_some_and(|flags| flags.split_whitespace().any(|flag| flag == "encrypted"))
        });
        Ok(Some(encrypted))
    }

    async fn run(&self, args: &[String]) -> Result<BackendCommandOutput> {
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

        let mut child = wrap.spawn().map_err(|source| {
            if source.kind() == std::io::ErrorKind::NotFound {
                SmartZipError::BackendUnavailable {
                    backend: self.id.clone(),
                }
            } else {
                SmartZipError::io(Some(self.executable.clone()), source)
            }
        })?;
        // Keep pipes for reading; on cancel we kill the whole group and wait.
        let stdout = child.stdout().take();
        let stderr = child.stderr().take();
        let stdout_task =
            stdout.map(|stream| tokio::spawn(crate::test_output::bounded_read(stream)));
        let stderr_task =
            stderr.map(|stream| tokio::spawn(crate::test_output::bounded_read(stream)));
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
        let stdout = collect_bounded_output(stdout_task).await?;
        let stderr = collect_bounded_output(stderr_task).await?;
        Ok(BackendCommandOutput {
            status: status.code(),
            stdout: String::from_utf8_lossy(&stdout).into_owned(),
            stderr: String::from_utf8_lossy(&stderr).into_owned(),
        })
    }

    fn password_arg(password: &Option<String>) -> String {
        match password {
            Some(password) => format!("-p{password}"),
            None => "-p-".into(),
        }
    }

    fn map_failure(&self, output: &BackendCommandOutput, path: &Path) -> SmartZipError {
        let combined = format!("{}\n{}", output.stdout, output.stderr);
        let lower = crate::test_output::diagnostic_text(&combined, "unrar");
        if lower.contains("wrong password") || lower.contains("incorrect password") {
            SmartZipError::WrongPassword {
                path: path.to_path_buf(),
            }
        } else if lower.contains("password") && lower.contains("required") {
            SmartZipError::PasswordRequired {
                path: path.to_path_buf(),
            }
        } else if lower.contains("unknown method") {
            SmartZipError::UnsupportedCodec {
                backend: self.id.clone(),
                path: path.to_path_buf(),
                codec: None,
            }
        } else if lower.contains("unsupported") || lower.contains("not rar archive") {
            SmartZipError::UnsupportedContainer {
                backend: self.id.clone(),
                path: path.to_path_buf(),
                container: Some("rar".into()),
            }
        } else if lower.contains("checksum") || lower.contains("corrupt") {
            SmartZipError::CorruptedArchive {
                path: path.to_path_buf(),
                detail: combined,
            }
        } else {
            SmartZipError::BackendFailed {
                backend: self.id.clone(),
                exit_code: output.status,
                stderr: combined,
            }
        }
    }
}

#[async_trait]
impl ArchiveAdapter for UnrarBackend {
    fn id(&self) -> &str {
        &self.id
    }
    fn executable_path(&self) -> Option<&Path> {
        Some(&self.executable)
    }
    fn diagnostic_family(&self) -> Option<&'static str> {
        Some("unrar")
    }

    async fn probe(&self, path: &Path) -> Result<ArchiveProbe> {
        let result = self
            .test(TestRequest {
                archive: path.to_path_buf(),
                format: Some(ArchiveFormat::Rar),
                password: None,
                encoding: smartzip_core::EncodingMode::Auto,
            })
            .await;
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
            format: Some(ArchiveFormat::Rar),
            encrypted,
            supported,
        })
    }

    async fn probe_with_context(
        &self,
        path: &Path,
        context: std::sync::Arc<TaskExecutionContext>,
    ) -> Result<ArchiveProbe> {
        let result = self
            .test_with_context(
                TestRequest {
                    archive: path.to_path_buf(),
                    format: Some(ArchiveFormat::Rar),
                    password: None,
                    encoding: smartzip_core::EncodingMode::Auto,
                },
                context,
            )
            .await;
        let (supported, encrypted) = match result {
            Ok(result) => (result.ok, result.encrypted),
            Err(SmartZipError::WrongPassword { .. })
            | Err(SmartZipError::PasswordRequired { .. }) => (true, Some(true)),
            Err(SmartZipError::UnsupportedContainer { .. }) => (false, None),
            Err(error) => return Err(error),
        };
        Ok(ArchiveProbe {
            path: path.to_path_buf(),
            format: Some(ArchiveFormat::Rar),
            encrypted,
            supported,
        })
    }

    async fn list(&self, request: ListRequest) -> Result<ArchiveListing> {
        let args = vec![
            "lb".to_string(),
            "-idq".to_string(),
            Self::password_arg(&request.password),
            request.archive.to_string_lossy().into_owned(),
        ];
        let output = self.run(&args).await?;
        if output.status != Some(0) {
            return Err(self.map_failure(&output, &request.archive));
        }

        let entries = output
            .stdout
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| ArchiveEntry {
                path: PathBuf::from(line),
                raw_name: Vec::new(),
                compressed_size: None,
                uncompressed_size: None,
                is_dir: line.ends_with('/') || line.ends_with('\\'),
            })
            .collect();

        Ok(ArchiveListing {
            format: Some(ArchiveFormat::Rar),
            entries,
        })
    }

    async fn list_with_context(
        &self,
        request: ListRequest,
        context: std::sync::Arc<TaskExecutionContext>,
    ) -> Result<ArchiveListing> {
        let token = context.cancellation_token();
        let args = vec![
            "lb".to_string(),
            "-idq".to_string(),
            Self::password_arg(&request.password),
            request.archive.to_string_lossy().into_owned(),
        ];
        let output = self.run_with_token(&args, &token).await?;
        if output.status != Some(0) {
            return Err(self.map_failure(&output, &request.archive));
        }
        let entries = output
            .stdout
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| ArchiveEntry {
                path: PathBuf::from(line),
                raw_name: Vec::new(),
                compressed_size: None,
                uncompressed_size: None,
                is_dir: line.ends_with('/') || line.ends_with('\\'),
            })
            .collect();
        Ok(ArchiveListing {
            format: Some(ArchiveFormat::Rar),
            entries,
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
        if request
            .format
            .as_ref()
            .is_some_and(|format| *format != ArchiveFormat::Rar)
        {
            return Err(SmartZipError::UnsupportedContainer {
                backend: self.id.clone(),
                path: request.archive,
                container: request.format.map(|format| format.as_str().to_string()),
            });
        }
        let args = vec![
            "t".to_string(),
            "-y".to_string(),
            "-idp".to_string(),
            Self::password_arg(&request.password),
            "--".to_string(),
            request.archive.to_string_lossy().into_owned(),
        ];
        let (output, truncated) = crate::test_output::run(
            &self.executable,
            &self.id,
            &args,
            &context.cancellation_token(),
        )
        .await?;
        if output.status != Some(0)
            && format!("{}\n{}", output.stdout, output.stderr)
                .to_ascii_lowercase()
                .contains("unknown method")
        {
            return Err(self.map_failure(&output, &request.archive));
        }
        Ok(crate::test_output::report(
            &self.id,
            "unrar",
            output,
            truncated,
            request.password.as_deref(),
        ))
    }

    async fn extract(&self, request: ExtractArchiveRequest) -> Result<ExtractArchiveResult> {
        let encrypted = self
            .validate_before_extract(
                &request.archive,
                &request.password,
                &CancellationToken::new(),
            )
            .await?;
        std::fs::create_dir_all(&request.output_dir)
            .map_err(|source| SmartZipError::io(Some(request.output_dir.clone()), source))?;
        let args = vec![
            "x".to_string(),
            "-cfg-".to_string(),
            "-ol-".to_string(),
            "-y".to_string(),
            "-o+".to_string(),
            "-idq".to_string(),
            Self::password_arg(&request.password),
            request.archive.to_string_lossy().into_owned(),
            request.output_dir.to_string_lossy().into_owned(),
        ];
        let output = self.run(&args).await?;
        if output.status != Some(0) {
            return Err(crate::test_output::password_error(
                &output,
                "unrar",
                request.password.as_deref(),
                &request.archive,
            )
            .unwrap_or_else(|| self.map_failure(&output, &request.archive)));
        }
        Ok(ExtractArchiveResult {
            output_dir: request.output_dir,
            encrypted,
        })
    }

    async fn extract_with_context(
        &self,
        request: ExtractArchiveRequest,
        context: std::sync::Arc<TaskExecutionContext>,
    ) -> Result<ExtractArchiveResult> {
        let encrypted = self
            .validate_before_extract(
                &request.archive,
                &request.password,
                &context.cancellation_token(),
            )
            .await?;
        std::fs::create_dir_all(&request.output_dir)
            .map_err(|source| SmartZipError::io(Some(request.output_dir.clone()), source))?;
        let args = vec![
            "x".to_string(),
            "-cfg-".to_string(),
            "-ol-".to_string(),
            "-y".to_string(),
            "-o+".to_string(),
            "-idq".to_string(),
            Self::password_arg(&request.password),
            request.archive.to_string_lossy().into_owned(),
            request.output_dir.to_string_lossy().into_owned(),
        ];
        let token = context.cancellation_token();
        let output = self.run_with_token(&args, &token).await?;
        if output.status != Some(0) {
            return Err(crate::test_output::password_error(
                &output,
                "unrar",
                request.password.as_deref(),
                &request.archive,
            )
            .unwrap_or_else(|| self.map_failure(&output, &request.archive)));
        }
        Ok(ExtractArchiveResult {
            output_dir: request.output_dir,
            encrypted,
        })
    }

    async fn compress(&self, request: CompressArchiveRequest) -> Result<CompressArchiveResult> {
        Err(SmartZipError::UnsupportedFormat {
            path: request.output,
            format: Some(request.format.as_str().to_string()),
        })
    }

    async fn compress_with_context(
        &self,
        request: CompressArchiveRequest,
        _context: std::sync::Arc<TaskExecutionContext>,
    ) -> Result<CompressArchiveResult> {
        self.compress(request).await
    }

    fn capabilities(&self) -> smartzip_core::AdapterCapabilities {
        crate::router::unrar_capabilities()
    }
}

fn locate_executable(bundled: Option<&PathBuf>, candidates: &[String]) -> Option<PathBuf> {
    if let Some(path) = bundled {
        if path.exists() {
            return Some(std::fs::canonicalize(path).unwrap_or_else(|_| path.clone()));
        }
    }

    for candidate in candidates {
        if let Ok(found) = which::which(candidate) {
            return Some(std::fs::canonicalize(&found).unwrap_or(found));
        }
    }
    None
}

// Technical listing exposes the original paths and link entry types before
// the external process is allowed to create anything in staging.
fn validate_extraction_listing(stdout: &str) -> Result<()> {
    for line in stdout.lines().map(str::trim_start) {
        if let Some(name) = line.strip_prefix("Name: ") {
            if crate::safety::safe_entry_path(name.as_bytes()).is_none() {
                return Err(SmartZipError::UnsafeArchivePath { entry: name.into() });
            }
        }
        if let Some(kind) = line.strip_prefix("Type: ") {
            if !matches!(kind.trim(), "File" | "Directory") {
                return Err(SmartZipError::UnsafeArchivePath {
                    entry: format!("unsupported RAR entry type: {kind}"),
                });
            }
        }
    }
    Ok(())
}
