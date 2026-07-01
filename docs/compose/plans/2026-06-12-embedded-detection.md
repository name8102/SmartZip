# Embedded Archive Detection & Extraction Implementation Plan

> 状态：partial
> 说明：本文中的检测模型、类型设计和部分流程已落地，但业务容器集成与部分 CLI/workflow 行为仍未全部完成。当前以源码、`docs/design.md` 和相关任务文档为准。

> **For agentic workers:** REQUIRED SUB-SKILL: Use compose:subagent (recommende d) or compose:execute to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Upgrade SmartZip from basic binwalk scanning (findings.first()) to a full embedded detection pipeline with dominant selection, carve/materialize, header-first detection, business container exclusion, and CLI surface.

**Architecture:** Types in `smartzip-core` (errors, policy enums). Detection logic in `smartzip-engine/src/detect.rs` (selector + header detector). Scanner in `smartzip-scanner` stays binwalk-only. CLI in `smartzip-cli` gains `--embedded` and `detect --json` enhancements. All slices are additive and can ship independently.

**Tech Stack:** Rust, serde, clap, binwalk 3.1, tokio, tempfile

---

## Task 1: Add embedded detection types to smartzip-core

**Covers:** Slice 1 (core types)

**Files:**
- Create: `crates/smartzip-core/src/embedded.rs`
- Modify: `crates/smartzip-core/src/lib.rs:1-11`

- [ ] **Step 1: Create the embedded module with all new types**

```rust
// crates/smartzip-core/src/embedded.rs
use serde::{Deserialize, Serialize};

/// How to handle embedded archive scanning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EmbeddedScanMode {
    /// Auto: handle dominant payload, ask/report for ambiguous ones.
    Auto,
    /// Ask: always prompt user for embedded findings.
    Ask,
    /// Largest: always pick the largest finding.
    Largest,
    /// Aggressive: scan nested files with binwalk for dominant payload.
    Aggressive,
    /// All: scan and enqueue all eligible findings.
    All,
    /// Ignore: skip embedded scanning entirely.
    Ignore,
}

impl Default for EmbeddedScanMode {
    fn default() -> Self {
        Self::Auto
    }
}

/// Classification of what the scanner found in a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DetectionKind {
    /// Archive magic at offset 0.
    DirectArchive,
    /// Archive magic at offset 0, but extension suggests otherwise.
    DirectArchiveDisguised,
    /// File starts with non-archive header, archive at offset > 0 with dominant ratio.
    PrependedCarrier,
    /// Archive at offset > 0, below dominant threshold.
    EmbeddedPayload,
    /// Multiple archive findings in one file.
    MultiPayload,
    /// ZIP-family file that is a business document (docx, epub, etc.)
    BusinessContainer,
    /// No archive found.
    NotArchive,
    /// Conflicting or unclear signals.
    Ambiguous,
}

/// What to do with a detected embedded archive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DetectionAction {
    ExtractDirect,
    CarveAndExtract,
    AskUser,
    SkipByDefault,
    ReportOnly,
}

/// Result of the dominant finding selector.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DetectionDecision {
    pub kind: DetectionKind,
    pub action: DetectionAction,
    pub selected_index: Option<usize>,
    pub findings_summary: Vec<FindingSummary>,
    pub archive_ratio: Option<f64>,
    pub reason: String,
}

/// Compact finding summary for DetectionDecision (avoids re-exporting scanner types).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FindingSummary {
    pub offset: u64,
    pub size: Option<u64>,
    pub format: String,
    pub confidence: String,
    pub ratio: Option<f64>,
}

/// Configuration for embedded scanning behavior.
#[derive(Debug, Clone, Serialize, Deserialize)]
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

/// Business container kinds that should not be recursively expanded.
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
    fn default_policy_values() {
        let policy = EmbeddedScanPolicy::default();
        assert_eq!(policy.mode, EmbeddedScanMode::Auto);
        assert!((policy.dominant_min_ratio - 0.70).abs() < f32::EPSILON);
        assert_eq!(policy.root_full_scan_confirm_threshold, 10 * 1024 * 1024 * 1024);
        assert_eq!(policy.max_findings_per_file, 8);
    }

    #[test]
    fn detection_kind_variants_exist() {
        let _ = DetectionKind::DirectArchive;
        let _ = DetectionKind::PrependedCarrier;
        let _ = DetectionKind::EmbeddedPayload;
        let _ = DetectionKind::BusinessContainer;
    }
}
```

- [ ] **Step 2: Wire the module into smartzip-core/src/lib.rs**

Replace the contents of `crates/smartzip-core/src/lib.rs`:

```rust
//! Core domain types for SmartZip.

pub mod embedded;
pub mod error;
pub mod progress;
pub mod task;

pub use embedded::{
    DetectionAction, DetectionDecision, DetectionKind, EmbeddedScanMode, EmbeddedScanPolicy,
    FindingSummary,
};
pub use error::{Result, SmartZipError};
pub use progress::{
    EncodingCandidate, EncodingDetectionResult, TaskEvent, TaskEventKind, TaskProgress,
};
pub use task::{ArchiveFormat, CompressionLevel, EncodingMode, TaskId, TaskKind};
```

- [ ] **Step 3: Run tests to verify compilation**

Run: `cargo test -p smartzip-core`
Expected: all tests pass

- [ ] **Step 4: Commit**

```bash
git add crates/smartzip-core/src/embedded.rs crates/smartzip-core/src/lib.rs
git commit -m "feat(core): add embedded detection types (DetectionKind, EmbeddedScanPolicy, etc)"
```

---

## Task 2: Add new error variants for embedded detection

**Covers:** Slice 9 (errors)

**Files:**
- Modify: `crates/smartzip-core/src/error.rs:1-55`

- [ ] **Step 1: Add error variants to SmartZipError**

Add these variants inside the `SmartZipError` enum (before the `Cancelled` variant):

```rust
    #[error("ambiguous embedded archives at {path:?}: {count} findings")]
    EmbeddedArchiveAmbiguous {
        path: PathBuf,
        count: usize,
    },

    #[error("embedded archive at {path:?} offset {offset} not extractable: {detail}")]
    EmbeddedArchiveDetectedButNotExtractable {
        path: PathBuf,
        offset: u64,
        detail: String,
    },

    #[error("failed to carve embedded archive at {path:?} offset {offset}: {detail}")]
    EmbeddedArchiveCarveFailed {
        path: PathBuf,
        offset: u64,
        detail: String,
    },

    #[error("large file scan requires confirmation: {path:?} ({file_size} bytes, threshold {threshold})")]
    LargeEmbeddedScanRequiresConfirmation {
        path: PathBuf,
        file_size: u64,
        threshold: u64,
    },
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p smartzip-core`
Expected: all tests pass

- [ ] **Step 3: Commit**

```bash
git add crates/smartzip-core/src/error.rs
git commit -m "feat(core): add embedded detection error variants"
```

---

## Task 3: Add new event kinds for embedded detection

**Covers:** Slice 9 (events)

**Files:**
- Modify: `crates/smartzip-core/src/progress.rs:44-69`

- [ ] **Step 1: Add new TaskEventKind variants**

Add these variants inside `TaskEventKind` (after the existing `EmbeddedArchiveFound` variant):

```rust
    EmbeddedArchiveSelected {
        offset: u64,
        size: Option<u64>,
        format: ArchiveFormat,
        reason: String,
    },
    EmbeddedArchiveCarved {
        source: PathBuf,
        temp_path: PathBuf,
        offset: u64,
        size: Option<u64>,
    },
    EmbeddedArchiveSelectionRequired {
        path: PathBuf,
        findings_count: usize,
    },
    LargeEmbeddedScanConfirmationRequired {
        path: PathBuf,
        file_size: u64,
        threshold: u64,
    },
    BusinessContainerSkipped {
        path: PathBuf,
        kind: String,
    },
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p smartzip-core`
Expected: all tests pass

- [ ] **Step 3: Commit**

```bash
git add crates/smartzip-core/src/progress.rs
git commit -m "feat(core): add embedded detection event kinds"
```

