use std::collections::HashSet;
use unicode_normalization::UnicodeNormalization;

/// Generic names that carry little information for output planning.
const GENERIC_NAMES: &[&str] = &[
    "download",
    "downloads",
    "archive",
    "archives",
    "files",
    "file",
    "data",
    "content",
    "contents",
    "image",
    "images",
    "folder",
    "folders",
    "temp",
    "tmp",
    "new folder",
    "untitled",
    "新建文件夹",
    "解压后",
    "压缩包",
    "未命名",
    "backup",
    "backups",
    "misc",
    "stuff",
    "other",
];

/// Semantic tokens that indicate a meaningful name.
const SEMANTIC_TOKENS: &[&str] = &[
    // version patterns handled separately, but these are literal words
    "v",
    "release",
    "beta",
    "alpha",
    "rc",
    "final",
    "stable",
    "dev",
    "nightly",
    "latest",
    // author / group indicators
    "by",
    "from",
    "author",
    "group",
    "team",
    "studio",
    "press",
    "publishing",
    "records",
    "productions",
    "inc",
    "ltd",
    "llc",
    // content descriptors
    "manual",
    "tutorial",
    "guide",
    "book",
    "novel",
    "comic",
    "manga",
    "lecture",
    "course",
    "episode",
    "season",
    "series",
    "movie",
    "film",
    "music",
    "album",
    "track",
    "audio",
    "video",
    "ebook",
    "audiobook",
    "edition",
    "volume",
    "part",
    "chapter",
];

/// Threshold: names at or above this similarity are considered Equivalent.
pub const SIMILARITY_EQUIVALENT: f32 = 0.85;
/// Threshold: names at or above this similarity (but below Equivalent) are Partial.
pub const SIMILARITY_PARTIAL: f32 = 0.55;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SimilarityLevel {
    Equivalent,
    Partial,
    Different,
}

pub fn classify_similarity(sim: f32) -> SimilarityLevel {
    if sim >= SIMILARITY_EQUIVALENT {
        SimilarityLevel::Equivalent
    } else if sim >= SIMILARITY_PARTIAL {
        SimilarityLevel::Partial
    } else {
        SimilarityLevel::Different
    }
}

/// Version-like regex patterns: v1.0, 1.2.3, r2, ver3, etc.
fn has_version_number(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    // v1, v1.0, v1.2.3, ver1, ver1.0, rev1, r1, r1.0, build123
    let version_prefixes = ["v", "ver", "rev", "r", "build", "release"];
    for prefix in &version_prefixes {
        if let Some(rest) = lower.strip_prefix(prefix) {
            if !rest.is_empty()
                && rest
                    .chars()
                    .next()
                    .map(|c| c.is_ascii_digit())
                    .unwrap_or(false)
            {
                return true;
            }
        }
    }
    // Bare digits: 1.0.3, 2.1, etc. (at start or after separator)
    let chars: Vec<char> = lower.chars().collect();
    for i in 0..chars.len() {
        if chars[i].is_ascii_digit() {
            // check if this starts a version-like sequence
            let mut j = i;
            let mut has_dot = false;
            while j < chars.len() && (chars[j].is_ascii_digit() || chars[j] == '.') {
                if chars[j] == '.' {
                    has_dot = true;
                }
                j += 1;
            }
            if has_dot && j - i >= 3 {
                // e.g. "1.0" or longer
                return true;
            }
        }
    }
    false
}

/// Detect bracket-enclosed info like [Author], (2024), {v2}.
fn has_bracket_info(s: &str) -> bool {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'[' || bytes[i] == b'(' || bytes[i] == b'{' {
            let open = bytes[i];
            let close = match open {
                b'[' => b']',
                b'(' => b')',
                b'{' => b'}',
                _ => unreachable!(),
            };
            i += 1;
            let start = i;
            while i < bytes.len() && bytes[i] != close {
                i += 1;
            }
            if i > start && i < bytes.len() {
                return true;
            }
        }
        i += 1;
    }
    false
}

/// Result of scoring a file/folder name.
#[derive(Debug, Clone, PartialEq)]
pub struct NameScore {
    /// Combined quality score (higher = more informative).
    pub total: f32,
    /// Number of semantic tokens detected.
    pub semantic_tokens: usize,
    /// Whether the name matches a known generic pattern.
    pub is_generic: bool,
}

