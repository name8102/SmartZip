//! Application-level orchestration for SmartZip workflows.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use smartzip_archive::{ArchiveBackend, ExtractArchiveRequest, ListRequest, TestRequest};
use smartzip_core::{ArchiveFormat, EncodingMode, TaskEvent, TaskEventKind, TaskId};
use smartzip_passwords::{PasswordCandidate, PasswordCandidateRequest, PasswordService};
use smartzip_scanner::{EmbeddedArchiveFinding, EmbeddedScanner, ScannerConfig};
use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};

/// Allows interactive password prompting during extraction.
///
/// When all stored/candidate passwords fail for an archive, the engine
/// calls this trait to give the user a chance to enter a password manually.
/// If the user provides one and it succeeds, the password is automatically
/// saved to the password database via [`PasswordService::record_success`].
#[async_trait]
pub trait InteractivePasswordPrompter: Send + Sync {
    /// Prompt the user for a password for the given archive.
    ///
    /// Return `Some(password)` if the user entered one, or `None` to skip
    /// this archive. Implementations should use `spawn_blocking` for any
    /// blocking I/O (e.g. stdin reads) to avoid stalling the async runtime.
    async fn prompt(&self, archive_path: &Path) -> Option<String>;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DetectRequest {
    pub path: PathBuf,
    pub scanner: ScannerConfig,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DetectResult {
    pub task_id: TaskId,
    pub path: PathBuf,
    pub findings: Vec<EmbeddedArchiveFinding>,
    pub events: Vec<TaskEvent>,
}

pub struct SmartZipEngine {
    scanner: EmbeddedScanner,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtractWorkflowRequest {
    pub inputs: Vec<PathBuf>,
    pub output_dir: PathBuf,
    pub recursion_limit: u8,
    pub encoding_mode: EncodingMode,
    pub scanner: ScannerConfig,
    pub password_candidates: PasswordCandidateRequest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtractionCandidate {
    pub path: PathBuf,
    pub depth: u8,
    pub source: CandidateSource,
    pub detected_format: Option<ArchiveFormat>,
    pub embedded_offset: Option<u64>,
    pub embedded_size: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CandidateSource {
    RootInput,
    ExtractedFile,
    EmbeddedFinding,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExtractWorkflowResult {
    pub task_id: TaskId,
    pub processed: Vec<ExtractionCandidate>,
    pub skipped: Vec<ExtractionCandidate>,
    pub enqueued: Vec<ExtractionCandidate>,
    pub events: Vec<TaskEvent>,
}

impl SmartZipEngine {
    pub fn new(scanner: EmbeddedScanner) -> Self {
        Self { scanner }
    }

    pub fn with_scanner_config(config: ScannerConfig) -> Self {
        Self::new(EmbeddedScanner::new(config))
    }

    pub fn detect(&self, request: DetectRequest) -> std::io::Result<DetectResult> {
        let task_id = TaskId::new();
        let mut events = vec![TaskEvent::started(task_id.clone())];

        let scanner = if request.scanner == *self.scanner.config() {
            None
        } else {
            Some(EmbeddedScanner::new(request.scanner.clone()))
        };
        let scanner = scanner.as_ref().unwrap_or(&self.scanner);

        events.push(TaskEvent {
            task_id: task_id.clone(),
            kind: TaskEventKind::Progress(smartzip_core::TaskProgress::indeterminate(format!(
                "Scanning {}",
                request.path.display()
            ))),
        });

        let findings = scanner.scan_path(&request.path)?;
        for finding in &findings {
            events.push(TaskEvent {
                task_id: task_id.clone(),
                kind: TaskEventKind::EmbeddedArchiveFound {
                    offset: finding.offset,
                    size: finding.size,
                    format: finding.format.clone(),
                    confidence: confidence_score(finding.confidence),
                    description: finding.description.clone(),
                },
            });
        }

        events.push(TaskEvent {
            task_id: task_id.clone(),
            kind: TaskEventKind::Completed,
        });

        Ok(DetectResult {
            task_id,
            path: request.path,
            findings,
            events,
        })
    }

    pub async fn extract_recursive<B: ArchiveBackend>(
        &self,
        backend: &B,
        passwords: &PasswordService<'_>,
        request: ExtractWorkflowRequest,
        password_prompter: Option<&dyn InteractivePasswordPrompter>,
    ) -> smartzip_core::Result<ExtractWorkflowResult> {
        let task_id = TaskId::new();
        let scanner = if request.scanner == *self.scanner.config() {
            None
        } else {
            Some(EmbeddedScanner::new(request.scanner.clone()))
        };
        let scanner = scanner.as_ref().unwrap_or(&self.scanner);

        let mut events = vec![TaskEvent::started(task_id.clone())];
        let mut queue = VecDeque::new();
        let mut seen = HashSet::new();
        let mut processed = Vec::new();
        let mut skipped = Vec::new();
        let mut enqueued = Vec::new();

        for input in request.inputs {
            queue.push_back(ExtractionCandidate {
                detected_format: format_from_extension(&input),
                path: input,
                depth: 0,
                source: CandidateSource::RootInput,
                embedded_offset: None,
                embedded_size: None,
            });
        }

        // C6: Cache password candidates once before the extraction loop.
        let password_candidates = passwords
            .ranked_candidates(request.password_candidates.clone())
            .map_err(|error| smartzip_core::SmartZipError::BackendFailed {
                backend: "password-db".into(),
                exit_code: None,
                stderr: error.to_string(),
            })?;

        while let Some(mut candidate) = queue.pop_front() {
            let key = candidate_key(&candidate);
            if !seen.insert(key)
                || candidate.depth > request.recursion_limit
                || !is_first_volume(&candidate.path)
            {
                skipped.push(candidate);
                continue;
            }

            // B1: magic-number (scanner) detection first, extension fallback
            let findings = scanner.scan_path(&candidate.path).unwrap_or_default();
            if candidate.detected_format.is_none() {
                candidate.detected_format = findings.first().map(|finding| finding.format.clone());
            }
            if candidate.detected_format.is_none() {
                candidate.detected_format = format_from_extension(&candidate.path);
            }

            if candidate.detected_format.is_none() {
                skipped.push(candidate);
                continue;
            }

            events.push(TaskEvent {
                task_id: task_id.clone(),
                kind: TaskEventKind::Progress(smartzip_core::TaskProgress::indeterminate(format!(
                    "Extracting {} at depth {}",
                    candidate.path.display(),
                    candidate.depth
                ))),
            });

            let mut encoding_result = None;
            if request.encoding_mode == EncodingMode::Auto {
                if let Ok(listing) = backend
                    .list(ListRequest {
                        archive: candidate.path.clone(),
                        password: None,
                        encoding: EncodingMode::Auto,
                    })
                    .await
                {
                    let raw_names: Vec<u8> = listing
                        .entries
                        .iter()
                        .flat_map(|entry| entry.path.as_os_str().as_encoded_bytes())
                        .copied()
                        .collect();
                    if !raw_names.is_empty() {
                        let mut detector = smartzip_encoding::ArchiveEncodingDetector::new();
                        let result = detector.detect(&raw_names);
                        events.push(TaskEvent {
                            task_id: task_id.clone(),
                            kind: TaskEventKind::EncodingDetected(
                                smartzip_core::EncodingDetectionResult {
                                    selected: EncodingMode::Override(result.selected.clone()),
                                    confidence: result.confidence,
                                    candidates: result
                                        .candidates
                                        .iter()
                                        .map(|c| smartzip_core::EncodingCandidate {
                                            name: c.name.clone(),
                                            confidence: c.confidence,
                                        })
                                        .collect(),
                                },
                            ),
                        });
                        encoding_result = Some(result);
                    }
                }
            }

            let output_dir = output_dir_for_candidate(&request.output_dir, &candidate);
            let mut extracted = false;
            let mut last_error = None;
            for password in &password_candidates {
                // B3: Test-first — use test() to check password, then extract once
                let pw_value = password_value(password);
                match backend
                    .test(TestRequest {
                        archive: candidate.path.clone(),
                        password: pw_value.clone(),
                    })
                    .await
                {
                    Ok(result) if result.ok => {
                        let _ = passwords.record_success(password);

                        // B2: If pre-list failed (encrypted archive), detect encoding now
                        if encoding_result.is_none()
                            && request.encoding_mode == EncodingMode::Auto
                        {
                            if let Ok(listing) = backend
                                .list(ListRequest {
                                    archive: candidate.path.clone(),
                                    password: pw_value.clone(),
                                    encoding: EncodingMode::Auto,
                                })
                                .await
                            {
                                let raw_names: Vec<u8> = listing
                                    .entries
                                    .iter()
                                    .flat_map(|entry| {
                                        entry.path.as_os_str().as_encoded_bytes()
                                    })
                                    .copied()
                                    .collect();
                                if !raw_names.is_empty() {
                                    let mut detector =
                                        smartzip_encoding::ArchiveEncodingDetector::new();
                                    let result = detector.detect(&raw_names);
                                    events.push(TaskEvent {
                                        task_id: task_id.clone(),
                                        kind: TaskEventKind::EncodingDetected(
                                            smartzip_core::EncodingDetectionResult {
                                                selected: EncodingMode::Override(
                                                    result.selected.clone(),
                                                ),
                                                confidence: result.confidence,
                                                candidates: result
                                                    .candidates
                                                    .iter()
                                                    .map(|c| {
                                                        smartzip_core::EncodingCandidate {
                                                            name: c.name.clone(),
                                                            confidence: c.confidence,
                                                        }
                                                    })
                                                    .collect(),
                                            },
                                        ),
                                    });
                                    encoding_result = Some(result);
                                }
                            }
                        }

                        let encoding_to_use = encoding_result
                            .as_ref()
                            .map(|r| EncodingMode::Override(r.selected.clone()))
                            .unwrap_or(EncodingMode::Auto);

                        // Single extract with the correct password + encoding
                        match backend
                            .extract(ExtractArchiveRequest {
                                archive: candidate.path.clone(),
                                output_dir: output_dir.clone(),
                                password: pw_value,
                                encoding: encoding_to_use,
                            })
                            .await
                        {
                            Ok(_) => {
                                extracted = true;
                                break;
                            }
                            Err(error) => {
                                last_error = Some(error);
                            }
                        }
                    }
                    Ok(_) => {
                        let _ = passwords.record_failure(password);
                    }
                    Err(error) => {
                        let _ = passwords.record_failure(password);
                        // WrongPassword is expected during test — only save serious errors
                        if !matches!(
                            &error,
                            smartzip_core::SmartZipError::WrongPassword { .. }
                        ) {
                            last_error = Some(error);
                        }
                    }
                }
            }

            if !extracted {
                // Interactive fallback: prompt the user for a password
                if let Some(prompter) = password_prompter {
                    if let Some(interactive_pw) = prompter.prompt(&candidate.path).await {
                        let pw = interactive_pw.trim().to_string();
                        if !pw.is_empty() {
                            match backend
                                .extract(ExtractArchiveRequest {
                                    archive: candidate.path.clone(),
                                    output_dir: output_dir.clone(),
                                    password: Some(pw.clone()),
                                    encoding: encoding_result
                                        .as_ref()
                                        .map(|r| EncodingMode::Override(r.selected.clone()))
                                        .unwrap_or(EncodingMode::Auto),
                                })
                                .await
                            {
                                Ok(_) => {
                                    // Save the successful interactive password to DB
                                    let _ = passwords.record_success(&PasswordCandidate {
                                        id: None,
                                        value: pw,
                                        source: smartzip_passwords::PasswordSource::Manual,
                                    });
                                    extracted = true;
                                }
                                Err(error) => {
                                    eprintln!(
                                        "Interactive password failed for {}: {}",
                                        candidate.path.display(),
                                        error
                                    );
                                }
                            }
                        }
                    }
                }
            }

            if !extracted {
                if let Some(error) = last_error {
                    events.push(TaskEvent::failed(task_id.clone(), &error));
                }
                skipped.push(candidate);
                continue;
            }

            let final_dir = collapse_single_output(&output_dir, &candidate).unwrap_or(output_dir);
            events.push(TaskEvent {
                task_id: task_id.clone(),
                kind: TaskEventKind::OutputCreated {
                    path: final_dir.clone(),
                },
            });

            processed.push(candidate.clone());
            for nested in discover_nested_candidates(scanner, &final_dir, candidate.depth + 1) {
                enqueued.push(nested.clone());
                queue.push_back(nested);
            }
        }

        events.push(TaskEvent {
            task_id: task_id.clone(),
            kind: TaskEventKind::Completed,
        });

        Ok(ExtractWorkflowResult {
            task_id,
            processed,
            skipped,
            enqueued,
            events,
        })
    }
}

impl Default for SmartZipEngine {
    fn default() -> Self {
        Self::new(EmbeddedScanner::default())
    }
}

fn confidence_score(confidence: smartzip_scanner::Confidence) -> f32 {
    match confidence {
        smartzip_scanner::Confidence::Low => 0.33,
        smartzip_scanner::Confidence::Medium => 0.66,
        smartzip_scanner::Confidence::High => 1.0,
    }
}

fn password_value(candidate: &PasswordCandidate) -> Option<String> {
    (!candidate.value.is_empty()).then_some(candidate.value.clone())
}

fn candidate_key(candidate: &ExtractionCandidate) -> String {
    format!(
        "{}:{}:{:?}",
        candidate.path.display(),
        candidate.embedded_offset.unwrap_or(0),
        candidate.source
    )
}

fn output_dir_for_candidate(base: &Path, candidate: &ExtractionCandidate) -> PathBuf {
    let stem = candidate
        .path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|stem| !stem.is_empty())
        .unwrap_or("archive");
    base.join(format!("{}-d{}", stem, candidate.depth))
}

fn collapse_single_output(
    extraction_dir: &Path,
    _candidate: &ExtractionCandidate,
) -> std::io::Result<PathBuf> {
    let entries: Vec<_> = std::fs::read_dir(extraction_dir)?
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_name() != "." && entry.file_name() != "..")
        .collect();

    if entries.len() != 1 {
        return Ok(extraction_dir.to_path_buf());
    }

    let entry_name = entries[0].file_name();
    let parent = extraction_dir.parent().unwrap_or(Path::new("."));
    let target = find_non_colliding_name(parent, &entry_name);
    std::fs::rename(entries[0].path(), &target)?;
    let _ = std::fs::remove_dir_all(extraction_dir);
    Ok(target)
}

/// Find a non-colliding target path. If `parent.join(name)` exists,
/// append `_collided_N` until a free name is found.
fn find_non_colliding_name(parent: &Path, name: &std::ffi::OsStr) -> PathBuf {
    let base = parent.join(name);
    if !base.exists() {
        return base;
    }
    let name_str = name.to_string_lossy();
    for n in 1..1000u32 {
        let alt = parent.join(format!("{name_str}_collided_{n}"));
        if !alt.exists() {
            return alt;
        }
    }
    parent.join(format!("{name_str}_{}", std::process::id()))
}

fn discover_nested_candidates(
    scanner: &EmbeddedScanner,
    root: &Path,
    depth: u8,
) -> Vec<ExtractionCandidate> {
    let mut candidates = Vec::new();

    // Handle single-file roots (produced by collapse_single_output).
    if root.is_file() {
        if let Some(format) = format_from_extension(root) {
            candidates.push(ExtractionCandidate {
                path: root.to_path_buf(),
                depth,
                source: CandidateSource::ExtractedFile,
                detected_format: Some(format),
                embedded_offset: None,
                embedded_size: None,
            });
            return candidates;
        }
        for finding in scanner.scan_path(root).unwrap_or_default() {
            candidates.push(ExtractionCandidate {
                path: root.to_path_buf(),
                depth,
                source: CandidateSource::EmbeddedFinding,
                detected_format: Some(finding.format),
                embedded_offset: Some(finding.offset),
                embedded_size: finding.size,
            });
        }
        return candidates;
    }

    let Ok(entries) = std::fs::read_dir(root) else {
        return candidates;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            candidates.extend(discover_nested_candidates(scanner, &path, depth));
            continue;
        }

        let detected_format = format_from_extension(&path);
        if detected_format.is_some() {
            candidates.push(ExtractionCandidate {
                path: path.clone(),
                depth,
                source: CandidateSource::ExtractedFile,
                detected_format,
                embedded_offset: None,
                embedded_size: None,
            });
            continue;
        }

        for finding in scanner.scan_path(&path).unwrap_or_default() {
            candidates.push(ExtractionCandidate {
                path: path.clone(),
                depth,
                source: CandidateSource::EmbeddedFinding,
                detected_format: Some(finding.format),
                embedded_offset: Some(finding.offset),
                embedded_size: finding.size,
            });
        }
    }

    candidates
}

pub fn is_first_volume(path: impl AsRef<std::path::Path>) -> bool {
    let file_name = path
        .as_ref()
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();

    if file_name.ends_with(".part1.rar") {
        return true;
    }

    if file_name.contains(".part") && file_name.ends_with(".rar") {
        return false;
    }

    if let Some(extension) = path.as_ref().extension().and_then(|ext| ext.to_str()) {
        if extension.len() == 3 && extension.chars().all(|ch| ch.is_ascii_digit()) {
            return extension == "001";
        }
    }

    true
}

pub fn format_from_extension(path: impl AsRef<std::path::Path>) -> Option<ArchiveFormat> {
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
        "gz" | "gzip" | "tgz" => Some(ArchiveFormat::Gzip),
        "bz2" => Some(ArchiveFormat::Bzip2),
        "xz" => Some(ArchiveFormat::Xz),
        "cab" => Some(ArchiveFormat::Cab),
        "iso" => Some(ArchiveFormat::Iso),
        "dmg" => Some(ArchiveFormat::Dmg),
        "zst" | "zstd" => Some(ArchiveFormat::Zstd),
        "lz4" => Some(ArchiveFormat::Lz4),
        "lzma" => Some(ArchiveFormat::Lzma),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use smartzip_archive::{
        ArchiveBackend, ArchiveListing, ArchiveProbe, BackendCapabilities, CompressArchiveRequest,
        CompressArchiveResult, ExtractArchiveRequest, ExtractArchiveResult, ListRequest,
        SevenZipBackend, TestRequest, TestResult,
    };
    use smartzip_db::{password::PasswordRepository, SmartZipDb};
    use smartzip_passwords::{PasswordCandidateRequest, PasswordService};
    use smartzip_scanner::ScanMode;
    use rstest::*;
    use std::sync::{Arc, Mutex};

    #[test]
    fn detects_empty_file_without_findings() {
        let path =
            std::env::temp_dir().join(format!("smartzip-engine-empty-{}", std::process::id()));
        std::fs::write(&path, []).unwrap();

        let engine = SmartZipEngine::default();
        let result = engine
            .detect(DetectRequest {
                path: path.clone(),
                scanner: ScannerConfig::default(),
            })
            .unwrap();
        let _ = std::fs::remove_file(path);

        assert!(result.findings.is_empty());
        assert!(matches!(
            result.events.first().unwrap().kind,
            TaskEventKind::Started
        ));
        assert!(matches!(
            result.events.last().unwrap().kind,
            TaskEventKind::Completed
        ));
    }

    #[test]
    fn recognizes_first_volume_rules() {
        assert!(is_first_volume("archive.part1.rar"));
        assert!(!is_first_volume("archive.part2.rar"));
        assert!(is_first_volume("archive.001"));
        assert!(!is_first_volume("archive.002"));
        assert!(is_first_volume("archive.zip"));
    }

    #[test]
    fn maps_common_extensions() {
        assert_eq!(format_from_extension("a.7z"), Some(ArchiveFormat::SevenZip));
        assert_eq!(format_from_extension("a.tgz"), Some(ArchiveFormat::Gzip));
        assert_eq!(format_from_extension("a.bin"), None);
    }

    #[rstest]
    #[case("a.zip", Some(ArchiveFormat::Zip))]
    #[case("a.7z", Some(ArchiveFormat::SevenZip))]
    #[case("a.rar", Some(ArchiveFormat::Rar))]
    #[case("a.tar", Some(ArchiveFormat::Tar))]
    #[case("a.gz", Some(ArchiveFormat::Gzip))]
    #[case("a.gzip", Some(ArchiveFormat::Gzip))]
    #[case("a.tgz", Some(ArchiveFormat::Gzip))]
    #[case("a.bz2", Some(ArchiveFormat::Bzip2))]
    #[case("a.xz", Some(ArchiveFormat::Xz))]
    #[case("a.cab", Some(ArchiveFormat::Cab))]
    #[case("a.iso", Some(ArchiveFormat::Iso))]
    #[case("a.dmg", Some(ArchiveFormat::Dmg))]
    #[case("a.zst", Some(ArchiveFormat::Zstd))]
    #[case("a.zstd", Some(ArchiveFormat::Zstd))]
    #[case("a.lz4", Some(ArchiveFormat::Lz4))]
    #[case("a.lzma", Some(ArchiveFormat::Lzma))]
    #[case("a.txt", None)]
    #[case("a.bin", None)]
    #[case("no-extension", None)]
    #[case("a.ZIP", Some(ArchiveFormat::Zip))]
    #[case("A.7Z", Some(ArchiveFormat::SevenZip))]
    fn format_from_extension_parametrized(#[case] path: &str, #[case] expected: Option<ArchiveFormat>) {
        assert_eq!(format_from_extension(path), expected);
    }

    #[rstest]
    #[case("archive.part1.rar", true)]
    #[case("archive.part2.rar", false)]
    #[case("archive.part5.rar", false)]
    #[case("archive.rar", true)]
    #[case("archive.001", true)]
    #[case("archive.002", false)]
    #[case("archive.010", false)]
    #[case("archive.zip", true)]
    #[case("archive.7z", true)]
    #[case("data.tar.gz", true)]
    fn is_first_volume_parametrized(#[case] path: &str, #[case] expected: bool) {
        assert_eq!(is_first_volume(path), expected);
    }

    #[test]
    fn engine_accepts_custom_scanner_config() {
        let engine = SmartZipEngine::with_scanner_config(ScannerConfig {
            mode: ScanMode::Deep,
            ..ScannerConfig::default()
        });
        assert_eq!(engine.scanner.config().mode, ScanMode::Deep);
    }

    #[tokio::test]
    async fn recursive_extract_enqueues_nested_archives_and_skips_non_first_volume() {
        let root =
            std::env::temp_dir().join(format!("smartzip-engine-recursive-{}", std::process::id()));
        let input = root.join("root.zip");
        let output = root.join("out");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(&input, b"not really a zip").unwrap();

        let backend = FakeBackend::default();
        let db = SmartZipDb::in_memory().unwrap();
        let service = PasswordService::new(PasswordRepository::new(db.connection()));
        service.add_password("secret", "manual", false).unwrap();

        let engine = SmartZipEngine::default();
        let result = engine
            .extract_recursive(
                &backend,
                &service,
                ExtractWorkflowRequest {
                    inputs: vec![input.clone(), root.join("skip.part2.rar")],
                    output_dir: output,
                    recursion_limit: 2,
                    encoding_mode: EncodingMode::Auto,
                    scanner: ScannerConfig::default(),
                    password_candidates: PasswordCandidateRequest {
                        include_empty: false,
                        limit: 10,
                        ..PasswordCandidateRequest::default()
                    },
                },
                None,
            )
            .await
            .unwrap();

        let calls = backend.calls.lock().unwrap().clone();
        assert!(calls.iter().any(|path| path.ends_with("root.zip")));
        assert!(calls.iter().any(|path| path.ends_with("nested.zip")));
        assert!(!calls.iter().any(|path| path.ends_with("skip.part2.rar")));
        assert!(result
            .processed
            .iter()
            .any(|candidate| candidate.path == input));
        assert!(result
            .enqueued
            .iter()
            .any(|candidate| candidate.path.ends_with("nested.zip")));
        assert!(result
            .skipped
            .iter()
            .any(|candidate| candidate.path.ends_with("skip.part2.rar")));

        let ranked = PasswordRepository::new(db.connection())
            .ranked_candidates(10)
            .unwrap();
        assert_eq!(ranked[0].success_count, 2);

        let _ = std::fs::remove_dir_all(root);
    }

    #[derive(Default, Clone)]
    struct FakeBackend {
        calls: Arc<Mutex<Vec<String>>>,
    }

    #[async_trait]
    impl ArchiveBackend for FakeBackend {
        async fn probe(&self, path: &std::path::Path) -> smartzip_core::Result<ArchiveProbe> {
            Ok(ArchiveProbe {
                path: path.to_path_buf(),
                format: format_from_extension(path),
                encrypted: Some(true),
                supported: true,
            })
        }

        async fn list(&self, _request: ListRequest) -> smartzip_core::Result<ArchiveListing> {
            Ok(ArchiveListing {
                format: Some(ArchiveFormat::Zip),
                entries: Vec::new(),
            })
        }

        async fn test(&self, _request: TestRequest) -> smartzip_core::Result<TestResult> {
            Ok(TestResult {
                ok: true,
                encrypted: Some(true),
            })
        }

        async fn extract(
            &self,
            request: ExtractArchiveRequest,
        ) -> smartzip_core::Result<ExtractArchiveResult> {
            self.calls
                .lock()
                .unwrap()
                .push(request.archive.display().to_string());
            std::fs::create_dir_all(&request.output_dir).map_err(|source| {
                smartzip_core::SmartZipError::io(Some(request.output_dir.clone()), source)
            })?;
            if request.archive.file_name().and_then(|name| name.to_str()) == Some("root.zip") {
                std::fs::write(request.output_dir.join("nested.zip"), b"nested").map_err(
                    |source| {
                        smartzip_core::SmartZipError::io(Some(request.output_dir.clone()), source)
                    },
                )?;
                std::fs::write(request.output_dir.join("readme.txt"), b"readme").map_err(
                    |source| {
                        smartzip_core::SmartZipError::io(Some(request.output_dir.clone()), source)
                    },
                )?;
            }
            Ok(ExtractArchiveResult {
                output_dir: request.output_dir,
            })
        }

        async fn compress(
            &self,
            request: CompressArchiveRequest,
        ) -> smartzip_core::Result<CompressArchiveResult> {
            Ok(CompressArchiveResult {
                output: request.output,
            })
        }

        fn capabilities(&self) -> BackendCapabilities {
            BackendCapabilities {
                can_extract: vec![ArchiveFormat::Zip],
                can_compress: vec![ArchiveFormat::Zip],
                supports_passwords: true,
                supports_listing: true,
                supports_test: true,
            }
        }
    }

    #[tokio::test]
    async fn extract_via_real_seven_zip_with_smart_output() {
        let root = std::env::temp_dir().join(format!("smartzip-int-{}", std::process::id()));
        let archive = root.join("test.zip");
        let extracted_file = root.join("hello.txt");
        let output = root.join("out");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(&extracted_file, b"hello world").unwrap();

        let status = std::process::Command::new("7z")
            .arg("a")
            .arg(&archive)
            .arg(&extracted_file)
            .current_dir(&root)
            .status()
            .unwrap();
        assert!(status.success(), "7z must be available in PATH");
        std::fs::remove_file(&extracted_file).unwrap();

        let backend = SevenZipBackend::locate(&smartzip_archive::SevenZipLocator::default())
            .expect("7z/7zz must be available");
        let db = SmartZipDb::in_memory().unwrap();
        let service = PasswordService::new(PasswordRepository::new(db.connection()));

        let engine = SmartZipEngine::default();
        let result = engine
            .extract_recursive(
                &backend,
                &service,
                ExtractWorkflowRequest {
                    inputs: vec![archive.clone()],
                    output_dir: output.clone(),
                    recursion_limit: 1,
                    encoding_mode: EncodingMode::Auto,
                    scanner: ScannerConfig::default(),
                    password_candidates: PasswordCandidateRequest::default(),
                },
                None,
            )
            .await
            .unwrap();

        assert_eq!(result.processed.len(), 1);

        // Verify the extracted content exists somewhere under output.
        let candidates = [
            output.join("hello.txt"),
            output.join("test-d0").join("hello.txt"),
            output.join("test").join("hello.txt"),
        ];
        assert!(
            candidates.iter().any(|p| p.exists()),
            "expected hello.txt in one of {:?}",
            candidates
        );

        let _ = std::fs::remove_dir_all(root);
    }
}