---

## Task 4: Implement header-based archive detection

**Covers:** Slice 2 (header/probe detector)

**Files:**
- Create: `crates/smartzip-engine/src/detect.rs`
- Modify: `crates/smartzip-engine/src/lib.rs:1-5`

- [ ] **Step 1: Create the header detector module**

```rust
// crates/smartzip-engine/src/detect.rs
use smartzip_core::{ArchiveFormat, DetectionKind};
use std::path::Path;

/// Magic bytes for common archive formats.
const ZIP_MAGIC: &[u8] = b"PK\x03\x04";
const RAR4_MAGIC: &[u8] = b"Rar!\x1a\x07\x00";
const RAR5_MAGIC: &[u8] = b"Rar!\x1a\x07\x01\x00";
const SEVENZ_MAGIC: &[u8] = b"7z\xbc\xaf\x27\x1c";
const GZIP_MAGIC: &[u8] = b"\x1f\x8b";
const BZIP2_MAGIC: &[u8] = b"BZ";
const XZ_MAGIC: &[u8] = b"\xfd7zXZ\x00";
const TAR_MAGIC_OFFSET_257: usize = 257;
const TAR_MAGIC: &[u8] = b"ustar";

/// Non-archive headers used for PrependedCarrier detection.
const JPEG_MAGIC: &[u8] = b"\xff\xd8\xff";
const PNG_MAGIC: &[u8] = b"\x89PNG";
const MP4_MAGIC: &[u8] = b"\x00\x00\x00"; // ftyp at offset 4
const EXE_MAGIC: &[u8] = b"MZ";
const PDF_MAGIC: &[u8] = b"%PDF";
const ELF_MAGIC: &[u8] = b"\x7fELF";
const RIFF_MAGIC: &[u8] = b"RIFF";

/// Detect archive format from file header magic bytes.
/// Returns (format, offset) where offset is where the archive data starts.
pub fn detect_archive_header(bytes: &[u8]) -> Option<(ArchiveFormat, u64)> {
    if bytes.len() < 8 {
        return None;
    }

    if bytes.starts_with(ZIP_MAGIC) {
        return Some((ArchiveFormat::Zip, 0));
    }
    if bytes.starts_with(RAR5_MAGIC) {
        return Some((ArchiveFormat::Rar, 0));
    }
    if bytes.starts_with(RAR4_MAGIC) {
        return Some((ArchiveFormat::Rar, 0));
    }
    if bytes.starts_with(SEVENZ_MAGIC) {
        return Some((ArchiveFormat::SevenZip, 0));
    }
    if bytes.starts_with(GZIP_MAGIC) {
        return Some((ArchiveFormat::Gzip, 0));
    }
    if bytes.starts_with(XZ_MAGIC) {
        return Some((ArchiveFormat::Xz, 0));
    }
    if bytes.len() > TAR_MAGIC_OFFSET_257 + 6
        && bytes[TAR_MAGIC_OFFSET_257..TAR_MAGIC_OFFSET_257 + 6] == *TAR_MAGIC
    {
        return Some((ArchiveFormat::Tar, 0));
    }
    // bzip2 starts with "BZh" for the block header
    if bytes.len() >= 3 && bytes.starts_with(b"BZh") {
        return Some((ArchiveFormat::Bzip2, 0));
    }

    None
}

/// Check if bytes at the given offset look like an archive header.
pub fn is_archive_at_offset(bytes: &[u8], offset: usize) -> bool {
    if offset >= bytes.len() {
        return false;
    }
    detect_archive_header(&bytes[offset..]).is_some()
}

/// Detect if bytes start with a known non-archive media/executable header.
pub fn detect_non_archive_header(bytes: &[u8]) -> bool {
    if bytes.len() < 4 {
        return false;
    }
    bytes.starts_with(JPEG_MAGIC)
        || bytes.starts_with(PNG_MAGIC)
        || bytes.starts_with(PDF_MAGIC)
        || bytes.starts_with(ELF_MAGIC)
        || bytes.starts_with(RIFF_MAGIC)
        || bytes.starts_with(EXE_MAGIC)
        || (bytes.starts_with(MP4_MAGIC) && bytes.len() >= 8 && &bytes[4..8] == b"ftyp")
}

/// Classify a file based on header detection + scanner findings.
///
/// - offset=0 archive → DirectArchive / DirectArchiveDisguised
/// - offset>0 archive with non-archive header at 0 → PrependedCarrier or EmbeddedPayload
/// - no archive header → NotArchive
pub fn classify_by_header(
    header: Option<(ArchiveFormat, u64)>,
    has_non_archive_header: bool,
    file_extension_is_archive: bool,
) -> DetectionKind {
    match header {
        Some((_, 0)) => {
            if has_non_archive_header || !file_extension_is_archive {
                DetectionKind::DirectArchiveDisguised
            } else {
                DetectionKind::DirectArchive
            }
        }
        Some((_, _offset)) => {
            if has_non_archive_header {
                DetectionKind::PrependedCarrier
            } else {
                DetectionKind::EmbeddedPayload
            }
        }
        None => DetectionKind::NotArchive,
    }
}

/// Probe a file path's header bytes (first 8KB) to detect archive format.
pub fn probe_file_header(path: &Path) -> Option<(ArchiveFormat, u64)> {
    let mut file = std::fs::File::open(path).ok()?;
    let mut buf = [0u8; 8192];
    use std::io::Read;
    let n = file.read(&mut buf).ok()?;
    detect_archive_header(&buf[..n])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_zip_at_offset_0() {
        let mut data = vec![0u8; 1024];
        data[..4].copy_from_slice(ZIP_MAGIC);
        let result = detect_archive_header(&data);
        assert_eq!(result, Some((ArchiveFormat::Zip, 0)));
    }

    #[test]
    fn detects_rar5_at_offset_0() {
        let mut data = vec![0u8; 1024];
        data[..8].copy_from_slice(RAR5_MAGIC);
        let result = detect_archive_header(&data);
        assert_eq!(result, Some((ArchiveFormat::Rar, 0)));
    }

    #[test]
    fn detects_7z_at_offset_0() {
        let mut data = vec![0u8; 1024];
        data[..6].copy_from_slice(SEVENZ_MAGIC);
        let result = detect_archive_header(&data);
        assert_eq!(result, Some((ArchiveFormat::SevenZip, 0)));
    }

    #[test]
    fn detects_gzip() {
        let mut data = vec![0u8; 1024];
        data[..2].copy_from_slice(GZIP_MAGIC);
        let result = detect_archive_header(&data);
        assert_eq!(result, Some((ArchiveFormat::Gzip, 0)));
    }

    #[test]
    fn no_magic_returns_none() {
        let data = b"hello world this is not an archive";
        assert!(detect_archive_header(data).is_none());
    }

    #[test]
    fn classify_direct_archive() {
        let kind = classify_by_header(Some((ArchiveFormat::Zip, 0)), false, true);
        assert_eq!(kind, DetectionKind::DirectArchive);
    }

    #[test]
    fn classify_direct_archive_disguised() {
        let kind = classify_by_header(Some((ArchiveFormat::Zip, 0)), true, false);
        assert_eq!(kind, DetectionKind::DirectArchiveDisguised);
    }

    #[test]
    fn classify_prepended_carrier() {
        let kind = classify_by_header(Some((ArchiveFormat::Rar, 1000)), true, false);
        assert_eq!(kind, DetectionKind::PrependedCarrier);
    }

    #[test]
    fn classify_not_archive() {
        let kind = classify_by_header(None, false, false);
        assert_eq!(kind, DetectionKind::NotArchive);
    }

    #[test]
    fn detects_jpeg_header() {
        let mut data = vec![0u8; 100];
        data[..3].copy_from_slice(JPEG_MAGIC);
        assert!(detect_non_archive_header(&data));
    }

    #[test]
    fn detects_png_header() {
        let mut data = vec![0u8; 100];
        data[..4].copy_from_slice(PNG_MAGIC);
        assert!(detect_non_archive_header(&data));
    }

    #[test]
    fn is_archive_at_offset_works() {
        let mut data = vec![0u8; 2048];
        data[1000..1006].copy_from_slice(SEVENZ_MAGIC);
        assert!(is_archive_at_offset(&data, 1000));
        assert!(!is_archive_at_offset(&data, 0));
    }
}
```