/// Score a name for informational quality.
///
/// High-quality names contain semantic tokens, author/group labels,
/// version numbers, or bracket info. Generic names (download, files,
/// data, etc.) or hash-like strings get a penalty.
pub fn score_name(name: &str) -> NameScore {
    if name.is_empty() {
        return NameScore {
            total: 0.0,
            semantic_tokens: 0,
            is_generic: true,
        };
    }

    let lower = name.to_ascii_lowercase();
    let normalized = normalize_for_compare(name);

    // Short numeric names (<=6 chars) are treated as generic/low quality
    let is_purely_numeric_short = name.len() <= 6 && name.chars().all(|c| c.is_ascii_digit());

    // Check for generic name
    let is_generic = is_purely_numeric_short
        || GENERIC_NAMES
            .iter()
            .any(|g| normalized == *g || lower == *g);

    let mut total: f32 = 0.0;
    let mut semantic_count: usize = 0;

    // Base score from name length (longer names tend to be more informative)
    let len_score = (name.len() as f32).sqrt().min(10.0) * 0.3;
    total += len_score;

    // Count semantic tokens
    let tokens = extract_tokens(name);
    let token_set: HashSet<&str> = tokens.iter().copied().collect();
    for token in &token_set {
        if SEMANTIC_TOKENS
            .iter()
            .any(|s| s.eq_ignore_ascii_case(token))
        {
            semantic_count += 1;
            total += 1.0;
        }
    }

    // Version number bonus
    if has_version_number(name) {
        total += 2.0;
        semantic_count += 1;
    }

    // Bracket info bonus
    if has_bracket_info(name) {
        total += 1.5;
        semantic_count += 1;
    }

    // Penalty for generic names
    if is_generic {
        total = (total * 0.2).max(0.0);
    }

    // Penalty for hash-like names (long hex strings)
    if is_hash_like(name) {
        total = (total * 0.3).max(0.0);
    }

    NameScore {
        total,
        semantic_tokens: semantic_count,
        is_generic,
    }
}

/// Archive format suffixes to strip during normalization.
const ARCHIVE_SUFFIXES: &[&str] = &[
    ".tar.gz",
    ".tar.bz2",
    ".tar.xz",
    ".tar.zst",
    ".tar.lz",
    ".tar.lzma",
    ".tar.zstd",
    ".tar",
    ".tgz",
    ".tbz2",
    ".txz",
    ".zip",
    ".7z",
    ".rar",
    ".gz",
    ".bz2",
    ".xz",
    ".zst",
    ".lz",
    ".lzma",
    ".cab",
    ".iso",
    ".dmg",
    ".jar",
    ".war",
    ".ear",
];

/// Strip leading/trailing bracket content like [Author], (C102), [DL版].
fn strip_bracket_noise(s: &str) -> String {
    let mut result = s;
    loop {
        let trimmed = result.trim();
        if trimmed.is_empty() {
            break;
        }
        let first_char = trimmed.as_bytes()[0];
        let close_char = match first_char {
            b'[' => b']',
            b'(' => b')',
            b'{' => b'}',
            _ => break,
        };
        if let Some(end_idx) = trimmed.find(close_char as char) {
            // Only strip if it's a prefix bracket group (starts at beginning)
            let after = &trimmed[end_idx + 1..];
            result = after.trim();
        } else {
            break;
        }
    }
    result.to_string()
}

/// Normalize a string for comparison: fullwidth→halfwidth, NFD decomposition,
/// strip combining marks, lowercase, strip separators, strip archive suffixes,
/// strip bracket noise.
pub fn normalize_for_compare(s: &str) -> String {
    // 1. Fullwidth → halfwidth conversion
    let halfwidth: String = s
        .chars()
        .map(|c| {
            if ('\u{FF01}'..='\u{FF5E}').contains(&c) {
                // Fullwidth Latin characters: offset by 0xFEE0
                char::from_u32(c as u32 - 0xFEE0).unwrap_or(c)
            } else if c == '\u{3000}' {
                // Fullwidth space → ASCII space
                ' '
            } else if ('\u{FF10}'..='\u{FF19}').contains(&c) {
                // Fullwidth digits (redundant with above range but explicit)
                char::from_u32(c as u32 - 0xFEE0).unwrap_or(c)
            } else {
                c
            }
        })
        .collect();

    // 2. Strip archive format suffixes (order: longest first)
    let mut stripped = halfwidth.to_ascii_lowercase();
    for suffix in ARCHIVE_SUFFIXES {
        if let Some(stem) = stripped.strip_suffix(suffix) {
            stripped = stem.to_string();
            break;
        }
    }

    // 3. Strip bracket noise from leading/trailing positions
    let no_brackets = strip_bracket_noise(&stripped);

    // 4. NFD decomposition, strip combining marks, strip separators
    no_brackets
        .nfd()
        .collect::<String>()
        .chars()
        .filter(|c| !matches!(c, '.' | '_' | '-' | ' '))
        .filter(|c| !unicode_normalization::char::is_combining_mark(*c))
        .collect::<String>()
        .to_ascii_lowercase()
}

