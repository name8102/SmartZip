//! Embedded detection integration tests using boundary-case fixtures.
//!
//! Generate fixtures first:
//!   python3 tests/fixtures/embedded/generate.py
//! Run:
//!   cargo test -p smartzip-engine --test embedded_integration

use smartzip_core::{ArchiveFormat, BusinessContainerKind, DetectionKind, EmbeddedScanPolicy};
use smartzip_engine::container::classify_zip_listing;
use smartzip_engine::detect::{
    classify_by_header, detect_archive_header, detect_non_archive_header,
};
use smartzip_engine::embedded::{compute_ratio, select_embedded_action};
use smartzip_scanner::{Confidence, EmbeddedScanner, ScannerConfig};
use std::path::PathBuf;

fn embedded_fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("tests")
        .join("fixtures")
        .join("embedded")
}

fn embedded_fixture(name: &str) -> PathBuf {
    if name == "root_embedded_zip_low_ratio.bin" {
        static GENERATED: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
        return GENERATED
            .get_or_init(|| {
                let dir = tempfile::tempdir().unwrap();
                let mut data = vec![0x42; 1024 * 1024];
                data.extend(
                    std::fs::read(embedded_fixture_dir().join("direct_zip_renamed_jpg.jpg"))
                        .unwrap(),
                );
                std::fs::write(dir.path().join(name), data).unwrap();
                dir
            })
            .path()
            .join(name);
    }
    embedded_fixture_dir().join(name)
}

fn read_fixture_bytes(name: &str) -> Vec<u8> {
    let path = embedded_fixture(name);
    assert!(
        path.exists(),
        "fixture missing: {name}. Run: python3 tests/fixtures/embedded/generate.py"
    );
    std::fs::read(&path).expect("should read fixture")
}

fn scanner() -> EmbeddedScanner {
    EmbeddedScanner::new(ScannerConfig {
        min_confidence: Confidence::Low,
        ..ScannerConfig::default()
    })
}

// ── direct_zip_renamed_jpg ─────────────────────────────────────────────────
// ZIP at offset 0 with .jpg extension → DirectArchiveDisguised

#[test]
fn detect_direct_zip_disguised_as_jpg() {
    let data = read_fixture_bytes("direct_zip_renamed_jpg.jpg");

    // Header detection finds ZIP at offset 0
    let header = detect_archive_header(&data);
    assert_eq!(header, Some((ArchiveFormat::Zip, 0)));

    // No non-archive header (the file starts with PK)
    assert!(!detect_non_archive_header(&data));

    // classify_by_header: archive header present → DirectArchive
    let kind = classify_by_header(header.map(|(f, _)| f), false, false);
    assert_eq!(kind, DetectionKind::DirectArchive);

    // Scanner should find the ZIP
    let scanner = scanner();
    let findings = scanner
        .scan_path(&embedded_fixture("direct_zip_renamed_jpg.jpg"))
        .unwrap();
    assert!(
        !findings.is_empty(),
        "scanner should find embedded ZIP in disguised .jpg"
    );

    // select_embedded_action: offset 0 + non-archive ext → DirectArchiveDisguised
    let file_size = data.len() as u64;
    let policy = EmbeddedScanPolicy::default();
    let decision = select_embedded_action(file_size, &findings, &policy, false);
    assert_eq!(decision.kind, DetectionKind::DirectArchiveDisguised);
    assert_eq!(
        decision.action,
        smartzip_core::DetectionAction::ExtractDirect
    );
    assert!(decision.selected_index.is_some());
}

// ── jpg_prefix_rar_dominant ────────────────────────────────────────────────
// JPEG header prepended to ZIP → PrependedCarrier (carrier scenario)

