use std::path::Path;
use unicode_normalization::UnicodeNormalization;

/// Bounded set of common duplicate/copy suffix aliases as secondary candidate views.
/// Alias processing must not globally strip suffixes before primary sequence hypotheses are formed.
/// Alias views may strengthen an existing hypothesis or fill a slot implied by it; they must not independently prove a volume set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AliasKind {
    UnderscoreNumeric, // 03_1
    ParenthesizedCopy, // file (1), file (2)
    DashCopy,          // file - copy, file copy
    ChineseCopy,       // file 副本, file 副本(1)
}

pub fn alias_stripped_name(path: &Path) -> Option<(String, AliasKind)> {
    path.file_name()?.to_str()?;
    // Need to consider extension handling: alias suffix is before extension or after?
    // Examples: `03_1` with maybe no extension? For `03_1.jpg`, `_1` is before extension.
    // We'll handle stem and extension separately.
    let stem = path.file_stem()?.to_str()?;
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");

    // Pattern 1: trailing underscore + digits  e.g., "03_1" -> "03"
    if let Some(pos) = stem.rfind('_') {
        let suffix = &stem[pos + 1..];
        if !suffix.is_empty() && suffix.chars().all(|c| c.is_ascii_digit()) {
            let base_stem = &stem[..pos];
            if !base_stem.is_empty() {
                let base = if ext.is_empty() {
                    base_stem.to_string()
                } else {
                    format!("{}.{}", base_stem, ext)
                };
                return Some((base, AliasKind::UnderscoreNumeric));
            }
        }
    }

    // Patterns with stem containing " (N)" at end
    // e.g., "foo_02 (1)" -> "foo_02"
    // Also "photo (1).jpg" -> but design says (1)/(2)/(3) may legitimately be sequence itself, so we must not globally strip? However alias view is secondary: we only produce alias view, primary keeps original. So it's safe to produce.
    if let Some(p) = stem.rfind(" (") {
        if stem.ends_with(')') {
            let inner = &stem[p + 2..stem.len() - 1];
            if !inner.is_empty() && inner.chars().all(|c| c.is_ascii_digit()) {
                let base_stem = stem[..p].trim_end();
                let base = if ext.is_empty() {
                    base_stem.to_string()
                } else {
                    format!("{}.{}", base_stem, ext)
                };
                return Some((base, AliasKind::ParenthesizedCopy));
            }
        }
    }
    // Without space: "file(1)"
    if let Some(p) = stem.rfind('(') {
        if stem.ends_with(')') && p > 0 {
            let inner = &stem[p + 1..stem.len() - 1];
            if !inner.is_empty() && inner.chars().all(|c| c.is_ascii_digit()) {
                let base_stem = stem[..p].trim_end();
                let base = if ext.is_empty() {
                    base_stem.to_string()
                } else {
                    format!("{}.{}", base_stem, ext)
                };
                return Some((base, AliasKind::ParenthesizedCopy));
            }
        }
    }

    // Dash copy: "foo - copy", "foo copy", case-insensitive, also "foo - Copy (2)"? Simplified.
    let lower_stem = stem.to_ascii_lowercase();
    for suffix in &[" - copy", " copy", " -副本", " 副本"] {
        if lower_stem.ends_with(suffix) {
            let base_stem = &stem[..stem.len() - suffix.len()];
            let base = if ext.is_empty() {
                base_stem.trim_end().to_string()
            } else {
                format!("{}.{}", base_stem.trim_end(), ext)
            };
            let kind = if suffix.contains("副本") {
                AliasKind::ChineseCopy
            } else {
                AliasKind::DashCopy
            };
            return Some((base, kind));
        }
    }
    // Also handle "foo(副本)"? Rare.
    if lower_stem.ends_with("副本") {
        let base_stem = &stem[..stem.len() - "副本".len()];
        let base_stem = base_stem
            .trim_end_matches(|c| c == ' ' || c == '-' || c == '_' || c == '(' || c == ')');
        let base = if ext.is_empty() {
            base_stem.to_string()
        } else {
            format!("{}.{}", base_stem, ext)
        };
        return Some((base, AliasKind::ChineseCopy));
    }

    None
}

/// For a given primary hypothesis prefix/suffix, collect alias candidates that would fill a slot.
/// We consider each file in directory; if its alias_stripped_name matches a base that fits the hypothesis pattern, then its alias view is candidate for that slot.
pub fn collect_alias_candidates(
    dir_files: &[crate::volumes::directory::DirectoryFile],
    hypothesis_prefix: &str,
    hypothesis_suffix: &str,
) -> Vec<(crate::volumes::directory::DirectoryFile, String, AliasKind)> {
    let mut out = Vec::new();
    for f in dir_files {
        if let Some((stripped, kind)) = alias_stripped_name(&f.path) {
            let normalized_stripped: String = stripped.nfkc().collect();
            if normalized_stripped.len() < hypothesis_prefix.len() + hypothesis_suffix.len() {
                continue;
            }
            if !normalized_stripped.starts_with(hypothesis_prefix)
                || !normalized_stripped.ends_with(hypothesis_suffix)
            {
                continue;
            }
            // If stripped matches pattern, then this file's alias view can be candidate.
            // We will later group by ordinal value derived from stripped middle.
            out.push((f.clone(), normalized_stripped, kind));
        }
    }
    out
}
