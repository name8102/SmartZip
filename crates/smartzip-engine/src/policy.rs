//! Business-container, scan-policy, and min-size gates.

use smartzip_core::TaskId;
use smartzip_scanner::{EmbeddedArchiveFinding, ScanMode, ScannerConfig};
use std::path::Path;

use crate::events::EventSink;
use crate::types::{CandidateSource, ExtractWorkflowRequest, ExtractionCandidate};

pub(crate) fn is_business_container(path: &Path) -> bool {
    ext_business_container_kind(path).is_some()
}

pub(crate) fn ext_business_container_kind(
    path: &Path,
) -> Option<smartzip_core::BusinessContainerKind> {
    let ext = path.extension().and_then(|e| e.to_str())?;
    match ext.to_ascii_lowercase().as_str() {
        "docx" => Some(smartzip_core::BusinessContainerKind::OfficeDocx),
        "xlsx" => Some(smartzip_core::BusinessContainerKind::OfficeXlsx),
        "pptx" => Some(smartzip_core::BusinessContainerKind::OfficePptx),
        "epub" => Some(smartzip_core::BusinessContainerKind::Epub),
        "apk" => Some(smartzip_core::BusinessContainerKind::Apk),
        "jar" => Some(smartzip_core::BusinessContainerKind::Jar),
        "cbr" => Some(smartzip_core::BusinessContainerKind::Cbr),
        _ => None,
    }
}

pub(crate) fn embedded_policy_from_request(
    request: &ExtractWorkflowRequest,
) -> smartzip_core::EmbeddedScanPolicy {
    smartzip_core::EmbeddedScanPolicy {
        mode: request.embedded_scan_mode,
        dominant_min_ratio: request.dominant_min_ratio,
        ..smartzip_core::EmbeddedScanPolicy::default()
    }
}

pub(crate) fn full_root_scanner_config(requested: &ScannerConfig) -> ScannerConfig {
    ScannerConfig {
        mode: ScanMode::Deep,
        max_scan_bytes: None,
        max_findings: usize::MAX,
        ..requested.clone()
    }
}

pub(crate) fn default_root_scanner_config(requested: &ScannerConfig) -> ScannerConfig {
    full_root_scanner_config(requested)
}

pub(crate) fn finding_meets_min_size(
    finding: &EmbeddedArchiveFinding,
    policy: &smartzip_core::EmbeddedScanPolicy,
) -> bool {
    finding.offset == 0
        || finding
            .size
            .is_none_or(|size| size >= policy.min_finding_size_bytes)
}

pub(crate) fn should_scan_candidate_for_embedded(
    candidate: &ExtractionCandidate,
    policy: &smartzip_core::EmbeddedScanPolicy,
    nested_embedded_enabled: bool,
    _confirm_large_scan: bool,
    _events: &EventSink,
    _task_id: &TaskId,
) -> bool {
    if matches!(policy.mode, smartzip_core::EmbeddedScanMode::Ignore) {
        return false;
    }

    if candidate.source == CandidateSource::EmbeddedFinding && candidate.embedded_offset.is_some() {
        return false;
    }

    if candidate.source != CandidateSource::RootInput && !nested_embedded_enabled {
        return false;
    }

    let file_size = std::fs::metadata(&candidate.path)
        .map(|metadata| metadata.len())
        .unwrap_or(0);
    if candidate.source != CandidateSource::RootInput
        && policy
            .inner_scan_max_bytes
            .is_some_and(|max_bytes| file_size > max_bytes)
    {
        return false;
    }

    true
}
