use regex::Regex;
use chinese_number::{ChineseCountMethod, ChineseToNumber};
use std::sync::OnceLock;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrdinalToken {
    /// byte range in normalized_name
    pub start: usize,
    pub end: usize,
    pub value: u64,
    pub raw: String,
}

/// Parse ordinal tokens from a normalized filename.
/// - ASCII digits (including full-width/circled after NFKC) via regex
/// - Chinese compound numerals via chinese-number crate
/// - Roman numerals where appropriate (e.g., "part IV") are handled via ASCII letters; we detect only when token is isolated roman form.
///
/// We return tokens sorted by start position. Overlapping tokens (e.g., chinese inside digits) are deduplicated preferring larger span?
pub fn parse_ordinal_tokens(normalized: &str) -> Vec<OrdinalToken> {
    let mut tokens = Vec::new();
    // 1. ASCII digit sequences – these already cover full-width/circled after NFKC.
    // NFKC converts ０-９ to 0-9 and ①-⑳ to 1-20 etc., so regex suffices.
    let re = digit_regex();
    for m in re.find_iter(normalized) {
        let raw = m.as_str().to_string();
        // Do not interpret fractions: if digits contain '.' or '/' we skip – but our regex only captures \d+, so fraction like "1/2" yields two tokens "1" and "2" independently. Design says do not interpret fractions as volume numbers, so we should not combine them. Keeping them separate is safe.
        if let Ok(v) = raw.parse::<u64>() {
            // Exclude zero? Zero is allowed? Volume ordinals may start from 0 or 1; allow 0.
            tokens.push(OrdinalToken {
                start: m.start(),
                end: m.end(),
                value: v,
                raw,
            });
        }
    }

    // 2. Chinese numerals – scan for runs of Chinese numeral chars.
    // Characters considered Chinese numerals: 零一二三四五六七八九十百千萬亿兆〇○兩两壹貳參 etc.
    // We look for substrings where all chars are in this set and length >=1, then attempt parse.
    // To avoid double-counting digits, we skip ranges already covered by digit tokens.
    let chinese_tokens = parse_chinese_tokens(normalized, &tokens);
    tokens.extend(chinese_tokens);

    // 3. Roman numerals – detect isolated tokens of [IVXLCDM]+ (upper) that parse via roman crate logic.
    // We add only if they are not already part of digit tokens and are likely ordinal (bounded 1..3999).
    // Use simple heuristic: isolated by non-alpha boundaries.
    let roman_tokens = parse_roman_tokens(normalized, &tokens);
    tokens.extend(roman_tokens);

    tokens.sort_by_key(|t| t.start);
    // Remove overlapping tokens: keep earliest, but if overlap, keep the one with larger raw? Simple: if tokens overlap, keep first encountered (digit preferred as already).
    let mut deduped = Vec::new();
    for tok in tokens {
        if let Some(last) = deduped.last() as Option<&OrdinalToken> {
            if tok.start < last.end {
                continue;
            }
        }
        deduped.push(tok);
    }
    deduped
}

fn digit_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\d+").unwrap())
}

