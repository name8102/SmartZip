use crate::backend::ArchiveAdapter;
use crate::types::*;
use async_trait::async_trait;
use smartzip_core::{ArchiveFormat, Result, SmartZipError};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use zip::write::SimpleFileOptions;
use zip::{ZipArchive, ZipWriter};

#[derive(Debug, Clone)]
pub struct NativeZipBackend {
    id: String,
}

impl Default for NativeZipBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl NativeZipBackend {
    pub fn new() -> Self {
        Self {
            id: "native-zip".into(),
        }
    }

    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.id = id.into();
        self
    }

    fn open_archive_read(path: &Path) -> Result<ZipArchive<File>> {
        let file = File::open(path)
            .map_err(|source| SmartZipError::io(Some(path.to_path_buf()), source))?;
        ZipArchive::new(file).map_err(|source| map_zip_error(source, path))
    }

    /// Check if any entry in the archive is encrypted.
    fn has_encrypted_entries(archive: &mut ZipArchive<File>) -> bool {
        (0..archive.len()).any(|i| archive.by_index_raw(i).ok().is_some_and(|e| e.encrypted()))
    }

    /// Open an entry by index, using password if provided.
    /// For encrypted entries without a password, returns PasswordRequired.
    fn open_entry<'a>(
        archive: &'a mut ZipArchive<File>,
        index: usize,
        password: &Option<String>,
        archive_path: &Path,
    ) -> Result<zip::read::ZipFile<'a, File>> {
        let raw = archive
            .by_index_raw(index)
            .map_err(|source| map_zip_error(source, archive_path))?;
        let is_encrypted = raw.encrypted();
        drop(raw);

        if is_encrypted {
            match password.as_deref() {
                None | Some("") => {
                    return Err(SmartZipError::PasswordRequired {
                        path: archive_path.to_path_buf(),
                    });
                }
                Some(pw) => {
                    return archive
                        .by_index_decrypt(index, pw.as_bytes())
                        .map_err(|source| map_zip_error(source, archive_path));
                }
            }
        }

        archive
            .by_index(index)
            .map_err(|source| map_zip_error(source, archive_path))
    }
}

#[async_trait]
impl ArchiveAdapter for NativeZipBackend {
    fn id(&self) -> &str {
        &self.id
    }

    async fn probe(&self, path: &Path) -> Result<ArchiveProbe> {
        let mut archive = Self::open_archive_read(path)
            .map_err(|error| with_backend_identity(error, &self.id))?;
        let encrypted = Self::has_encrypted_entries(&mut archive);
        Ok(ArchiveProbe {
            path: path.to_path_buf(),
            format: Some(ArchiveFormat::Zip),
            encrypted: Some(encrypted),
            supported: true,
        })
    }

    async fn list(&self, request: ListRequest) -> Result<ArchiveListing> {
        let mut archive = Self::open_archive_read(&request.archive)
            .map_err(|error| with_backend_identity(error, &self.id))?;
        let mut entries = Vec::new();
        for i in 0..archive.len() {
            let entry = archive
                .by_index_raw(i)
                .map_err(|source| map_zip_error(source, &request.archive))
                .map_err(|error| with_backend_identity(error, &self.id))?;
            let name_bytes = entry.name_raw();
            // Prefer archive metadata UTF-8 (bit11 / 0x7075 rewritten by zip crate),
            // then caller override, then Bandizip-style auto-detect on raw bytes.
            let name = decode_entry_name(name_bytes, &request.encoding);
            entries.push(ArchiveEntry {
                path: PathBuf::from(&name),
                raw_name: name_bytes.to_vec(),
                compressed_size: Some(entry.compressed_size()),
                uncompressed_size: Some(entry.size()),
                is_dir: entry.is_dir(),
            });
        }

        Ok(ArchiveListing {
            format: Some(ArchiveFormat::Zip),
            entries,
        })
    }

    async fn test(&self, request: TestRequest) -> Result<TestResult> {
        let mut archive = Self::open_archive_read(&request.archive)
            .map_err(|error| with_backend_identity(error, &self.id))?;
        let has_encrypted = Self::has_encrypted_entries(&mut archive);
        let len = archive.len();
        let mut buf = vec![0u8; 8192];

        for i in 0..len {
            let mut entry = Self::open_entry(&mut archive, i, &request.password, &request.archive)
                .map_err(|error| with_backend_identity(error, &self.id))?;
            loop {
                let n = entry
                    .read(&mut buf)
                    .map_err(|source| SmartZipError::io(Some(request.archive.clone()), source))?;
                if n == 0 {
                    break;
                }
            }
        }

        Ok(TestResult {
            ok: true,
            encrypted: Some(has_encrypted),
        })
    }

    async fn extract(&self, request: ExtractArchiveRequest) -> Result<ExtractArchiveResult> {
        let path = request.archive.clone();
        let password = request.password.clone();
        let backend_id = self.id.clone();
        let result = tokio::task::spawn_blocking(move || extract_sync(&path, &password, &request))
            .await
            .map_err(|error| SmartZipError::BackendFailed {
                backend: backend_id.clone(),
                exit_code: None,
                stderr: format!("task join error: {error}"),
            })?;
        result.map_err(|error| with_backend_identity(error, &backend_id))
    }

    async fn compress(&self, request: CompressArchiveRequest) -> Result<CompressArchiveResult> {
        if request.format != ArchiveFormat::Zip {
            return Err(SmartZipError::UnsupportedContainer {
                backend: self.id.clone(),
                path: request.output.clone(),
                container: Some(request.format.as_str().to_string()),
            });
        }
        let path = request.output.clone();
        let backend_id = self.id.clone();
        let result = tokio::task::spawn_blocking(move || compress_sync(&path, &request))
            .await
            .map_err(|error| SmartZipError::BackendFailed {
                backend: backend_id.clone(),
                exit_code: None,
                stderr: format!("task join error: {error}"),
            })?;
        result.map_err(|error| with_backend_identity(error, &backend_id))
    }

