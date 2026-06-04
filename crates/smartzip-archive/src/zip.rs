use crate::backend::ArchiveBackend;
use crate::types::*;
use async_trait::async_trait;
use async_zip::base::{read::mem::ZipFileReader, write::ZipFileWriter};
use async_zip::{Compression, ZipEntryBuilder};
use futures_lite::io::AsyncReadExt;
use smartzip_core::{ArchiveFormat, Result, SmartZipError};
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone, Default)]
pub struct ZipBackend;

impl ZipBackend {
    pub fn new() -> Self {
        Self
    }

    async fn reader(&self, path: &Path) -> Result<ZipFileReader> {
        let data = std::fs::read(path)
            .map_err(|source| SmartZipError::io(Some(path.to_path_buf()), source))?;
        ZipFileReader::new(data)
            .await
            .map_err(|source| map_zip_error(source, path))
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
impl ArchiveBackend for ZipBackend {
    async fn probe(&self, path: &Path) -> Result<ArchiveProbe> {
        let supported = self.reader(path).await.is_ok();
        Ok(ArchiveProbe {
            path: path.to_path_buf(),
            format: Some(ArchiveFormat::Zip),
            encrypted: None,
            supported,
        })
    }

    async fn list(&self, request: ListRequest) -> Result<ArchiveListing> {
        Self::reject_password(&request.archive, &request.password)?;
        let reader = self.reader(&request.archive).await?;
        let entries = reader
            .file()
            .entries()
            .iter()
            .map(|entry| {
                let name = String::from_utf8_lossy(entry.filename().as_bytes()).into_owned();
                ArchiveEntry {
                    path: PathBuf::from(&name),
                    size: Some(entry.uncompressed_size()),
                    is_dir: name.ends_with('/'),
                }
            })
            .collect();

        Ok(ArchiveListing {
            format: Some(ArchiveFormat::Zip),
            entries,
        })
    }

    async fn test(&self, request: TestRequest) -> Result<TestResult> {
        Self::reject_password(&request.archive, &request.password)?;
        let reader = self.reader(&request.archive).await?;
        for index in 0..reader.file().entries().len() {
            let mut entry_reader = reader
                .reader_without_entry(index)
                .await
                .map_err(|source| map_zip_error(source, &request.archive))?;
            let mut sink = Vec::new();
            entry_reader
                .read_to_end(&mut sink)
                .await
                .map_err(|source| SmartZipError::io(Some(request.archive.clone()), source))?;
        }

        Ok(TestResult {
            ok: true,
            encrypted: None,
        })
    }

    async fn extract(&self, request: ExtractArchiveRequest) -> Result<ExtractArchiveResult> {
        Self::reject_password(&request.archive, &request.password)?;
        let reader = self.reader(&request.archive).await?;
        std::fs::create_dir_all(&request.output_dir)
            .map_err(|source| SmartZipError::io(Some(request.output_dir.clone()), source))?;

        for index in 0..reader.file().entries().len() {
            let entry =
                reader
                    .file()
                    .entries()
                    .get(index)
                    .ok_or_else(|| SmartZipError::BackendFailed {
                        backend: "zip".into(),
                        exit_code: None,
                        stderr: format!("entry index {index} out of bounds"),
                    })?;
            let raw_name = String::from_utf8_lossy(entry.filename().as_bytes());
            let relative_path =
                safe_entry_path(&raw_name).ok_or_else(|| SmartZipError::UnsafeArchivePath {
                    entry: raw_name.into_owned(),
                })?;
            let output_path = request.output_dir.join(relative_path);

            if entry
                .dir()
                .map_err(|source| map_zip_error(source, &request.archive))?
            {
                std::fs::create_dir_all(&output_path)
                    .map_err(|source| SmartZipError::io(Some(output_path.clone()), source))?;
                continue;
            }

            if let Some(parent) = output_path.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|source| SmartZipError::io(Some(parent.to_path_buf()), source))?;
            }

            let mut entry_reader = reader
                .reader_without_entry(index)
                .await
                .map_err(|source| map_zip_error(source, &request.archive))?;
            let mut contents =
                Vec::with_capacity(entry.uncompressed_size().min(usize::MAX as u64) as usize);
            entry_reader
                .read_to_end(&mut contents)
                .await
                .map_err(|source| SmartZipError::io(Some(request.archive.clone()), source))?;
            std::fs::write(&output_path, contents)
                .map_err(|source| SmartZipError::io(Some(output_path), source))?;
        }

        Ok(ExtractArchiveResult {
            output_dir: request.output_dir,
        })
    }

