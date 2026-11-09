mod tests {
    use crate::EmbeddedScanner;
    use crate::{ScanMode, ScannerConfig};
    use smartzip_core::ArchiveFormat;

    fn rar(size: usize) -> Vec<u8> {
        let mut data = b"Rar!\x1a\x07\x01\x00".to_vec();
        let main = [3, 1, 0, 0];
        data.extend_from_slice(&crc32fast::hash(&main).to_le_bytes());
        data.extend_from_slice(&main);
        // Checked file block with an explicit packed-data length.
        let packed = size - data.len() - 9 - 8;
        assert!(packed < 16384);
        let header = [4, 2, 2, (packed as u8 & 0x7f) | 0x80, (packed >> 7) as u8];
        data.extend_from_slice(&crc32fast::hash(&header).to_le_bytes());
        data.extend_from_slice(&header);
        data.resize(size - 8, 0);
        data.extend_from_slice(b"\x1d\x77\x56\x51\x03\x05\x04\x00");
        data
    }

    fn scanner() -> EmbeddedScanner {
        EmbeddedScanner::new(ScannerConfig {
            mode: ScanMode::Deep,
            max_scan_bytes: None,
            ..Default::default()
        })
    }

    #[test]
    fn complete_archive_crosses_windows_then_search_resumes_at_its_end() {
        let mut data = rar(8192);
        data.extend_from_slice(&[0; 100]);
        data.extend_from_slice(&rar(4096));
        // Empty windows between archives must not hide later findings.
        data.extend_from_slice(&[0; 2048]);
        data.extend_from_slice(&rar(200));

        let findings = scanner()
            .scan_reader(
                &mut std::io::Cursor::new(&data),
                data.len() as u64,
                &|| false,
                1024,
            )
            .unwrap();
        assert_eq!(findings.len(), 3);
        assert_eq!(findings[0].offset, 0);
        assert_eq!(findings[0].size, Some(8192));
        assert_eq!(findings[1].offset, 8292);
        assert_eq!(findings[1].size, Some(4096));
        assert_eq!(findings[2].offset, 14436);
        assert_eq!(findings[2].size, Some(200));
    }

    #[test]
    fn file_scan_finds_archive_after_empty_prefix_windows() {
        use std::io::{Seek, SeekFrom, Write};
        let mut file = tempfile::NamedTempFile::new().unwrap();
        let offset = 128 * 1024 * 1024 - 2;
        file.seek(SeekFrom::Start(offset)).unwrap();
        file.write_all(&rar(4096)).unwrap();
        let findings = scanner().scan_path(file.path()).unwrap();
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].offset, offset);
        assert_eq!(findings[0].size, Some(4096));
        let bounded = EmbeddedScanner::new(ScannerConfig {
            max_scan_bytes: Some(64 * 1024 * 1024),
            ..Default::default()
        });
        assert!(bounded.scan_path(file.path()).unwrap().is_empty());
    }

    #[test]
    fn header_straddling_window_edge_is_parsed_in_full() {
        let mut data = vec![0; 1022];
        data.extend_from_slice(&rar(8192));
        let findings = scanner()
            .scan_reader(
                &mut std::io::Cursor::new(&data),
                data.len() as u64,
                &|| false,
                1024,
            )
            .unwrap();
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].format, ArchiveFormat::Rar);
        assert_eq!(findings[0].offset, 1022);
        assert_eq!(findings[0].size, Some(8192));
    }

    #[test]
    fn unknown_archive_end_remains_unknown() {
        let mut data = rar(8192);
        data.truncate(8184);
        let findings = scanner()
            .scan_reader(
                &mut std::io::Cursor::new(&data),
                data.len() as u64,
                &|| false,
                1024,
            )
            .unwrap();
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].offset, 0);
        assert_eq!(findings[0].size, None);
    }

    #[test]
    fn malformed_third_party_parser_input_does_not_abort_later_findings() {
        let mut next = u64::MAX.to_le_bytes().to_vec();
        next.extend_from_slice(&1u64.to_le_bytes());
        next.extend_from_slice(&0u32.to_le_bytes());
        let mut data = b"7z\xbc\xaf\x27\x1c\x00\x04".to_vec();
        data.extend_from_slice(&crc32fast::hash(&next).to_le_bytes());
        data.extend_from_slice(&next);
        data.extend_from_slice(&rar(4096));
        let findings = scanner().scan_bytes(&data);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].offset, 32);
        assert_eq!(findings[0].format, ArchiveFormat::Rar);
    }
}