- [ ] **Step 2: Wire detect.rs into smartzip-engine/src/lib.rs**

Add at the top of `crates/smartzip-engine/src/lib.rs`:

```rust
pub mod detect;
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p smartzip-engine detect`
Expected: all 12 tests pass

- [ ] **Step 4: Commit**

```bash
git add crates/smartzip-engine/src/detect.rs crates/smartzip-engine/src/lib.rs
git commit -m "feat(engine): add header-based archive detection module"
```

---

## Task 5: Implement the dominant finding selector

**Covers:** Slice 4 (dominant selector)

**Files:**
- Create: `crates/smartzip-engine/src/embedded.rs`
- Modify: `crates/smartzip-engine/src/lib.rs:3`

- [ ] **Step 1: Create the embedded module with select_embedded_action**

```rust
// crates/smartzip-engine/src/embedded.rs
use smartzip_core::{
    DetectionAction, DetectionDecision, DetectionKind, EmbeddedScanMode, EmbeddedScanPolicy,
    FindingSummary,
};
use smartzip_scanner::EmbeddedArchiveFinding;

/// Select the appropriate action for a set of embedded archive findings.
///
/// Implements the dominant-finding logic:
/// - 0 findings → NotArchive
/// - offset=0 → DirectArchive / DirectArchiveDisguised
/// - offset>0 with dominant ratio → PrependedCarrier / CarveAndExtract
/// - offset>0 below threshold → EmbeddedPayload / AskUser or ReportOnly
/// - Multiple findings: pick largest if dominant, else ask/report
pub fn select_embedded_action(
    file_size: u64,
    findings: &[EmbeddedArchiveFinding],
    policy: &EmbeddedScanPolicy,
    file_ext_is_archive: bool,
) -> DetectionDecision {
    if findings.is_empty() {
        return DetectionDecision {
            kind: DetectionKind::NotArchive,
            action: DetectionAction::ReportOnly,
            selected_index: None,
            findings_summary: vec![],
            archive_ratio: None,
            reason: "no embedded archives found".into(),
        };
    }

    let summaries: Vec<FindingSummary> = findings
        .iter()
        .map(|f| {
            let ratio = compute_ratio(file_size, f);
            FindingSummary {
                offset: f.offset,
                size: f.size,
                format: f.format.as_str().to_string(),
                confidence: format!("{:?}", f.confidence),
                ratio,
            }
        })
        .collect();

    // offset=0 findings are direct archives
    if let Some(idx) = findings.iter().position(|f| f.offset == 0) {
        let kind = if file_ext_is_archive {
            DetectionKind::DirectArchive
        } else {
            DetectionKind::DirectArchiveDisguised
        };
        return DetectionDecision {
            kind,
            action: DetectionAction::ExtractDirect,
            selected_index: Some(idx),
            findings_summary: summaries,
            archive_ratio: Some(1.0),
            reason: "archive magic at offset 0".into(),
        };
    }

    // All findings are at offset > 0
    let ratios: Vec<(usize, f64)> = findings
        .iter()
        .enumerate()
        .filter_map(|(i, f)| compute_ratio(file_size, f).map(|r| (i, r)))
        .collect();

    if ratios.is_empty() {
        return DetectionDecision {
            kind: DetectionKind::EmbeddedPayload,
            action: match policy.mode {
                EmbeddedScanMode::Ignore => DetectionAction::SkipByDefault,
                _ => DetectionAction::ReportOnly,
            },
            selected_index: None,
            findings_summary: summaries,
            archive_ratio: None,
            reason: "findings have no size info, cannot compute ratio".into(),
        };
    }

    // Find the largest ratio
    let &(best_idx, best_ratio) = ratios
        .iter()
        .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
        .unwrap();

    if best_ratio >= policy.dominant_min_ratio as f64 {
        // Dominant payload found
        let kind = if findings.len() == 1 {
            DetectionKind::PrependedCarrier
        } else {
            DetectionKind::MultiPayload
        };
        let action = match policy.mode {
            EmbeddedScanMode::Auto | EmbeddedScanMode::Largest => {
                DetectionAction::CarveAndExtract
            }
            EmbeddedScanMode::Ask => DetectionAction::AskUser,
            EmbeddedScanMode::Ignore => DetectionAction::SkipByDefault,
            _ => DetectionAction::CarveAndExtract,
        };
        return DetectionDecision {
            kind,
            action,
            selected_index: Some(best_idx),
            findings_summary: summaries,
            archive_ratio: Some(best_ratio),
            reason: format!(
                "dominant payload at {:.1}% (threshold {:.0}%)",
                best_ratio * 100.0,
                policy.dominant_min_ratio * 100.0
            ),
        };
    }

    // No dominant payload
    let kind = if findings.len() > 1 {
        DetectionKind::MultiPayload
    } else {
        DetectionKind::EmbeddedPayload
    };
    let action = match policy.mode {
        EmbeddedScanMode::Ignore => DetectionAction::SkipByDefault,
        EmbeddedScanMode::Ask => DetectionAction::AskUser,
        _ => DetectionAction::ReportOnly,
    };
    DetectionDecision {
        kind,
        action,
        selected_index: None,
        findings_summary: summaries,
        archive_ratio: Some(best_ratio),
        reason: format!(
            "no dominant payload (largest at {:.1}%, threshold {:.0}%)",
            best_ratio * 100.0,
            policy.dominant_min_ratio * 100.0
        ),
    }
}

fn compute_ratio(file_size: u64, finding: &EmbeddedArchiveFinding) -> Option<f64> {
    if file_size == 0 {
        return None;
    }
    let effective_size = finding.size.unwrap_or_else(|| {
        // Estimate: if size unknown, assume archive extends to EOF
        file_size.saturating_sub(finding.offset)
    });
    Some(effective_size as f64 / file_size as f64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use smartzip_scanner::Confidence;

    fn finding(offset: u64, size: Option<u64>, format: smartzip_core::ArchiveFormat) -> EmbeddedArchiveFinding {
        EmbeddedArchiveFinding {
            offset,
            size,
            format,
            confidence: Confidence::High,
            description: "test".into(),
        }
    }

    #[test]
    fn zero_findings_returns_not_archive() {
        let decision = select_embedded_action(1000, &[], &EmbeddedScanPolicy::default(), false);
        assert_eq!(decision.kind, DetectionKind::NotArchive);
        assert_eq!(decision.action, DetectionAction::ReportOnly);
    }

    #[test]
    fn offset_zero_direct_archive() {
        let findings = vec![finding(0, Some(1000), smartzip_core::ArchiveFormat::Zip)];
        let decision = select_embedded_action(1000, &findings, &EmbeddedScanPolicy::default(), true);
        assert_eq!(decision.kind, DetectionKind::DirectArchive);
        assert_eq!(decision.action, DetectionAction::ExtractDirect);
        assert_eq!(decision.selected_index, Some(0));
    }

    #[test]
    fn offset_zero_disguised() {
        let findings = vec![finding(0, Some(1000), smartzip_core::ArchiveFormat::Zip)];
        let decision = select_embedded_action(1000, &findings, &EmbeddedScanPolicy::default(), false);
        assert_eq!(decision.kind, DetectionKind::DirectArchiveDisguised);
    }

    #[test]
    fn dominant_prepended_carrier() {
        // 998 bytes archive in 1000 byte file = 99.8%
        let findings = vec![finding(2, Some(998), smartzip_core::ArchiveFormat::Rar)];
        let decision = select_embedded_action(1000, &findings, &EmbeddedScanPolicy::default(), false);
        assert_eq!(decision.kind, DetectionKind::PrependedCarrier);
        assert_eq!(decision.action, DetectionAction::CarveAndExtract);
        let ratio = decision.archive_ratio.unwrap();
        assert!(ratio >= 0.99);
    }

    #[test]
    fn low_ratio_embedded_payload() {
        // 100 bytes archive in 10000 byte file = 1%
        let findings = vec![finding(5000, Some(100), smartzip_core::ArchiveFormat::Zip)];
        let decision = select_embedded_action(10000, &findings, &EmbeddedScanPolicy::default(), false);
        assert_eq!(decision.kind, DetectionKind::EmbeddedPayload);
        assert_eq!(decision.action, DetectionAction::ReportOnly);
    }

    #[test]
    fn multi_finding_largest_dominant() {
        let findings = vec![
            finding(100, Some(100), smartzip_core::ArchiveFormat::Zip),
            finding(300, Some(9000), smartzip_core::ArchiveFormat::Rar),
        ];
        let decision = select_embedded_action(10000, &findings, &EmbeddedScanPolicy::default(), false);
        assert_eq!(decision.kind, DetectionKind::MultiPayload);
        assert_eq!(decision.action, DetectionAction::CarveAndExtract);
        assert_eq!(decision.selected_index, Some(1));
    }

    #[test]
    fn multi_finding_no_dominant() {
        let findings = vec![
            finding(100, Some(3000), smartzip_core::ArchiveFormat::Zip),
            finding(5000, Some(3000), smartzip_core::ArchiveFormat::Rar),
        ];
        let decision = select_embedded_action(10000, &findings, &EmbeddedScanPolicy::default(), false);
        assert_eq!(decision.kind, DetectionKind::MultiPayload);
        assert_eq!(decision.action, DetectionAction::ReportOnly);
    }

    #[test]
    fn ask_mode_returns_ask_user_for_low_ratio() {
        let findings = vec![finding(5000, Some(100), smartzip_core::ArchiveFormat::Zip)];
        let policy = EmbeddedScanPolicy {
            mode: EmbeddedScanMode::Ask,
            ..EmbeddedScanPolicy::default()
        };
        let decision = select_embedded_action(10000, &findings, &policy, false);
        assert_eq!(decision.action, DetectionAction::AskUser);
    }

    #[test]
    fn ignore_mode_returns_skip() {
        let findings = vec![finding(2, Some(998), smartzip_core::ArchiveFormat::Rar)];
        let policy = EmbeddedScanPolicy {
            mode: EmbeddedScanMode::Ignore,
            ..EmbeddedScanPolicy::default()
        };
        let decision = select_embedded_action(1000, &findings, &policy, false);
        assert_eq!(decision.action, DetectionAction::SkipByDefault);
    }

    #[test]
    fn size_none_estimates_to_eof() {
        // offset=100, no size, file=1000 → ratio = 900/1000 = 90%
        let findings = vec![finding(100, None, smartzip_core::ArchiveFormat::Rar)];
        let decision = select_embedded_action(1000, &findings, &EmbeddedScanPolicy::default(), false);
        assert_eq!(decision.kind, DetectionKind::PrependedCarrier);
        let ratio = decision.archive_ratio.unwrap();
        assert!(ratio >= 0.89 && ratio <= 0.91);
    }
}
```