#[test]
fn detect_jpg_prefix_prepended_carrier() {
    let data = read_fixture_bytes("jpg_prefix_rar_dominant.jpg");

    // Non-archive header detected (JPEG)
    assert!(detect_non_archive_header(&data));

    // No archive header at offset 0
    let header = detect_archive_header(&data);
    assert!(header.is_none());

    // classify_by_header: non-archive header + non-archive ext → NotArchive
    let kind = classify_by_header(header.map(|(f, _)| f), true, false);
    assert_eq!(kind, DetectionKind::NotArchive);

    // Scanner should find ZIP after the JPEG prefix
    let scanner = scanner();
    let findings = scanner
        .scan_path(&embedded_fixture("jpg_prefix_rar_dominant.jpg"))
        .unwrap();
    assert!(
        !findings.is_empty(),
        "scanner should find ZIP payload after JPEG header"
    );

    let zip_finding = findings.iter().find(|f| f.format == ArchiveFormat::Zip);
    assert!(zip_finding.is_some(), "should find a ZIP finding");
    let finding = zip_finding.unwrap();
    assert!(finding.offset > 0, "ZIP should be after the JPEG prefix");

    // select_embedded_action: dominant ratio → PrependedCarrier
    let file_size = data.len() as u64;
    let policy = EmbeddedScanPolicy::default();
    let decision = select_embedded_action(file_size, &findings, &policy, false);
    assert_eq!(decision.kind, DetectionKind::PrependedCarrier);
    assert_eq!(
        decision.action,
        smartzip_core::DetectionAction::CarveAndExtract
    );
}

// ── root_embedded_zip_low_ratio ────────────────────────────────────────────
// Small ZIP in large file → should NOT be auto-extracted

#[test]
fn detect_low_ratio_not_auto_extracted() {
    let data = read_fixture_bytes("root_embedded_zip_low_ratio.bin");

    // No non-archive header (just 0x42 bytes)
    assert!(!detect_non_archive_header(&data));

    // No archive header at offset 0
    let header = detect_archive_header(&data);
    assert!(header.is_none());

    // Scanner should find the embedded ZIP
    let scanner = scanner();
    let findings = scanner
        .scan_path(&embedded_fixture("root_embedded_zip_low_ratio.bin"))
        .unwrap();
    assert!(
        !findings.is_empty(),
        "scanner should find ZIP in large .bin file"
    );

    // Verify low ratio
    let file_size = data.len() as u64;
    for finding in &findings {
        let ratio = compute_ratio(file_size, finding);
        if let Some(r) = ratio {
            assert!(
                r < 0.10,
                "ZIP ratio should be < 10% for low-ratio fixture, got {r:.2}"
            );
        }
    }

    // select_embedded_action: low ratio → EmbeddedPayload (not auto-extract)
    let policy = EmbeddedScanPolicy::default();
    let decision = select_embedded_action(file_size, &findings, &policy, false);
    assert_eq!(decision.kind, DetectionKind::EmbeddedPayload);
    assert!(
        decision.action == smartzip_core::DetectionAction::AskUser
            || decision.action == smartzip_core::DetectionAction::ReportOnly,
        "low ratio should not auto-extract, got action: {:?}",
        decision.action
    );
}

// ── nested_docx_business_container ─────────────────────────────────────────
// ZIP with docx-like entry paths → classified as OfficeDocx

#[test]
fn classify_nested_docx_as_business_container() {
    let data = read_fixture_bytes("nested_docx_business_container.zip");

    // Header detects ZIP
    let header = detect_archive_header(&data);
    assert_eq!(header, Some((ArchiveFormat::Zip, 0)));

    // Entry paths match docx structure
    let entry_paths = vec![
        "[Content_Types].xml".to_string(),
        "word/document.xml".to_string(),
        "word/styles.xml".to_string(),
    ];
    let kind = classify_zip_listing(&entry_paths, false);
    assert_eq!(kind, Some(BusinessContainerKind::OfficeDocx));
}

// ── nested_fake_docx_real_zip ──────────────────────────────────────────────
// .docx file that is actually a plain ZIP → NOT detected as business container

