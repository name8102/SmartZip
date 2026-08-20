use crate::backend::ArchiveAdapter;
use crate::types::*;
use async_trait::async_trait;
use process_wrap::tokio::{CommandWrap, KillOnDrop};
#[cfg(unix)]
use process_wrap::tokio::ProcessGroup;
#[cfg(windows)]
use process_wrap::tokio::JobObject;
use smartzip_core::{ArchiveFormat, Result, SmartZipError};
use std::path::{Path, PathBuf};
use std::process::Stdio;

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

    async fn run(&self, args: &[String]) -> Result<BackendCommandOutput> {
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

        let child = wrap
            .spawn()
            .map_err(|source| {
                if source.kind() == std::io::ErrorKind::NotFound {
                    SmartZipError::BackendUnavailable {
                        backend: self.id.clone(),
                    }
                } else {
                    SmartZipError::io(Some(self.executable.clone()), source)
                }
            })?;
        let output = Box::into_pin(child.wait_with_output())
            .await
            .map_err(|source| SmartZipError::io(Some(self.executable.clone()), source))?;

        Ok(BackendCommandOutput {
            status: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
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
        let lower = combined.to_lowercase();
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

    async fn test(&self, request: TestRequest) -> Result<TestResult> {
        let args = vec![
            "t".to_string(),
            "-y".to_string(),
            "-idq".to_string(),
            Self::password_arg(&request.password),
            request.archive.to_string_lossy().into_owned(),
        ];
        let output = self.run(&args).await?;
        if output.status != Some(0) {
            return Err(self.map_failure(&output, &request.archive));
        }
        Ok(TestResult {
            ok: true,
            encrypted: None,
        })
    }

    async fn extract(&self, request: ExtractArchiveRequest) -> Result<ExtractArchiveResult> {
        std::fs::create_dir_all(&request.output_dir)
            .map_err(|source| SmartZipError::io(Some(request.output_dir.clone()), source))?;
        let args = vec![
            "x".to_string(),
            "-y".to_string(),
            "-o+".to_string(),
            "-idq".to_string(),
            Self::password_arg(&request.password),
            request.archive.to_string_lossy().into_owned(),
            request.output_dir.to_string_lossy().into_owned(),
        ];
        let output = self.run(&args).await?;
        if output.status != Some(0) {
            return Err(self.map_failure(&output, &request.archive));
        }
        Ok(ExtractArchiveResult {
            output_dir: request.output_dir,
        })
    }

    async fn compress(&self, request: CompressArchiveRequest) -> Result<CompressArchiveResult> {
        Err(SmartZipError::UnsupportedFormat {
            path: request.output,
            format: Some(request.format.as_str().to_string()),
        })
    }

    fn profile(&self) -> smartzip_core::BackendCapabilityProfile {
        let mut profile =
            crate::router::builtin_profile(&[ArchiveFormat::Rar], &[], true, true, true);
        crate::router::restrict_profile_to_containers(&mut profile, &[ArchiveFormat::Rar]);
        profile
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
