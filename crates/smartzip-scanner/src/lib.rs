//! Embedded archive scanner backed by the `binwalk` crate.

use serde::{Deserialize, Serialize};
use smartzip_core::ArchiveFormat;
use std::fs;
use std::io::Read;
use std::path::Path;

/// Minimum confidence threshold for scanner findings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Confidence {
    Low,
    Medium,
    High,
}

impl Confidence {
    pub fn from_binwalk(value: u8) -> Self {
        match value {
            0..=1 => Self::Low,
            2 => Self::Medium,
            _ => Self::High,
        }
    }
}

/// Scanner depth/IO strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScanMode {
    /// Scan only up to the configured limit. Suitable for default GUI usage.
    Fast,
    /// Scan all data unless `max_scan_bytes` caps it.
    Deep,
}

/// Scanner configuration used by CLI and GUI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScannerConfig {
    pub mode: ScanMode,
    pub max_scan_bytes: Option<u64>,
    pub max_findings: usize,
    pub min_confidence: Confidence,
    pub include_formats: Vec<ArchiveFormat>,
}

impl Default for ScannerConfig {
    fn default() -> Self {
        Self {
            mode: ScanMode::Fast,
            max_scan_bytes: Some(64 * 1024 * 1024),
            max_findings: 64,
            min_confidence: Confidence::Medium,
            include_formats: default_include_formats(),
        }
    }
}

/// One embedded archive finding in a scanned file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmbeddedArchiveFinding {
    pub offset: u64,
    pub size: Option<u64>,
    pub format: ArchiveFormat,
    pub confidence: Confidence,
    pub description: String,
}

/// Scanner wrapper that owns a configured `binwalk::Binwalk` instance.
pub struct EmbeddedScanner {
    binwalk: binwalk::Binwalk,
    config: ScannerConfig,
}

impl EmbeddedScanner {
    pub fn new(config: ScannerConfig) -> Self {
        let include = if config.include_formats.is_empty() {
            None
        } else {
            Some(
                config
                    .include_formats
                    .iter()
                    .flat_map(format_to_binwalk_names)
                    .map(str::to_owned)
                    .collect(),
            )
        };

        let full_search = matches!(config.mode, ScanMode::Deep);
        let binwalk = binwalk::Binwalk::configure(None, None, include, None, None, full_search)
            .unwrap_or_else(|_| binwalk::Binwalk::new());

        Self { binwalk, config }
    }

    pub fn config(&self) -> &ScannerConfig {
        &self.config
    }

    /// Scan a byte slice that is already loaded by the caller.
    pub fn scan_bytes(&self, data: &[u8]) -> Vec<EmbeddedArchiveFinding> {
        self.binwalk
            .scan(data)
            .into_iter()
            .filter_map(map_signature_result)
            .filter(|finding| self.config.include_formats.contains(&finding.format))
            .filter(|finding| finding.confidence >= self.config.min_confidence)
            .take(self.config.max_findings)
            .collect()
    }

    /// Read a bounded amount of data from disk and scan it.
    pub fn scan_path(
        &self,
        path: impl AsRef<Path>,
    ) -> std::io::Result<Vec<EmbeddedArchiveFinding>> {
        let mut file = fs::File::open(path)?;
        let mut data = Vec::new();
        if let Some(max_bytes) = self.config.max_scan_bytes {
            let mut limited = file.take(max_bytes);
            limited.read_to_end(&mut data)?;
        } else {
            file.read_to_end(&mut data)?;
        }
        Ok(self.scan_bytes(&data))
    }
}

impl Default for EmbeddedScanner {
    fn default() -> Self {
        Self::new(ScannerConfig::default())
    }
}

pub fn default_include_formats() -> Vec<ArchiveFormat> {
    vec![
        ArchiveFormat::Zip,
        ArchiveFormat::SevenZip,
        ArchiveFormat::Rar,
        ArchiveFormat::Tar,
        ArchiveFormat::Gzip,
        ArchiveFormat::Bzip2,
        ArchiveFormat::Xz,
        ArchiveFormat::Cab,
        ArchiveFormat::Iso,
        ArchiveFormat::Dmg,
        ArchiveFormat::Zstd,
        ArchiveFormat::Lz4,
        ArchiveFormat::Lzma,
    ]
}

