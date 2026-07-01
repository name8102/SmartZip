use crate::backend::{ArchiveBackend, ExtractionProgressCallback};
use crate::types::*;
use async_trait::async_trait;
use smartzip_core::{ArchiveFormat, Result, SmartZipError};
use std::fs::File;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::io::{AsyncRead, AsyncReadExt};
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

    async fn run_with_progress(
        &self,
        args: &[String],
        progress: Option<ExtractionProgressCallback>,
    ) -> Result<BackendCommandOutput> {
        let mut child = Command::new(&self.executable)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|source| SmartZipError::io(Some(self.executable.clone()), source))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| SmartZipError::BackendFailed {
                backend: "7zz".into(),
                exit_code: None,
                stderr: "failed to capture 7z stdout".into(),
            })?;
        let mut stderr = child
            .stderr
            .take()
            .ok_or_else(|| SmartZipError::BackendFailed {
                backend: "7zz".into(),
                exit_code: None,
                stderr: "failed to capture 7z stderr".into(),
            })?;

        let stdout_future = read_progress_stream(stdout, progress);
        let stderr_future = async {
            let mut bytes = Vec::new();
            stderr.read_to_end(&mut bytes).await?;
            Ok::<_, std::io::Error>(bytes)
        };
        let wait_future = child.wait();
        let (stdout, stderr, status) = tokio::try_join!(stdout_future, stderr_future, wait_future)
            .map_err(|source| SmartZipError::io(Some(self.executable.clone()), source))?;

        Ok(BackendCommandOutput {
            status: status.code(),
            stdout: String::from_utf8_lossy(&stdout).into_owned(),
            stderr: String::from_utf8_lossy(&stderr).into_owned(),
        })
    }

    fn output_indicates_failure(output: &BackendCommandOutput) -> bool {
        let combined = format!("{}\n{}", output.stdout, output.stderr);
        let lower = combined.to_ascii_lowercase();
        lower.contains("error:")
            || lower.contains("errors:")
            || lower.contains("headers error")
            || lower.contains("unexpected end of archive")
            || lower.contains("can not open the file as archive")
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

fn cheap_probe_format(path: &Path) -> Option<ArchiveFormat> {
    let mut file = File::open(path).ok()?;
    let mut buf = [0u8; 512];
    let n = file.read(&mut buf).ok()?;
    let bytes = &buf[..n];

    if bytes.starts_with(b"PK\x03\x04") || bytes.starts_with(b"PK\x05\x06") {
        return Some(ArchiveFormat::Zip);
    }
    if bytes.starts_with(b"Rar!\x1a\x07\x00") || bytes.starts_with(b"Rar!\x1a\x07\x01\x00") {
        return Some(ArchiveFormat::Rar);
    }
    if bytes.starts_with(b"\x37\x7a\xbc\xaf\x27\x1c") {
        return Some(ArchiveFormat::SevenZip);
    }
    if bytes.starts_with(b"\x1f\x8b") {
        return Some(ArchiveFormat::Gzip);
    }
    if bytes.starts_with(b"BZh") || bytes.starts_with(b"BZ") {
        return Some(ArchiveFormat::Bzip2);
    }
    if bytes.starts_with(b"\xfd\x37\x7a\x58\x5a\x00") {
        return Some(ArchiveFormat::Xz);
    }
    if bytes.len() >= 263 && &bytes[257..263] == b"ustar\0" {
        return Some(ArchiveFormat::Tar);
    }

    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| extension.to_ascii_lowercase())
        .as_deref()
    {
        Some("7z") => Some(ArchiveFormat::SevenZip),
        Some("rar") => Some(ArchiveFormat::Rar),
        Some("tar") => Some(ArchiveFormat::Tar),
        Some("gz") | Some("tgz") => Some(ArchiveFormat::Gzip),
        Some("bz2") | Some("tbz2") => Some(ArchiveFormat::Bzip2),
        Some("xz") | Some("txz") => Some(ArchiveFormat::Xz),
        Some("cab") => Some(ArchiveFormat::Cab),
        Some("iso") => Some(ArchiveFormat::Iso),
        Some("dmg") => Some(ArchiveFormat::Dmg),
        Some("zst") | Some("zstd") => Some(ArchiveFormat::Zstd),
        Some("lz4") => Some(ArchiveFormat::Lz4),
        Some("lzma") => Some(ArchiveFormat::Lzma),
        Some("zip") => Some(ArchiveFormat::Zip),
        _ => None,
    }
}

