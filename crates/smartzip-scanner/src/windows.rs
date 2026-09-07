//! Search windows bound the gap between archives, not the archives themselves.

use aho_corasick::AhoCorasick;

use crate::{map_signature_result, Confidence, EmbeddedArchiveFinding, EmbeddedScanner};

impl EmbeddedScanner {
    pub(super) fn scan_windows(
        &self,
        data: &[u8],
        window_bytes: usize,
    ) -> Vec<EmbeddedArchiveFinding> {
        let matcher = AhoCorasick::new(&self.binwalk.patterns)
            .expect("built-in archive signatures form a valid matcher");
        let overlap = self
            .binwalk
            .patterns
            .iter()
            .map(Vec::len)
            .max()
            .unwrap_or(1)
            - 1;
        let mut cursor = 0usize;
        let mut findings = Vec::new();

        while cursor < data.len() && findings.len() < self.config.max_findings {
            let window_end = cursor.saturating_add(window_bytes).min(data.len());
            let search_end = window_end.saturating_add(overlap).min(data.len());
            let mut next_cursor = None;
            for magic in matcher.find_overlapping_iter(&data[cursor..search_end]) {
                let magic_offset = cursor + magic.start();
                if magic_offset >= window_end {
                    break;
                }
                let signature = &self.binwalk.pattern_signature_table[&magic.pattern().as_usize()];
                // The parser sees the complete input, even when EOF or the next
                // archive header lies far beyond the signature search window.
                let Ok(Ok(mut result)) =
                    std::panic::catch_unwind(|| (signature.parser)(data, magic_offset))
                else {
                    continue;
                };
                if result.offset < cursor
                    || result
                        .offset
                        .checked_add(result.size)
                        .is_none_or(|end| end > data.len())
                {
                    continue;
                }
                result.name = signature.name.clone();
                // Use the parser's real size (zero = unknown), bypassing
                // binwalk's post-processing that guesses a size up to EOF.
                let Some(finding) = map_signature_result(result, data) else {
                    continue;
                };
                if !self.config.include_formats.contains(&finding.format)
                    || finding.confidence < self.config.min_confidence
                {
                    continue;
                }

                // Continue after a complete archive. A header-only finding
                // stays size-unknown, and cannot truncate the carved payload.
                let archive_end = if finding.confidence >= Confidence::Medium {
                    finding.size.map(|size| (finding.offset + size) as usize)
                } else {
                    None
                };
                next_cursor = Some(
                    archive_end
                        .unwrap_or(magic_offset + 1)
                        .max(magic_offset + 1),
                );
                findings.push(finding);
                break;
            }
            let Some(next) = next_cursor else {
                break; // No validated finding in this search window.
            };
            cursor = next;
        }
        findings
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
        // A whole window with no finding terminates this scan chain.
        data.extend_from_slice(&[0; 2048]);
        data.extend_from_slice(&rar(200));

        let findings = scanner().scan_windows(&data, 1024);
        assert_eq!(findings.len(), 2);
        assert_eq!(findings[0].offset, 0);
        assert_eq!(findings[0].size, Some(8192));
        assert_eq!(findings[1].offset, 8292);
        assert_eq!(findings[1].size, Some(4096));
    }

    #[test]
    fn header_straddling_window_edge_is_parsed_in_full() {
        let mut data = vec![0; 1022];
        data.extend_from_slice(&rar(8192));
        let findings = scanner().scan_windows(&data, 1024);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].format, ArchiveFormat::Rar);
        assert_eq!(findings[0].offset, 1022);
        assert_eq!(findings[0].size, Some(8192));
    }

    #[test]
    fn unknown_archive_end_remains_unknown() {
        let mut data = rar(8192);
        data.truncate(8184);
        let findings = scanner().scan_windows(&data, 1024);
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
