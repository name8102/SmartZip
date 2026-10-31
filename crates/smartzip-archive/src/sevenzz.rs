use crate::backend::ArchiveAdapter;
use crate::locator;
use crate::types::*;
use async_trait::async_trait;
use smartzip_core::{ArchiveFormat, Result, SmartZipError, TaskExecutionContext};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::io::AsyncReadExt;
use tokio_util::sync::CancellationToken;

type Observer = Arc<dyn Fn(f32, Option<String>) + Send + Sync>;

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
        let bundled_candidates = self.bundled.iter().cloned().collect::<Vec<_>>();
        let mut path_candidates = Vec::new();
        for candidate in &self.candidates {
            if let Ok(iter) = which::which_all(candidate) {
                path_candidates.extend(iter);
            }
        }
        let mut fallback_candidates = Vec::new();
        for candidate in &self.candidates {
            for found in locator::known_macos_executables(candidate) {
                fallback_candidates.push(found);
            }
        }
        locator::ordered_executables(
            bundled_candidates,
            path_candidates.into_iter().chain(fallback_candidates),
        )
    }
}

fn normalize_executable_path(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

#[derive(Debug, Clone)]
pub struct SevenZipBackend {
    id: String,
    executable: PathBuf,
}

impl SevenZipBackend {
    pub fn new(executable: PathBuf) -> Self {
        let executable = normalize_executable_path(&executable);
        let id = format!("sevenzip:{}", executable.display());
        Self { id, executable }
    }

    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.id = id.into();
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

    async fn run_with_token(
        &self,
        args: &[String],
        token: &CancellationToken,
    ) -> Result<BackendCommandOutput> {
        crate::process::run_bounded(
            &self.executable,
            &self.id,
            args,
            token,
            crate::process::Mode::Ordinary,
        )
        .await
        .map(|(output, _)| output)
    }

    pub(crate) async fn run_streaming_with_context(
        &self,
        args: &[String],
        context: Arc<TaskExecutionContext>,
        progress: bool,
    ) -> Result<BackendCommandOutput> {
        let token = context.cancellation_token();
        crate::process::run(
            &self.executable,
            &self.id,
            args,
            &token,
            crate::process::Mode::Streaming,
            |stdout, pipe| {
                let context = Arc::clone(&context);
                let observer: Option<Observer> = (stdout && progress).then(|| {
                    Arc::new(move |percent, item: Option<String>| {
                        context.emit_progress(smartzip_core::TaskProgress::percent(
                            percent,
                            item.unwrap_or_else(|| "extracting".into()),
                        ));
                    }) as Observer
                });
                async move { Ok((read_stream(pipe, observer).await?, false)) }
            },
        )
        .await
        .map(|(output, _)| output)
    }

    pub(crate) fn encoding_arg(encoding: &smartzip_core::EncodingMode) -> Option<String> {
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
        let lower = crate::test_output::diagnostic_text(&combined, "7z");
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
            || lower.contains("headers error")
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
            || lower
                .lines()
                .any(|line| line == "can not open the file as [7z] archive")
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

    fn map_password_failure(
        &self,
        output: &BackendCommandOutput,
        path: &Path,
        password: Option<&str>,
    ) -> SmartZipError {
        crate::test_output::password_error(output, "7z", password, path)
            .unwrap_or_else(|| self.map_failure(output, path))
    }

    fn password_arg(password: &Option<String>) -> Option<String> {
        password.as_ref().map(|password| {
            if password.is_empty() {
                "-p".to_string()
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
    fn executable_path(&self) -> Option<&Path> {
        Some(&self.executable)
    }
    fn diagnostic_family(&self) -> Option<&'static str> {
        Some("7z")
    }

    async fn probe_with_context(
        &self,
        path: &Path,
        context: Arc<TaskExecutionContext>,
    ) -> Result<ArchiveProbe> {
        let args = vec![
            "t".into(),
            "-bsp1".into(),
            "-p".into(),
            path.to_string_lossy().into_owned(),
        ];
        let output = self
            .run_streaming_with_context(&args, context, false)
            .await?;
        let result = if output.status == Some(0) {
            Ok((true, parse_encrypted(&output.stdout, &output.stderr)))
        } else {
            Err(self.map_password_failure(&output, path, None))
        };
        let (supported, encrypted) = match result {
            Ok(facts) => facts,
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
        let format = parse_slt_archive_format(&output.stdout);
        let entries = if format == Some(ArchiveFormat::Zip) {
            if let smartzip_core::EncodingMode::Override(encoding) = &request.encoding {
                let entries = crate::decoded_zip::entries(&request.archive, encoding)?.0;
                if entries.len() != parse_entries(&output.stdout).len() {
                    return Err(SmartZipError::BackendProtocolError {
                        backend: self.id().into(),
                        detail: "ambiguous ZIP directory".into(),
                    });
                }
                entries
            } else {
                parse_entries(&output.stdout)
            }
        } else {
            parse_entries(&output.stdout)
        };
        Ok(ArchiveListing { format, entries })
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

    async fn extract_with_context(
        &self,
        request: ExtractArchiveRequest,
        context: Arc<TaskExecutionContext>,
    ) -> Result<ExtractArchiveResult> {
        let token = context.cancellation_token();
        let mut listing_args = vec![
            "l".into(),
            "-slt".into(),
            "-sccUTF-8".into(),
            format!("-p{}", request.password.as_deref().unwrap_or_default()),
        ];
        if let Some(enc) = Self::encoding_arg(&request.encoding) {
            listing_args.push(enc);
        }
        listing_args.push("--".into());
        listing_args.push(request.archive.to_string_lossy().into_owned());
        let listing = self.run_with_token(&listing_args, &token).await?;
        if listing.status != Some(0) {
            return Err(self.map_password_failure(
                &listing,
                &request.archive,
                request.password.as_deref(),
            ));
        }
        validate_extraction_listing(&listing.stdout)?;
        let encrypted = listing
            .stdout
            .lines()
            .any(|line| line.trim_end() == "Encrypted = +");
        if !crate::decoded_zip::extract_if_needed(
            &request,
            parse_entries(&listing.stdout).len(),
            &token,
        )
        .await?
        {
            let mut args = vec!["x".into(), "-y".into(), "-bsp1".into()];
            if let Some(pw) = Self::password_arg(&request.password) {
                args.push(pw);
            }
            if let Some(enc) = Self::encoding_arg(&request.encoding) {
                args.push(enc);
            }
            args.push(format!("-o{}", request.output_dir.display()));
            args.push("--".into());
            args.push(request.archive.to_string_lossy().into_owned());
            let output = self
                .run_streaming_with_context(&args, context, true)
                .await?;
            if output.status != Some(0) {
                return Err(self.map_password_failure(
                    &output,
                    &request.archive,
                    request.password.as_deref(),
                ));
            }
        }
        Ok(ExtractArchiveResult {
            output_dir: request.output_dir,
            encrypted: Some(encrypted),
        })
    }

    fn capabilities(&self) -> smartzip_core::AdapterCapabilities {
        let mut capabilities = crate::router::seven_zip_capabilities();
        // Builds of the same 7z version can omit handlers (notably RAR in
        // free-only p7zip packages). Query each installation, not its family.
        if let Some(containers) = std::process::Command::new(&self.executable)
            .arg("i")
            .output()
            .ok()
            .filter(|output| output.status.success())
            .and_then(|output| parse_read_containers(&String::from_utf8_lossy(&output.stdout)))
        {
            capabilities.read_containers = containers;
        }
        capabilities
    }
}

fn parse_read_containers(output: &str) -> Option<Vec<ArchiveFormat>> {
    let mut lines = output.lines();
    lines.find(|line| line.trim() == "Formats:")?;
    let supported = crate::router::seven_zip_capabilities().read_containers;
    let mut containers = Vec::new();
    // Both p7zip and current 7-Zip separate the format table from Codecs with
    // a blank line. Flag columns differ, so match the supported format names.
    for line in lines
        .skip_while(|line| line.trim().is_empty())
        .take_while(|line| !line.trim().is_empty() && !line.trim_end().ends_with(':'))
    {
        if let Some(format) = line
            .split_whitespace()
            .map(parse_archive_format)
            .find(|format| supported.contains(format))
        {
            if !containers.contains(&format) {
                containers.push(format);
            }
        }
    }
    // Free-only 7zz can list RAR headers while omitting the non-free decoder.
    // A readable header alone is insufficient for test/extract routing.
    let has_rar_decoder = output.split_once("Codecs:").is_some_and(|(_, codecs)| {
        codecs
            .lines()
            .skip_while(|line| line.trim().is_empty())
            .take_while(|line| !line.trim().is_empty() && !line.trim_end().ends_with(':'))
            .any(|line| {
                line.split_whitespace().any(|field| field.contains('D'))
                    && line
                        .split_whitespace()
                        .any(|field| matches!(field, "Rar1" | "Rar2" | "Rar3" | "Rar5"))
            })
    });
    if !has_rar_decoder {
        containers.retain(|format| *format != ArchiveFormat::Rar);
    }
    Some(containers)
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

pub(crate) fn validate_extraction_listing(stdout: &str) -> Result<()> {
    for entry in parse_entries(stdout) {
        // TAR writers commonly include `.` or `./` for the extraction root.
        // Only a directory made entirely of current-directory components is
        // harmless; empty paths, files named `.`, links, and traversal still fail.
        if entry.is_dir
            && entry.path.components().next().is_some()
            && entry
                .path
                .components()
                .all(|part| part == std::path::Component::CurDir)
        {
            continue;
        }
        if crate::safety::safe_entry_path(entry.path.to_string_lossy().as_bytes()).is_none() {
            return Err(SmartZipError::UnsafeArchivePath {
                entry: entry.path.display().to_string(),
            });
        }
    }
    Ok(())
}

pub(crate) fn parse_entries(stdout: &str) -> Vec<ArchiveEntry> {
    let mut entries = Vec::new();
    let mut current_path: Option<PathBuf> = None;
    let mut current_size: Option<u64> = None;
    let mut current_packed_size: Option<u64> = None;
    let mut current_is_dir = false;
    let mut current_is_archive = false;
    let mut current_is_link = false;

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
                        is_dir: current_is_dir && !current_is_link,
                    });
                }
            }
            current_path = Some(PathBuf::from(path));
            current_size = None;
            current_packed_size = None;
            current_is_dir = false;
            current_is_archive = false;
            current_is_link = false;
        } else if line
            .strip_prefix("Symbolic Link = ")
            .is_some_and(|v| !v.is_empty())
            || line
                .strip_prefix("Hard Link = ")
                .is_some_and(|v| !v.is_empty())
        {
            current_is_link = true;
        } else if let Some(_type) = line.strip_prefix("Type = ") {
            current_is_archive = true;
        } else if let Some(size) = line.strip_prefix("Size = ") {
            current_size = size.parse().ok();
        } else if let Some(size) = line.strip_prefix("Packed Size = ") {
            current_packed_size = size.parse().ok();
        } else if let Some(folder) = line.strip_prefix("Folder = ") {
            current_is_dir = folder == "+";
        } else if let Some(attributes) = line.strip_prefix("Attributes = ") {
            current_is_dir |= attributes.contains('D');
        }
    }

    if let Some(path) = current_path.take() {
        if !current_is_archive {
            entries.push(ArchiveEntry {
                path,
                raw_name: Vec::new(),
                compressed_size: current_packed_size,
                uncompressed_size: current_size,
                is_dir: current_is_dir && !current_is_link,
            });
        }
    }

    entries
}

const MAX_RECORD: usize = 64 * 1024;

async fn read_stream<R: tokio::io::AsyncRead + Unpin>(
    mut reader: R,
    observer: Option<Observer>,
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
        let remaining = crate::process::MAX_OUTPUT.saturating_sub(raw.len());
        raw.extend_from_slice(&chunk[..count.min(remaining)]);
        for &byte in &chunk[..count] {
            if matches!(byte, b'\r' | b'\n' | 0x08) {
                if !oversized {
                    emit_record(&record, observer.as_ref());
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
        emit_record(&record, observer.as_ref());
    }
    Ok(raw)
}

fn emit_record(record: &[u8], observer: Option<&Observer>) {
    let Some(observer) = observer else { return };
    if let Some((percent, item)) = parse_progress(&String::from_utf8_lossy(record)) {
        observer(percent, item);
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

fn parse_encrypted(stdout: &str, stderr: &str) -> Option<bool> {
    stdout
        .split(['\r', '\n', '\u{8}'])
        .chain(stderr.split(['\r', '\n', '\u{8}']))
        .filter_map(|line| {
            let (key, value) = line.trim().split_once(" = ").or_else(|| {
                line.trim()
                    .split_once(':')
                    .map(|(key, value)| (key, value.trim()))
            })?;
            (key == "Encrypted").then_some(value)
        })
        .filter_map(|value| match value {
            "+" | "1" | "true" => Some(true),
            "-" | "0" | "false" => Some(false),
            _ => None,
        })
        .last()
}

fn parse_slt_archive_format(output: &str) -> Option<ArchiveFormat> {
    output.lines().find_map(|line| {
        line.strip_prefix("Type = ")
            .map(|value| parse_archive_format(value.trim()))
    })
}

fn parse_archive_format(value: &str) -> ArchiveFormat {
    match value.to_ascii_lowercase().as_str() {
        "zip" => ArchiveFormat::Zip,
        "7z" => ArchiveFormat::SevenZip,
        "rar" | "rar5" => ArchiveFormat::Rar,
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

    #[test]
    fn extraction_listing_accepts_tar_root_directory_but_rejects_unsafe_entries() {
        validate_extraction_listing("Path = .\nFolder = +\n\nPath = ./hello.txt\nFolder = -\n")
            .unwrap();
        validate_extraction_listing("Path = ./\nFolder = +\n").unwrap();
        validate_extraction_listing("Path = shortcut\nSymbolic Link = data.txt\n").unwrap();
        validate_extraction_listing("Path = copy\nHard Link = data.txt\n").unwrap();
        for listing in [
            "Path = .\nFolder = -\n",
            "Path = \nFolder = +\n",
            "Path = /\nFolder = +\n",
            "Path = ../escape\nFolder = +\n",
            "Path = .\nFolder = +\nSymbolic Link = /tmp\n",
        ] {
            assert!(validate_extraction_listing(listing).is_err(), "{listing}");
        }
    }

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
            let observer: Observer =
                Arc::new(move |percent, item| captured.lock().unwrap().push((percent, item)));
            let task = tokio::spawn(read_stream(reader, Some(observer)));
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
        let observer: Observer =
            Arc::new(move |percent, item| captured.lock().unwrap().push((percent, item)));
        let task = tokio::spawn(read_stream(reader, Some(observer)));
        tokio::io::AsyncWriteExt::write_all(&mut writer, bytes)
            .await
            .unwrap();
        drop(writer);
        task.await.unwrap().unwrap();
        let events = events.lock().unwrap();
        assert!(events.iter().any(|event| matches!(event, (percent, Some(item)) if *percent == 42.0 && item == "second file.txt")));
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
        let observer: Observer =
            Arc::new(move |percent, item| captured.lock().unwrap().push((percent, item)));
        let task = tokio::spawn(read_stream(reader, Some(observer)));
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
        struct Sink {
            release: PathBuf,
            events: Arc<std::sync::Mutex<Vec<smartzip_core::TaskEvent>>>,
        }
        impl smartzip_core::TaskEventSink for Sink {
            fn push(&self, event: smartzip_core::TaskEvent) {
                if matches!(&event.kind, smartzip_core::TaskEventKind::Progress(progress) if progress.percent == Some(25.0))
                {
                    std::fs::write(&self.release, b"go").unwrap();
                }
                self.events.lock().unwrap().push(event);
            }
        }
        let events = Arc::new(std::sync::Mutex::new(Vec::new()));
        let context = Arc::new(TaskExecutionContext::new(
            smartzip_core::TaskId::new(),
            Arc::new(Sink {
                release,
                events: Arc::clone(&events),
            }),
        ));
        let backend = SevenZipBackend::new(executable);
        let result = backend
            .run_streaming_with_context(&[], context, true)
            .await
            .unwrap();
        assert_eq!(result.status, Some(0));
        assert!(result.stderr.ends_with("WARNING: final diagnostic"));
        assert!(events.lock().unwrap().iter().any(|event| matches!(&event.kind, smartzip_core::TaskEventKind::Progress(progress) if progress.percent == Some(100.0) && progress.message == "final.txt")));
    }

    #[test]
    fn parses_encryption_evidence_without_fabricating_missing_values() {
        assert_eq!(parse_encrypted("Encrypted = +\n", ""), Some(true));
        assert_eq!(parse_encrypted("Encrypted = -\n", ""), Some(false));
        assert_eq!(parse_encrypted("", "Encrypted: true\n"), Some(true));
        assert_eq!(parse_encrypted("Encrypted = unknown\n", ""), None);
        assert_eq!(parse_encrypted("Files: 1\n", ""), None);
    }

    #[test]
    fn slt_entry_metadata_does_not_override_archive_header() {
        let output = "Path = archive.7z\nType = 7z\nPhysical Size = 100\nEncrypted = -\n\nPath = secret.txt\nSize = 200\nPacked Size = 50\nEncrypted = +\n";
        assert_eq!(
            parse_slt_archive_format(output),
            Some(ArchiveFormat::SevenZip)
        );
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

        assert_eq!(
            parse_slt_archive_format(output),
            Some(ArchiveFormat::SevenZip)
        );
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
        assert!(matches!(
            backend.map_failure(&output(2, "ERROR: Password is required"), path),
            SmartZipError::PasswordRequired { .. }
        ));
        assert!(matches!(
            backend.map_failure(&output(2, "ERROR: CRC Failed"), path),
            SmartZipError::CorruptedArchive { .. }
        ));
        assert!(matches!(
            backend.map_failure(&output(2, "ERRORS:\nHeaders Error"), path),
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
    fn p7zip_open_errors_retry_only_the_exact_protocol_message() {
        let backend = SevenZipBackend::new(PathBuf::from("7z"));
        let path = Path::new("payload.7z.001");
        let output = |text: &str| BackendCommandOutput {
            status: Some(2),
            stdout: text.into(),
            stderr: String::new(),
        };
        for text in [
            "ERROR: Can not open the file as [7z] archive",
            "Open ERROR: Can not open the file as [7z] archive",
        ] {
            assert!(matches!(
                backend.map_failure(&output(text), path),
                SmartZipError::UnsupportedContainer { .. }
            ));
        }
        for text in [
            "ERROR: Can not open the file as [zip] archive",
            "Open ERROR: Can not open the file as [zip] archive",
            "ERROR: Can not open the file as [7z] archive because of unknown corruption",
            "Open ERROR: Can not open the file as [7z] archive because of unknown corruption",
            "Path = Open ERROR: Can not open the file as [7z] archive",
        ] {
            assert!(matches!(
                backend.map_failure(&output(text), path),
                SmartZipError::BackendFailed { .. }
            ));
        }
        assert!(matches!(
            backend.map_failure(
                &output("Open ERROR: Can not open the file as [7z] archive\nHeaders Error"),
                path
            ),
            SmartZipError::CorruptedArchive { .. }
        ));
        assert!(matches!(
            backend.map_failure(
                &output("Open ERROR: Can not open the file as [7z] archive\nWrong password?"),
                path
            ),
            SmartZipError::WrongPassword { .. }
        ));
    }

    #[test]
    fn empty_password_is_passed_explicitly() {
        assert_eq!(
            SevenZipBackend::password_arg(&Some(String::new())),
            Some("-p".into())
        );
    }

    #[test]
    fn capabilities_follow_reported_formats() {
        assert_eq!(
            parse_read_containers(
                "7-Zip 26.04\nFormats:\n ...F.... Rar rar r00\n ...F.... Rar5 rar r00\n CK...... zstd zst\n\nCodecs:\n D 40305 Rar5\n"
            ),
            Some(vec![ArchiveFormat::Rar, ArchiveFormat::Zstd]),
        );
        assert_eq!(
            parse_read_containers("Formats:\n\nCodecs:\n 0 ED Rar5\n"),
            Some(vec![])
        );
        assert_eq!(parse_read_containers("7-Zip 26.04\n"), None);
        assert_eq!(
            parse_read_containers(
                "Formats:\n ...F.... Rar5 rar r00\n CK...... zip zip\n\nCodecs:\n ED 40108 Deflate\n\nHashers:\n D 40305 Rar5\n"
            ),
            Some(vec![ArchiveFormat::Zip]),
        );
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
