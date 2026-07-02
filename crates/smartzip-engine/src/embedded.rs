use smartzip_core::{
    DetectionAction, DetectionDecision, DetectionKind, EmbeddedScanMode, EmbeddedScanPolicy,
    FindingSummary,
};
use smartzip_scanner::{Confidence, EmbeddedArchiveFinding};

fn confidence_to_string(c: Confidence) -> String {
    match c {
        Confidence::Low => "low".into(),
        Confidence::Medium => "medium".into(),
        Confidence::High => "high".into(),
    }
}

pub fn compute_ratio(file_size: u64, finding: &EmbeddedArchiveFinding) -> Option<f64> {
    if file_size == 0 {
        return None;
    }
    let effective = finding
        .size
        .unwrap_or(file_size.saturating_sub(finding.offset));
    if effective == 0 {
        return None;
    }
    Some(effective as f64 / file_size as f64)
}

fn to_summary(finding: &EmbeddedArchiveFinding, ratio: Option<f64>) -> FindingSummary {
    FindingSummary {
        offset: finding.offset,
        size: finding.size,
        format: finding.format.as_str().into(),
        confidence: confidence_to_string(finding.confidence),
        ratio,
    }
}

pub fn select_embedded_action(
    file_size: u64,
    findings: &[EmbeddedArchiveFinding],
    policy: &EmbeddedScanPolicy,
    file_ext_is_archive: bool,
) -> DetectionDecision {
    let summaries: Vec<FindingSummary> = findings
        .iter()
        .map(|f| to_summary(f, compute_ratio(file_size, f)))
        .collect();

    if findings.is_empty() {
        return DetectionDecision {
            kind: DetectionKind::NotArchive,
            action: DetectionAction::ReportOnly,
            selected_index: None,
            findings_summary: summaries,
            archive_ratio: None,
            reason: "no embedded findings".into(),
        };
    }

    let dominated = find_dominant(findings, file_size);

    match policy.mode {
        EmbeddedScanMode::Ignore => {
            return DetectionDecision {
                kind: DetectionKind::EmbeddedPayload,
                action: DetectionAction::SkipByDefault,
                selected_index: None,
                findings_summary: summaries,
                archive_ratio: dominated.map(|(_, r)| r),
                reason: "scan mode is Ignore".into(),
            };
        }
        _ => {}
    }

    if let Some((idx, ratio)) = dominated {
        let finding = &findings[idx];

        if finding.offset == 0 {
            let (kind, reason) = if file_ext_is_archive {
                (
                    DetectionKind::DirectArchive,
                    "archive at offset 0 with archive extension".into(),
                )
            } else {
                (
                    DetectionKind::DirectArchiveDisguised,
                    "archive at offset 0 but file extension is not an archive type".into(),
                )
            };
            return DetectionDecision {
                kind,
                action: DetectionAction::ExtractDirect,
                selected_index: Some(idx),
                findings_summary: summaries,
                archive_ratio: Some(ratio),
                reason,
            };
        }

        if ratio >= policy.dominant_min_ratio as f64 {
            return DetectionDecision {
                kind: DetectionKind::PrependedCarrier,
                action: DetectionAction::CarveAndExtract,
                selected_index: Some(idx),
                findings_summary: summaries,
                archive_ratio: Some(ratio),
                reason: format!(
                    "dominant finding at offset {} covers {:.0}% of file",
                    finding.offset,
                    ratio * 100.0
                ),
            };
        }

        let action = match policy.mode {
            EmbeddedScanMode::Ask => DetectionAction::AskUser,
            EmbeddedScanMode::Auto => DetectionAction::AskUser,
            EmbeddedScanMode::Largest | EmbeddedScanMode::Aggressive | EmbeddedScanMode::All => {
                DetectionAction::CarveAndExtract
            }
            EmbeddedScanMode::Ignore => DetectionAction::SkipByDefault,
        };

        return DetectionDecision {
            kind: DetectionKind::EmbeddedPayload,
            action,
            selected_index: Some(idx),
            findings_summary: summaries,
            archive_ratio: Some(ratio),
            reason: format!(
                "dominant finding at offset {} covers {:.0}% of file (below {:.0}% threshold)",
                finding.offset,
                ratio * 100.0,
                policy.dominant_min_ratio as f64 * 100.0
            ),
        };
    }

    DetectionDecision {
        kind: DetectionKind::NotArchive,
        action: DetectionAction::ReportOnly,
        selected_index: None,
        findings_summary: summaries,
        archive_ratio: None,
        reason: "no findings with calculable ratio".into(),
    }
}