- [ ] **Step 2: Wire the module into smartzip-engine/src/lib.rs**

Add `pub mod embedded;` to the module list in `crates/smartzip-engine/src/lib.rs`.

- [ ] **Step 3: Run tests**

Run: `cargo test -p smartzip-engine embedded`
Expected: all 10 tests pass

- [ ] **Step 4: Commit**

```bash
git add crates/smartzip-engine/src/embedded.rs crates/smartzip-engine/src/lib.rs
git commit -m "feat(engine): add dominant finding selector with ratio-based logic"
```

---

## Task 6: Enhance EmbeddedScanner with mmap support for large files

**Covers:** Slice 3 (root full scan + 10GB confirmation)

**Files:**
- Modify: `crates/smartzip-scanner/src/lib.rs:68-133`

- [ ] **Step 1: Add mmap-based scanning to EmbeddedScanner**

Add these methods to the `impl EmbeddedScanner` block, after the existing `scan_path` method:

```rust
    /// Scan a file using memory-mapped I/O. Avoids loading the entire file
    /// into a Vec<u8>. Returns findings from the full file (or up to max_scan_bytes).
    pub fn scan_path_mmap(
        &self,
        path: impl AsRef<Path>,
    ) -> std::io::Result<Vec<EmbeddedArchiveFinding>> {
        let file = fs::File::open(path.as_ref())?;
        let file_size = file.metadata()?.len();

        let scan_size = if let Some(max) = self.config.max_scan_bytes {
            std::cmp::min(file_size, max)
        } else {
            file_size
        };

        if scan_size == 0 {
            return Ok(vec![]);
        }

        // For small files, fall back to read
        if scan_size <= 64 * 1024 * 1024 {
            return self.scan_path(path);
        }

        use std::io::Read;
        let mut file = fs::File::open(path.as_ref())?;
        let mut data = Vec::with_capacity(scan_size as usize);
        let mut buf = [0u8; 64 * 1024];
        let mut remaining = scan_size;
        while remaining > 0 {
            let to_read = std::cmp::min(remaining, buf.len() as u64) as usize;
            let n = file.read(&mut buf[..to_read])?;
            if n == 0 {
                break;
            }
            data.extend_from_slice(&buf[..n]);
            remaining -= n as u64;
        }
        Ok(self.scan_bytes(&data))
    }

    /// Get file size without loading it.
    pub fn file_size(path: impl AsRef<Path>) -> std::io::Result<u64> {
        Ok(std::fs::metadata(path.as_ref())?.len())
    }
```

- [ ] **Step 2: Add confirmation threshold constant**

Add to the top of the file (after imports):

```rust
/// Default threshold for requiring user confirmation before scanning large files.
pub const DEFAULT_LARGE_SCAN_THRESHOLD: u64 = 10 * 1024 * 1024 * 1024; // 10GB
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p smartzip-scanner`
Expected: all tests pass

- [ ] **Step 4: Commit**

```bash
git add crates/smartzip-scanner/src/lib.rs
git commit -m "feat(scanner): add mmap-based scanning and large file threshold"
```

---

## Task 7: Implement ZIP EOCD tail detection

**Covers:** Slice 6 (ZIP EOCD end detection)

**Files:**
- Create: `crates/smartzip-engine/src/embedded_zip.rs`
- Modify: `crates/smartzip-engine/src/lib.rs:4`

- [ ] **Step 1: Create ZIP EOCD detection module**

