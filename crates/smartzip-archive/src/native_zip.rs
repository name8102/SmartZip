use crate::backend::ArchiveBackend;
use crate::types::*;
use async_trait::async_trait;
use smartzip_core::{ArchiveFormat, Result, SmartZipError};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use zip::write::SimpleFileOptions;
use zip::{ZipArchive, ZipWriter};

#[derive(Debug, Clone, Default)]
pub struct NativeZipBackend;

impl NativeZipBackend {
    pub fn new() -> Self {
        Self
    }

    fn open_archive_read(path: &Path) -> Result<ZipArchive<File>> {
        let file = File::open(path)
            .map_err(|source| SmartZipError::io(Some(path.to_path_buf()), source))?;
        ZipArchive::new(file).map_err(|source| map_zip_error(source, path))
    }

    fn reject_password(path: &Path, password: &Option<String>) -> Result<()> {
        if password
            .as_ref()
            .is_some_and(|password| !password.is_empty())
        {
            return Err(SmartZipError::UnsupportedFormat {
                path: path.to_path_buf(),
                format: Some("encrypted zip".into()),
            });
        }
        Ok(())
    }
}

#[async_trait]
impl ArchiveBackend for NativeZipBackend {
    async fn probe(&self, path: &Path) -> Result<ArchiveProbe> {
        let supported = Self::open_archive_read(path).is_ok();
        Ok(ArchiveProbe {
            path: path.to_path_buf(),
            format: Some(ArchiveFormat::Zip),
            encrypted: None,
            supported,
        })
    }

    async fn list(&self, request: ListRequest) -> Result<ArchiveListing> {
        Self::reject_password(&request.archive, &request.password)?;
        let mut archive = Self::open_archive_read(&request.archive)?;
        let mut entries = Vec::new();
        for i in 0..archive.len() {
            let entry = archive
                .by_index_raw(i)
                .map_err(|source| map_zip_error(source, &request.archive))?;
            let name_bytes = entry.name_raw();
            let name = String::from_utf8_lossy(name_bytes).into_owned();
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
        Self::reject_password(&request.archive, &request.password)?;
        let mut archive = Self::open_archive_read(&request.archive)?;
        let len = archive.len();
        let mut buf = vec![0u8; 8192];
        for i in 0..len {
            let mut entry = archive
                .by_index(i)
                .map_err(|source| map_zip_error(source, &request.archive))?;
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
            encrypted: None,
        })
    }

    async fn extract(&self, request: ExtractArchiveRequest) -> Result<ExtractArchiveResult> {
        Self::reject_password(&request.archive, &request.password)?;
        let path = request.archive.clone();
        tokio::task::spawn_blocking(move || extract_sync(&path, &request))
            .await
            .map_err(|e| SmartZipError::BackendFailed {
                backend: "native-zip".into(),
                exit_code: None,
                stderr: format!("task join error: {e}"),
            })?
    }

    async fn compress(&self, request: CompressArchiveRequest) -> Result<CompressArchiveResult> {
        if request.format != ArchiveFormat::Zip {
            return Err(SmartZipError::UnsupportedFormat {
                path: request.output.clone(),
                format: Some(request.format.as_str().to_string()),
            });
        }
        Self::reject_password(&request.output, &request.password)?;
        let path = request.output.clone();
        tokio::task::spawn_blocking(move || compress_sync(&path, &request))
            .await
            .map_err(|e| SmartZipError::BackendFailed {
                backend: "native-zip".into(),
                exit_code: None,
                stderr: format!("task join error: {e}"),
            })?
    }

    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities {
            can_extract: vec![ArchiveFormat::Zip],
            can_compress: vec![ArchiveFormat::Zip],
            supports_passwords: false,
            supports_listing: true,
            supports_test: true,
        }
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

fn extract_sync(archive_path: &Path, request: &ExtractArchiveRequest) -> Result<ExtractArchiveResult> {
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
        let mut entry = archive
            .by_index(i)
            .map_err(|source| map_zip_error(source, archive_path))?;

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

        let raw_name = entry.name_raw();
        let relative_path = match &request.encoding {
            smartzip_core::EncodingMode::Override(enc) => {
                let decoded = smartzip_encoding::decode_name(raw_name, enc).ok_or_else(|| {
                    SmartZipError::UnsafeArchivePath {
                        entry: String::from_utf8_lossy(raw_name).into_owned(),
                    }
                })?;
                crate::safety::safe_entry_path(decoded.as_bytes()).ok_or_else(|| {
                    SmartZipError::UnsafeArchivePath { entry: decoded }
                })?
            }
            smartzip_core::EncodingMode::Auto => {
                crate::safety::safe_entry_path(raw_name).ok_or_else(|| {
                    SmartZipError::UnsafeArchivePath {
                        entry: String::from_utf8_lossy(raw_name).into_owned(),
                    }
                })?
            }
        };
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

fn compress_sync(output_path: &Path, request: &CompressArchiveRequest) -> Result<CompressArchiveResult> {
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

fn map_zip_error(source: zip::result::ZipError, path: &Path) -> SmartZipError {
    match source {
        zip::result::ZipError::UnsupportedArchive(feature) => SmartZipError::UnsupportedFormat {
            path: path.to_path_buf(),
            format: Some(feature.to_string()),
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

    #[tokio::test]
    async fn native_zip_probe_succeeds_for_valid_zip() {
        let temp = tempfile::tempdir().unwrap();
        let archive = temp.path().join("test.zip");
        std::fs::write(&archive, create_minimal_zip()).unwrap();

        let backend = NativeZipBackend::new();
        let probe = backend.probe(&archive).await.unwrap();
        assert!(probe.supported);
        assert_eq!(probe.format, Some(ArchiveFormat::Zip));
    }

    fn create_minimal_zip() -> Vec<u8> {
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut writer = ZipWriter::new(&mut buf);
            let options = SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            writer.start_file("test.txt", options).unwrap();
            writer.write_all(b"hello").unwrap();
            writer.finish().unwrap();
        }
        buf.into_inner()
    }
}