fn parse_chinese_tokens(normalized: &str, existing: &[OrdinalToken]) -> Vec<OrdinalToken> {
    // Cheap scan: iterate char indices, build runs of chinese numeral chars.
    const CN_CHARS: &str = "零一二三四五六七八九十百千萬万亿億兆〇○兩两壹貳參肆伍陸柒捌玖拾佰仟";
    let mut out = Vec::new();
    let chars: Vec<(usize, char)> = normalized.char_indices().collect();
    let mut run_start: Option<usize> = None;
    let mut run_chars = String::new();
    let mut run_byte_start = 0usize;

    let flush = |run_chars: &str, byte_start: usize, byte_end: usize, out: &mut Vec<OrdinalToken>| {
        if run_chars.is_empty() {
            return;
        }
        // Attempt to parse via chinese-number with TenThousand method (most common).
        // Also try naive if method fails? Try both.
        let parsed = <&str as ChineseToNumber<u64>>::to_number(&run_chars, ChineseCountMethod::TenThousand)
            .or_else(|_| <&str as ChineseToNumber<u64>>::to_number_naive(&run_chars))
            .ok();
        if let Some(v) = parsed {
            // Filter out zero? Allow.
            // Avoid single char '十' ambiguous? But still value 10, treat as ordinal.
            out.push(OrdinalToken {
                start: byte_start,
                end: byte_end,
                value: v,
                raw: run_chars.to_string(),
            });
        }
    };

    for idx in 0..=chars.len() {
        let (byte_pos, ch) = if idx < chars.len() {
            chars[idx]
        } else {
            // sentinel flush
            (normalized.len(), '\0')
        };
        let is_cn = CN_CHARS.contains(ch);
        if is_cn {
            if run_start.is_none() {
                run_start = Some(idx);
                run_byte_start = byte_pos;
                run_chars.clear();
            }
            run_chars.push(ch);
        } else {
            if let Some(_) = run_start {
                let byte_end = byte_pos;
                // Check overlap with existing digit tokens
                let overlaps = existing.iter().any(|t| !(byte_end <= t.start || run_byte_start >= t.end));
                if !overlaps {
                    // Need to handle that chinese run may be part of larger word like "資源第...卷"? Our run already isolates consecutive CN chars, which may be "二十三". Good.
                    flush(&run_chars, run_byte_start, byte_end, &mut out);
                }
                run_start = None;
                run_chars.clear();
            }
        }
    }
    out
}

fn parse_roman_tokens(normalized: &str, existing: &[OrdinalToken]) -> Vec<OrdinalToken> {
    // Simple roman parse: find word boundaries with [IVXLCDM]+ (2+ chars? Allow single I,V,X etc)
    // Use regex.
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"(?i)\b[IVXLCDM]+\b").unwrap());
    let mut out = Vec::new();
    for m in re.find_iter(normalized) {
        let raw = m.as_str();
        // Skip if overlaps existing
        if existing.iter().any(|t| !(m.end() <= t.start || m.start() >= t.end)) {
            continue;
        }
        // Also skip if already counted as roman inside chinese? no.
        // Avoid single letter like "I" inside normal word? Boundaries ensure, but still maybe false positive.
        // Require that roman token is upper-ish and length maybe <=8 and value 1..3999 canonical.
        let upper = raw.to_ascii_uppercase();
        if let Some(v) = roman::from(&upper) {
            // roman::from returns Some if canonical? It does validate.
            let is_candidate = v >= 1 && v <= 3999;
            if is_candidate {
                // Further filter: if surrounding is like "part" prefix? We accept all for now.
                out.push(OrdinalToken {
                    start: m.start(),
                    end: m.end(),
                    value: v as u64,
                    raw: raw.to_string(),
                });
            }
        }
    }
    out
}

/// Generate filename hypotheses by varying exactly one ordinal token.
/// Returns list of hypotheses, each with varying token index and members grouped.
#[derive(Debug, Clone)]
pub struct SequenceHypothesis {
    pub varying_token_idx: usize,
    pub varying_token_value_seed: u64,
    pub prefix: String,
    pub suffix: String,
    /// filename_ordinal -> list of candidates (usually 1)
    pub groups: std::collections::BTreeMap<u64, Vec<crate::volumes::directory::DirectoryFile>>,
    /// gap warning: true if ordinal sequence has holes
    pub has_gap: bool,
}

