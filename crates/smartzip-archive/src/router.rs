use crate::backend::{ArchiveBackend, ExtractionProgressCallback};
use crate::native_zip::NativeZipBackend;
use crate::sevenzz::{SevenZipBackend, SevenZipLocator};
use crate::types::*;
use crate::unrar::{UnrarBackend, UnrarLocator};
use async_trait::async_trait;
use smartzip_core::{ArchiveFormat, Result, SmartZipError};
use std::path::Path;

#[derive(Debug, Clone)]
pub struct BackendRouter {
    zip: NativeZipBackend,
    unrar: Option<UnrarBackend>,
    sevenzip: Option<SevenZipBackend>,
}

impl BackendRouter {
    pub fn new(
        zip: NativeZipBackend,
        unrar: Option<UnrarBackend>,
        sevenzip: Option<SevenZipBackend>,
    ) -> Self {
        Self {
            zip,
            unrar,
            sevenzip,
        }
    }

    pub fn locate() -> Result<Self> {
        let unrar = UnrarBackend::locate(&UnrarLocator::default()).ok();
        let sevenzip = SevenZipBackend::locate(&SevenZipLocator::default()).ok();
        Ok(Self::new(NativeZipBackend::new(), unrar, sevenzip))
    }

    pub fn zip(&self) -> &NativeZipBackend {
        &self.zip
    }

    pub fn unrar(&self) -> Option<&UnrarBackend> {
        self.unrar.as_ref()
    }

    pub fn sevenzip(&self) -> Option<&SevenZipBackend> {
        self.sevenzip.as_ref()
    }

    fn backends_for_path_and_format(
        &self,
        _path: &Path,
        format: Option<&ArchiveFormat>,
    ) -> Vec<&dyn ArchiveBackend> {
        match format {
            Some(ArchiveFormat::Zip) => {
                let mut backends: Vec<&dyn ArchiveBackend> = Vec::new();
                if let Some(sevenzip) = &self.sevenzip {
                    backends.push(sevenzip);
                }
                backends.push(&self.zip);
                backends
            }
            Some(ArchiveFormat::Rar) => {
                let mut backends: Vec<&dyn ArchiveBackend> = Vec::new();
                if let Some(unrar) = &self.unrar {
                    backends.push(unrar);
                }
                if let Some(sevenzip) = &self.sevenzip {
                    backends.push(sevenzip);
                }
                backends
            }
            None => {
                let mut backends: Vec<&dyn ArchiveBackend> = vec![&self.zip];
                if let Some(unrar) = &self.unrar {
                    backends.push(unrar);
                }
                if let Some(sevenzip) = &self.sevenzip {
                    backends.push(sevenzip);
                }
                backends
            }
            _ => self
                .sevenzip
                .as_ref()
                .map(|backend| vec![backend as &dyn ArchiveBackend])
                .unwrap_or_default(),
        }
    }
}

#[async_trait]
impl ArchiveBackend for BackendRouter {
    async fn probe(&self, path: &Path) -> Result<ArchiveProbe> {
        let format = format_from_extension(path);
        let backends = self.backends_for_path_and_format(path, format.as_ref());
        let mut last_error = None;
        for backend in backends {
            match backend.probe(path).await {
                Ok(probe) if probe.supported => return Ok(probe),
                Ok(probe) => {
                    last_error = Some(SmartZipError::UnsupportedFormat {
                        path: probe.path,
                        format: probe.format.map(|format| format.as_str().to_string()),
                    });
                }
                Err(error) if should_fallback(&error) => last_error = Some(error),
                Err(error) => return Err(error),
            }
        }
        Err(
            last_error.unwrap_or_else(|| SmartZipError::BackendUnavailable {
                backend: "archive-router".into(),
            }),
        )
    }

