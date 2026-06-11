use crate::backend::ArchiveBackend;
use crate::types::*;
use async_trait::async_trait;
use smartzip_core::{ArchiveFormat, Result, SmartZipError};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::process::Command;

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
    executable: PathBuf,
}

impl UnrarBackend {
    pub fn new(executable: PathBuf) -> Self {
        Self { executable }
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
        let output = Command::new(&self.executable)
            .args(args)
            .stdin(Stdio::null())
            .output()
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
        if lower.contains("wrong password")
            || lower.contains("incorrect password")
            || lower.contains("checksum error")
        {
            SmartZipError::WrongPassword {
                path: path.to_path_buf(),
            }
        } else if lower.contains("password") && lower.contains("required") {
            SmartZipError::PasswordRequired {
                path: path.to_path_buf(),
            }
        } else if lower.contains("unknown method") || lower.contains("unsupported") {
            SmartZipError::UnsupportedFormat {
                path: path.to_path_buf(),
                format: Some("rar".into()),
            }
        } else if lower.contains("checksum") || lower.contains("corrupt") {
            SmartZipError::CorruptedArchive {
                path: path.to_path_buf(),
                detail: combined,
            }
        } else {
            SmartZipError::BackendFailed {
                backend: "unrar".into(),
                exit_code: output.status,
                stderr: combined,
            }
        }
    }
}

#[async_trait]
impl ArchiveBackend for UnrarBackend {
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
            Err(_) => (false, None),
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

    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities {
            can_extract: vec![ArchiveFormat::Rar],
            can_compress: Vec::new(),
            supports_passwords: true,
            supports_listing: true,
            supports_test: true,
        }
    }
}

fn locate_executable(bundled: Option<&PathBuf>, candidates: &[String]) -> Option<PathBuf> {
    if let Some(path) = bundled {
        if path.exists() {
            return Some(path.clone());
        }
    }

    candidates.iter().find_map(|candidate| {
        std::env::var_os("PATH").and_then(|paths| {
            std::env::split_paths(&paths)
                .map(|dir| dir.join(candidate))
                .find(|path| path.exists())
        })
    })
}