/// Compute similarity between two names (0.0 = no match, 1.0 = identical).
///
/// Uses normalized comparison: lowercase, unicode NFC, strip separators,
/// then computes longest common subsequence ratio.
pub fn name_similarity(a: &str, b: &str) -> f32 {
    if a == b {
        return 1.0;
    }
    let na = normalize_for_compare(a);
    let nb = normalize_for_compare(b);

    if na == nb {
        return 1.0;
    }
    if na.is_empty() || nb.is_empty() {
        return 0.0;
    }

    // LCS length
    let lcs_len = lcs_length(&na, &nb);
    let max_len = na.len().max(nb.len()) as f32;
    lcs_len as f32 / max_len
}

fn lcs_length(a: &str, b: &str) -> usize {
    let a_bytes = a.as_bytes();
    let b_bytes = b.as_bytes();
    let (a_len, b_len) = (a_bytes.len(), b_bytes.len());
    let mut prev = vec![0usize; b_len + 1];
    let mut curr = vec![0usize; b_len + 1];

    for i in 1..=a_len {
        for j in 1..=b_len {
            if a_bytes[i - 1] == b_bytes[j - 1] {
                curr[j] = prev[j - 1] + 1;
            } else {
                curr[j] = prev[j].max(curr[j - 1]);
            }
        }
        std::mem::swap(&mut prev, &mut curr);
        curr.iter_mut().for_each(|x| *x = 0);
    }
    prev[b_len]
}

fn extract_tokens(name: &str) -> Vec<&str> {
    name.split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|t| !t.is_empty())
        .collect()
}