    fn profile(&self) -> smartzip_core::BackendCapabilityProfile {
        crate::router::builtin_profile(
            &[ArchiveFormat::Zip],
            &[ArchiveFormat::Zip],
            true,
            true,
            true,
        )
    }
}

fn collect_files(input: &Path) -> Result<Vec<PathBuf>> {
    if input.is_file() {
        return Ok(vec![input.to_path_buf()]);
    }

    let mut files = Vec::new();
    let mut stack = vec![input.to_path_buf()];
    while let Some(path) = stack.pop() {
        let entries = std::fs::read_dir(&path)
            .map_err(|source| SmartZipError::io(Some(path.clone()), source))?;
        for entry in entries {
            let entry = entry.map_err(|source| SmartZipError::io(Some(path.clone()), source))?;
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.is_file() {
                files.push(path);
            }
        }
    }
    files.sort();
    Ok(files)
}


/// Decode a ZIP entry name using override encoding or auto-detection.
///
/// When `raw` is already valid UTF-8 (GPBF bit11 / Info-ZIP 0x7075 handled by
/// the `zip` crate), that wins. Otherwise Auto runs the Bandizip-inspired
/// detector; Override forces a specific encoding_rs label.
fn decode_entry_name(raw: &[u8], encoding: &smartzip_core::EncodingMode) -> String {
    match encoding {
        smartzip_core::EncodingMode::Override(enc) => smartzip_encoding::decode_name(raw, enc)
            .or_else(|| decode_name_auto(raw))
            .unwrap_or_else(|| String::from_utf8_lossy(raw).into_owned()),
        smartzip_core::EncodingMode::Auto => decode_name_auto(raw)
            .unwrap_or_else(|| String::from_utf8_lossy(raw).into_owned()),
    }
}

fn decode_name_auto(raw: &[u8]) -> Option<String> {
    let mut detector = smartzip_encoding::ArchiveEncodingDetector::new();
    let detected = detector.detect(raw);
    smartzip_encoding::decode_name(raw, &detected.selected)
}

fn extract_sync(
    archive_path: &Path,
    password: &Option<String>,
    request: &ExtractArchiveRequest,
) -> Result<ExtractArchiveResult> {
    let mut archive = NativeZipBackend::open_archive_read(archive_path)?;
    std::fs::create_dir_all(&request.output_dir)
        .map_err(|source| SmartZipError::io(Some(request.output_dir.clone()), source))?;

    let limits = ExtractionLimits::default();
    let mut total_written: u64 = 0;
    let len = archive.len();

    if len > limits.max_entries {
        return Err(SmartZipError::BackendFailed {
            backend: "native-zip".into(),
            exit_code: None,
            stderr: format!(
                "archive has {} entries, limit is {}",
                len, limits.max_entries
            ),
        });
    }

    for i in 0..len {
        let mut entry = NativeZipBackend::open_entry(&mut archive, i, password, archive_path)?;

        let uncompressed = entry.size();
        if uncompressed > limits.max_single_entry_bytes {
            return Err(SmartZipError::BackendFailed {
                backend: "native-zip".into(),
                exit_code: None,
                stderr: format!(
                    "entry {} uncompressed size {} exceeds limit {}",
                    i, uncompressed, limits.max_single_entry_bytes
                ),
            });
        }

        let compressed = entry.compressed_size();
        if compressed > 0 && uncompressed / compressed > limits.max_compression_ratio as u64 {
            return Err(SmartZipError::BackendFailed {
                backend: "native-zip".into(),
                exit_code: None,
                stderr: format!(
                    "entry {} compression ratio {}:{} exceeds limit {}:{}",
                    i, uncompressed, compressed, limits.max_compression_ratio, 1
                ),
            });
        }

        if total_written + uncompressed > limits.max_total_output_bytes {
            return Err(SmartZipError::BackendFailed {
                backend: "native-zip".into(),
                exit_code: None,
                stderr: format!(
                    "projected total output {} bytes would exceed limit {}",
                    total_written + uncompressed,
                    limits.max_total_output_bytes
                ),
            });
        }

        let raw_name = entry.name_raw();
        let decoded_name = decode_entry_name(raw_name, &request.encoding);
        let relative_path = crate::safety::safe_entry_path(decoded_name.as_bytes()).ok_or(
            SmartZipError::UnsafeArchivePath {
                entry: decoded_name,
            },
        )?;
        let output_path = request.output_dir.join(relative_path);

        if entry.is_dir() {
            std::fs::create_dir_all(&output_path)
                .map_err(|source| SmartZipError::io(Some(output_path.clone()), source))?;
            continue;
        }

        if let Some(parent) = output_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|source| SmartZipError::io(Some(parent.to_path_buf()), source))?;
        }

        let mut outfile = File::create(&output_path)
            .map_err(|source| SmartZipError::io(Some(output_path.clone()), source))?;
        let written = std::io::copy(&mut entry, &mut outfile)
            .map_err(|source| SmartZipError::io(Some(output_path), source))?;
        total_written += written;

        if total_written > limits.max_total_output_bytes {
            return Err(SmartZipError::BackendFailed {
                backend: "native-zip".into(),
                exit_code: None,
                stderr: format!(
                    "total output {} bytes exceeds limit {}",
                    total_written, limits.max_total_output_bytes
                ),
            });
        }
    }

    Ok(ExtractArchiveResult {
        output_dir: request.output_dir.clone(),
    })
}

