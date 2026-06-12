use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum EmbeddedScanMode {
    #[default]
    Auto,
    Ask,
    Largest,
    Aggressive,
    All,
    Ignore,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DetectionKind {
    DirectArchive,
    DirectArchiveDisguised,
    PrependedCarrier,
    EmbeddedPayload,
    MultiPayload,
    BusinessContainer,
    NotArchive,
    Ambiguous,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DetectionAction {
    ExtractDirect,
    CarveAndExtract,
    AskUser,
    SkipByDefault,
    ReportOnly,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DetectionDecision {
    pub kind: DetectionKind,
    pub action: DetectionAction,
    pub selected_index: Option<usize>,
    pub findings_summary: Vec<FindingSummary>,
    pub archive_ratio: Option<f64>,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FindingSummary {
    pub offset: u64,
    pub size: Option<u64>,
    pub format: String,
    pub confidence: String,
    pub ratio: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EmbeddedScanPolicy {
    pub mode: EmbeddedScanMode,
    pub dominant_min_ratio: f32,
    pub root_full_scan_confirm_threshold: u64,
    pub max_findings_per_file: usize,
    pub inner_scan_max_bytes: Option<u64>,
}

impl Default for EmbeddedScanPolicy {
    fn default() -> Self {
        Self {
            mode: EmbeddedScanMode::Auto,
            dominant_min_ratio: 0.70,
            root_full_scan_confirm_threshold: 10 * 1024 * 1024 * 1024,
            max_findings_per_file: 8,
            inner_scan_max_bytes: Some(4 * 1024 * 1024 * 1024),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BusinessContainerKind {
    OfficeDocx,
    OfficeXlsx,
    OfficePptx,
    Epub,
    Apk,
    Jar,
    Cbz,
    Cbr,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_embedded_scan_mode_is_auto() {
        assert_eq!(EmbeddedScanMode::default(), EmbeddedScanMode::Auto);
    }

    #[test]
    fn default_embedded_scan_policy_values() {
        let policy = EmbeddedScanPolicy::default();
        assert_eq!(policy.mode, EmbeddedScanMode::Auto);
        assert!((policy.dominant_min_ratio - 0.70).abs() < f32::EPSILON);
        assert_eq!(policy.root_full_scan_confirm_threshold, 10 * 1024 * 1024 * 1024);
        assert_eq!(policy.max_findings_per_file, 8);
        assert_eq!(policy.inner_scan_max_bytes, Some(4 * 1024 * 1024 * 1024));
    }

    #[test]
    fn detection_kind_variants_exist() {
        let _ = DetectionKind::DirectArchive;
        let _ = DetectionKind::DirectArchiveDisguised;
        let _ = DetectionKind::PrependedCarrier;
        let _ = DetectionKind::EmbeddedPayload;
        let _ = DetectionKind::MultiPayload;
        let _ = DetectionKind::BusinessContainer;
        let _ = DetectionKind::NotArchive;
        let _ = DetectionKind::Ambiguous;
    }

    #[test]
    fn detection_action_variants_exist() {
        let _ = DetectionAction::ExtractDirect;
        let _ = DetectionAction::CarveAndExtract;
        let _ = DetectionAction::AskUser;
        let _ = DetectionAction::SkipByDefault;
        let _ = DetectionAction::ReportOnly;
    }

    #[test]
    fn embedded_scan_mode_variants_exist() {
        let _ = EmbeddedScanMode::Auto;
        let _ = EmbeddedScanMode::Ask;
        let _ = EmbeddedScanMode::Largest;
        let _ = EmbeddedScanMode::Aggressive;
        let _ = EmbeddedScanMode::All;
        let _ = EmbeddedScanMode::Ignore;
    }

    #[test]
    fn business_container_kind_variants_exist() {
        let _ = BusinessContainerKind::OfficeDocx;
        let _ = BusinessContainerKind::OfficeXlsx;
        let _ = BusinessContainerKind::OfficePptx;
        let _ = BusinessContainerKind::Epub;
        let _ = BusinessContainerKind::Apk;
        let _ = BusinessContainerKind::Jar;
        let _ = BusinessContainerKind::Cbz;
        let _ = BusinessContainerKind::Cbr;
    }

    #[test]
    fn detection_decision_serialization_roundtrip() {
        let decision = DetectionDecision {
            kind: DetectionKind::EmbeddedPayload,
            action: DetectionAction::CarveAndExtract,
            selected_index: Some(2),
            findings_summary: vec![FindingSummary {
                offset: 1024,
                size: Some(4096),
                format: "ZIP".into(),
                confidence: "high".into(),
                ratio: Some(0.95),
            }],
            archive_ratio: Some(0.85),
            reason: "embedded payload at offset 1024".into(),
        };
        let json = serde_json::to_string(&decision).unwrap();
        let roundtrip: DetectionDecision = serde_json::from_str(&json).unwrap();
        assert_eq!(roundtrip.kind, DetectionKind::EmbeddedPayload);
        assert_eq!(roundtrip.action, DetectionAction::CarveAndExtract);
        assert_eq!(roundtrip.selected_index, Some(2));
        assert_eq!(roundtrip.findings_summary.len(), 1);
    }

    #[test]
    fn embedded_scan_policy_serialization_roundtrip() {
        let policy = EmbeddedScanPolicy::default();
        let json = serde_json::to_string(&policy).unwrap();
        let roundtrip: EmbeddedScanPolicy = serde_json::from_str(&json).unwrap();
        assert_eq!(roundtrip.mode, EmbeddedScanMode::Auto);
        assert!((roundtrip.dominant_min_ratio - 0.70).abs() < f32::EPSILON);
        assert_eq!(roundtrip.root_full_scan_confirm_threshold, 10 * 1024 * 1024 * 1024);
        assert_eq!(roundtrip.max_findings_per_file, 8);
        assert_eq!(roundtrip.inner_scan_max_bytes, Some(4 * 1024 * 1024 * 1024));
    }
}