fn find_dominant(findings: &[EmbeddedArchiveFinding], file_size: u64) -> Option<(usize, f64)> {
    findings
        .iter()
        .enumerate()
        .filter_map(|(i, f)| compute_ratio(file_size, f).map(|r| (i, r)))
        .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
}

#[cfg(test)]
mod tests {
    use super::*;
    use smartzip_core::ArchiveFormat;

    fn finding(offset: u64, size: Option<u64>, format: ArchiveFormat) -> EmbeddedArchiveFinding {
        EmbeddedArchiveFinding {
            offset,
            size,
            format,
            confidence: Confidence::High,
            description: String::new(),
        }
    }

    fn default_policy() -> EmbeddedScanPolicy {
        EmbeddedScanPolicy::default()
    }

    #[test]
    fn zero_findings() {
        let d = select_embedded_action(1024, &[], &default_policy(), false);
        assert_eq!(d.kind, DetectionKind::NotArchive);
        assert_eq!(d.action, DetectionAction::ReportOnly);
        assert!(d.selected_index.is_none());
        assert!(d.findings_summary.is_empty());
    }

    #[test]
    fn offset_zero_direct() {
        let f = finding(0, Some(100), ArchiveFormat::Zip);
        let d = select_embedded_action(100, &[f], &default_policy(), true);
        assert_eq!(d.kind, DetectionKind::DirectArchive);
        assert_eq!(d.action, DetectionAction::ExtractDirect);
        assert_eq!(d.selected_index, Some(0));
    }

    #[test]
    fn offset_zero_disguised() {
        let f = finding(0, Some(100), ArchiveFormat::Zip);
        let d = select_embedded_action(100, &[f], &default_policy(), false);
        assert_eq!(d.kind, DetectionKind::DirectArchiveDisguised);
        assert_eq!(d.action, DetectionAction::ExtractDirect);
    }

    #[test]
    fn dominant_prepended() {
        // prefix.jpg 1KB + rar 99KB = 100KB total, rar ratio 99%
        let f = finding(1024, Some(99 * 1024), ArchiveFormat::Rar);
        let d = select_embedded_action(100 * 1024, &[f], &default_policy(), false);
        assert_eq!(d.kind, DetectionKind::PrependedCarrier);
        assert_eq!(d.action, DetectionAction::CarveAndExtract);
        assert_eq!(d.selected_index, Some(0));
        let ratio = d.archive_ratio.unwrap();
        assert!(ratio >= 0.90, "ratio was {ratio}");
    }

    #[test]
    fn low_ratio() {
        // 10MB image + 5MB zip: ratio = 5/15 ≈ 0.33
        let f = finding(10 * 1024 * 1024, Some(5 * 1024 * 1024), ArchiveFormat::Zip);
        let total = 15 * 1024 * 1024;
        let d = select_embedded_action(total, &[f], &default_policy(), false);
        assert_eq!(d.kind, DetectionKind::EmbeddedPayload);
        assert_eq!(d.action, DetectionAction::AskUser);
    }

