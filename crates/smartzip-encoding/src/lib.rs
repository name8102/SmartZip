//! Archive entry name encoding detection.
//!
//! Feeds raw bytes to `chardetng` and validates/cross-checks with the CJK
//! encodings that SmartZip specifically targets.

use chardetng::EncodingDetector;
use encoding_rs::Encoding;
use serde::{Deserialize, Serialize};
use std::str;

/// Built-in encodings that SmartZip will always test.
pub const CJK_CANDIDATES: &[&str] = &[
    "UTF-8",
    "GB18030",
    "GBK",
    "Big5",
    "Shift_JIS",
    "EUC-JP",
    "EUC-KR",
];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EncodingDetectionResult {
    pub selected: String,
    pub confidence: f32,
    pub candidates: Vec<EncodingCandidate>,
    pub decoded_sample: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EncodingCandidate {
    pub name: String,
    pub confidence: f32,
}

pub struct ArchiveEncodingDetector {
    detector: EncodingDetector,
}

impl Default for ArchiveEncodingDetector {
    fn default() -> Self {
        Self {
            detector: EncodingDetector::new(),
        }
    }
}

impl ArchiveEncodingDetector {
    pub fn new() -> Self {
        Self::default()
    }

    /// Detect the most likely text encoding for a byte slice, e.g. raw
    /// archive entry names.
    pub fn detect(&mut self, bytes: &[u8]) -> EncodingDetectionResult {
        if bytes.is_empty() {
            return empty_result();
        }

        // UTF-8 fast path
        if let Ok(decoded) = str::from_utf8(bytes) {
            let (cjk_ratio, _) = classify_text(decoded);
            let confidence = if cjk_ratio > 0.5 { 0.95 } else { 0.85 };
            return EncodingDetectionResult {
                selected: "UTF-8".into(),
                confidence,
                candidates: vec![EncodingCandidate {
                    name: "UTF-8".into(),
                    confidence,
                }],
                decoded_sample: sample(decoded),
            };
        }

        // Feed bytes to chardetng
        self.detector.feed(bytes, false);
        self.detector.feed(&[], true); // flush
        let (encoding, _reliable) = self.detector.guess_assess(None, true);

        let primary = primary_candidate(encoding, 0.7, bytes);
        let alternatives = cross_check_candidates(bytes, &primary);
        let best = primary.clone();

        // Reset detector for next call
        self.detector = EncodingDetector::new();

        EncodingDetectionResult {
            selected: best.name.clone(),
            confidence: best.confidence,
            candidates: {
                let mut all = vec![EncodingCandidate {
                    name: best.name.clone(),
                    confidence: best.confidence,
                }];
                all.extend(alternatives);
                all
            },
            decoded_sample: sample(&primary.decoded),
        }
    }
}

fn primary_candidate(encoding: &'static Encoding, confidence: f32, bytes: &[u8]) -> Candidate {
    let (decoded, _) = encoding.decode_without_bom_handling(bytes);
    let (cjk_ratio, replacement_ratio) = classify_text(&decoded);
    let adjusted_confidence =
        confidence * ((1.0 - replacement_ratio) + 0.3 * cjk_ratio).clamp(0.0, 1.0);
    Candidate {
        name: encoding.name().into(),
        confidence: (adjusted_confidence * 100.0).round() / 100.0,
        decoded: decoded.into_owned(),
    }
}

fn cross_check_candidates(bytes: &[u8], primary: &Candidate) -> Vec<EncodingCandidate> {
    let mut candidates = Vec::new();

    for label in CJK_CANDIDATES {
        if *label == primary.name {
            continue;
        }

        let Some(encoding) = Encoding::for_label(label.as_bytes()) else {
            continue;
        };

        let (decoded, _) = encoding.decode_without_bom_handling(bytes);
        let (cjk_ratio, replacement_ratio) = classify_text(&decoded);

        // Skip if all replacement chars
        if replacement_ratio > 0.9 {
            continue;
        }

        let confidence = ((1.0 - replacement_ratio) * 0.6 + cjk_ratio * 0.4).min(1.0);
        candidates.push(EncodingCandidate {
            name: label.to_string(),
            confidence: (confidence * 100.0).round() / 100.0,
        });
    }

    // Sort by confidence descending
    candidates.sort_by(|a, b| b.confidence.partial_cmp(&a.confidence).unwrap());

    candidates
}

#[derive(Debug, Clone)]
struct Candidate {
    name: String,
    confidence: f32,
    decoded: String,
}

fn classify_text(decoded: &str) -> (f32, f32) {
    let total = decoded.chars().count().max(1) as f32;
    let mut cjk = 0u32;
    let mut replacement = 0u32;

    for ch in decoded.chars() {
        if ch == '\u{FFFD}' {
            replacement += 1;
        } else if is_cjk_or_kana(ch) {
            cjk += 1;
        }
    }

    (cjk as f32 / total, replacement as f32 / total)
}

fn is_cjk_or_kana(ch: char) -> bool {
    matches!(
        ch,
        '\u{4E00}'..='\u{9FFF}'   // CJK Unified
        | '\u{3400}'..='\u{4DBF}' // CJK Extension A
        | '\u{20000}'..='\u{2A6DF}' // CJK Extension B
        | '\u{F900}'..='\u{FAFF}' // CJK Compatibility
        | '\u{3040}'..='\u{309F}' // Hiragana
        | '\u{30A0}'..='\u{30FF}' // Katakana
        | '\u{AC00}'..='\u{D7AF}' // Hangul
        | '\u{1100}'..='\u{11FF}' // Hangul Jamo
    )
}

fn empty_result() -> EncodingDetectionResult {
    EncodingDetectionResult {
        selected: "UTF-8".into(),
        confidence: 1.0,
        candidates: vec![EncodingCandidate {
            name: "UTF-8".into(),
            confidence: 1.0,
        }],
        decoded_sample: String::new(),
    }
}

/// Decode raw bytes using the specified encoding name.
/// Returns `None` if the encoding is unknown or the bytes can't be decoded.
pub fn decode_name(raw: &[u8], encoding: &str) -> Option<String> {
    if raw.is_empty() {
        return Some(String::new());
    }
    let enc = Encoding::for_label(encoding.as_bytes())?;
    let (decoded, had_replacements) = enc.decode_without_bom_handling(raw);
    if had_replacements {
        return None;
    }
    Some(decoded.into_owned())
}

fn sample(text: &str) -> String {
    text.chars()
        .take(80)
        .map(|ch| {
            if ch.is_control() && ch != '\n' && ch != '\t' {
                '\u{FFFD}'
            } else {
                ch
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::*;

    #[test]
    fn detects_utf8_quickly() {
        let mut detector = ArchiveEncodingDetector::new();
        let result = detector.detect("你好世界hello.zip".as_bytes());
        assert_eq!(result.selected, "UTF-8");
        assert!(result.confidence > 0.8);
    }

    #[test]
    fn detects_shift_jis() {
        // 日本語のテストファイル in Shift_JIS (enough data for chardetng)
        let shift_jis = b"\x93\xFA\x96{\x8C\xEA\x82\xCC\x83e\x83X\x83g\x83t\x83@\x83C\x83\x8B";
        let mut detector = ArchiveEncodingDetector::new();
        let result = detector.detect(shift_jis);
        assert!(
            result.candidates.iter().any(|c| c.name == "Shift_JIS"),
            "Shift_JIS should appear in candidates: {:?}",
            result.candidates
        );
    }

    #[test]
    fn detects_gbk() {
        // 你好世界欢迎使用解压缩工具 in GBK
        let gbk = b"\xC4\xE3\xBA\xC3\xCA\xC0\xBD\xE7\xBB\xB6\xD3\xAD\xCA\xB9\xD3\xC3\xBD\xE2\xD1\xB9\xCB\xF5\xB9\xA4\xBE\xDF";
        let mut detector = ArchiveEncodingDetector::new();
        let result = detector.detect(gbk);
        assert!(
            result
                .candidates
                .iter()
                .any(|c| c.name == "GBK" || c.name == "GB18030"),
            "GBK/GB18030 should appear in candidates: {:?}",
            result.candidates
        );
    }

    #[test]
    fn empty_returns_utf8() {
        let mut detector = ArchiveEncodingDetector::new();
        let result = detector.detect(&[]);
        assert_eq!(result.selected, "UTF-8");
        assert_eq!(result.confidence, 1.0);
    }

    // ── Fixture-based parametrized encoding detection tests ─────────────

    /// A single encoding test case.
    struct EncodingTestCase {
        label: &'static str,
        data: &'static [u8],
        expected: &'static [&'static str],
    }

    fn get_encoding_test_cases() -> Vec<EncodingTestCase> {
        vec![
            EncodingTestCase {
                label: "GBK",
                data: b"\xd6\xd0\xce\xc4\xce\xc4\xbc\xfe\xc3\xfb\xb2\xe2\xca\xd4.txt/\xd1\xb9\xcb\xf5\xb0\xfc\xcb\xb5\xc3\xf7\xce\xc4\xb5\xb5.doc",
                expected: &["GB18030", "GBK"],
            },
            EncodingTestCase {
                label: "Shift_JIS",
                data: b"\x93\xfa\x96\x7b\x8c\xea\x83\x74\x83\x40\x83\x43\x83\x8b\x96\xbc\x83\x65\x83\x58\x83\x67.txt/\x8e\x91\x97\xbf/\x89\xef\x8b\x63\x83\x81\x83\x82.docx",
                expected: &["Shift_JIS"],
            },
            EncodingTestCase {
                label: "EUC-KR",
                data: b"\xc7\xd1\xb1\xdb\xc6\xc4\xc0\xcf\xc0\xcc\xb8\xa7.txt/\xba\xb8\xb0\xed\xbc\xad_2024.hwp",
                expected: &["EUC-KR"],
            },
            EncodingTestCase {
                label: "Big5",
                data: b"\xc1\x63\xc5\xe9\xa4\xa4\xa4\xe5\xc0\xc9\xae\xd7\xa6W\xba\xd9.txt/\xb7\x7c\xc4\xb3\xb0O\xbf\xfd.doc",
                expected: &["Big5"],
            },
            EncodingTestCase {
                label: "UTF-8",
                data: "中文文件名测试.txt/日本語テスト.txt/한국어테스트.txt/English_File.txt".as_bytes(),
                expected: &["UTF-8"],
            },
        ]
    }

    #[test]
    fn parametrized_encoding_detection() {
        for case in get_encoding_test_cases() {
            let mut detector = ArchiveEncodingDetector::new();
            let result = detector.detect(case.data);

            let found = case.expected.iter().any(|enc| {
                result.selected == *enc || result.candidates.iter().any(|c| c.name == *enc)
            });

            assert!(
                found,
                "{}: expected one of {:?} — got selected='{}', candidates={:?}",
                case.label,
                case.expected,
                result.selected,
                result
                    .candidates
                    .iter()
                    .map(|c| &c.name)
                    .collect::<Vec<_>>()
            );
        }
    }

    #[rstest]
    #[case::ascii("Hello, world! this is plain ASCII text.", "UTF-8")]
    #[case::gbk("GBK bytes", "GB18030,GBK")]
    #[case::sjis("Shift_JIS bytes", "Shift_JIS")]
    fn detect_single_encoding(#[case] label: &str, #[case] expected_csv: &str) {
        let data: &[u8] = match label {
            "GBK bytes" => b"\xC4\xE3\xBA\xC3\xCA\xC0\xBD\xE7",
            "Shift_JIS bytes" => b"\x93\xFA\x96{\x8C\xEA\x82\xCC\x83e\x83X\x83g",
            _ => b"Hello, world! this is plain ASCII text.",
        };

        let expected_list: Vec<&str> = expected_csv.split(',').collect();

        let mut detector = ArchiveEncodingDetector::new();
        let result = detector.detect(data);

        let found = expected_list
            .iter()
            .any(|enc| result.selected == *enc || result.candidates.iter().any(|c| c.name == *enc));

        assert!(
            found,
            "{label}: expected one of {expected_list:?} — got selected='{}', candidates={:?}",
            result.selected,
            result
                .candidates
                .iter()
                .map(|c| &c.name)
                .collect::<Vec<_>>()
        );
    }

    #[rstest]
    fn detector_is_reusable() {
        let mut detector = ArchiveEncodingDetector::new();

        // First detection
        let r1 = detector.detect("你好世界".as_bytes());
        assert_eq!(r1.selected, "UTF-8");

        // Second detection — detector should be reset internally
        let r2 =
            detector.detect(b"\x93\xFA\x96{\x8C\xEA\x82\xCC\x83e\x83X\x83g\x83t\x83@\x83C\x83\x8B");
        assert!(
            r2.candidates.iter().any(|c| c.name == "Shift_JIS"),
            "second detect: Shift_JIS should appear: {:?}",
            r2.candidates
        );
    }
}
