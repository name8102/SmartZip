//! Encoding mode resolve, zip assessment, mojibake heuristics.

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

pub(crate) fn applied_encoding_label(
    mode: &EncodingMode,
    assessment: Option<&EncodingConfirmationContext>,
) -> String {
    if *mode == EncodingMode::Auto {
        if let Some(assessment) = assessment.filter(|a| a.native_names) {
            return encoding_mode_label(&assessment.detected.selected);
        }
    }
    encoding_mode_label(mode)
}

pub(crate) async fn resolve_encoding_mode(
    archive_path: &Path,
    requested: EncodingMode,
    assessment: Option<&EncodingConfirmationContext>,
    prompter: Option<&dyn InteractiveEncodingPrompter>,
) -> smartzip_core::Result<Option<EncodingMode>> {
    if requested != EncodingMode::Auto {
        return Ok(Some(requested));
    }

    let Some(assessment) = assessment else {
        return Ok(Some(EncodingMode::Auto));
    };

    if !assessment.suspicious_reasons.is_empty() {
        if let Some(prompter) = prompter {
            match prompter.prompt(archive_path, assessment).await {
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

    Ok(Some(if assessment.native_names {
        EncodingMode::Auto
    } else {
        assessment.detected.selected.clone()
    }))
}

pub(crate) fn assess_zip_encoding(
    input: &smartzip_core::ArchiveInput,
) -> Option<EncodingConfirmationContext> {
    assess_zip_names(&smartzip_archive::zip_names::read(input).ok()?)
}

pub fn assess_zip_names(
    entries: &[smartzip_archive::zip_names::ZipName],
) -> Option<EncodingConfirmationContext> {
    if entries.is_empty() {
        return None;
    }
    // Do not feed names whose encoding is already known into the legacy
    // detector: a large Unicode subset can otherwise dominate its decision.
    let unresolved: Vec<_> = entries
        .iter()
        .filter(|name| name.native_name().is_none())
        .collect();
    if unresolved.is_empty() {
        let selected = EncodingMode::Override("UTF-8".into());
        return Some(EncodingConfirmationContext {
            detected: smartzip_core::EncodingDetectionResult {
                selected,
                confidence: 1.0,
                candidates: vec![smartzip_core::EncodingCandidate {
                    name: "UTF-8".into(),
                    confidence: 1.0,
                }],
            },
            preview_names: Vec::new(),
            suspicious_reasons: Vec::new(),
            native_names: true,
        });
    }

    let capacity = unresolved
        .iter()
        .map(|entry| entry.raw_name.len() + 1)
        .sum();
    let mut raw_names = Vec::with_capacity(capacity);
    for name in &unresolved {
        raw_names.extend_from_slice(&name.raw_name);
        raw_names.push(b'/');
    }
    let mut detector = smartzip_encoding::ArchiveEncodingDetector::new();
    let detected_raw = detector.detect(&raw_names);
    let detected = to_core_encoding_detection(&detected_raw);
    let preview_names = unresolved
        .iter()
        .take(6)
        .map(|entry| decode_preview_name(&entry.raw_name, &detected_raw.selected))
        .collect::<Vec<_>>();
    let suspicious_reasons = suspicious_encoding_reasons(&detected_raw, &preview_names, &raw_names);
    Some(EncodingConfirmationContext {
        detected,
        preview_names,
        suspicious_reasons,
        native_names: false,
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
    raw_names: &[u8],
) -> Vec<String> {
    let mut reasons = Vec::new();
    if detected.selected.eq_ignore_ascii_case("utf-8") && std::str::from_utf8(raw_names).is_ok() {
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