    #[test]
    fn multi_finding_largest() {
        // 8KB jpg prefix + 80KB RAR + 12KB junk = 100KB. RAR ratio = 80%
        let f1 = finding(0, Some(8 * 1024), ArchiveFormat::Zip);
        let f2 = finding(8 * 1024, Some(80 * 1024), ArchiveFormat::Rar);
        let f3 = finding(88 * 1024, Some(12 * 1024), ArchiveFormat::Gzip);
        let d = select_embedded_action(100 * 1024, &[f1, f2, f3], &default_policy(), false);
        assert_eq!(d.selected_index, Some(1), "should select RAR as dominant");
        assert_eq!(d.kind, DetectionKind::PrependedCarrier);
        assert_eq!(d.action, DetectionAction::CarveAndExtract);
        let ratio = d.archive_ratio.unwrap();
        assert!(ratio >= 0.75, "ratio was {ratio}");
    }

    #[test]
    fn multi_finding_no_dominant() {
        // 40KB + 40KB + 20KB = 100KB. Largest is 40% < 70% threshold
        let f1 = finding(0, Some(40 * 1024), ArchiveFormat::Zip);
        let f2 = finding(40 * 1024, Some(40 * 1024), ArchiveFormat::Rar);
        let d = select_embedded_action(100 * 1024, &[f1, f2], &default_policy(), false);
        assert_eq!(d.kind, DetectionKind::EmbeddedPayload);
        assert_eq!(d.action, DetectionAction::AskUser);
    }

    #[test]
    fn ask_mode() {
        let f = finding(1024, Some(5 * 1024), ArchiveFormat::Zip);
        let mut policy = default_policy();
        policy.mode = EmbeddedScanMode::Ask;
        let d = select_embedded_action(10 * 1024, &[f], &policy, false);
        assert_eq!(d.kind, DetectionKind::EmbeddedPayload);
        assert_eq!(d.action, DetectionAction::AskUser);
    }

    #[test]
    fn ignore_mode() {
        let f = finding(0, Some(100), ArchiveFormat::Zip);
        let mut policy = default_policy();
        policy.mode = EmbeddedScanMode::Ignore;
        let d = select_embedded_action(100, &[f], &policy, true);
        assert_eq!(d.action, DetectionAction::SkipByDefault);
    }

    #[test]
    fn size_none_estimates_to_eof() {
        // size=None at offset=10 in a 100-byte file → effective = 90
        let f = finding(10, None, ArchiveFormat::Rar);
        let d = select_embedded_action(100, &[f], &default_policy(), false);
        let ratio = d.archive_ratio.unwrap();
        assert!((ratio - 0.90).abs() < 0.01, "expected ~0.90, got {ratio}");
        assert_eq!(d.kind, DetectionKind::PrependedCarrier);
    }

    #[test]
    fn compute_ratio_basic() {
        let f = finding(10, Some(90), ArchiveFormat::Zip);
        let r = compute_ratio(100, &f).unwrap();
        assert!((r - 0.90).abs() < f64::EPSILON);
    }

    #[test]
    fn compute_ratio_zero_file_size() {
        let f = finding(0, Some(100), ArchiveFormat::Zip);
        assert!(compute_ratio(0, &f).is_none());
    }

    #[test]
    fn compute_ratio_none_size_uses_eof() {
        let f = finding(20, None, ArchiveFormat::Zip);
        let r = compute_ratio(100, &f).unwrap();
        assert!((r - 0.80).abs() < f64::EPSILON);
    }

    #[test]
    fn compute_ratio_offset_at_eof() {
        let f = finding(100, None, ArchiveFormat::Zip);
        assert!(compute_ratio(100, &f).is_none());
    }

    #[test]
    fn findings_summary_populated() {
        let f = finding(50, Some(50), ArchiveFormat::Rar);
        let d = select_embedded_action(100, &[f], &default_policy(), false);
        assert_eq!(d.findings_summary.len(), 1);
        assert_eq!(d.findings_summary[0].offset, 50);
        assert_eq!(d.findings_summary[0].format, "rar");
        assert!(d.findings_summary[0].ratio.is_some());
    }
}