    async fn compress(&self, request: CompressArchiveRequest) -> Result<CompressArchiveResult> {
        if request.format != ArchiveFormat::Zip {
            return Err(SmartZipError::UnsupportedFormat {
                path: request.output,
                format: Some(request.format.as_str().to_string()),
            });
        }
        if request
            .password
            .as_ref()
            .is_some_and(|password| !password.is_empty())
        {
            return Err(SmartZipError::UnsupportedFormat {
                path: request.output,
                format: Some("encrypted zip".into()),
            });
        }

        let mut writer = ZipFileWriter::new(Vec::new());
        for input in &request.inputs {
            let base = input.parent().unwrap_or_else(|| Path::new("."));
            for file in collect_files(input)? {
                let entry_name = file
                    .strip_prefix(base)
                    .unwrap_or(file.as_path())
                    .to_string_lossy()
                    .replace('\\', "/");
                let contents = std::fs::read(&file)
                    .map_err(|source| SmartZipError::io(Some(file.clone()), source))?;
                let builder = ZipEntryBuilder::new(entry_name.into(), Compression::Deflate);
                writer
                    .write_entry_whole(builder, &contents)
                    .await
                    .map_err(|source| map_zip_error(source, &request.output))?;
            }
        }
        let archive = writer
            .close()
            .await
            .map_err(|source| map_zip_error(source, &request.output))?;
        std::fs::write(&request.output, archive)
            .map_err(|source| SmartZipError::io(Some(request.output.clone()), source))?;

        Ok(CompressArchiveResult {
            output: request.output,
        })
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

fn safe_entry_path(raw: &str) -> Option<PathBuf> {
    let normalized = raw.replace('\\', "/");
    let mut path = PathBuf::new();
    for component in Path::new(&normalized).components() {
        match component {
            Component::Normal(part) => path.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }

    (!path.as_os_str().is_empty()).then_some(path)
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

fn map_zip_error(source: async_zip::error::ZipError, path: &Path) -> SmartZipError {
    match source {
        async_zip::error::ZipError::FeatureNotSupported("encryption") => {
            SmartZipError::PasswordRequired {
                path: path.to_path_buf(),
            }
        }
        async_zip::error::ZipError::FeatureNotSupported(feature) => {
            SmartZipError::UnsupportedFormat {
                path: path.to_path_buf(),
                format: Some(feature.into()),
            }
        }
        async_zip::error::ZipError::CompressionNotSupported(method) => {
            SmartZipError::UnsupportedFormat {
                path: path.to_path_buf(),
                format: Some(format!("zip compression method {method}")),
            }
        }
        async_zip::error::ZipError::CRC32CheckError => SmartZipError::CorruptedArchive {
            path: path.to_path_buf(),
            detail: "zip CRC32 check failed".into(),
        },
        async_zip::error::ZipError::UpstreamReadError(source) => {
            SmartZipError::io(Some(path.to_path_buf()), source)
        }
        other => SmartZipError::BackendFailed {
            backend: "zip".into(),
            exit_code: None,
            stderr: other.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smartzip_core::{CompressionLevel, EncodingMode};

    #[tokio::test]
    async fn native_zip_can_compress_list_test_and_extract() {
        let temp = tempfile::tempdir().unwrap();
        let input_dir = temp.path().join("input");
        let nested_dir = input_dir.join("nested");
        std::fs::create_dir_all(&nested_dir).unwrap();
        std::fs::write(input_dir.join("hello.txt"), "hello zip\n").unwrap();
        std::fs::write(nested_dir.join("data.txt"), "nested data\n").unwrap();

        let archive = temp.path().join("archive.zip");
        let backend = ZipBackend::new();
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

    #[test]
    fn safe_entry_path_rejects_traversal() {
        assert!(safe_entry_path("ok/file.txt").is_some());
        assert!(safe_entry_path("../escape.txt").is_none());
        assert!(safe_entry_path("/absolute.txt").is_none());
    }
}
