//! Encoding mode resolve, zip assessment, mojibake heuristics.

use smartzip_archive::native_zip::NativeZipBackend;
#[cfg(test)]
use smartzip_archive::ArchiveListing;
use smartzip_core::EncodingMode;
use std::path::Path;

use crate::interactive::{
    EncodingConfirmationChoice, EncodingConfirmationContext, InteractiveEncodingPrompter,
};

pub(crate) fn encoding_mode_label(mode: &EncodingMode) -> String {
    match mode {
        EncodingMode::Auto => "auto".to_string(),
        EncodingMode::Override(name) => name.clone(),
    }
}

/// Append a `skipped` row to `file_extractions` for a candidate that never
/// reached extraction. `reason` is one of the skip reason strings from the v3
/// schema (`duplicate` / `recursion_limit` / `not_first_volume` /

pub(crate) async fn resolve_encoding_mode(
    archive_path: &Path,
    requested: EncodingMode,
    assessment: Option<&ZipEncodingAssessment>,
    prompter: Option<&dyn InteractiveEncodingPrompter>,
) -> smartzip_core::Result<Option<EncodingMode>> {
    if requested != EncodingMode::Auto {
        return Ok(Some(requested));
    }

    let Some(assessment) = assessment else {
        return Ok(Some(EncodingMode::Auto));
    };

    if assessment.should_confirm {
        if let Some(prompter) = prompter {
            match prompter.prompt(archive_path, &assessment.context).await {
                EncodingConfirmationChoice::AcceptDetected => {}
                EncodingConfirmationChoice::Override(encoding) => {
                    return Ok(Some(EncodingMode::Override(encoding)));
                }
                EncodingConfirmationChoice::SkipArchive => {
                    return Ok(None);
                }
            }
        }
    }

    Ok(Some(EncodingMode::Override(
        assessment.detected_raw.selected.clone(),
    )))
}

#[derive(Debug, Clone)]
pub(crate) struct ZipEncodingAssessment {
    pub(crate) detected_raw: smartzip_encoding::EncodingDetectionResult,
    pub(crate) context: EncodingConfirmationContext,
    pub(crate) should_confirm: bool,
}

pub(crate) async fn assess_zip_encoding(
    archive_path: &Path,
    _password: Option<String>,
) -> Option<ZipEncodingAssessment> {
    // ZIP raw filename reading is now via the narrowed NativeZip helper, not
    // via the generic ArchiveAdapter/list path. This keeps raw bytes intact
    // for detector and avoids routing through the backend router.
    let reader = NativeZipBackend::new();
    let entries = reader.raw_entries(archive_path).ok()?;
    assess_raw_names(entries.iter().map(|entry| entry.raw_name.as_slice()))
}

#[cfg(test)]
pub(crate) fn build_zip_encoding_assessment(
    listing: ArchiveListing,
) -> Option<ZipEncodingAssessment> {
    assess_raw_names(
        listing
            .entries
            .iter()
            .map(|entry| entry.raw_name.as_slice()),
    )
}

fn assess_raw_names<'a>(
    names: impl IntoIterator<Item = &'a [u8]>,
) -> Option<ZipEncodingAssessment> {
    let raw_entries: Vec<&[u8]> = names.into_iter().filter(|raw| !raw.is_empty()).collect();
    if raw_entries.is_empty() {
        return None;
    }

    let ascii_only = raw_entries.iter().all(|raw| raw.is_ascii());
    let capacity = raw_entries.iter().map(|raw| raw.len()).sum::<usize>() + raw_entries.len() - 1;
    let mut raw_names = Vec::with_capacity(capacity);
    for (idx, raw) in raw_entries.iter().enumerate() {
        if idx > 0 {
            raw_names.push(b'/');
        }
        raw_names.extend_from_slice(raw);
    }

    let mut detector = smartzip_encoding::ArchiveEncodingDetector::new();
    let detected_raw = detector.detect(&raw_names);
    let detected = to_core_encoding_detection(&detected_raw);
    let preview_names = raw_entries
        .iter()
        .take(6)
        .map(|raw| decode_preview_name(raw, &detected_raw.selected))
        .collect::<Vec<_>>();
    let suspicious_reasons =
        suspicious_encoding_reasons(&detected_raw, &preview_names, ascii_only, &raw_entries);
    Some(ZipEncodingAssessment {
        detected_raw,
        context: EncodingConfirmationContext {
            detected,
            preview_names,
            suspicious_reasons: suspicious_reasons.clone(),
        },
        should_confirm: !suspicious_reasons.is_empty(),
    })
}

pub(crate) fn to_core_encoding_detection(
    result: &smartzip_encoding::EncodingDetectionResult,
) -> smartzip_core::EncodingDetectionResult {
    smartzip_core::EncodingDetectionResult {
        selected: EncodingMode::Override(result.selected.clone()),
        confidence: result.confidence,
        candidates: result
            .candidates
            .iter()
            .map(|candidate| smartzip_core::EncodingCandidate {
                name: candidate.name.clone(),
                confidence: candidate.confidence,
            })
            .collect(),
    }
}

pub(crate) fn decode_preview_name(raw_name: &[u8], encoding: &str) -> String {
    smartzip_encoding::decode_name(raw_name, encoding)
        .unwrap_or_else(|| String::from_utf8_lossy(raw_name).into_owned())
}

pub(crate) fn suspicious_encoding_reasons(
    detected: &smartzip_encoding::EncodingDetectionResult,
    preview_names: &[String],
    ascii_only: bool,
    raw_entries: &[&[u8]],
) -> Vec<String> {
    let mut reasons = Vec::new();
    if ascii_only {
        return reasons;
    }

    if raw_entries
        .iter()
        .all(|raw| std::str::from_utf8(raw).is_ok())
        && detected.selected.eq_ignore_ascii_case("utf-8")
    {
        return reasons;
    }

    let second_confidence = detected
        .candidates
        .get(1)
        .map(|candidate| candidate.confidence)
        .unwrap_or(0.0);
    if detected.confidence < 0.90 {
        reasons.push(format!(
            "low confidence {:.0}%",
            detected.confidence * 100.0
        ));
    }
    if (detected.confidence - second_confidence).abs() < 0.15 {
        reasons.push("top encoding candidates are close".into());
    }
    if preview_names.iter().any(|name| looks_like_mojibake(name)) {
        reasons.push("previewed names look garbled".into());
    }

    reasons
}

pub(crate) fn looks_like_mojibake(value: &str) -> bool {
    if value.contains('\u{FFFD}') {
        return true;
    }
    let suspicious_markers = ['Ã', 'Â', 'Ð', 'Ñ', 'æ', 'ç', 'ø', '¢', '¤', '¥'];
    let suspicious_count = value
        .chars()
        .filter(|ch| suspicious_markers.contains(ch))
        .count();
    suspicious_count >= 2
        || value
            .chars()
            .any(|ch| ch.is_control() && ch != '\n' && ch != '\t')
}
