//! Nested candidate discovery, carve, recycle, volume helpers.

use smartzip_core::{ArchiveFormat, TaskId};
use smartzip_scanner::{EmbeddedArchiveFinding, EmbeddedScanner};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use crate::name_score;
use crate::policy::{finding_meets_min_size, is_business_container};
use crate::types::{ArchiveRecycleHandler, CandidateSource, ExtractionCandidate};

pub(crate) fn record_skip(
    history: Option<&dyn crate::history::TaskHistoryRecorder>,
    task_id: &TaskId,
    candidate: &ExtractionCandidate,
    reason: &str,
) {
    if let Some(recorder) = history {
        recorder.record_file_extraction(
            task_id,
            crate::history::FileExtractionRow::skipped(
                &candidate.path,
                candidate.embedded_offset,
                reason,
            ),
        );
    }
}

pub(crate) fn archive_output_name(path: &Path) -> PathBuf {
    PathBuf::from(archive_stem(path))
}

pub(crate) fn archive_stem(path: &Path) -> std::ffi::OsString {
    std::ffi::OsString::from(name_score::archive_display_stem(path))
}

pub(crate) fn candidate_key(candidate: &ExtractionCandidate) -> String {
    format!(
        "{}:{}:{:?}",
        candidate.path.display(),
        candidate.embedded_offset.unwrap_or(0),
        candidate.source
    )
}

pub(crate) fn root_embedded_candidates(
    root: &ExtractionCandidate,
    findings: &[EmbeddedArchiveFinding],
) -> Vec<ExtractionCandidate> {
    if root.source != CandidateSource::RootInput
        || root.embedded_offset.is_some()
        || (findings.len() == 1 && findings[0].offset == 0)
    {
        return Vec::new();
    }

    findings
        .iter()
        .enumerate()
        .map(|(index, finding)| {
            let mut relative_path = root.relative_path.clone();
            let base_name = relative_path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy();
            if findings.len() > 1 {
                relative_path.set_file_name(format!("{base_name}-{}", index + 1));
            }
            ExtractionCandidate {
                path: root.path.clone(),
                relative_path,
                depth: root.depth,
                source: CandidateSource::EmbeddedFinding,
                detected_format: Some(finding.format.clone()),
                embedded_offset: Some(finding.offset),
                embedded_size: finding.size,
            }
        })
        .collect()
}

pub(crate) fn output_dir_for_candidate(base: &Path, candidate: &ExtractionCandidate) -> PathBuf {
    match candidate.source {
        CandidateSource::RootInput => base.join(candidate_output_relative_path(candidate)),
        CandidateSource::EmbeddedFinding if candidate.depth == 0 => {
            base.join(candidate_output_relative_path(candidate))
        }
        CandidateSource::ExtractedFile | CandidateSource::EmbeddedFinding => candidate
            .path
            .parent()
            .unwrap_or(base)
            .join(archive_output_name(&candidate.path)),
    }
}

pub(crate) fn candidate_output_relative_path(candidate: &ExtractionCandidate) -> PathBuf {
    candidate.relative_path.clone()
}

pub(crate) fn recyclable_nested_archive_path(
    candidate: &ExtractionCandidate,
    managed_output_root: &Path,
) -> Option<PathBuf> {
    if candidate.source != CandidateSource::ExtractedFile
        || candidate.embedded_offset.is_some_and(|offset| offset > 0)
    {
        return None;
    }

    let metadata = std::fs::symlink_metadata(&candidate.path).ok()?;
    if !metadata.file_type().is_file() {
        return None;
    }

    let canonical_output_root = managed_output_root.canonicalize().ok()?;
    let canonical_path = candidate.path.canonicalize().ok()?;
    canonical_path
        .starts_with(&canonical_output_root)
        .then_some(candidate.path.clone())
}

pub(crate) async fn recycle_archive(
    archive_recycler: ArchiveRecycleHandler,
    path: PathBuf,
) -> std::io::Result<()> {
    tokio::task::spawn_blocking(move || archive_recycler(path))
        .await
        .map_err(std::io::Error::other)?
}

pub(crate) struct ArchiveInput {
    pub(crate) path: PathBuf,
    pub(crate) _temp: Option<tempfile::NamedTempFile>,
}

pub(crate) fn materialize_archive_input(
    candidate: &ExtractionCandidate,
    staging_root: Option<&Path>,
) -> smartzip_core::Result<ArchiveInput> {
    if let Some(offset) = candidate
        .embedded_offset
        .filter(|offset| *offset > 0 || candidate.source == CandidateSource::EmbeddedFinding)
    {
        let temp = carve_embedded_archive(
            &candidate.path,
            offset,
            candidate.embedded_size,
            candidate.detected_format.as_ref(),
            staging_root,
        )
        .map_err(
            |source| smartzip_core::SmartZipError::EmbeddedArchiveCarveFailed {
                path: candidate.path.clone(),
                offset,
                detail: source.to_string(),
            },
        )?;
        let path = temp.path().to_path_buf();
        Ok(ArchiveInput {
            path,
            _temp: Some(temp),
        })
    } else {
        Ok(ArchiveInput {
            path: candidate.path.clone(),
            _temp: None,
        })
    }
}