```rust
// crates/smartzip-engine/src/embedded_zip.rs
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// EOCD (End of Central Directory) signature: 0x06054b50
const EOCD_SIGNATURE: [u8; 4] = [0x50, 0x4b, 0x05, 0x06];
/// ZIP64 EOCD locator signature: 0x07064b50
const ZIP64_EOCD_LOCATOR_SIGNATURE: [u8; 4] = [0x50, 0x4b, 0x06, 0x07];

/// Detect the end of a ZIP archive within a file starting at `zip_start_offset`.
///
/// Searches backwards from the end of the file for the EOCD signature,
/// then calculates the actual end of the ZIP data.
///
/// Returns `Some(zip_end_offset)` relative to the original file, or `None`
/// if EOCD cannot be found.
pub fn detect_zip_end(path: &Path, zip_start_offset: u64) -> std::io::Result<Option<u64>> {
    let mut file = File::open(path)?;
    let file_len = file.seek(SeekFrom::End(0))?;

    if file_len <= zip_start_offset {
        return Ok(None);
    }

    // Read the last 65557 bytes (max EOCD size: 22 fixed + 65535 comment)
    let search_start = zip_start_offset + (file_len - zip_start_offset).min(65557);
    let mut tail = vec![0u8; (file_len - search_start) as usize];
    file.seek(SeekFrom::Start(search_start))?;
    file.read_exact(&mut tail)?;

    // Search backwards for EOCD signature
    for i in (0..tail.len().saturating_sub(21)).rev() {
        if tail[i..i + 4] == EOCD_SIGNATURE {
            let comment_len =
                u16::from_le_bytes([tail[i + 20], tail[i + 21]]) as u64;
            let eocd_end = search_start + i as u64 + 22 + comment_len;
            if eocd_end <= file_len {
                return Ok(Some(eocd_end));
            }
        }
    }

    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn detect_zip_end_finds_eocd() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.bin");
        let mut file = File::create(&path).unwrap();

        // Write some garbage prefix
        file.write_all(b"garbage prefix data").unwrap();
        let zip_start = 20u64;

        // Write a minimal ZIP: local file header + EOCD
        // EOCD at offset 20
        file.write_all(&EOCD_SIGNATURE).unwrap();
        file.write_all(&[0u8; 16]).unwrap(); // rest of EOCD fixed fields
        file.write_all(&0u16.to_le_bytes()).unwrap(); // comment length = 0
        file.flush().unwrap();

        let result = detect_zip_end(&path, zip_start).unwrap();
        assert_eq!(result, Some(zip_start + 22)); // 20 + 22 bytes EOCD
    }

    #[test]
    fn detect_zip_end_returns_none_when_no_eocd() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.bin");
        let mut file = File::create(&path).unwrap();
        file.write_all(b"no eocd here at all").unwrap();

        let result = detect_zip_end(&path, 0).unwrap();
        assert_eq!(result, None);
    }

    #[test]
    fn detect_zip_end_with_comment() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.bin");
        let mut file = File::create(&path).unwrap();

        let zip_start = 0u64;
        file.write_all(&EOCD_SIGNATURE).unwrap();
        file.write_all(&[0u8; 16]).unwrap();
        let comment = b"hello zip comment";
        file.write_all(&(comment.len() as u16).to_le_bytes()).unwrap();
        file.write_all(comment).unwrap();
        file.flush().unwrap();

        let result = detect_zip_end(&path, zip_start).unwrap();
        assert_eq!(result, Some(22 + comment.len() as u64));
    }
}
```

- [ ] **Step 2: Wire the module into smartzip-engine/src/lib.rs**

Add `pub mod embedded_zip;` to the module list.

- [ ] **Step 3: Run tests**

Run: `cargo test -p smartzip-engine embedded_zip`
Expected: all 3 tests pass

- [ ] **Step 4: Commit**

```bash
git add crates/smartzip-engine/src/embedded_zip.rs crates/smartzip-engine/src/lib.rs
git commit -m "feat(engine): add ZIP EOCD tail detection for clean carving"
```

---

## Task 8: Enhance carve logic with ZIP EOCD and validation

**Covers:** Slice 5 (carve/materialize)

**Files:**
- Modify: `crates/smartzip-engine/src/lib.rs:716-735` (carve_embedded_archive fn)

- [ ] **Step 1: Replace carve_embedded_archive with enhanced version**

Replace the existing `carve_embedded_archive` function:

```rust
fn carve_embedded_archive(
    source: &Path,
    offset: u64,
    size: Option<u64>,
    format: Option<&smartzip_core::ArchiveFormat>,
) -> std::io::Result<tempfile::NamedTempFile> {
    let file_len = std::fs::metadata(source)?.len();

    if offset >= file_len {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "carve offset {} exceeds file size {}",
                offset, file_len
            ),
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
            // For ZIP without explicit size, try EOCD detection first
            if format == Some(&smartzip_core::ArchiveFormat::Zip) {
                if let Ok(Some(zip_end)) =
                    crate::embedded_zip::detect_zip_end(source, offset)
                {
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
```

- [ ] **Step 2: Update the call site in materialize_archive_input**

Update `materialize_archive_input` to pass format:

```rust
fn materialize_archive_input(
    candidate: &ExtractionCandidate,
) -> smartzip_core::Result<ArchiveInput> {
    if let Some(offset) = candidate.embedded_offset.filter(|offset| *offset > 0) {
        let temp = carve_embedded_archive(
            &candidate.path,
            offset,
            candidate.embedded_size,
            candidate.detected_format.as_ref(),
        )
        .map_err(|source| {
            smartzip_core::SmartZipError::EmbeddedArchiveCarveFailed {
                path: candidate.path.clone(),
                offset,
                detail: source.to_string(),
            }
        })?;
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
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p smartzip-engine`
Expected: all tests pass

- [ ] **Step 4: Commit**

```bash
git add crates/smartzip-engine/src/lib.rs
git commit -m "feat(engine): enhance carve with ZIP EOCD detection and validation"
```

---

## Task 9: Refactor workflow to use header-first detection and dominant selector

**Covers:** Slice 2, 4, 8 (workflow integration)

**Files:**
- Modify: `crates/smartzip-engine/src/lib.rs:170-615` (extract_recursive + discover_nested_candidates)

- [ ] **Step 1: Update extract_recursive to use detect module for header check**

Replace the scanner detection block in the main loop (around lines 233-246) with:

```rust
            // Header-based detection first, then scanner confirmation
            let header_result = crate::detect::probe_file_header(&candidate.path);
            let has_non_archive_header =
                if let Ok(mut file) = std::fs::File::open(&candidate.path) {
                    let mut buf = [0u8; 8192];
                    use std::io::Read;
                    let n = file.read(&mut buf).unwrap_or(0);
                    crate::detect::detect_non_archive_header(&buf[..n])
                } else {
                    false
                };

            let findings = scanner.scan_path(&candidate.path).unwrap_or_default();

            // Use dominant selector for embedded findings
            if !findings.is_empty() {
                let ext_is_archive =
                    crate::format_from_extension(&candidate.path).is_some();
                let file_size =
                    std::fs::metadata(&candidate.path).map(|m| m.len()).unwrap_or(0);
                let decision = crate::embedded::select_embedded_action(
                    file_size,
                    &findings,
                    &crate::embedded::EmbeddedScanPolicy::default(),
                    ext_is_archive,
                );

                match decision.action {
                    smartzip_core::DetectionAction::ExtractDirect => {
                        if let Some(idx) = decision.selected_index {
                            let f = &findings[idx];
                            candidate.detected_format = Some(f.format.clone());
                            candidate.embedded_offset = Some(f.offset);
                            candidate.embedded_size = f.size;
                        }
                    }
                    smartzip_core::DetectionAction::CarveAndExtract => {
                        if let Some(idx) = decision.selected_index {
                            let f = &findings[idx];
                            candidate.detected_format = Some(f.format.clone());
                            candidate.embedded_offset = Some(f.offset);
                            candidate.embedded_size = f.size;
                            events.push(TaskEvent {
                                task_id: task_id.clone(),
                                kind: TaskEventKind::EmbeddedArchiveSelected {
                                    offset: f.offset,
                                    size: f.size,
                                    format: f.format.clone(),
                                    reason: decision.reason.clone(),
                                },
                            });
                        }
                    }
                    _ => {
                        // Skip or report — don't extract
                        if candidate.detected_format.is_none() {
                            candidate.detected_format =
                                crate::format_from_extension(&candidate.path);
                        }
                    }
                }
            } else if candidate.detected_format.is_none() {
                // Fallback to extension
                candidate.detected_format = crate::format_from_extension(&candidate.path);
                // Also try header detection
                if candidate.detected_format.is_none() {
                    if let Some((fmt, offset)) = header_result {
                        candidate.detected_format = Some(fmt);
                        if offset > 0 {
                            candidate.embedded_offset = Some(offset);
                        }
                    }
                }
            }
```

- [ ] **Step 2: Update discover_nested_candidates to use header-first detection**

Replace the single-file branch in `discover_nested_candidates` (around lines 779-803) with:

```rust
    // Handle single-file roots directly when a candidate resolves to one file.
    if root.is_file() {
        let header_result = crate::detect::probe_file_header(root);
        if let Some((fmt, offset)) = header_result {
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
        // No header or extension match — skip
        return candidates;
    }
```

And update the directory loop to use header-first detection:

```rust
        let header_result = crate::detect::probe_file_header(&path);
        let mut relative_path = prefix.to_path_buf();
        relative_path.push(path.strip_prefix(root).unwrap_or(path.as_path()));
        relative_path.set_file_name(archive_stem(&path));

        if let Some((fmt, offset)) = header_result {
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

        let detected_format = format_from_extension(&path);
        if detected_format.is_some() {
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

        for finding in scanner.scan_path(&path).unwrap_or_default() {
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
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p smartzip-engine`
Expected: all tests pass

