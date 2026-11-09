//! LZMA_Alone detection without binwalk's size and parameter heuristics.

use binwalk::signatures::common::{CONFIDENCE_HIGH, CONFIDENCE_LOW};

pub(super) fn magic() -> Vec<Vec<u8>> {
    let mut patterns = Vec::new();
    // All lc/lp/pb combinations supported by liblzma, and all portable
    // dictionary encodings (2^n or 2^n + 2^(n-1)), not just four presets.
    // See xz/doc/lzma-file-format.txt, sections 1.1.1 and 1.1.2.
    for pb in 0..=4 {
        for lp in 0..=4 {
            for lc in 0..=4 - lp {
                let property = (pb * 5 + lp) * 9 + lc;
                for n in 12..=31 {
                    for dictionary in [1u32 << n, (1u32 << n) + (1u32 << (n - 1))] {
                        let mut pattern = vec![property];
                        pattern.extend_from_slice(&dictionary.to_le_bytes());
                        patterns.push(pattern);
                    }
                }
            }
        }
    }
    patterns
}

pub(super) fn valid_header(data: &[u8]) -> bool {
    // The range coder starts with a zero byte. Output length is a u64;
    // small/empty streams and sizes above 4 GiB are not invalid headers.
    data.len() >= 14 && data[0] < 225 && data[13] == 0
}

pub(super) fn confidence(consumed: u64, produced: u64) -> u8 {
    // A zero-size header followed by five range-coder bytes is technically
    // decodable, but common in ordinary binary metadata. Without an EOS
    // marker or any output it is weak evidence, not a checked payload.
    if consumed == 18 && produced == 0 {
        CONFIDENCE_LOW
    } else {
        CONFIDENCE_HIGH
    }
}