pub(crate) fn output_relative_path_for(base: &Path, output_dir: &Path) -> PathBuf {
    output_dir
        .strip_prefix(base)
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            output_dir
                .file_name()
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("archive"))
        })
}

pub(crate) fn carve_embedded_archive(
    source: &Path,
    offset: u64,
    size: Option<u64>,
    format: Option<&ArchiveFormat>,
    staging_root: Option<&Path>,
) -> std::io::Result<tempfile::NamedTempFile> {
    let file_len = std::fs::metadata(source)?.len();

    if offset >= file_len {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("carve offset {} exceeds file size {}", offset, file_len),
        ));
    }

    let effective_end = match size {
        Some(s) => {
            if s == 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "carve size cannot be zero",
                ));
            }
            offset.saturating_add(s).min(file_len)
        }
        None => {
            if format == Some(&ArchiveFormat::Zip) {
                if let Ok(Some(zip_end)) = crate::embedded_zip::detect_zip_end(source, offset) {
                    zip_end
                } else {
                    file_len
                }
            } else {
                file_len
            }
        }
    };

    if effective_end <= offset {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "carve range is empty",
        ));
    }

    let mut input = File::open(source)?;
    input.seek(SeekFrom::Start(offset))?;

    let mut output = match staging_root {
        Some(root) => {
            std::fs::create_dir_all(root)?;
            tempfile::Builder::new()
                .prefix(".smartzip-carve-")
                .tempfile_in(root)?
        }
        None => tempfile::NamedTempFile::new()?,
    };
    let bytes_to_copy = effective_end - offset;
    std::io::copy(&mut input.take(bytes_to_copy), &mut output)?;
    output.flush()?;

    Ok(output)
}

#[derive(Clone, Copy)]
struct NestedDiscoveryPolicy<'a> {
    cancellation: &'a tokio_util::sync::CancellationToken,
    embedded: &'a smartzip_core::EmbeddedScanPolicy,
    nested_embedded_enabled: bool,
    scan_unrecognized: bool,
}

pub(crate) fn discover_nested_candidates(
    scanner: &EmbeddedScanner,
    root: &Path,
    depth: u8,
    prefix: &Path,
    policy: &smartzip_core::EmbeddedScanPolicy,
    nested_embedded_enabled: bool,
    scan_unrecognized: bool,
    cancellation: &tokio_util::sync::CancellationToken,
) -> Vec<ExtractionCandidate> {
    let policy = NestedDiscoveryPolicy {
        cancellation,
        embedded: policy,
        nested_embedded_enabled,
        scan_unrecognized,
    };
    // Links are archive contents, not additional archives to follow outside output.
    if root.is_symlink() {
        return Vec::new();
    }
    // Preserve the collapsed single-output contract: header/extension only.
    if root.is_file() {
        return classify_nested_file(
            None,
            root,
            prefix.join(archive_stem(root)),
            depth,
            None,
            &policy,
        );
    }
    let mut candidates = Vec::new();
    for entry in walkdir::WalkDir::new(root)
        .follow_links(false)
        .min_depth(1)
        .into_iter()
        .filter_map(|entry| entry.ok())
    {
        if cancellation.is_cancelled() {
            break;
        }
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        let mut relative_path = prefix.join(path.strip_prefix(root).unwrap_or(path));
        relative_path.set_file_name(archive_stem(path));
        candidates.extend(classify_nested_file(
            Some(scanner),
            path,
            relative_path,
            depth,
            None,
            &policy,
        ));
    }
    candidates
}

pub(crate) fn discover_nested_candidates_from_inventory(
    scanner: &EmbeddedScanner,
    root: &Path,
    files: &[crate::budget::InventoryFile],
    depth: u8,
    prefix: &Path,
    policy: &smartzip_core::EmbeddedScanPolicy,
    nested_embedded_enabled: bool,
    scan_unrecognized: bool,
    cancellation: &tokio_util::sync::CancellationToken,
) -> Vec<ExtractionCandidate> {
    let policy = NestedDiscoveryPolicy {
        cancellation,
        embedded: policy,
        nested_embedded_enabled,
        scan_unrecognized,
    };
    let mut candidates = Vec::new();
    for file in files {
        if cancellation.is_cancelled() {
            break;
        }
        let path = if file.relative_path.as_os_str().is_empty() {
            root.to_path_buf()
        } else {
            root.join(&file.relative_path)
        };
        let mut relative_path = if file.relative_path.as_os_str().is_empty() {
            prefix.join(archive_stem(&path))
        } else {
            prefix.join(&file.relative_path)
        };
        relative_path.set_file_name(archive_stem(&path));
        candidates.extend(classify_nested_file(
            (!file.relative_path.as_os_str().is_empty()).then_some(scanner),
            &path,
            relative_path,
            depth,
            Some(file.size),
            &policy,
        ));
    }
    candidates
}

