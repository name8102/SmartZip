use crate::backend::ArchiveBackend;
use crate::types::*;
use async_trait::async_trait;
use smartzip_core::{ArchiveFormat, Result, SmartZipError};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::process::Command;

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
        if let Some(path) = &self.bundled {
            if path.exists() {
                return Some(path.clone());
            }
        }

        self.candidates.iter().find_map(|candidate| {
            std::env::var_os("PATH").and_then(|paths| {
                std::env::split_paths(&paths)
                    .map(|dir| dir.join(candidate))
                    .find(|path| path.exists())
            })
        })
    }
}

#[derive(Debug, Clone)]
pub struct SevenZipBackend {
    executable: PathBuf,
}

impl SevenZipBackend {
    pub fn new(executable: PathBuf) -> Self {
        Self { executable }
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
        if lower.contains("wrong password")
            || (lower.contains("password") && lower.contains("error"))
        {
            SmartZipError::WrongPassword {
                path: path.to_path_buf(),
            }
        } else if lower.contains("unsupported") {
            SmartZipError::UnsupportedFormat {
                path: path.to_path_buf(),
                format: None,
            }
        } else {
            SmartZipError::BackendFailed {
                backend: "7zz".into(),
                exit_code: output.status,
                stderr: combined,
            }
        }
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
impl ArchiveBackend for SevenZipBackend {
    async fn probe(&self, path: &Path) -> Result<ArchiveProbe> {
        let request = TestRequest {
            archive: path.to_path_buf(),
            password: Some(String::new()),
            encoding: smartzip_core::EncodingMode::Auto,
        };
        let result = self.test(request).await;
        let (supported, encrypted) = match result {
            Ok(result) => (result.ok, result.encrypted),
            Err(SmartZipError::WrongPassword { .. }) => (true, Some(true)),
            Err(_) => (false, None),
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
        Ok(ArchiveListing {
            format: None,
            entries: parse_entries(&output.stdout),
        })
    }

    async fn test(&self, request: TestRequest) -> Result<TestResult> {
        let mut args: Vec<String> = vec!["t".into()];
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
        Ok(TestResult {
            ok: true,
            encrypted: None,
        })
    }

    async fn extract(&self, request: ExtractArchiveRequest) -> Result<ExtractArchiveResult> {
        let mut args: Vec<String> = vec!["x".into(), "-y".into()];
        if let Some(pw) = Self::password_arg(&request.password) {
            args.push(pw);
        }
        if let Some(enc) = Self::encoding_arg(&request.encoding) {
            args.push(enc);
        }
        args.push(format!("-o{}", request.output_dir.display()));
        args.push(request.archive.to_string_lossy().into_owned());
        let output = self.run(&args).await?;
        if output.status != Some(0) {
            return Err(self.map_failure(&output, &request.archive));
        }
        Ok(ExtractArchiveResult {
            output_dir: request.output_dir,
        })
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

    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities {
            can_extract: vec![
                ArchiveFormat::Zip,
                ArchiveFormat::SevenZip,
                ArchiveFormat::Rar,
                ArchiveFormat::Tar,
                ArchiveFormat::Gzip,
                ArchiveFormat::Bzip2,
                ArchiveFormat::Xz,
                ArchiveFormat::Cab,
            ],
            can_compress: vec![ArchiveFormat::Zip, ArchiveFormat::SevenZip],
            supports_passwords: true,
            supports_listing: true,
            supports_test: true,
        }
    }
}

fn parse_entries(stdout: &str) -> Vec<ArchiveEntry> {
    let mut entries = Vec::new();
    let mut current_path: Option<PathBuf> = None;
    let mut current_size: Option<u64> = None;
    let mut current_is_dir = false;
    let mut current_is_archive = false;

    for line in stdout.lines() {
        if let Some(path) = line.strip_prefix("Path = ") {
            if let Some(path) = current_path.take() {
                if !current_is_archive {
                    entries.push(ArchiveEntry {
                        path,
                        size: current_size,
                        is_dir: current_is_dir,
                    });
                }
            }
            current_path = Some(PathBuf::from(path));
            current_size = None;
            current_is_dir = false;
            current_is_archive = false;
        } else if let Some(_type) = line.strip_prefix("Type = ") {
            current_is_archive = true;
        } else if let Some(size) = line.strip_prefix("Size = ") {
            current_size = size.parse().ok();
        } else if let Some(attributes) = line.strip_prefix("Attributes = ") {
            current_is_dir = attributes.contains('D');
        }
    }

    if let Some(path) = current_path.take() {
        if !current_is_archive {
            entries.push(ArchiveEntry {
                path,
                size: current_size,
                is_dir: current_is_dir,
            });
        }
    }

    entries
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locator_finds_bundled_path() {
        let path = std::env::current_exe().unwrap();
        let locator = SevenZipLocator::bundled(path.clone());
        assert_eq!(locator.locate(), Some(path));
    }

    #[test]
    fn parses_slt_entries() {
        let stdout = "Path = archive.zip\nType = zip\n\nPath = file.txt\nSize = 42\nAttributes = A\n\nPath = dir\nSize = 0\nAttributes = D\n";
        let entries = parse_entries(stdout);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].path, PathBuf::from("file.txt"));
        assert_eq!(entries[0].size, Some(42));
        assert!(entries[1].is_dir);
    }

    #[test]
    fn empty_password_is_passed_explicitly() {
        assert_eq!(
            SevenZipBackend::password_arg(&Some(String::new())),
            Some("-p\"\"".into())
        );
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
}
