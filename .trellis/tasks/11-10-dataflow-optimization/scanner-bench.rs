use smartzip_core::ArchiveFormat;
use smartzip_scanner::{EmbeddedScanner, ScanMode, ScannerConfig};
use std::{io::Write, time::Instant};
fn main() {
    // Valid local ZIP headers without a central directory: each is a candidate,
    // not a reason to rescan the rest of the carrier for EOCD.
    let mut data = vec![0x42; 8 * 1024 * 1024];
    for i in 0..256 {
        let start = i * 32768;
        data[start..start + 30].fill(0);
        data[start..start + 4].copy_from_slice(b"PK\x03\x04");
    }
    let mut file = tempfile::NamedTempFile::new().unwrap();
    file.write_all(&data).unwrap();
    let scanner = EmbeddedScanner::new(ScannerConfig {
        mode: ScanMode::Deep,
        max_scan_bytes: None,
        max_findings: usize::MAX,
        include_formats: vec![ArchiveFormat::Zip],
        ..Default::default()
    });
    for _ in 0..4 {
        let t = Instant::now();
        let f = scanner.scan_path(file.path()).unwrap();
        println!(
            "{{\"seconds\":{},\"findings\":{}}}",
            t.elapsed().as_secs_f64(),
            f.len()
        );
    }
}