    async fn list(&self, request: ListRequest) -> Result<ArchiveListing> {
        let format = request
            .format
            .clone()
            .or_else(|| format_from_extension(&request.archive));
        let backends = self.backends_for_path_and_format(&request.archive, format.as_ref());
        let mut last_error = None;
        for backend in backends {
            match backend.list(request.clone()).await {
                Ok(result) => return Ok(result),
                Err(error) if should_fallback(&error) => last_error = Some(error),
                Err(error) => return Err(error),
            }
        }
        Err(
            last_error.unwrap_or_else(|| SmartZipError::BackendUnavailable {
                backend: "archive-router".into(),
            }),
        )
    }

    async fn test(&self, request: TestRequest) -> Result<TestResult> {
        let format = request
            .format
            .clone()
            .or_else(|| format_from_extension(&request.archive));
        let backends = self.backends_for_path_and_format(&request.archive, format.as_ref());
        let mut last_error = None;
        for backend in backends {
            match backend.test(request.clone()).await {
                Ok(result) => return Ok(result),
                Err(error) if should_fallback(&error) => last_error = Some(error),
                Err(error) => return Err(error),
            }
        }
        Err(
            last_error.unwrap_or_else(|| SmartZipError::BackendUnavailable {
                backend: "archive-router".into(),
            }),
        )
    }

    async fn extract(&self, request: ExtractArchiveRequest) -> Result<ExtractArchiveResult> {
        self.extract_with_progress(request, None).await
    }

    async fn extract_with_progress(
        &self,
        request: ExtractArchiveRequest,
        progress: Option<ExtractionProgressCallback>,
    ) -> Result<ExtractArchiveResult> {
        let format = request
            .format
            .clone()
            .or_else(|| format_from_extension(&request.archive));
        let backends = self.backends_for_path_and_format(&request.archive, format.as_ref());
        let mut last_error = None;
        for backend in backends {
            match backend
                .extract_with_progress(request.clone(), progress.clone())
                .await
            {
                Ok(result) => return Ok(result),
                Err(error) if should_fallback(&error) => last_error = Some(error),
                Err(error) => return Err(error),
            }
        }
        Err(
            last_error.unwrap_or_else(|| SmartZipError::BackendUnavailable {
                backend: "archive-router".into(),
            }),
        )
    }

    async fn compress(&self, request: CompressArchiveRequest) -> Result<CompressArchiveResult> {
        let backends = self.backends_for_path_and_format(&request.output, Some(&request.format));
        let mut last_error = None;
        for backend in backends {
            match backend.compress(request.clone()).await {
                Ok(result) => return Ok(result),
                Err(error) if should_fallback(&error) => last_error = Some(error),
                Err(error) => return Err(error),
            }
        }
        Err(
            last_error.unwrap_or_else(|| SmartZipError::BackendUnavailable {
                backend: "archive-router".into(),
            }),
        )
    }

    fn capabilities(&self) -> BackendCapabilities {
        let mut capabilities = self.zip.capabilities();
        if let Some(unrar) = &self.unrar {
            merge_capabilities(&mut capabilities, unrar.capabilities());
        }
        if let Some(sevenzip) = &self.sevenzip {
            merge_capabilities(&mut capabilities, sevenzip.capabilities());
        }
        capabilities
    }

    fn should_test_before_extract(&self, archive: &Path, format: Option<&ArchiveFormat>) -> bool {
        self.backends_for_path_and_format(archive, format)
            .first()
            .map(|backend| backend.should_test_before_extract(archive, format))
            .unwrap_or(true)
    }
}

fn should_fallback(error: &SmartZipError) -> bool {
    matches!(
        error,
        SmartZipError::UnsupportedFormat { .. }
            | SmartZipError::BackendUnavailable { .. }
            | SmartZipError::BackendFailed { .. }
            | SmartZipError::PasswordRequired { .. }
    )
}

fn merge_capabilities(target: &mut BackendCapabilities, source: BackendCapabilities) {
    for format in source.can_extract {
        if !target.can_extract.contains(&format) {
            target.can_extract.push(format);
        }
    }
    for format in source.can_compress {
        if !target.can_compress.contains(&format) {
            target.can_compress.push(format);
        }
    }
    target.supports_passwords |= source.supports_passwords;
    target.supports_listing |= source.supports_listing;
    target.supports_test |= source.supports_test;
}