fn is_hash_like(name: &str) -> bool {
    let stem = name
        .rsplit_once('.')
        .map(|(s, _)| s)
        .unwrap_or(name);
    // A name is hash-like if it's 8+ hex chars with no other structure
    if stem.len() >= 8 && stem.chars().all(|c| c.is_ascii_hexdigit()) {
        return true;
    }
    // Also check for md5/sha-like patterns
    if stem.len() >= 32 && stem.chars().all(|c| c.is_ascii_hexdigit()) {
        return true;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_name_scores_zero() {
        let score = score_name("");
        assert_eq!(score.total, 0.0);
        assert!(score.is_generic);
        assert_eq!(score.semantic_tokens, 0);
    }

    #[test]
    fn generic_names_get_penalty() {
        for name in &[
            "download",
            "files",
            "data",
            "archive",
            "新建文件夹",
            "未命名",
            "解压后",
            "压缩包",
        ] {
            let score = score_name(name);
            assert!(score.is_generic, "{name} should be generic");
            assert!(
                score.total <= 1.0,
                "{name} should have low score, got {}",
                score.total
            );
        }
    }

    #[test]
    fn new_folder_is_generic() {
        let score = score_name("new folder");
        assert!(score.is_generic, "new folder should be generic");
    }

    #[test]
    fn versioned_name_scores_higher() {
        let plain = score_name("myproject");
        let versioned = score_name("myproject-v2.1.0");
        assert!(versioned.total > plain.total);
        assert!(versioned.semantic_tokens >= plain.semantic_tokens);
    }

    #[test]
    fn bracket_info_increases_score() {
        let plain = score_name("ebook");
        let with_bracket = score_name("ebook [Author Name]");
        assert!(with_bracket.total > plain.total);
    }

    #[test]
    fn hash_like_names_get_penalized() {
        let score = score_name("a1b2c3d4e5f6");
        assert!(score.total <= 1.5, "hash-like name should be low: {}", score.total);
    }

    #[test]
    fn long_descriptive_name_scores_high() {
        let score = score_name("The Great Gatsby - F. Scott Fitzgerald (2024 Edition)");
        assert!(score.total > 3.0);
        assert!(score.semantic_tokens >= 2);
        assert!(!score.is_generic);
    }

    #[test]
    fn normalize_for_compare_strips_separators() {
        assert_eq!(normalize_for_compare("Hello-World"), "helloworld");
        assert_eq!(normalize_for_compare("hello_world"), "helloworld");
        assert_eq!(normalize_for_compare("Hello World"), "helloworld");
        assert_eq!(normalize_for_compare("Hello.World"), "helloworld");
    }

    #[test]
    fn normalize_for_compare_handles_unicode() {
        // NFC normalization
        let input = "caf\u{00e9}"; // already NFC
        let normalized = normalize_for_compare(input);
        assert_eq!(normalized, "cafe");
    }

    #[test]
    fn name_similarity_identical() {
        assert_eq!(name_similarity("foo", "foo"), 1.0);
        assert_eq!(name_similarity("Foo.Bar", "foo-bar"), 1.0);
        assert_eq!(name_similarity("hello_world", "hello world"), 1.0);
    }

    #[test]
    fn name_similarity_empty() {
        assert_eq!(name_similarity("", "foo"), 0.0);
        assert_eq!(name_similarity("foo", ""), 0.0);
        assert_eq!(name_similarity("", ""), 1.0);
    }

    #[test]
    fn name_similarity_similar() {
        let sim = name_similarity("archive-2024", "archive_2024");
        assert!(sim > 0.9, "should be very similar: {sim}");
    }

    #[test]
    fn name_similarity_different() {
        let sim = name_similarity("abc", "xyz");
        assert!(sim < 0.2, "should be very different: {sim}");
    }

    #[test]
    fn name_similarity_partial_overlap() {
        let sim = name_similarity("hello world", "hello there");
        assert!(sim > 0.3 && sim < 0.8, "should have partial overlap: {sim}");
    }

    #[test]
    fn has_version_number_detects_patterns() {
        assert!(has_version_number("v1.0"));
        assert!(has_version_number("V2.3.1"));
        assert!(has_version_number("ver3"));
        assert!(has_version_number("release1.2"));
        assert!(has_version_number("archive-1.0.3.zip"));
        assert!(!has_version_number("hello"));
        assert!(!has_version_number("a"));
    }

    #[test]
    fn has_bracket_info_detects_brackets() {
        assert!(has_bracket_info("file [info]"));
        assert!(has_bracket_info("file (2024)"));
        assert!(has_bracket_info("file {v2}"));
        assert!(!has_bracket_info("file"));
        assert!(!has_bracket_info("file []"));
    }

    #[test]
    fn fullwidth_normalization() {
        assert_eq!(name_similarity("\u{FF21}", "A"), 1.0);
        assert_eq!(name_similarity("\u{FF11}", "1"), 1.0);
        assert_eq!(name_similarity("Hello\u{3000}World", "Hello World"), 1.0);
    }

    #[test]
    fn archive_suffix_stripping() {
        assert_eq!(name_similarity("archive.zip", "archive"), 1.0);
        assert_eq!(name_similarity("data.tar.gz", "data"), 1.0);
        assert_eq!(name_similarity("backup.7z", "backup"), 1.0);
        assert_eq!(name_similarity("file.rar", "file"), 1.0);
        assert_eq!(name_similarity("image.tgz", "image"), 1.0);
    }

    #[test]
    fn bracket_noise_stripping() {
        let sim = name_similarity("[Author] book", "book");
        assert!(sim > 0.8, "bracket-stripped names should be similar: {sim}");
        let sim2 = name_similarity("(C102) content", "content");
        assert!(sim2 > 0.8, "paren-stripped names should be similar: {sim2}");
    }

    #[test]
    fn short_numeric_names_are_generic() {
        assert!(score_name("1").is_generic);
        assert!(score_name("01").is_generic);
        assert!(score_name("123456").is_generic);
        assert!(!score_name("1234567").is_generic);
        assert!(!score_name("abc123").is_generic);
    }

    #[test]
    fn classify_similarity_boundaries() {
        assert_eq!(classify_similarity(1.0), SimilarityLevel::Equivalent);
        assert_eq!(classify_similarity(0.85), SimilarityLevel::Equivalent);
        assert_eq!(classify_similarity(0.84), SimilarityLevel::Partial);
        assert_eq!(classify_similarity(0.55), SimilarityLevel::Partial);
        assert_eq!(classify_similarity(0.54), SimilarityLevel::Different);
        assert_eq!(classify_similarity(0.0), SimilarityLevel::Different);
    }
}