fn compress_sync(
    output_path: &Path,
    request: &CompressArchiveRequest,
) -> Result<CompressArchiveResult> {
    let file = File::create(output_path)
        .map_err(|source| SmartZipError::io(Some(output_path.to_path_buf()), source))?;
    let mut writer = ZipWriter::new(file);
    let options = SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .compression_level(Some(6));

    for input in &request.inputs {
        let base = input.parent().unwrap_or_else(|| Path::new("."));
        for file in collect_files(input)? {
            let entry_name = file
                .strip_prefix(base)
                .unwrap_or(file.as_path())
                .to_string_lossy()
                .replace('\\', "/");
            writer
                .start_file(entry_name, options)
                .map_err(|source| map_zip_error(source, output_path))?;
            let mut contents = File::open(&file)
                .map_err(|source| SmartZipError::io(Some(file.clone()), source))?;
            std::io::copy(&mut contents, &mut writer)
                .map_err(|source| SmartZipError::io(Some(output_path.to_path_buf()), source))?;
        }
    }

    writer
        .finish()
        .map_err(|source| map_zip_error(source, output_path))?;

    Ok(CompressArchiveResult {
        output: output_path.to_path_buf(),
    })
}

fn with_backend_identity(error: SmartZipError, backend_id: &str) -> SmartZipError {
    match error {
        SmartZipError::UnsupportedContainer {
            path, container, ..
        } => SmartZipError::UnsupportedContainer {
            backend: backend_id.to_owned(),
            path,
            container,
        },
        SmartZipError::UnsupportedCodec { path, codec, .. } => SmartZipError::UnsupportedCodec {
            backend: backend_id.to_owned(),
            path,
            codec,
        },
        SmartZipError::BackendFailed {
            exit_code, stderr, ..
        } => SmartZipError::BackendFailed {
            backend: backend_id.to_owned(),
            exit_code,
            stderr,
        },
        other => other,
    }
}