#[test]
fn classify_fake_docx_not_business_container() {
    let data = read_fixture_bytes("nested_fake_docx_real_zip.docx");

    // Header detects ZIP
    let header = detect_archive_header(&data);
    assert_eq!(header, Some((ArchiveFormat::Zip, 0)));

    // Entry paths are plain text, not docx structure
    let entry_paths = vec!["readme.txt".to_string(), "notes.txt".to_string()];
    let kind = classify_zip_listing(&entry_paths, false);
    assert_eq!(
        kind, None,
        "plain zip should not be classified as business container"
    );
}

// ── nested_cbz_should_skip ─────────────────────────────────────────────────
// ZIP with image entries remains an ordinary archive.

#[test]
fn image_only_zip_is_not_a_business_container() {
    let data = read_fixture_bytes("nested_cbz_should_skip.zip");

    // Header detects ZIP
    let header = detect_archive_header(&data);
    assert_eq!(header, Some((ArchiveFormat::Zip, 0)));

    // Image-only contents must not trigger business-container skipping.
    let entry_paths = vec![
        "page001.jpg".to_string(),
        "page002.jpg".to_string(),
        "page003.png".to_string(),
        "cover.webp".to_string(),
    ];
    let kind = classify_zip_listing(&entry_paths, false);
    assert_eq!(kind, None);
}

// ── multi_payload_largest_80 ───────────────────────────────────────────────
// Two ZIPs concatenated, one dominant → largest selected

#[test]
fn multi_payload_selects_largest() {
    let data = read_fixture_bytes("multi_payload_largest_80.bin");

    // Scanner should find both ZIPs
    let scanner = scanner();
    let findings = scanner
        .scan_path(&embedded_fixture("multi_payload_largest_80.bin"))
        .unwrap();

    assert!(
        findings.len() >= 2,
        "should find at least 2 embedded ZIPs, found {}",
        findings.len()
    );

    let file_size = data.len() as u64;
    let policy = EmbeddedScanPolicy::default();
    let decision = select_embedded_action(file_size, &findings, &policy, false);

    // Should select the dominant one
    assert!(
        decision.selected_index.is_some(),
        "should select a dominant finding"
    );
    let ratio = decision.archive_ratio.unwrap();
    assert!(
        ratio >= 0.70,
        "dominant ratio should be >= 70%, got {ratio:.2}"
    );
}

// ── nested_no_extension_zip ────────────────────────────────────────────────
// ZIP with no extension nested in outer ZIP

#[test]
fn detect_nested_no_extension_zip() {
    let data = read_fixture_bytes("nested_no_extension_zip");

    // Outer container is a ZIP
    let header = detect_archive_header(&data);
    assert_eq!(header, Some((ArchiveFormat::Zip, 0)));

    // Extract the inner file via scanner
    let scanner = scanner();
    let findings = scanner
        .scan_path(&embedded_fixture("nested_no_extension_zip"))
        .unwrap();
    assert!(
        !findings.is_empty(),
        "scanner should find inner archive in nested_no_extension_zip"
    );

    // The inner ZIP should be found (no extension means format_from_extension returns None)
    let zip_finding = findings.iter().find(|f| f.format == ArchiveFormat::Zip);
    assert!(zip_finding.is_some(), "should detect inner ZIP by header");
}

// ── Fixture existence ──────────────────────────────────────────────────────

#[test]
fn all_embedded_fixtures_exist() {
    let fixtures = [
        "direct_zip_renamed_jpg.jpg",
        "jpg_prefix_rar_dominant.jpg",
        "root_embedded_zip_low_ratio.bin",
        "nested_no_extension_zip",
        "nested_docx_business_container.zip",
        "nested_fake_docx_real_zip.docx",
        "nested_cbz_should_skip.zip",
        "multi_payload_largest_80.bin",
    ];
    for name in &fixtures {
        let path = embedded_fixture(name);
        assert!(
            path.exists(),
            "fixture '{name}' not found at {}. Run: python3 tests/fixtures/embedded/generate.py",
            path.display()
        );
    }
}
