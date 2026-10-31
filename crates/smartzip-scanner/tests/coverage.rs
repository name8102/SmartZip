use smartzip_core::ArchiveFormat;
use smartzip_scanner::{Confidence, EmbeddedScanner, ScanMode, ScannerConfig};
use std::io::{Cursor, Write};
use zip::{write::SimpleFileOptions, AesMode, CompressionMethod, ZipWriter};

fn zip(method: CompressionMethod, aes: Option<AesMode>) -> Vec<u8> {
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    let mut options = SimpleFileOptions::default().compression_method(method);
    if let Some(aes) = aes {
        options = options.with_aes_encryption(aes, "fixture-password");
    }
    writer.start_file("目录/文件.txt", options).unwrap();
    writer.write_all(b"archive contents").unwrap();
    writer.finish().unwrap().into_inner()
}

fn assert_scan(payload: &[u8], format: ArchiveFormat, found: bool) {
    // A carrier prefix, a boundary-crossing header, and unrelated trailing data.
    let offset = 1024 * 1024 - 2;
    let mut bytes = vec![0x42; offset];
    bytes.extend_from_slice(payload);
    bytes.extend_from_slice(&[0x42; 14353]);
    let mut file = tempfile::NamedTempFile::new().unwrap();
    file.write_all(&bytes).unwrap();
    for mode in [ScanMode::Fast, ScanMode::Deep] {
        for limit in [Some(bytes.len() as u64), None] {
            let scanner = EmbeddedScanner::new(ScannerConfig {
                mode,
                max_scan_bytes: limit,
                include_formats: vec![format.clone()],
                ..Default::default()
            });
            for findings in [
                scanner.scan_bytes(&bytes),
                scanner.scan_path(file.path()).unwrap(),
            ] {
                assert_eq!(
                    findings.len(),
                    usize::from(found),
                    "{mode:?} {limit:?}: {findings:?}"
                );
                if found {
                    assert_eq!(findings[0].format, format);
                    assert_eq!(findings[0].offset, offset as u64);
                    assert_eq!(findings[0].size, Some(payload.len() as u64));
                    assert_eq!(findings[0].confidence, Confidence::High);
                }
            }
        }
    }
}

#[test]
fn zip_aes_variants_are_detected_in_every_scan_mode() {
    for aes in [AesMode::Aes128, AesMode::Aes192, AesMode::Aes256] {
        for method in [CompressionMethod::Stored, CompressionMethod::Deflated] {
            assert_scan(&zip(method, Some(aes)), ArchiveFormat::Zip, true);
        }
    }
}

#[test]
fn zip_modern_compression_methods_are_detected() {
    for method in [CompressionMethod::Zstd, CompressionMethod::Xz] {
        assert_scan(&zip(method, None), ArchiveFormat::Zip, true);
        assert_scan(
            &zip(method, Some(AesMode::Aes256)),
            ArchiveFormat::Zip,
            true,
        );
    }
}

#[test]
fn aes_zip_without_directory_keeps_an_unknown_boundary() {
    let zip = zip(CompressionMethod::Stored, Some(AesMode::Aes256));
    let central = zip
        .windows(4)
        .position(|bytes| bytes == b"PK\x01\x02")
        .unwrap();
    let mut file = tempfile::NamedTempFile::new().unwrap();
    file.write_all(&zip[..central]).unwrap();
    for mode in [ScanMode::Fast, ScanMode::Deep] {
        for max_scan_bytes in [None, Some(central as u64)] {
            let scanner = EmbeddedScanner::new(ScannerConfig {
                mode,
                max_scan_bytes,
                include_formats: vec![ArchiveFormat::Zip],
                ..Default::default()
            });
            for findings in [
                scanner.scan_bytes(&zip[..central]),
                scanner.scan_path(file.path()).unwrap(),
            ] {
                assert_eq!(findings.len(), 1);
                assert_eq!(findings[0].size, None);
                assert_eq!(findings[0].confidence, Confidence::Medium);
            }
        }
    }
}

#[test]
fn malformed_aes_metadata_is_not_a_zip_finding() {
    let valid = zip(CompressionMethod::Stored, Some(AesMode::Aes256));
    let name_len = u16::from_le_bytes(valid[26..28].try_into().unwrap()) as usize;
    let aes = 30 + name_len;
    assert_eq!(&valid[aes..aes + 2], b"\x01\x99");
    for (position, value) in [
        (6, 0),
        (aes, 0),
        (aes + 4, 3),
        (aes + 6, b'X'),
        (aes + 8, 0),
        (aes + 9, 99),
    ] {
        let mut invalid = valid.clone();
        invalid[position] = value;
        assert_scan(&invalid, ArchiveFormat::Zip, false);
    }
    assert_scan(&valid[..aes + 10], ArchiveFormat::Zip, false);
}

fn lzma(dictionary: u32, uncommon_properties: bool, content: &[u8], known_size: bool) -> Vec<u8> {
    let mut options = xz2::stream::LzmaOptions::new_preset(1).unwrap();
    options.dict_size(dictionary);
    if uncommon_properties {
        options
            .literal_context_bits(2)
            .literal_position_bits(1)
            .position_bits(0);
    }
    let stream = xz2::stream::Stream::new_lzma_encoder(&options).unwrap();
    let mut writer = xz2::write::XzEncoder::new_stream(Vec::new(), stream);
    writer.write_all(content).unwrap();
    let mut bytes = writer.finish().unwrap();
    if known_size {
        bytes[5..13].copy_from_slice(&(content.len() as u64).to_le_bytes());
    }
    bytes
}

#[test]
fn lzma_small_dictionaries_and_properties_are_detected() {
    for dictionary in [4096, 12288] {
        for uncommon in [false, true] {
            assert_scan(
                &lzma(dictionary, uncommon, b"contents", false),
                ArchiveFormat::Lzma,
                true,
            );
        }
    }
}

#[test]
fn lzma_small_and_empty_known_sizes_are_detected() {
    for size in [0, 3, 255] {
        assert_scan(
            &lzma(1024 * 1024, false, &vec![b'a'; size], true),
            ArchiveFormat::Lzma,
            true,
        );
    }
}

#[test]
fn lzma_bad_or_incomplete_streams_are_not_findings() {
    let valid = lzma(4096, false, b"contents", false);
    for position in [0, 13] {
        let mut invalid = valid.clone();
        invalid[position] = 255;
        assert_scan(&invalid, ArchiveFormat::Lzma, false);
    }
    assert_scan(&valid[..14], ArchiveFormat::Lzma, false);
}

#[test]
fn mp4_metadata_that_decodes_as_empty_lzma_is_not_a_default_finding() {
    // Real MP4 metadata can look like a zero-size LZMA stream with no EOS
    // marker: properties 0, dictionary 16 MiB, and thirteen zero bytes.
    let mut bytes = vec![0; 18];
    bytes[4] = 1;
    assert_scan(&bytes, ArchiveFormat::Lzma, false);
    let scanner = EmbeddedScanner::new(ScannerConfig {
        min_confidence: Confidence::Low,
        include_formats: vec![ArchiveFormat::Lzma],
        ..Default::default()
    });
    let findings = scanner.scan_bytes(&bytes);
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].confidence, Confidence::Low);
}