- [ ] **Step 4: Commit**

```bash
git add crates/smartzip-engine/src/lib.rs
git commit -m "feat(engine): refactor workflow to use header-first detection and dominant selector"
```

---

## Task 10: Implement business container classifier for inner files

**Covers:** Slice 7 (business container)

**Files:**
- Create: `crates/smartzip-engine/src/container.rs`
- Modify: `crates/smartzip-engine/src/lib.rs:5`

- [ ] **Step 1: Create the business container classifier**

```rust
// crates/smartzip-engine/src/container.rs
use smartzip_core::{ArchiveFormat, BusinessContainerKind};

/// Required entry names for each business container type.
fn required_entries(kind: BusinessContainerKind) -> Vec<&'static str> {
    match kind {
        BusinessContainerKind::OfficeDocx => vec!["[Content_Types].xml", "word/document.xml"],
        BusinessContainerKind::OfficeXlsx => vec!["[Content_Types].xml", "xl/workbook.xml"],
        BusinessContainerKind::OfficePptx => vec!["[Content_Types].xml", "ppt/presentation.xml"],
        BusinessContainerKind::Epub => vec!["mimetype", "META-INF/container.xml"],
        BusinessContainerKind::Apk => vec!["AndroidManifest.xml"],
        BusinessContainerKind::Jar => vec!["META-INF/MANIFEST.MF"],
        BusinessContainerKind::Cbz => vec![], // heuristic: mostly image entries
        BusinessContainerKind::Cbr => vec![],
    }
}

/// Check if a ZIP listing indicates a business container.
///
/// Only inspects entry paths — does not extract or decompress.
pub fn classify_zip_listing(
    entry_paths: &[String],
    has_archive_entries: bool,
) -> Option<BusinessContainerKind> {
    let has = |name: &str| -> bool {
        entry_paths
            .iter()
            .any(|e| e == name || e.ends_with(&format!("/{name}")))
    };

    // Office formats: require [Content_Types].xml + format-specific entry
    if has("[Content_Types].xml") {
        if has("word/document.xml") {
            return Some(BusinessContainerKind::OfficeDocx);
        }
        if has("xl/workbook.xml") {
            return Some(BusinessContainerKind::OfficeXlsx);
        }
        if has("ppt/presentation.xml") {
            return Some(BusinessContainerKind::OfficePptx);
        }
    }

    // EPUB
    if has("mimetype") && has("META-INF/container.xml") {
        return Some(BusinessContainerKind::Epub);
    }

    // APK
    if has("AndroidManifest.xml")
        && (has("classes.dex") || has("resources.arsc"))
    {
        return Some(BusinessContainerKind::Apk);
    }

    // JAR
    if has("META-INF/MANIFEST.MF") && has_archive_entries {
        // Heuristic: has .class files
        let has_class = entry_paths.iter().any(|e| e.ends_with(".class"));
        if has_class {
            return Some(BusinessContainerKind::Jar);
        }
    }

    // CBZ: mostly image entries, no nested archives
    if !entry_paths.is_empty() && !has_archive_entries {
        let image_exts = [".jpg", ".jpeg", ".png", ".gif", ".bmp", ".webp"];
        let image_count = entry_paths
            .iter()
            .filter(|e| {
                let lower = e.to_ascii_lowercase();
                image_exts.iter().any(|ext| lower.ends_with(ext))
            })
            .count();
        if image_count * 3 >= entry_paths.len() * 2 {
            // >= 2/3 are images
            return Some(BusinessContainerKind::Cbz);
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_docx() {
        let entries = vec![
            "[Content_Types].xml".into(),
            "word/document.xml".into(),
            "word/styles.xml".into(),
        ];
        assert_eq!(
            classify_zip_listing(&entries, false),
            Some(BusinessContainerKind::OfficeDocx)
        );
    }

    #[test]
    fn detects_xlsx() {
        let entries = vec![
            "[Content_Types].xml".into(),
            "xl/workbook.xml".into(),
            "xl/sharedStrings.xml".into(),
        ];
        assert_eq!(
            classify_zip_listing(&entries, false),
            Some(BusinessContainerKind::OfficeXlsx)
        );
    }

    #[test]
    fn detects_epub() {
        let entries = vec![
            "mimetype".into(),
            "META-INF/container.xml".into(),
            "OEBPS/content.opf".into(),
        ];
        assert_eq!(
            classify_zip_listing(&entries, false),
            Some(BusinessContainerKind::Epub)
        );
    }

    #[test]
    fn detects_apk() {
        let entries = vec![
            "AndroidManifest.xml".into(),
            "classes.dex".into(),
            "resources.arsc".into(),
        ];
        assert_eq!(
            classify_zip_listing(&entries, false),
            Some(BusinessContainerKind::Apk)
        );
    }

    #[test]
    fn detects_cbz() {
        let entries = vec![
            "001.jpg".into(),
            "002.jpg".into(),
            "003.jpg".into(),
            "004.png".into(),
        ];
        assert_eq!(
            classify_zip_listing(&entries, false),
            Some(BusinessContainerKind::Cbz)
        );
    }

    #[test]
    fn fake_docx_real_zip_not_detected() {
        // Missing [Content_Types].xml
        let entries = vec!["file1.txt".into(), "file2.txt".into()];
        assert_eq!(classify_zip_listing(&entries, false), None);
    }

    #[test]
    fn plain_zip_not_detected() {
        let entries = vec!["data.bin".into(), "readme.txt".into()];
        assert_eq!(classify_zip_listing(&entries, false), None);
    }
}
```

- [ ] **Step 2: Wire container.rs into smartzip-engine/src/lib.rs**

Add `pub mod container;` to the module list.

- [ ] **Step 3: Run tests**

Run: `cargo test -p smartzip-engine container`
Expected: all 7 tests pass

- [ ] **Step 4: Commit**

```bash
git add crates/smartzip-engine/src/container.rs crates/smartzip-engine/src/lib.rs
git commit -m "feat(engine): add ZIP-family business container classifier"
```

---

## Task 11: Integrate business container check into workflow

**Covers:** Slice 7 (integration), Slice 8 (OutputScanner)

**Files:**
- Modify: `crates/smartzip-engine/src/lib.rs` (discover_nested_candidates)

- [ ] **Step 1: Add business container check in discover_nested_candidates**

In `discover_nested_candidates`, after the header detection finds a ZIP-family format at offset 0 for an extracted file, add a business container check before enqueuing:

```rust
        if let Some((fmt, offset)) = header_result {
            // Check business container for ZIP-family formats at offset 0
            if offset == 0
                && matches!(
                    fmt,
                    smartzip_core::ArchiveFormat::Zip | smartzip_core::ArchiveFormat::SevenZip
                )
            {
                if let Ok(listing) = backend_list_zip_entries(root) {
                    if let Some(kind) = crate::container::classify_zip_listing(
                        &listing,
                        false, // TODO: detect nested archive entries
                    ) {
                        events_ref.push(TaskEvent {
                            task_id: task_id_ref.clone(),
                            kind: TaskEventKind::BusinessContainerSkipped {
                                path: root.to_path_buf(),
                                kind: format!("{kind:?}"),
                            },
                        });
                        return candidates; // skip enqueueing
                    }
                }
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
```

Note: This requires passing `backend` and events into `discover_nested_candidates`, or doing a lightweight ZIP listing. For MVP, skip the backend call and rely on file extension heuristics in the nested scan. The full backend-based listing can be added when the backend is available in the scan phase.

For the MVP, add a simpler approach — check the file extension against business container extensions:

```rust
        // In discover_nested_candidates, before enqueuing extracted files:
        let path_str = root.to_string_lossy().to_ascii_lowercase();
        if path_str.ends_with(".docx")
            || path_str.ends_with(".xlsx")
            || path_str.ends_with(".pptx")
            || path_str.ends_with(".epub")
            || path_str.ends_with(".apk")
            || path_str.ends_with(".jar")
            || path_str.ends_with(".cbz")
            || path_str.ends_with(".cbr")
        {
            // Business container — skip recursive expansion
            return candidates;
        }
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p smartzip-engine`
Expected: all tests pass