fn classify_nested_file(
    scanner: Option<&EmbeddedScanner>,
    path: &Path,
    relative_path: PathBuf,
    depth: u8,
    known_size: Option<u64>,
    discovery: &NestedDiscoveryPolicy<'_>,
) -> Vec<ExtractionCandidate> {
    let NestedDiscoveryPolicy {
        embedded: policy,
        nested_embedded_enabled,
        scan_unrecognized,
        cancellation,
    } = *discovery;
    let path = path.to_path_buf();
    let detected_format = format_from_extension(&path);
    let mut candidates = Vec::new();
    let unrecognized_size = (scan_unrecognized && detected_format.is_none())
        .then(|| {
            known_size.or_else(|| std::fs::metadata(&path).ok().map(|metadata| metadata.len()))
        })
        .flatten();
    let scan_unrecognized_header = scan_unrecognized
        && unrecognized_size.is_some_and(|size| size >= policy.min_finding_size_bytes);
    let header_result = (nested_embedded_enabled
        && (detected_format.is_some() || scan_unrecognized_header))
        .then(|| crate::detect::probe_file_header(&path))
        .flatten();
    if let Some((fmt, offset)) = header_result {
        if is_business_container(&path) {
            return candidates;
        }
        candidates.push(ExtractionCandidate {
            path: path.clone(),
            relative_path,
            depth,
            source: CandidateSource::ExtractedFile,
            detected_format: Some(fmt),
            embedded_offset: if offset > 0 { Some(offset) } else { None },
            embedded_size: None,
        });
        return candidates;
    }

    if detected_format.is_some() {
        if is_business_container(&path) {
            return candidates;
        }
        candidates.push(ExtractionCandidate {
            path: path.clone(),
            relative_path,
            depth,
            source: CandidateSource::ExtractedFile,
            detected_format,
            embedded_offset: None,
            embedded_size: None,
        });
        return candidates;
    }

    let Some(scanner) = scanner else {
        return candidates;
    };
    if !nested_embedded_enabled || !scan_unrecognized {
        return candidates;
    }
    let file_size = unrecognized_size.unwrap_or(0);
    if file_size < policy.min_finding_size_bytes {
        return candidates;
    }
    if policy
        .inner_scan_max_bytes
        .is_some_and(|max_bytes| file_size > max_bytes)
    {
        return candidates;
    }
    let findings: Vec<_> = scanner
        .scan_path_cancellable(&path, &|| cancellation.is_cancelled())
        .unwrap_or_default()
        .into_iter()
        .filter(|finding| finding_meets_min_size(finding, policy))
        .collect();
    if findings.is_empty() {
        return candidates;
    }
    if matches!(
        policy.mode,
        smartzip_core::EmbeddedScanMode::Auto
            | smartzip_core::EmbeddedScanMode::Ask
            | smartzip_core::EmbeddedScanMode::Aggressive
            | smartzip_core::EmbeddedScanMode::All
    ) {
        for finding in findings {
            candidates.push(ExtractionCandidate {
                path: path.clone(),
                relative_path: relative_path.clone(),
                depth,
                source: CandidateSource::EmbeddedFinding,
                detected_format: Some(finding.format),
                embedded_offset: Some(finding.offset),
                embedded_size: finding.size,
            });
        }
        return candidates;
    }

    let decision = crate::embedded::select_embedded_action(file_size, &findings, policy, false);
    if let Some(idx) = decision.selected_index {
        let finding = &findings[idx];
        if matches!(
            decision.action,
            smartzip_core::DetectionAction::ExtractDirect
                | smartzip_core::DetectionAction::CarveAndExtract
        ) {
            candidates.push(ExtractionCandidate {
                path: path.clone(),
                relative_path: relative_path.clone(),
                depth,
                source: CandidateSource::EmbeddedFinding,
                detected_format: Some(finding.format.clone()),
                embedded_offset: Some(finding.offset),
                embedded_size: finding.size,
            });
        }
    }
    candidates
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
mod target_staging_tests {
    use super::*;

    #[test]
    fn embedded_input_is_carved_on_target_and_removed_after_use() {
        let source = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        let path = source.path().join("carrier.bin");
        std::fs::write(&path, b"prefixPAYLOADsuffix").unwrap();
        let mut candidate = ExtractionCandidate::root(path.clone());
        candidate.embedded_offset = Some(6);
        candidate.embedded_size = Some(7);
        let input = materialize_archive_input(&candidate, Some(target.path())).unwrap();
        assert_eq!(input.path.parent(), Some(target.path()));
        assert_eq!(std::fs::read(&input.path).unwrap(), b"PAYLOAD");
        assert_eq!(std::fs::read_dir(source.path()).unwrap().count(), 1);
        let staging = input.path.clone();
        drop(input);
        assert!(!staging.exists());
        assert_eq!(std::fs::read(path).unwrap(), b"prefixPAYLOADsuffix");
    }
}
