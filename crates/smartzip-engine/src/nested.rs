//! Nested candidate discovery, carve, recycle, volume helpers.

use smartzip_core::{ArchiveFormat, TaskId};
use smartzip_scanner::{EmbeddedArchiveFinding, EmbeddedScanner};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use crate::interactive::{InteractiveOutputPrompter, OutputCollisionStrategy};
use crate::materialize::{CollisionAction, CollisionResolver};
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
            crate::history::FileExtractionRow {
                input_path: &candidate.path,
                sample_hash: None,
                file_size: None,
                offset: candidate.embedded_offset.map(|o| o as i64),
                output_path: None,
                has_password: false,
                password_id: None,
                status: "skipped",
                reason: Some(reason),
                encoding: None,
                encoding_corrected: false,
                damaged_volumes_json: None,
            },
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
        || findings.iter().any(|finding| finding.offset == 0)
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
            relative_path.set_file_name(format!(
                "{base_name}-embedded-{}-{:X}",
                index + 1,
                finding.offset
            ));
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
) -> smartzip_core::Result<ArchiveInput> {
    if let Some(offset) = candidate.embedded_offset.filter(|offset| *offset > 0) {
        let temp = carve_embedded_archive(
            &candidate.path,
            offset,
            candidate.embedded_size,
            candidate.detected_format.as_ref(),
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

pub(crate) fn make_collision_resolver<'a>(
    prompter: &'a dyn InteractiveOutputPrompter,
) -> CollisionResolver<'a> {
    Box::new(move |archive_path, target_path, _plan| {
        let prompter = prompter;
        Box::pin(async move {
            let strategy = prompter.prompt(archive_path, target_path).await;
            match strategy {
                OutputCollisionStrategy::Skip => CollisionAction::Skip,
                OutputCollisionStrategy::Overwrite => CollisionAction::Overwrite,
                OutputCollisionStrategy::Rename => CollisionAction::Rename,
            }
        })
    })
}

pub(crate) fn carve_embedded_archive(
    source: &Path,
    offset: u64,
    size: Option<u64>,
    format: Option<&ArchiveFormat>,
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

    let mut output = tempfile::NamedTempFile::new()?;
    let bytes_to_copy = effective_end - offset;
    std::io::copy(&mut input.take(bytes_to_copy), &mut output)?;
    output.flush()?;

    Ok(output)
}

pub(crate) fn discover_nested_candidates(
    scanner: &EmbeddedScanner,
    root: &Path,
    depth: u8,
    prefix: &Path,
    policy: &smartzip_core::EmbeddedScanPolicy,
    nested_embedded_enabled: bool,
) -> Vec<ExtractionCandidate> {
    let mut candidates = Vec::new();

    // Handle single-file roots directly when a candidate resolves to one file.
    if root.is_file() {
        let header_result = crate::detect::probe_file_header(root);
        if let Some((fmt, offset)) = header_result {
            if is_business_container(root) || crate::container::classify_zip_path(root).is_some() {
                return candidates;
            }
            candidates.push(ExtractionCandidate {
                path: root.to_path_buf(),
                relative_path: prefix.join(archive_stem(root)),
                depth,
                source: CandidateSource::ExtractedFile,
                detected_format: Some(fmt),
                embedded_offset: if offset > 0 { Some(offset) } else { None },
                embedded_size: None,
            });
            return candidates;
        }

        if let Some(format) = format_from_extension(root) {
            if is_business_container(root) || crate::container::classify_zip_path(root).is_some() {
                return candidates;
            }
            candidates.push(ExtractionCandidate {
                path: root.to_path_buf(),
                relative_path: prefix.join(archive_stem(root)),
                depth,
                source: CandidateSource::ExtractedFile,
                detected_format: Some(format),
                embedded_offset: None,
                embedded_size: None,
            });
            return candidates;
        }
        return candidates;
    }

    // Use `walkdir` for robust recursion: handles symlink loops, FD limits,
    // and does not follow symlinks by default (unlike `Path::is_dir()`).
    for entry in walkdir::WalkDir::new(root)
        .follow_links(false)
        .min_depth(1)
        .into_iter()
        .filter_map(|entry| entry.ok())
    {
        // Only process regular files; directories are traversed by WalkDir itself.
        // Symlinks are not followed and not treated as archives (safer for untrusted output).
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path().to_path_buf();

        let detected_format = format_from_extension(&path);
        let mut relative_path = prefix.to_path_buf();
        relative_path.push(path.strip_prefix(root).unwrap_or(path.as_path()));
        relative_path.set_file_name(archive_stem(&path));

        let header_result = crate::detect::probe_file_header(&path);
        if let Some((fmt, offset)) = header_result {
            if is_business_container(&path) || crate::container::classify_zip_path(&path).is_some()
            {
                continue;
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
            continue;
        }

        if detected_format.is_some() {
            if is_business_container(&path) || crate::container::classify_zip_path(&path).is_some()
            {
                continue;
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
            continue;
        }

        if !nested_embedded_enabled {
            continue;
        }
        let file_size = std::fs::metadata(&path)
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        if policy
            .inner_scan_max_bytes
            .is_some_and(|max_bytes| file_size > max_bytes)
        {
            continue;
        }
        let findings: Vec<_> = scanner
            .scan_path(&path)
            .unwrap_or_default()
            .into_iter()
            .filter(|finding| finding_meets_min_size(finding, policy))
            .collect();
        if findings.is_empty() {
            continue;
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
            continue;
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