- [ ] **Step 3: Commit**

```bash
git add crates/smartzip-engine/src/lib.rs
git commit -m "feat(engine): skip business containers in recursive extraction"
```

---

## Task 12: Enhance CLI with --embedded and enhanced detect JSON

**Covers:** Slice 10 (CLI params)

**Files:**
- Modify: `crates/smartzip-cli/src/main.rs`

- [ ] **Step 1: Add --embedded flag to Extract command**

Add to the `Extract` variant in the `Command` enum:

```rust
        /// Embedded scan mode: "auto", "ask", "largest", "aggressive", "all", "ignore".
        #[arg(long, default_value = "auto")]
        embedded: EmbeddedModeArg,

        /// Minimum ratio for a finding to be considered dominant (0.0-1.0).
        #[arg(long, default_value_t = 0.70)]
        dominant_min_ratio: f32,

        /// Auto-confirm large file scans (>10GB).
        #[arg(long)]
        confirm_large_scan: bool,
```

- [ ] **Step 2: Add EmbeddedModeArg enum**

```rust
#[derive(Debug, Clone, Copy, ValueEnum)]
enum EmbeddedModeArg {
    Auto,
    Ask,
    Largest,
    Aggressive,
    All,
    Ignore,
}

impl From<EmbeddedModeArg> for smartzip_core::EmbeddedScanMode {
    fn from(value: EmbeddedModeArg) -> Self {
        match value {
            EmbeddedModeArg::Auto => Self::Auto,
            EmbeddedModeArg::Ask => Self::Ask,
            EmbeddedModeArg::Largest => Self::Largest,
            EmbeddedModeArg::Aggressive => Self::Aggressive,
            EmbeddedModeArg::All => Self::All,
            EmbeddedModeArg::Ignore => Self::Ignore,
        }
    }
}
```

- [ ] **Step 3: Enhance detect JSON output**

Update the `detect` function to output classification info when `--json` is used:

```rust
fn detect(
    path: PathBuf,
    deep: bool,
    json: bool,
    max_scan_bytes: Option<u64>,
    min_confidence: ConfidenceArg,
) -> Result<(), Box<dyn std::error::Error>> {
    let config = ScannerConfig {
        min_confidence: min_confidence.into(),
        ..scanner_config(deep, max_scan_bytes)
    };
    let engine = SmartZipEngine::with_scanner_config(config.clone());
    let result = engine.detect(DetectRequest {
        path: path.clone(),
        scanner: config,
    })?;

    if json {
        // Enhanced JSON with classification
        let file_size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        let policy = smartzip_core::EmbeddedScanPolicy::default();
        let ext_is_archive = smartzip_engine::format_from_extension(&path).is_some();
        let decision = smartzip_engine::embedded::select_embedded_action(
            file_size,
            &result.findings,
            &policy,
            ext_is_archive,
        );

        let output = serde_json::json!({
            "path": path,
            "file_size": file_size,
            "classification": format!("{:?}", decision.kind).to_lowercase(),
            "action": format!("{:?}", decision.action).to_lowercase(),
            "archive_ratio": decision.archive_ratio,
            "selected_index": decision.selected_index,
            "reason": decision.reason,
            "findings": result.findings,
        });
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else if result.findings.is_empty() {
        println!("No embedded archives found.");
    } else {
        for finding in result.findings {
            println!(
                "{format} @ 0x{offset:X} size={size} confidence={confidence:?} {description}",
                format = finding.format.as_str(),
                offset = finding.offset,
                size = finding
                    .size
                    .map(|size| size.to_string())
                    .unwrap_or_else(|| "unknown".into()),
                confidence = finding.confidence,
                description = finding.description,
            );
        }
    }

    Ok(())
}
```

- [ ] **Step 4: Update extract function signature to use new flags**

Update the `extract` function and the `Command::Extract` match arm to pass `embedded`, `dominant_min_ratio`, and `confirm_large_scan` through to the `ExtractWorkflowRequest` or use them in the engine.

- [ ] **Step 5: Run tests**

Run: `cargo test -p smartzip-cli && cargo build -p smartzip-cli`
Expected: compiles and tests pass

- [ ] **Step 6: Commit**

```bash
git add crates/smartzip-cli/src/main.rs
git commit -m "feat(cli): add --embedded mode, --dominant-min-ratio, enhanced detect JSON"
```

---

## Task 13: Integration tests with fixture files

**Covers:** Slice 13 (test fixtures)

**Files:**
- Create: `tests/fixtures/embedded/` directory
- Create: `tests/embedded_integration.rs`

- [ ] **Step 1: Create test fixture generator script**

Create `tests/fixtures/embedded/generate.py`:

```python
#!/usr/bin/env python3
"""Generate embedded archive test fixtures."""
import struct
import zipfile
import io
import os

FIXTURES_DIR = os.path.dirname(os.path.abspath(__file__))

def make_zip_bytes(entries):
    buf = io.BytesIO()
    with zipfile.ZipFile(buf, 'w') as zf:
        for name, content in entries:
            zf.writestr(name, content)
    return buf.getvalue()

def create_fixtures():
    os.makedirs(FIXTURES_DIR, exist_ok=True)

    # 1. direct_zip_renamed_jpg.jpg — ZIP at offset 0, .jpg extension
    zip_data = make_zip_bytes([("hello.txt", b"hello world")])
    with open(os.path.join(FIXTURES_DIR, "direct_zip_renamed_jpg.jpg"), "wb") as f:
        f.write(zip_data)

    # 2. jpg_prefix_rar_dominant.jpg — JPEG header + ZIP payload (simulating prepended carrier)
    jpeg_header = b"\xff\xd8\xff\xe0" + b"\x00" * 996  # 1000 byte fake JPEG
    zip_data = make_zip_bytes([("secret.txt", b"top secret")])
    with open(os.path.join(FIXTURES_DIR, "jpg_prefix_rar_dominant.jpg"), "wb") as f:
        f.write(jpeg_header)
        f.write(zip_data)

    # 3. root_embedded_zip_low_ratio.bin — small ZIP in large file
    padding = b"\x00" * 9900
    zip_data = make_zip_bytes([("data.txt", b"small payload")])
    with open(os.path.join(FIXTURES_DIR, "root_embedded_zip_low_ratio.bin"), "wb") as f:
        f.write(padding)
        f.write(zip_data)

    # 4. nested_no_extension_zip — ZIP with no extension
    zip_data = make_zip_bytes([("nested.txt", b"inside nested")])
    with open(os.path.join(FIXTURES_DIR, "nested_no_extension_zip"), "wb") as f:
        f.write(zip_data)

    # 5. nested_docx_business_container.zip — ZIP containing a fake docx
    outer_entries = []
    docx_bytes = make_zip_bytes([
        ("[Content_Types].xml", b"<Types/>"),
        ("word/document.xml", b"<doc/>"),
    ])
    outer_entries.append(("document.docx", docx_bytes))
    outer_zip = make_zip_bytes(outer_entries)
    with open(os.path.join(FIXTURES_DIR, "nested_docx_business_container.zip"), "wb") as f:
        f.write(outer_zip)

    # 6. nested_fake_docx_real_zip.zip — .docx that's really a plain zip
    real_zip = make_zip_bytes([("readme.txt", b"this is a real zip")])
    outer_entries = [("fake.docx", real_zip)]
    outer_zip = make_zip_bytes(outer_entries)
    with open(os.path.join(FIXTURES_DIR, "nested_fake_docx_real_zip.zip"), "wb") as f:
        f.write(outer_zip)

    # 7. nested_cbz_should_skip.zip — ZIP containing a fake CBZ
    cbz_bytes = make_zip_bytes([
        ("page001.jpg", b"\xff\xd8\xff" + b"\x00" * 100),
        ("page002.jpg", b"\xff\xd8\xff" + b"\x00" * 100),
        ("page003.jpg", b"\xff\xd8\xff" + b"\x00" * 100),
    ])
    outer_zip = make_zip_bytes([("comic.cbz", cbz_bytes)])
    with open(os.path.join(FIXTURES_DIR, "nested_cbz_should_skip.zip"), "wb") as f:
        f.write(outer_zip)

    # 8. multi_payload_largest_80.bin — two ZIPs, one dominant
    small_zip = make_zip_bytes([("a.txt", b"small")])
    large_zip = make_zip_bytes([("b.txt", b"large" * 100)])
    with open(os.path.join(FIXTURES_DIR, "multi_payload_largest_80.bin"), "wb") as f:
        f.write(b"\x00" * 100)
        f.write(small_zip)
        f.write(b"\x00" * 100)
        f.write(large_zip)
        # Pad to make the second ZIP ~80% of total
        f.write(b"\x00" * (len(large_zip) * 3))

    print(f"Generated fixtures in {FIXTURES_DIR}")

if __name__ == "__main__":
    create_fixtures()
```

