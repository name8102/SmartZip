//! Embedded archive scanner backed by the `binwalk` crate.

use serde::{Deserialize, Serialize};
use smartzip_core::ArchiveFormat;
use std::fs;
use std::io::Read;
use std::path::Path;

mod rar;
mod windows;
mod zip;

/// Signature search window. Root parsers can read complete archives beyond it.
pub const DEFAULT_SCAN_BYTES: u64 = 64 * 1024 * 1024;

pub const DEFAULT_LARGE_SCAN_THRESHOLD: u64 = 10 * 1024 * 1024 * 1024; // 10GB

/// Minimum confidence threshold for scanner findings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Confidence {
    Low,
    Medium,
    High,
}

impl Confidence {
    pub fn from_binwalk(value: u8) -> Self {
        if value >= binwalk::signatures::common::CONFIDENCE_HIGH {
            Self::High
        } else if value >= binwalk::signatures::common::CONFIDENCE_MEDIUM {
            Self::Medium
        } else {
            Self::Low
        }
    }
}

/// Scanner depth/IO strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScanMode {
    /// Scan only up to the configured limit. Suitable for default GUI usage.
    Fast,
    /// Search signatures thoroughly, including short signatures inside a file.
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
            max_scan_bytes: Some(DEFAULT_SCAN_BYTES),
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
    pub fn scan_limit(&self) -> Option<u64> {
        self.config.max_scan_bytes
    }

    pub fn scan_bytes(&self, data: &[u8]) -> Vec<EmbeddedArchiveFinding> {
        let scan_len = self.scan_limit().map_or(data.len(), |limit| {
            usize::try_from(limit).unwrap_or(usize::MAX).min(data.len())
        });
        let data = &data[..scan_len];
        if self.scan_limit().is_none() && self.config.mode == ScanMode::Deep {
            return self.scan_windows(data, DEFAULT_SCAN_BYTES as usize);
        }
        self.binwalk
            .scan(data)
            .into_iter()
            .filter_map(|result| map_signature_result(result, data))
            .filter(|finding| self.config.include_formats.contains(&finding.format))
            .filter(|finding| finding.confidence >= self.config.min_confidence)
            .take(self.config.max_findings)
            .collect()
    }

    /// Make complete root archive data available to parsers; nested scans may cap IO.
    pub fn scan_path(
        &self,
        path: impl AsRef<Path>,
    ) -> std::io::Result<Vec<EmbeddedArchiveFinding>> {
        let mut file = fs::File::open(path)?;
        let mut data = Vec::new();
        if self.scan_limit().is_none() && self.config.mode == ScanMode::Deep {
            let overlap = self
                .binwalk
                .patterns
                .iter()
                .map(Vec::len)
                .max()
                .unwrap_or(1)
                - 1;
            (&mut file)
                .take(DEFAULT_SCAN_BYTES + overlap as u64)
                .read_to_end(&mut data)?;
            let matcher = aho_corasick::AhoCorasick::new(&self.binwalk.patterns)
                .map_err(std::io::Error::other)?;
            if !matcher
                .find_iter(&data)
                .any(|magic| magic.start() < DEFAULT_SCAN_BYTES as usize)
            {
                return Ok(Vec::new());
            }
            file.read_to_end(&mut data)?;
        } else {
            file.take(self.scan_limit().unwrap_or(u64::MAX))
                .read_to_end(&mut data)?;
        }
        Ok(self.scan_bytes(&data))
    }

    /// Get file size without loading it.
    pub fn file_size(path: impl AsRef<Path>) -> std::io::Result<u64> {
        Ok(std::fs::metadata(path.as_ref())?.len())
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
    data: &[u8],
) -> Option<EmbeddedArchiveFinding> {
    let format = binwalk_name_to_format(&result.name)?;
    let mut confidence = Confidence::from_binwalk(result.confidence);
    let mut size = (result.size > 0).then_some(result.size as u64);
    let mut description = result.description;
    if format == ArchiveFormat::Rar {
        let archive = data.get(result.offset..)?;
        if !rar::has_checked_initial_header(archive) {
            return None;
        }
        size = rar::checked_size(archive).map(|size| size as u64);
        confidence = Confidence::Medium;
        description = match size {
            Some(size) => {
                format!("RAR archive; checked block boundaries; total size: {size} bytes")
            }
            None => "RAR archive; initial header checksum verified; archive size unknown".into(),
        };
    } else if format == ArchiveFormat::Zip {
        size = zip::checked_size(data.get(result.offset..)?).map(|size| size as u64);
        if let Some(size) = size {
            description =
                format!("ZIP archive; checked central directory; total size: {size} bytes");
        } else {
            confidence = Confidence::Medium;
            description = "ZIP archive; directory boundary unknown".into();
        }
    }
    Some(EmbeddedArchiveFinding {
        offset: result.offset as u64,
        size,
        format,
        confidence,
        description,
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
    fn unlimited_scan_has_no_implicit_window_or_hard_cap() {
        let scanner = EmbeddedScanner::new(ScannerConfig {
            max_scan_bytes: None,
            ..Default::default()
        });
        assert_eq!(scanner.scan_limit(), None);
        let scanner = EmbeddedScanner::new(ScannerConfig {
            max_scan_bytes: Some(u64::MAX),
            ..Default::default()
        });
        assert_eq!(scanner.scan_limit(), Some(u64::MAX));
        let root = std::env::temp_dir().join(format!("smartzip-sparse-{}", std::process::id()));
        let file = fs::File::create(&root).unwrap();
        file.set_len(16 * 1024 * 1024 * 1024).unwrap();
        let scanner = EmbeddedScanner::new(ScannerConfig {
            max_scan_bytes: Some(4096),
            ..Default::default()
        });
        assert!(scanner.scan_path(&root).unwrap().is_empty());
        let root_scanner = EmbeddedScanner::new(ScannerConfig {
            mode: ScanMode::Deep,
            max_scan_bytes: None,
            ..Default::default()
        });
        assert!(root_scanner.scan_path(&root).unwrap().is_empty());
        fs::remove_file(root).unwrap();
    }

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
    fn confidence_uses_binwalk_constants() {
        use binwalk::signatures::common::*;
        assert_eq!(Confidence::from_binwalk(CONFIDENCE_LOW), Confidence::Low);
        assert_eq!(
            Confidence::from_binwalk(CONFIDENCE_MEDIUM),
            Confidence::Medium
        );
        assert_eq!(Confidence::from_binwalk(CONFIDENCE_HIGH), Confidence::High);
    }

    #[test]
    fn rar_beyond_scan_window_keeps_checked_header_without_inventing_size() {
        use std::io::Write;
        // Main headers with valid checksums; EOF is outside the 1 KiB window.
        let mut rar5 = b"Rar!\x1a\x07\x01\x00".to_vec();
        let main = [3, 1, 0, 0]; // size, type, header flags, archive flags
        rar5.extend_from_slice(&crc32fast::hash(&main).to_le_bytes());
        rar5.extend_from_slice(&main);
        let mut rar4 = b"Rar!\x1a\x07\x00".to_vec();
        let main = [0x73, 0, 0, 13, 0, 0, 0, 0, 0, 0, 0];
        rar4.extend_from_slice(&(crc32fast::hash(&main) as u16).to_le_bytes());
        rar4.extend_from_slice(&main);

        for (header, end, crc_offset) in [
            (rar5, b"\x1d\x77\x56\x51\x03\x05\x04\x00".as_slice(), 8),
            (rar4, b"\xc4\x3d\x7b\x00\x40\x07\x00".as_slice(), 7),
        ] {
            let mut bytes = vec![0x42; 128];
            bytes.extend_from_slice(&header);
            bytes.resize(8192, 0);
            bytes.extend_from_slice(end);
            let mut file = tempfile::NamedTempFile::new().unwrap();
            file.write_all(&bytes).unwrap();
            let scanner = EmbeddedScanner::new(ScannerConfig {
                max_scan_bytes: Some(1024),
                ..ScannerConfig::default()
            });
            let findings = scanner.scan_path(file.path()).unwrap();
            let rar = findings
                .iter()
                .find(|f| f.format == ArchiveFormat::Rar)
                .unwrap();
            assert_eq!(rar.offset, 128);
            assert_eq!(rar.size, None, "the scan window is not archive EOF");
            assert_eq!(rar.confidence, Confidence::Medium);
            assert_eq!(scanner.scan_bytes(&bytes), findings);

            let full = EmbeddedScanner::default().scan_bytes(&bytes);
            assert_eq!(
                full[0].size, None,
                "padding with EOF bytes is not a valid block chain"
            );

            bytes[128 + crc_offset] ^= 1;
            assert!(
                scanner.scan_bytes(&bytes).is_empty(),
                "bad header CRC cannot restore confidence"
            );
        }
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