fn map_zip_error(source: zip::result::ZipError, path: &Path) -> SmartZipError {
    match source {
        zip::result::ZipError::UnsupportedArchive(feature) => SmartZipError::UnsupportedCodec {
            backend: "native-zip".into(),
            path: path.to_path_buf(),
            codec: Some(feature.to_string()),
        },
        zip::result::ZipError::Io(source) => SmartZipError::io(Some(path.to_path_buf()), source),
        zip::result::ZipError::InvalidArchive(detail) => SmartZipError::CorruptedArchive {
            path: path.to_path_buf(),
            detail: detail.to_string(),
        },
        zip::result::ZipError::InvalidPassword => SmartZipError::WrongPassword {
            path: path.to_path_buf(),
        },
        other => SmartZipError::BackendFailed {
            backend: "native-zip".into(),
            exit_code: None,
            stderr: other.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smartzip_core::{CompressionLevel, EncodingMode};
    use std::io::Write;
    use zip::write::FileOptions;

    // ── Fixture generators ────────────────────────────────────────────

    fn create_minimal_zip() -> Vec<u8> {
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut writer = ZipWriter::new(&mut buf);
            let options =
                SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
            writer.start_file("test.txt", options).unwrap();
            writer.write_all(b"hello").unwrap();
            writer.finish().unwrap();
        }
        buf.into_inner()
    }

    fn create_encrypted_zip(path: &Path, password: &str) {
        let file = File::create(path).unwrap();
        let mut writer = ZipWriter::new(file);
        let options: FileOptions<'_, zip::write::ExtendedFileOptions> = FileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .with_aes_encryption(zip::AesMode::Aes128, password);
        writer.start_file("hello.txt", options).unwrap();
        writer.write_all(b"hello encrypted\n").unwrap();
        writer.finish().unwrap();
    }

    /// Create a raw ZIP with non-UTF-8 encoded filenames (no UTF-8 flag).
    fn create_raw_zip_with_encoding(path: &Path, entries: &[(&[u8], &[u8])]) {
        use std::io::Seek;
        let mut f = File::create(path).unwrap();
        let mut offsets = Vec::new();

        for (fname, content) in entries {
            offsets.push(f.stream_position().unwrap());
            // Local file header
            f.write_all(b"PK\x03\x04").unwrap();
            let flags: u16 = 0; // no UTF-8 flag
            let method: u16 = 0; // stored
            let crc = crc32fast::hash(content);
            f.write_all(&20u16.to_le_bytes()).unwrap(); // version needed
            f.write_all(&flags.to_le_bytes()).unwrap();
            f.write_all(&method.to_le_bytes()).unwrap();
            f.write_all(&0u16.to_le_bytes()).unwrap(); // mod time
            f.write_all(&0u16.to_le_bytes()).unwrap(); // mod date
            f.write_all(&crc.to_le_bytes()).unwrap();
            f.write_all(&(content.len() as u32).to_le_bytes()).unwrap(); // compressed
            f.write_all(&(content.len() as u32).to_le_bytes()).unwrap(); // uncompressed
            f.write_all(&(fname.len() as u16).to_le_bytes()).unwrap();
            f.write_all(&0u16.to_le_bytes()).unwrap(); // extra len
            f.write_all(fname).unwrap();
            f.write_all(content).unwrap();
        }

        let cd_offset = f.stream_position().unwrap();
        for (i, (fname, content)) in entries.iter().enumerate() {
            f.write_all(b"PK\x01\x02").unwrap();
            let crc = crc32fast::hash(content);
            f.write_all(&20u16.to_le_bytes()).unwrap(); // version made by
            f.write_all(&20u16.to_le_bytes()).unwrap(); // version needed
            f.write_all(&0u16.to_le_bytes()).unwrap(); // flags
            f.write_all(&0u16.to_le_bytes()).unwrap(); // method
            f.write_all(&0u16.to_le_bytes()).unwrap(); // mod time
            f.write_all(&0u16.to_le_bytes()).unwrap(); // mod date
            f.write_all(&crc.to_le_bytes()).unwrap();
            f.write_all(&(content.len() as u32).to_le_bytes()).unwrap(); // compressed
            f.write_all(&(content.len() as u32).to_le_bytes()).unwrap(); // uncompressed
            f.write_all(&(fname.len() as u16).to_le_bytes()).unwrap();
            f.write_all(&0u16.to_le_bytes()).unwrap(); // extra len
            f.write_all(&0u16.to_le_bytes()).unwrap(); // comment len
            f.write_all(&0u16.to_le_bytes()).unwrap(); // disk start
            f.write_all(&0u16.to_le_bytes()).unwrap(); // internal attrs
            f.write_all(&0u32.to_le_bytes()).unwrap(); // external attrs
            f.write_all(&(offsets[i] as u32).to_le_bytes()).unwrap(); // local header offset
            f.write_all(fname).unwrap();
        }

        let cd_size = (f.stream_position().unwrap() - cd_offset) as u32;
        f.write_all(b"PK\x05\x06").unwrap();
        // EOCD: 18-byte fixed payload (signature already written above)
        f.write_all(&0u16.to_le_bytes()).unwrap(); // disk number
        f.write_all(&0u16.to_le_bytes()).unwrap(); // CD disk number
        f.write_all(&(entries.len() as u16).to_le_bytes()).unwrap(); // CD entries this disk
        f.write_all(&(entries.len() as u16).to_le_bytes()).unwrap(); // CD entries total
        f.write_all(&cd_size.to_le_bytes()).unwrap(); // CD size
        f.write_all(&(cd_offset as u32).to_le_bytes()).unwrap(); // CD offset
        f.write_all(&0u16.to_le_bytes()).unwrap(); // comment length
    }

    /// Create a raw ZIP with a path traversal entry (../foo).
    fn create_zip_slip_zip(path: &Path) {
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut writer = ZipWriter::new(&mut buf);
            let options =
                SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
            writer.start_file("good.txt", options).unwrap();
            writer.write_all(b"good content").unwrap();
            // This entry attempts path traversal
            writer.start_file("../../../etc/passwd", options).unwrap();
            writer.write_all(b"evil").unwrap();
            writer.finish().unwrap();
        }
        std::fs::write(path, buf.into_inner()).unwrap();
    }

    /// Create a raw ZIP with an absolute path entry (/tmp/evil).
    fn create_absolute_path_zip(path: &Path) {
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut writer = ZipWriter::new(&mut buf);
            let options =
                SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
            writer.start_file("/tmp/evil.txt", options).unwrap();
            writer.write_all(b"evil").unwrap();
            writer.finish().unwrap();
        }
        std::fs::write(path, buf.into_inner()).unwrap();
    }

    /// Create a raw ZIP with a Windows drive letter path (C:\Windows\system32\evil).
    fn create_windows_drive_zip(path: &Path) {
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut writer = ZipWriter::new(&mut buf);
            let options =
                SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
            writer
                .start_file("C:\\Windows\\system32\\evil.txt", options)
                .unwrap();
            writer.write_all(b"evil").unwrap();
            writer.finish().unwrap();
        }
        std::fs::write(path, buf.into_inner()).unwrap();
    }

    /// Create a raw ZIP with backslash traversal (foo\..\..\etc\passwd).
    fn create_backslash_traversal_zip(path: &Path) {
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut writer = ZipWriter::new(&mut buf);
            let options =
                SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
            writer
                .start_file("foo\\..\\..\\etc\\passwd", options)
                .unwrap();
            writer.write_all(b"evil").unwrap();
            writer.finish().unwrap();
        }
        std::fs::write(path, buf.into_inner()).unwrap();
    }

    /// Create a ZIP with only empty directories.
    fn create_empty_dir_zip(path: &Path) {
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut writer = ZipWriter::new(&mut buf);
            let options = SimpleFileOptions::default();
            writer.add_directory("dir1/", options).unwrap();
            writer.add_directory("dir1/subdir/", options).unwrap();
            writer.finish().unwrap();
        }
        std::fs::write(path, buf.into_inner()).unwrap();
    }

    /// Create a ZIP with many entries (1000 files).
    fn create_many_entries_zip(path: &Path) {
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut writer = ZipWriter::new(&mut buf);
            let options =
                SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
            for i in 0..1000 {
                let name = format!("file_{:04}.txt", i);
                writer.start_file(&name, options).unwrap();
                writeln!(writer, "content of file {}", i).unwrap();
            }
            writer.finish().unwrap();
        }
        std::fs::write(path, buf.into_inner()).unwrap();
    }

    /// Create a ZIP with a small highly-compressible file.
    fn create_high_ratio_small_zip(path: &Path) {
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut writer = ZipWriter::new(&mut buf);
            let options = SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated)
                .compression_level(Some(9));
            writer.start_file("small.txt", options).unwrap();
            // 10000 identical bytes — very high compression ratio
            writer.write_all(&[b'A'; 10000]).unwrap();
            writer.finish().unwrap();
        }
        std::fs::write(path, buf.into_inner()).unwrap();
    }

    /// Create a ZIP with a large highly-compressible file.
    fn create_high_ratio_large_zip(path: &Path) {
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut writer = ZipWriter::new(&mut buf);
            let options = SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated)
                .compression_level(Some(9));
            writer.start_file("large.txt", options).unwrap();
            // 1MB of identical bytes
            let chunk = [b'X'; 1024];
            for _ in 0..1024 {
                writer.write_all(&chunk).unwrap();
            }
            writer.finish().unwrap();
        }
        std::fs::write(path, buf.into_inner()).unwrap();
    }

    /// Create a raw ZIP where the central directory declares a much larger
    /// uncompressed size than the actual content — simulates a zip bomb
    /// with inflated declared sizes to trigger the compression ratio limit.
    fn create_high_ratio_bomb_zip(path: &Path, declared_uncompressed: u32) {
        use std::io::Seek;
        let mut f = File::create(path).unwrap();
        let content = b"X"; // 1 byte actual
        let fname = b"bomb.bin";

        // Local file header
        let local_offset = f.stream_position().unwrap();
        f.write_all(b"PK\x03\x04").unwrap();
        f.write_all(&20u16.to_le_bytes()).unwrap(); // version needed
        f.write_all(&0u16.to_le_bytes()).unwrap(); // flags
        f.write_all(&0u16.to_le_bytes()).unwrap(); // method (stored)
        f.write_all(&0u16.to_le_bytes()).unwrap(); // mod time
        f.write_all(&0u16.to_le_bytes()).unwrap(); // mod date
        let crc = crc32fast::hash(content);
        f.write_all(&crc.to_le_bytes()).unwrap();
        f.write_all(&(content.len() as u32).to_le_bytes()).unwrap(); // compressed = 1
        f.write_all(&(content.len() as u32).to_le_bytes()).unwrap(); // uncompressed = 1
        f.write_all(&(fname.len() as u16).to_le_bytes()).unwrap(); // name len
        f.write_all(&0u16.to_le_bytes()).unwrap(); // extra len
        f.write_all(fname).unwrap();
        f.write_all(content).unwrap();

        // Central directory — declare inflated uncompressed size
        let cd_offset = f.stream_position().unwrap();
        f.write_all(b"PK\x01\x02").unwrap();
        f.write_all(&20u16.to_le_bytes()).unwrap(); // version made by
        f.write_all(&20u16.to_le_bytes()).unwrap(); // version needed
        f.write_all(&0u16.to_le_bytes()).unwrap(); // flags
        f.write_all(&0u16.to_le_bytes()).unwrap(); // method
        f.write_all(&0u16.to_le_bytes()).unwrap(); // mod time
        f.write_all(&0u16.to_le_bytes()).unwrap(); // mod date
        f.write_all(&crc.to_le_bytes()).unwrap();
        f.write_all(&(content.len() as u32).to_le_bytes()).unwrap(); // compressed = 1
        f.write_all(&declared_uncompressed.to_le_bytes()).unwrap(); // uncompressed = inflated
        f.write_all(&(fname.len() as u16).to_le_bytes()).unwrap(); // name len
        f.write_all(&0u16.to_le_bytes()).unwrap(); // extra len
        f.write_all(&0u16.to_le_bytes()).unwrap(); // comment len
        f.write_all(&0u16.to_le_bytes()).unwrap(); // disk start
        f.write_all(&0u16.to_le_bytes()).unwrap(); // internal attrs
        f.write_all(&0u32.to_le_bytes()).unwrap(); // external attrs
        f.write_all(&(local_offset as u32).to_le_bytes()).unwrap(); // local header offset
        f.write_all(fname).unwrap();

        // EOCD
        let cd_size = (f.stream_position().unwrap() - cd_offset) as u32;
        f.write_all(b"PK\x05\x06").unwrap();
        f.write_all(&0u16.to_le_bytes()).unwrap();
        f.write_all(&0u16.to_le_bytes()).unwrap();
        f.write_all(&1u16.to_le_bytes()).unwrap(); // CD entries this disk
        f.write_all(&1u16.to_le_bytes()).unwrap(); // CD entries total
        f.write_all(&cd_size.to_le_bytes()).unwrap();
        f.write_all(&(cd_offset as u32).to_le_bytes()).unwrap();
        f.write_all(&0u16.to_le_bytes()).unwrap();
    }

    // ── Tests: ZipCrypto ──────────────────────────────────────────────
    // ZipCrypto writing is not publicly supported by zip crate v8.
    // The AES tests above already validate the password dispatch path.
    // ZipCrypto reading is tested via the engine integration tests with
    // real fixtures. The key code paths (open_entry → by_index_decrypt,
    // InvalidPassword → WrongPassword, missing password → PasswordRequired)
    // are shared between AES and ZipCrypto.

    // ── Tests: password probe ────────────────────────────────────────

    #[tokio::test]
    async fn native_zip_test_with_correct_password_succeeds() {
        let temp = tempfile::tempdir().unwrap();
        let archive = temp.path().join("encrypted.zip");
        create_encrypted_zip(&archive, "secret");

        let backend = NativeZipBackend::new();
        let result = backend
            .test(TestRequest {
                archive,
                format: Some(ArchiveFormat::Zip),
                password: Some("secret".into()),
                encoding: EncodingMode::Auto,
            })
            .await
            .unwrap();
        assert!(result.ok);
        assert_eq!(result.encrypted, Some(true));
    }

    #[tokio::test]
    async fn native_zip_test_with_wrong_password_fails() {
        let temp = tempfile::tempdir().unwrap();
        let archive = temp.path().join("encrypted.zip");
        create_encrypted_zip(&archive, "secret");

        let backend = NativeZipBackend::new();
        let result = backend
            .test(TestRequest {
                archive,
                format: Some(ArchiveFormat::Zip),
                password: Some("wrong".into()),
                encoding: EncodingMode::Auto,
            })
            .await;
        assert!(matches!(result, Err(SmartZipError::WrongPassword { .. })));
    }

    #[tokio::test]
    async fn native_zip_test_encrypted_without_password_fails() {
        let temp = tempfile::tempdir().unwrap();
        let archive = temp.path().join("encrypted.zip");
        create_encrypted_zip(&archive, "secret");

        let backend = NativeZipBackend::new();
        let result = backend
            .test(TestRequest {
                archive,
                format: Some(ArchiveFormat::Zip),
                password: None,
                encoding: EncodingMode::Auto,
            })
            .await;
        assert!(matches!(
            result,
            Err(SmartZipError::PasswordRequired { .. })
        ));
    }

    #[tokio::test]
    async fn native_zip_extract_encrypted_with_correct_password() {
        let temp = tempfile::tempdir().unwrap();
        let archive = temp.path().join("encrypted.zip");
        create_encrypted_zip(&archive, "secret");

        let backend = NativeZipBackend::new();
        let output_dir = temp.path().join("output");
        backend
            .extract(ExtractArchiveRequest {
                archive,
                format: Some(ArchiveFormat::Zip),
                output_dir: output_dir.clone(),
                password: Some("secret".into()),
                encoding: EncodingMode::Auto,
            })
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(output_dir.join("hello.txt")).unwrap(),
            "hello encrypted\n"
        );
    }

    #[tokio::test]
    async fn native_zip_extract_encrypted_with_wrong_password_fails() {
        let temp = tempfile::tempdir().unwrap();
        let archive = temp.path().join("encrypted.zip");
        create_encrypted_zip(&archive, "secret");

        let backend = NativeZipBackend::new();
        let output_dir = temp.path().join("output");
        let result = backend
            .extract(ExtractArchiveRequest {
                archive,
                format: Some(ArchiveFormat::Zip),
                output_dir,
                password: Some("wrong".into()),
                encoding: EncodingMode::Auto,
            })
            .await;
        assert!(matches!(result, Err(SmartZipError::WrongPassword { .. })));
    }

    #[tokio::test]
    async fn native_zip_profile_contains_builtin_rules() {
        let backend = NativeZipBackend::new();
        assert!(!backend.profile().rules.is_empty());
    }

    // ── Restored: end-to-end compress → list → test → extract ────────

    #[tokio::test]
    async fn native_zip_can_compress_list_test_and_extract() {
        let temp = tempfile::tempdir().unwrap();
        let input_dir = temp.path().join("input");
        let nested_dir = input_dir.join("nested");
        std::fs::create_dir_all(&nested_dir).unwrap();
        std::fs::write(input_dir.join("hello.txt"), "hello zip\n").unwrap();
        std::fs::write(nested_dir.join("data.txt"), "nested data\n").unwrap();

        let archive = temp.path().join("archive.zip");
        let backend = NativeZipBackend::new();
        backend
            .compress(CompressArchiveRequest {
                inputs: vec![input_dir.clone()],
                output: archive.clone(),
                format: ArchiveFormat::Zip,
                level: CompressionLevel::Balanced,
                password: None,
            })
            .await
            .unwrap();

        let listing = backend
            .list(ListRequest {
                archive: archive.clone(),
                format: Some(ArchiveFormat::Zip),
                password: None,
                encoding: EncodingMode::Auto,
            })
            .await
            .unwrap();
        assert!(listing
            .entries
            .iter()
            .any(|entry| entry.path.ends_with("input/hello.txt")));
        assert!(
            backend
                .test(TestRequest {
                    archive: archive.clone(),
                    format: Some(ArchiveFormat::Zip),
                    password: None,
                    encoding: EncodingMode::Auto,
                })
                .await
                .unwrap()
                .ok
        );

        let output_dir = temp.path().join("output");
        backend
            .extract(ExtractArchiveRequest {
                archive,
                format: Some(ArchiveFormat::Zip),
                output_dir: output_dir.clone(),
                password: None,
                encoding: EncodingMode::Auto,
            })
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(output_dir.join("input/hello.txt")).unwrap(),
            "hello zip\n"
        );
        assert_eq!(
            std::fs::read_to_string(output_dir.join("input/nested/data.txt")).unwrap(),
            "nested data\n"
        );
    }

    // ── Restored: probe tests ────────────────────────────────────────

    #[tokio::test]
    async fn native_zip_probe_succeeds_for_valid_zip() {
        let temp = tempfile::tempdir().unwrap();
        let archive = temp.path().join("test.zip");
        std::fs::write(&archive, create_minimal_zip()).unwrap();

        let backend = NativeZipBackend::new();
        let probe = backend.probe(&archive).await.unwrap();
        assert!(probe.supported);
        assert_eq!(probe.format, Some(ArchiveFormat::Zip));
        assert_eq!(probe.encrypted, Some(false));
    }

    #[tokio::test]
    async fn native_zip_probe_detects_encrypted_archive() {
        let temp = tempfile::tempdir().unwrap();
        let archive = temp.path().join("encrypted.zip");
        create_encrypted_zip(&archive, "secret");

        let backend = NativeZipBackend::new();
        let probe = backend.probe(&archive).await.unwrap();
        assert!(probe.supported);
        assert_eq!(probe.format, Some(ArchiveFormat::Zip));
        assert_eq!(probe.encrypted, Some(true));
    }

    // ── Tests: path safety ───────────────────────────────────────────

    #[tokio::test]
    async fn native_zip_extract_rejects_zip_slip() {
        let temp = tempfile::tempdir().unwrap();
        let archive = temp.path().join("zipslip.zip");
        create_zip_slip_zip(&archive);

        let backend = NativeZipBackend::new();
        let output_dir = temp.path().join("output");
        let result = backend
            .extract(ExtractArchiveRequest {
                archive,
                format: Some(ArchiveFormat::Zip),
                output_dir,
                password: None,
                encoding: EncodingMode::Auto,
            })
            .await;
        assert!(
            matches!(result, Err(SmartZipError::UnsafeArchivePath { .. })),
            "expected UnsafeArchivePath, got: {:?}",
            result
        );
    }

    #[tokio::test]
    async fn native_zip_extract_rejects_absolute_path() {
        let temp = tempfile::tempdir().unwrap();
        let archive = temp.path().join("absolute.zip");
        create_absolute_path_zip(&archive);

        let backend = NativeZipBackend::new();
        let output_dir = temp.path().join("output");
        let result = backend
            .extract(ExtractArchiveRequest {
                archive,
                format: Some(ArchiveFormat::Zip),
                output_dir,
                password: None,
                encoding: EncodingMode::Auto,
            })
            .await;
        assert!(
            matches!(result, Err(SmartZipError::UnsafeArchivePath { .. })),
            "expected UnsafeArchivePath, got: {:?}",
            result
        );
    }

    #[tokio::test]
    async fn native_zip_extract_rejects_windows_drive_letter() {
        let temp = tempfile::tempdir().unwrap();
        let archive = temp.path().join("windrive.zip");
        create_windows_drive_zip(&archive);

        let backend = NativeZipBackend::new();
        let output_dir = temp.path().join("output");
        let result = backend
            .extract(ExtractArchiveRequest {
                archive,
                format: Some(ArchiveFormat::Zip),
                output_dir,
                password: None,
                encoding: EncodingMode::Auto,
            })
            .await;
        assert!(
            matches!(result, Err(SmartZipError::UnsafeArchivePath { .. })),
            "expected UnsafeArchivePath, got: {:?}",
            result
        );
    }

    #[tokio::test]
    async fn native_zip_extract_rejects_backslash_traversal() {
        let temp = tempfile::tempdir().unwrap();
        let archive = temp.path().join("backslash.zip");
        create_backslash_traversal_zip(&archive);

        let backend = NativeZipBackend::new();
        let output_dir = temp.path().join("output");
        let result = backend
            .extract(ExtractArchiveRequest {
                archive,
                format: Some(ArchiveFormat::Zip),
                output_dir,
                password: None,
                encoding: EncodingMode::Auto,
            })
            .await;
        assert!(
            matches!(result, Err(SmartZipError::UnsafeArchivePath { .. })),
            "expected UnsafeArchivePath, got: {:?}",
            result
        );
    }

    // ── Tests: encoding (raw filename preservation) ───────────────────

    #[tokio::test]
    async fn native_zip_list_preserves_gbk_raw_name_bytes() {
        let temp = tempfile::tempdir().unwrap();
        let archive = temp.path().join("gbk.zip");
        // "测试文件.txt" in GBK
        let gbk_name = b"\xb2\xe2\xca\xd4\xce\xc4\xbc\xfe.txt";
        let content = b"hello";
        create_raw_zip_with_encoding(&archive, &[(gbk_name, content)]);

        let backend = NativeZipBackend::new();
        let listing = backend
            .list(ListRequest {
                archive,
                format: Some(ArchiveFormat::Zip),
                password: None,
                encoding: EncodingMode::Auto,
            })
            .await
            .unwrap();
        assert_eq!(listing.entries.len(), 1);
        assert_eq!(listing.entries[0].raw_name, gbk_name);
        // raw_name keeps original bytes; path is auto-decoded under EncodingMode::Auto.
        let expected = smartzip_encoding::decode_name(gbk_name, "gbk").unwrap();
        assert_eq!(listing.entries[0].path, PathBuf::from(&expected));
    }

    #[tokio::test]
    async fn native_zip_list_preserves_shift_jis_raw_name_bytes() {
        let temp = tempfile::tempdir().unwrap();
        let archive = temp.path().join("sjis.zip");
        // "テスト" in Shift_JIS
        let sjis_name = b"\x83\x65\x83\x58\x83\x67.txt";
        let content = b"hello";
        create_raw_zip_with_encoding(&archive, &[(sjis_name, content)]);

        let backend = NativeZipBackend::new();
        let listing = backend
            .list(ListRequest {
                archive,
                format: Some(ArchiveFormat::Zip),
                password: None,
                encoding: EncodingMode::Auto,
            })
            .await
            .unwrap();
        assert_eq!(listing.entries.len(), 1);
        assert_eq!(listing.entries[0].raw_name, sjis_name);
        let expected = smartzip_encoding::decode_name(sjis_name, "shift_jis").unwrap();
        assert_eq!(listing.entries[0].path, PathBuf::from(&expected));
    }

    #[tokio::test]
    async fn native_zip_extract_auto_encoding_gbk() {
        let temp = tempfile::tempdir().unwrap();
        let archive = temp.path().join("gbk.zip");
        // Longer sample so Auto can score GBK over other CJK pages.
        let gbk_name =
            b"\xC4\xE3\xBA\xC3\xCA\xC0\xBD\xE7\xBB\xB6\xD3\xAD.txt"; // 你好世界欢迎.txt
        let content = b"content";
        create_raw_zip_with_encoding(&archive, &[(gbk_name, content)]);

        let backend = NativeZipBackend::new();
        let output_dir = temp.path().join("output");
        backend
            .extract(ExtractArchiveRequest {
                archive,
                format: Some(ArchiveFormat::Zip),
                output_dir: output_dir.clone(),
                password: None,
                encoding: EncodingMode::Auto,
            })
            .await
            .unwrap();
        let expected_name = smartzip_encoding::decode_name(gbk_name, "gbk").unwrap();
        assert!(
            output_dir.join(&expected_name).exists(),
            "expected auto-decoded path {expected_name}"
        );
    }

    #[tokio::test]
    async fn native_zip_extract_override_encoding_gbk() {
        let temp = tempfile::tempdir().unwrap();
        let archive = temp.path().join("gbk.zip");
        // "测试" in GBK
        let gbk_name = b"\xb2\xe2\xca\xd4";
        let content = b"content";
        create_raw_zip_with_encoding(&archive, &[(gbk_name, content)]);

        let backend = NativeZipBackend::new();
        let output_dir = temp.path().join("output");
        backend
            .extract(ExtractArchiveRequest {
                archive,
                format: Some(ArchiveFormat::Zip),
                output_dir: output_dir.clone(),
                password: None,
                encoding: EncodingMode::Override("gb18030".into()),
            })
            .await
            .unwrap();
        // Should create file with decoded GBK name (not garbled)
        let expected_name = smartzip_encoding::decode_name(gbk_name, "gb18030").unwrap();
        assert!(output_dir.join(&expected_name).exists());
        assert_eq!(
            std::fs::read_to_string(output_dir.join(&expected_name)).unwrap(),
            "content"
        );
    }

    #[tokio::test]
    async fn native_zip_extract_override_encoding_shift_jis() {
        let temp = tempfile::tempdir().unwrap();
        let archive = temp.path().join("sjis.zip");
        // "テスト" in Shift_JIS
        let sjis_name = b"\x83\x65\x83\x58\x83\x67";
        let content = b"content";
        create_raw_zip_with_encoding(&archive, &[(sjis_name, content)]);

        let backend = NativeZipBackend::new();
        let output_dir = temp.path().join("output");
        backend
            .extract(ExtractArchiveRequest {
                archive,
                format: Some(ArchiveFormat::Zip),
                output_dir: output_dir.clone(),
                password: None,
                encoding: EncodingMode::Override("shift_jis".into()),
            })
            .await
            .unwrap();
        let expected_name = smartzip_encoding::decode_name(sjis_name, "shift_jis").unwrap();
        assert!(output_dir.join(&expected_name).exists());
        assert_eq!(
            std::fs::read_to_string(output_dir.join(&expected_name)).unwrap(),
            "content"
        );
    }

    // ── Tests: structural edge cases ─────────────────────────────────

    #[tokio::test]
    async fn native_zip_extract_empty_dirs() {
        let temp = tempfile::tempdir().unwrap();
        let archive = temp.path().join("empty_dirs.zip");
        create_empty_dir_zip(&archive);

        let backend = NativeZipBackend::new();
        let output_dir = temp.path().join("output");
        backend
            .extract(ExtractArchiveRequest {
                archive,
                format: Some(ArchiveFormat::Zip),
                output_dir: output_dir.clone(),
                password: None,
                encoding: EncodingMode::Auto,
            })
            .await
            .unwrap();
        assert!(output_dir.join("dir1").is_dir());
        assert!(output_dir.join("dir1/subdir").is_dir());
    }

    #[tokio::test]
    async fn native_zip_extract_many_entries() {
        let temp = tempfile::tempdir().unwrap();
        let archive = temp.path().join("many.zip");
        create_many_entries_zip(&archive);

        let backend = NativeZipBackend::new();
        let output_dir = temp.path().join("output");
        backend
            .extract(ExtractArchiveRequest {
                archive,
                format: Some(ArchiveFormat::Zip),
                output_dir: output_dir.clone(),
                password: None,
                encoding: EncodingMode::Auto,
            })
            .await
            .unwrap();
        for i in 0..1000 {
            let name = format!("file_{:04}.txt", i);
            assert!(output_dir.join(&name).exists(), "missing {name}");
            assert!(std::fs::read_to_string(output_dir.join(&name))
                .unwrap()
                .contains(&format!("content of file {i}")));
        }
    }

    // ── Tests: compression ratio ──────────────────────────────────────

    #[tokio::test]
    async fn native_zip_extract_high_ratio_small_below_limit() {
        let temp = tempfile::tempdir().unwrap();
        let archive = temp.path().join("ratio_small.zip");
        create_high_ratio_small_zip(&archive);

        let backend = NativeZipBackend::new();
        let output_dir = temp.path().join("output");
        // 10000 bytes compressed to ~30 bytes → ratio ~333, well below 10_000
        backend
            .extract(ExtractArchiveRequest {
                archive,
                format: Some(ArchiveFormat::Zip),
                output_dir: output_dir.clone(),
                password: None,
                encoding: EncodingMode::Auto,
            })
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(output_dir.join("small.txt"))
                .unwrap()
                .len(),
            10000
        );
    }

    #[tokio::test]
    async fn native_zip_extract_high_ratio_large_below_limit() {
        let temp = tempfile::tempdir().unwrap();
        let archive = temp.path().join("ratio_large.zip");
        create_high_ratio_large_zip(&archive);

        let backend = NativeZipBackend::new();
        let output_dir = temp.path().join("output");
        // 1MB of identical bytes → ratio maybe a few hundred, below 10_000
        backend
            .extract(ExtractArchiveRequest {
                archive,
                format: Some(ArchiveFormat::Zip),
                output_dir: output_dir.clone(),
                password: None,
                encoding: EncodingMode::Auto,
            })
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(output_dir.join("large.txt"))
                .unwrap()
                .len(),
            1024 * 1024
        );
    }

    #[tokio::test]
    async fn native_zip_extract_rejects_high_ratio_bomb() {
        let temp = tempfile::tempdir().unwrap();
        let archive = temp.path().join("bomb.zip");
        // 1 byte actual, declared 100_001 uncompressed → ratio 100_001 > 10_000 limit
        create_high_ratio_bomb_zip(&archive, 100_001);

        let backend = NativeZipBackend::new();
        let output_dir = temp.path().join("output");
        let result = backend
            .extract(ExtractArchiveRequest {
                archive,
                format: Some(ArchiveFormat::Zip),
                output_dir,
                password: None,
                encoding: EncodingMode::Auto,
            })
            .await;
        assert!(
            matches!(result, Err(SmartZipError::BackendFailed { .. })),
            "expected BackendFailed for compression ratio bomb, got: {:?}",
            result
        );
    }
}