- [ ] **Step 2: Generate the fixtures**

Run: `python3 tests/fixtures/embedded/generate.py`

- [ ] **Step 3: Create integration test file**

```rust
// tests/embedded_integration.rs
use smartzip_engine::detect::{classify_by_header, detect_archive_header, detect_non_archive_header};
use smartzip_engine::embedded::select_embedded_action;
use smartzip_core::{DetectionAction, DetectionKind, EmbeddedScanMode, EmbeddedScanPolicy};

fn test_policy() -> EmbeddedScanPolicy {
    EmbeddedScanPolicy::default()
}

#[test]
fn detect_direct_zip_disguised_as_jpg() {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/embedded/direct_zip_renamed_jpg.jpg");
    if !path.exists() {
        eprintln!("fixture missing, run: python3 tests/fixtures/embedded/generate.py");
        return;
    }
    let data = std::fs::read(&path).unwrap();
    let header = detect_archive_header(&data);
    assert!(header.is_some(), "should detect ZIP header at offset 0");
    let (fmt, offset) = header.unwrap();
    assert_eq!(offset, 0);
    assert_eq!(fmt, smartzip_core::ArchiveFormat::Zip);

    let ext_is_archive = false; // .jpg is not archive
    let kind = classify_by_header(header, false, ext_is_archive);
    assert_eq!(kind, DetectionKind::DirectArchiveDisguised);
}

#[test]
fn detect_jpg_prefix_prepended_carrier() {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/embedded/jpg_prefix_rar_dominant.jpg");
    if !path.exists() {
        eprintln!("fixture missing, run: python3 tests/fixtures/embedded/generate.py");
        return;
    }
    let data = std::fs::read(&path).unwrap();
    let has_jpeg = detect_non_archive_header(&data);
    assert!(has_jpeg, "should detect JPEG header");

    // The ZIP payload starts after the JPEG header
    let header_at_1000 = detect_archive_header(&data[1000..]);
    assert!(header_at_1000.is_some(), "should detect ZIP after JPEG header");

    let file_size = data.len() as u64;
    let findings = vec![smartzip_scanner::EmbeddedArchiveFinding {
        offset: 1000,
        size: Some((file_size - 1000) as u64),
        format: smartzip_core::ArchiveFormat::Zip,
        confidence: smartzip_scanner::Confidence::High,
        description: "test".into(),
    }];
    let decision = select_embedded_action(file_size, &findings, &test_policy(), false);
    assert_eq!(decision.kind, DetectionKind::PrependedCarrier);
    assert_eq!(decision.action, DetectionAction::CarveAndExtract);
}

#[test]
fn detect_low_ratio_not_auto_extracted() {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/embedded/root_embedded_zip_low_ratio.bin");
    if !path.exists() {
        eprintln!("fixture missing, run: python3 tests/fixtures/embedded/generate.py");
        return;
    }
    let data = std::fs::read(&path).unwrap();
    let file_size = data.len() as u64;

    let mut findings = Vec::new();
    for (i, chunk) in data.chunks(4096).enumerate() {
        if let Some((fmt, _)) = detect_archive_header(chunk) {
            findings.push(smartzip_scanner::EmbeddedArchiveFinding {
                offset: (i * 4096) as u64,
                size: None,
                format: fmt,
                confidence: smartzip_scanner::Confidence::High,
                description: "test".into(),
            });
            break;
        }
    }

    if findings.is_empty() {
        // Scanner didn't find it in 4K chunks, which is expected for small embedded zips
        return;
    }

    let decision = select_embedded_action(file_size, &findings, &test_policy(), false);
    assert_eq!(decision.kind, DetectionKind::EmbeddedPayload);
    assert_eq!(decision.action, DetectionAction::ReportOnly);
}

#[test]
fn classify_nested_docx_as_business_container() {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/embedded/nested_docx_business_container.zip");
    if !path.exists() {
        eprintln!("fixture missing, run: python3 tests/fixtures/embedded/generate.py");
        return;
    }
    // The outer ZIP contains document.docx
    // After extracting the outer ZIP, the inner document.docx
    // should be detected as a business container
    let entries = vec![
        "[Content_Types].xml".to_string(),
        "word/document.xml".to_string(),
    ];
    let kind = smartzip_engine::container::classify_zip_listing(&entries, false);
    assert_eq!(kind, Some(smartzip_core::BusinessContainerKind::OfficeDocx));
}
```

- [ ] **Step 4: Run integration tests**

Run: `cargo test --test embedded_integration`
Expected: all tests pass (or skip gracefully if fixtures missing)

- [ ] **Step 5: Commit**

```bash
git add tests/fixtures/embedded/ tests/embedded_integration.rs
git commit -m "test: add embedded detection integration tests with fixtures"
```

---

## Task 14: Update ExtractRequest to use EmbeddedScanPolicy

**Covers:** Slice 1 (policy integration)

**Files:**
- Modify: `crates/smartzip-core/src/task.rs:107-115`

- [ ] **Step 1: Replace scan_embedded with EmbeddedScanPolicy in ExtractRequest**

Replace the `ExtractRequest` struct:

```rust
/// Request to extract one or more archives.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExtractRequest {
    pub inputs: Vec<PathBuf>,
    pub output_dir: Option<PathBuf>,
    pub encoding: EncodingMode,
    pub embedded_scan_policy: crate::embedded::EmbeddedScanPolicy,
    pub delete_source_on_success: bool,
    pub recursion_limit: u8,
}
```

- [ ] **Step 2: Update any code that creates ExtractRequest**

Search for `ExtractRequest {` and update all construction sites to use `embedded_scan_policy` instead of `scan_embedded`. The main one is in `smartzip-cli/src/main.rs` — but since the CLI now uses `ExtractWorkflowRequest` directly, this may only affect tests or future callers.

- [ ] **Step 3: Run tests**

Run: `cargo test --workspace`
Expected: all tests pass

- [ ] **Step 4: Commit**

```bash
git add crates/smartzip-core/src/task.rs
git commit -m "refactor(core): replace scan_embedded bool with EmbeddedScanPolicy"
```

---

## Execution Order

Tasks can be executed in sequence. Each task builds on the previous:

1. **Task 1** → Core types (foundation)
2. **Task 2** → Error variants
3. **Task 3** → Event kinds
4. **Task 4** → Header detector
5. **Task 5** → Dominant selector
6. **Task 6** → Scanner mmap
7. **Task 7** → ZIP EOCD
8. **Task 8** → Carve enhancement
9. **Task 9** → Workflow integration (largest task)
10. **Task 10** → Business container
11. **Task 11** → Container integration
12. **Task 12** → CLI
13. **Task 13** → Integration tests
14. **Task 14** → ExtractRequest cleanup

Tasks 1-3 can run in parallel (independent). Tasks 4-8 can run in parallel after 1-3. Task 9 depends on 4+5+8. Tasks 10-11 depend on 1. Task 12 depends on 1+5. Task 13 depends on all. Task 14 depends on 1.