fn map_signature_result(
    result: binwalk::signatures::common::SignatureResult,
) -> Option<EmbeddedArchiveFinding> {
    Some(EmbeddedArchiveFinding {
        offset: result.offset as u64,
        size: (result.size > 0).then_some(result.size as u64),
        format: binwalk_name_to_format(&result.name)?,
        confidence: Confidence::from_binwalk(result.confidence),
        description: result.description,
    })
}

fn binwalk_name_to_format(name: &str) -> Option<ArchiveFormat> {
    match name {
        "zip" => Some(ArchiveFormat::Zip),
        "7zip" | "sevenzip" | "7z" => Some(ArchiveFormat::SevenZip),
        "rar" => Some(ArchiveFormat::Rar),
        "tar" | "tarball" => Some(ArchiveFormat::Tar),
        "gzip" => Some(ArchiveFormat::Gzip),
        "bzip2" => Some(ArchiveFormat::Bzip2),
        "xz" => Some(ArchiveFormat::Xz),
        "cab" => Some(ArchiveFormat::Cab),
        "iso9660" | "iso" => Some(ArchiveFormat::Iso),
        "dmg" => Some(ArchiveFormat::Dmg),
        "zstd" => Some(ArchiveFormat::Zstd),
        "lz4" => Some(ArchiveFormat::Lz4),
        "lzma" => Some(ArchiveFormat::Lzma),
        _ => None,
    }
}

fn format_to_binwalk_names(format: &ArchiveFormat) -> Vec<&'static str> {
    match format {
        ArchiveFormat::Zip => vec!["zip"],
        ArchiveFormat::SevenZip => vec!["7zip", "sevenzip", "7z"],
        ArchiveFormat::Rar => vec!["rar"],
        ArchiveFormat::Tar => vec!["tar", "tarball"],
        ArchiveFormat::Gzip => vec!["gzip"],
        ArchiveFormat::Bzip2 => vec!["bzip2"],
        ArchiveFormat::Xz => vec!["xz"],
        ArchiveFormat::Cab => vec!["cab"],
        ArchiveFormat::Iso => vec!["iso9660", "iso"],
        ArchiveFormat::Dmg => vec!["dmg"],
        ArchiveFormat::Zstd => vec!["zstd"],
        ArchiveFormat::Lz4 => vec!["lz4"],
        ArchiveFormat::Lzma => vec!["lzma"],
        ArchiveFormat::Unknown(_) => vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn maps_known_binwalk_names() {
        assert_eq!(binwalk_name_to_format("zip"), Some(ArchiveFormat::Zip));
        assert_eq!(
            binwalk_name_to_format("7zip"),
            Some(ArchiveFormat::SevenZip)
        );
        assert_eq!(binwalk_name_to_format("unknown"), None);
    }

    #[test]
    fn scanner_respects_empty_input() {
        let scanner = EmbeddedScanner::default();
        assert!(scanner.scan_bytes(&[]).is_empty());
    }

    #[test]
    fn scan_path_respects_max_scan_bytes() {
        let root =
            std::env::temp_dir().join(format!("smartzip-scanner-limit-{}", std::process::id()));
        let payload_path = root.join("payload.bin");
        fs::create_dir_all(&root).unwrap();

        let fixture = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("tests")
            .join("fixtures")
            .join("enc_utf8.zip");
        let mut payload = vec![0_u8; 4096];
        payload.extend_from_slice(&fs::read(fixture).unwrap());
        fs::write(&payload_path, payload).unwrap();

        let limited_scanner = EmbeddedScanner::new(ScannerConfig {
            max_scan_bytes: Some(1024),
            min_confidence: Confidence::Low,
            ..ScannerConfig::default()
        });
        let full_scanner = EmbeddedScanner::new(ScannerConfig {
            max_scan_bytes: Some(8192),
            min_confidence: Confidence::Low,
            ..ScannerConfig::default()
        });

        let limited = limited_scanner.scan_path(&payload_path).unwrap();
        let full = full_scanner.scan_path(&payload_path).unwrap();

        assert!(
            limited.is_empty(),
            "signature past max_scan_bytes should be skipped"
        );
        assert!(
            full.iter()
                .any(|finding| finding.format == ArchiveFormat::Zip),
            "scanner should find the zip signature once the read limit includes it"
        );

        let _ = fs::remove_dir_all(root);
    }
}