#[async_trait]
impl ArchiveBackend for SevenZipBackend {
    async fn probe(&self, path: &Path) -> Result<ArchiveProbe> {
        let format = cheap_probe_format(path);
        Ok(ArchiveProbe {
            path: path.to_path_buf(),
            format: format.clone(),
            encrypted: None,
            supported: format.is_some(),
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
        if output.status != Some(0) || Self::output_indicates_failure(&output) {
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
        if output.status != Some(0) || Self::output_indicates_failure(&output) {
            return Err(self.map_failure(&output, &request.archive));
        }
        Ok(TestResult {
            ok: true,
            encrypted: None,
        })
    }

    async fn extract(&self, request: ExtractArchiveRequest) -> Result<ExtractArchiveResult> {
        self.extract_with_progress(request, None).await
    }

    async fn extract_with_progress(
        &self,
        request: ExtractArchiveRequest,
        progress: Option<ExtractionProgressCallback>,
    ) -> Result<ExtractArchiveResult> {
        let mut args: Vec<String> = vec!["x".into(), "-y".into()];
        if progress.is_some() {
            args.push("-bsp1".into());
        }
        if let Some(pw) = Self::password_arg(&request.password) {
            args.push(pw);
        }
        if let Some(enc) = Self::encoding_arg(&request.encoding) {
            args.push(enc);
        }
        args.push(format!("-o{}", request.output_dir.display()));
        args.push(request.archive.to_string_lossy().into_owned());
        let completion_callback = progress.clone();
        let output = self.run_with_progress(&args, progress).await?;
        if output.status != Some(0) || Self::output_indicates_failure(&output) {
            return Err(self.map_failure(&output, &request.archive));
        }
        if let Some(callback) = completion_callback {
            callback(100.0);
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

    fn should_test_before_extract(&self, _archive: &Path, _format: Option<&ArchiveFormat>) -> bool {
        false
    }
}

async fn read_progress_stream(
    mut reader: impl AsyncRead + Unpin,
    progress: Option<ExtractionProgressCallback>,
) -> std::io::Result<Vec<u8>> {
    let mut output = Vec::new();
    let mut pending = Vec::new();
    let mut buffer = [0u8; 4096];
    let mut last_percent = None;

    loop {
        let read = reader.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        output.extend_from_slice(&buffer[..read]);
        pending.extend_from_slice(&buffer[..read]);

        let mut start = 0;
        for index in 0..pending.len() {
            if matches!(pending[index], b'\r' | b'\n') {
                report_progress(&pending[start..index], &progress, &mut last_percent);
                start = index + 1;
            }
        }
        if start > 0 {
            pending.drain(..start);
        }
    }
    report_progress(&pending, &progress, &mut last_percent);
    Ok(output)
}

fn report_progress(
    line: &[u8],
    callback: &Option<ExtractionProgressCallback>,
    last_percent: &mut Option<u8>,
) {
    let Some(percent) = parse_progress_percent(line) else {
        return;
    };
    if last_percent.replace(percent) != Some(percent) {
        if let Some(callback) = callback {
            callback(f32::from(percent));
        }
    }
}

fn parse_progress_percent(line: &[u8]) -> Option<u8> {
    let percent_index = line.iter().position(|byte| *byte == b'%')?;
    let digits_end = line[..percent_index]
        .iter()
        .rposition(|byte| byte.is_ascii_digit())?
        + 1;
    let digits_start = line[..digits_end]
        .iter()
        .rposition(|byte| !byte.is_ascii_digit())
        .map_or(0, |index| index + 1);
    let value = std::str::from_utf8(&line[digits_start..digits_end])
        .ok()?
        .parse::<u8>()
        .ok()?;
    (value <= 100).then_some(value)
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
                        raw_name: Vec::new(),
                        compressed_size: None,
                        uncompressed_size: current_size,
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
                raw_name: Vec::new(),
                compressed_size: None,
                uncompressed_size: current_size,
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
        assert_eq!(entries[0].uncompressed_size, Some(42));
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

    #[test]
    fn detects_fatal_markers_even_when_exit_code_is_missing() {
        let output = BackendCommandOutput {
            status: Some(0),
            stdout: "ERRORS:\nHeaders Error\n".into(),
            stderr: String::new(),
        };
        assert!(SevenZipBackend::output_indicates_failure(&output));
    }

    #[test]
    fn parses_7z_progress_lines() {
        assert_eq!(parse_progress_percent(b"  0%"), Some(0));
        assert_eq!(
            parse_progress_percent(b" 42% 12 - nested/path/file.txt"),
            Some(42)
        );
        assert_eq!(parse_progress_percent(b"100% Everything is Ok"), Some(100));
        assert_eq!(parse_progress_percent(b"Size = 42"), None);
        assert_eq!(parse_progress_percent(b"101% invalid"), None);
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
        assert_eq!(probe.format, Some(ArchiveFormat::SevenZip));
        assert_eq!(probe.encrypted, None);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn extract_reports_progress_and_writes_output() {
        use std::sync::{Arc, Mutex};

        let root = tempfile::tempdir().unwrap();
        let archive = root.path().join("progress.7z");
        let input = root.path().join("hello.txt");
        let output = root.path().join("output");
        std::fs::write(&input, b"hello progress").unwrap();

        let status = std::process::Command::new("7z")
            .arg("a")
            .arg(&archive)
            .arg(&input)
            .status()
            .unwrap();
        assert!(status.success(), "7z must be available in PATH");

        let percentages = Arc::new(Mutex::new(Vec::new()));
        let callback_values = Arc::clone(&percentages);
        let callback: ExtractionProgressCallback = Arc::new(move |percent| {
            callback_values.lock().unwrap().push(percent);
        });
        let backend =
            SevenZipBackend::locate(&SevenZipLocator::default()).expect("7z/7zz must be available");

        backend
            .extract_with_progress(
                ExtractArchiveRequest {
                    archive,
                    format: Some(ArchiveFormat::SevenZip),
                    output_dir: output.clone(),
                    password: None,
                    encoding: smartzip_core::EncodingMode::Auto,
                },
                Some(callback),
            )
            .await
            .unwrap();

        assert_eq!(percentages.lock().unwrap().last(), Some(&100.0));
        assert_eq!(
            std::fs::read(output.join("hello.txt")).unwrap(),
            b"hello progress"
        );
    }
}