fn format_from_extension(path: impl AsRef<Path>) -> Option<ArchiveFormat> {
    let extension = path
        .as_ref()
        .extension()
        .and_then(|extension| extension.to_str())?
        .to_ascii_lowercase();

    match extension.as_str() {
        "zip" => Some(ArchiveFormat::Zip),
        "7z" => Some(ArchiveFormat::SevenZip),
        "rar" => Some(ArchiveFormat::Rar),
        "tar" => Some(ArchiveFormat::Tar),
        "gz" | "tgz" => Some(ArchiveFormat::Gzip),
        "bz2" | "tbz2" => Some(ArchiveFormat::Bzip2),
        "xz" | "txz" => Some(ArchiveFormat::Xz),
        "cab" => Some(ArchiveFormat::Cab),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Write;
    use zip::write::SimpleFileOptions;
    use zip::ZipWriter;

    #[test]
    fn unknown_extension_routes_through_native_zip_unrar_then_sevenzip() {
        let router = BackendRouter::new(
            NativeZipBackend::new(),
            Some(UnrarBackend::new(Path::new("/usr/bin/unrar").to_path_buf())),
            Some(SevenZipBackend::new(
                Path::new("/usr/bin/7zz").to_path_buf(),
            )),
        );

        assert_eq!(
            router
                .backends_for_path_and_format(Path::new("archive.bin"), None)
                .len(),
            3
        );
    }

    #[test]
    fn format_hint_routes_disguised_rar_to_unrar_first() {
        let router = BackendRouter::new(
            NativeZipBackend::new(),
            Some(UnrarBackend::new(Path::new("/usr/bin/unrar").to_path_buf())),
            Some(SevenZipBackend::new(
                Path::new("/usr/bin/7zz").to_path_buf(),
            )),
        );

        assert_eq!(
            router
                .backends_for_path_and_format(Path::new("archive.rar"), Some(&ArchiveFormat::Rar))
                .len(),
            2
        );
    }

    #[test]
    fn zip_routes_to_sevenzip_first_when_available() {
        let router = BackendRouter::new(
            NativeZipBackend::new(),
            Some(UnrarBackend::new(Path::new("/usr/bin/unrar").to_path_buf())),
            Some(SevenZipBackend::new(
                Path::new("/usr/bin/7zz").to_path_buf(),
            )),
        );

        let backends =
            router.backends_for_path_and_format(Path::new("split.zip"), Some(&ArchiveFormat::Zip));
        assert_eq!(backends.len(), 2);
        assert!(backends[0]
            .capabilities()
            .can_extract
            .contains(&ArchiveFormat::SevenZip));
    }

    #[tokio::test]
    async fn default_router_extracts_zip_with_overlong_filename() {
        let temp = tempfile::tempdir().unwrap();
        let archive = temp.path().join("long-name.zip");
        let long_name = format!("{}.txt", "a".repeat(300));
        let file = File::create(&archive).unwrap();
        let mut writer = ZipWriter::new(file);
        writer
            .start_file(long_name, SimpleFileOptions::default())
            .unwrap();
        writer.write_all(b"routed content").unwrap();
        writer.finish().unwrap();

        let output_dir = temp.path().join("output");
        BackendRouter::locate()
            .unwrap()
            .extract(ExtractArchiveRequest {
                archive,
                format: Some(ArchiveFormat::Zip),
                output_dir: output_dir.clone(),
                password: None,
                encoding: smartzip_core::EncodingMode::Auto,
            })
            .await
            .unwrap();

        let extracted: Vec<_> = std::fs::read_dir(output_dir)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        assert_eq!(extracted.len(), 1);
        assert_eq!(std::fs::read(&extracted[0]).unwrap(), b"routed content");
    }
}