pub fn generate_single_token_hypotheses(
    seed_path: &std::path::Path,
    index: &crate::volumes::directory::DirectoryVolumeIndex,
) -> Vec<SequenceHypothesis> {
    let seed_file = match index.find_file(seed_path) {
        Some(f) => f,
        None => return Vec::new(),
    };
    if seed_file.filename_ordinals.is_empty() {
        return Vec::new();
    }
    let mut hypotheses = Vec::new();
    for token_idx in 0..seed_file.filename_ordinals.len() {
        let tok = &seed_file.filename_ordinals[token_idx];
        let prefix = seed_file.normalized_name[..tok.start].to_string();
        let suffix = seed_file.normalized_name[tok.end..].to_string();
        let mut groups: std::collections::BTreeMap<u64, Vec<crate::volumes::directory::DirectoryFile>> = Default::default();
        for file in &index.files {
            // Must have same number of tokens? Not necessarily, but other tokens must match fixed parts.
            // Simplistic: check if normalized_name starts with prefix and ends with suffix, and middle can be parsed as ordinal.
            if file.normalized_name.len() < prefix.len() + suffix.len() {
                continue;
            }
            if !file.normalized_name.starts_with(&prefix) {
                continue;
            }
            if !file.normalized_name.ends_with(&suffix) {
                continue;
            }
            let mid_start = prefix.len();
            let mid_end = file.normalized_name.len() - suffix.len();
            if mid_start > mid_end {
                continue;
            }
            let mid = &file.normalized_name[mid_start..mid_end];
            // Mid should be parseable as ordinal integer (via same logic as seed token)
            // For simplicity, try parse as u64, else try chinese/roman parse via parse_ordinal_tokens on mid string (must be single token covering whole mid)
            let mid_tokens = parse_ordinal_tokens(mid);
            // Mid should be exactly one token covering whole mid (trimmed)
            // But allow leading zeros etc. For mid "03_1" alias? That will be handled via alias module later, not here. So for primary hypothesis, we require mid is pure ordinal token string that matches full mid.
            // So check if mid tokens len ==1 and token covers trimmed mid and not empty.
            let trimmed = mid.trim();
            if trimmed.is_empty() {
                continue;
            }
            // If mid parsing yields one token that spans trimmed, accept.
            // Else if mid is numeric string directly, accept parse.
            let parsed_value = if mid_tokens.len() == 1 && mid_tokens[0].raw == trimmed {
                Some(mid_tokens[0].value)
            } else if let Ok(v) = trimmed.parse::<u64>() {
                Some(v)
            } else {
                // Try chinese directly
                <&str as ChineseToNumber<u64>>::to_number(&trimmed, ChineseCountMethod::TenThousand)
                    .or_else(|_| <&str as ChineseToNumber<u64>>::to_number_naive(&trimmed))
                    .ok()
            };
            let Some(v) = parsed_value else { continue };
            // Additionally, for other tokens in file to be considered matching, we must ensure that all other token positions match seed's fixed tokens?
            // Our prefix/suffix check already ensures fixed parts are equal, but we also need to ensure that other ordinal tokens (outside varying) are not varying arbitrarily.
            // Example seed "资源2026_第①卷.jpg" has tokens [2026, 1]. Varying token idx 1 -> prefix "资源2026_第", suffix "卷.jpg". Any file with same prefix/suffix and any mid ordinal would be grouped, regardless of other token 2026 fixed as part of prefix. That's correct because other token is fixed inside prefix.
            // So hypothesis grouping via prefix/suffix already enforces single varying token.
            groups.entry(v).or_default().push(file.clone());
        }
        // Hypothesis must contain seed's ordinal
        if !groups.contains_key(&tok.value) {
            continue;
        }
        // Must have at least 1 member, but for volume sets we handle 1..N. At least seed present.
        // To avoid combinatorial explosion, we keep hypothesis even if only 1 member (will be handled as possible single vs incomplete)
        let has_gap = check_gap(&groups);
        hypotheses.push(SequenceHypothesis {
            varying_token_idx: token_idx,
            varying_token_value_seed: tok.value,
            prefix,
            suffix,
            groups,
            has_gap,
        });
    }
    hypotheses
}

fn check_gap(groups: &std::collections::BTreeMap<u64, Vec<crate::volumes::directory::DirectoryFile>>) -> bool {
    if groups.is_empty() {
        return false;
    }
    let keys: Vec<u64> = groups.keys().cloned().collect();
    for w in keys.windows(2) {
        if w[1] != w[0] + 1 {
            return true;
        }
    }
    false
}
