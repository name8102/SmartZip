//! Pure whole-manifest planning. Filesystem equivalence is confirmed separately before writing.
use smartzip_core::path_policy::*;
use smartzip_core::{Result, SmartZipError};
use std::collections::{BTreeMap, BTreeSet};
use unicode_normalization::UnicodeNormalization;
use unicode_segmentation::UnicodeSegmentation;

#[derive(Debug, Clone)]
pub struct PathEntry {
    pub id: u64,
    pub source: String,
    pub raw_name: Option<Vec<u8>>,
    pub source_kind: SourceNameKind,
    pub is_dir: bool,
}
#[derive(Default)]
struct Node {
    children: BTreeMap<String, Node>,
    entries: Vec<usize>,
    directory: bool,
    identity: Vec<u8>,
    mapped: String,
    reasons: Vec<PathMappingReason>,
    full_shortened: bool,
}
fn failure(reason: PathConstraintReason, detail: &str, policy: &TargetPathPolicy) -> SmartZipError {
    let mut d = PathDiagnostic::new(reason, PathStage::Preflight, detail);
    d.policy = Some(policy.clone());
    d.error()
}
/// Parse decoded format paths, never split an undecoded multibyte raw name.
pub fn logical_components(path: &str) -> Result<Vec<String>> {
    if path.is_empty() || path.contains('\0') || path.starts_with(['/', '\\']) {
        return Err(SmartZipError::UnsafeArchivePath { entry: path.into() });
    }
    let normalized = path.replace('\\', "/");
    let first = normalized.split('/').next().unwrap_or_default();
    if first.as_bytes().get(1) == Some(&b':') {
        return Err(SmartZipError::UnsafeArchivePath { entry: path.into() });
    }
    let mut parts = Vec::new();
    for part in normalized.split('/') {
        if part == ".." {
            return Err(SmartZipError::UnsafeArchivePath { entry: path.into() });
        }
        if part.is_empty() || part == "." {
            continue;
        }
        parts.push(part.to_owned());
    }
    if parts.is_empty() {
        return Err(SmartZipError::UnsafeArchivePath { entry: path.into() });
    }
    if parts.len() > 256 {
        return Err(SmartZipError::ResourceLimit {
            detail: "archive path exceeds 256 components".into(),
        });
    }
    Ok(parts)
}
fn put_field(out: &mut Vec<u8>, field: &[u8]) {
    out.extend_from_slice(&(field.len() as u64).to_le_bytes());
    out.extend_from_slice(field);
}
fn node_identity(
    parts: &[String],
    directory: bool,
    raw: Option<&[u8]>,
    source_kind: SourceNameKind,
) -> Vec<u8> {
    let mut out = b"smartzip-path-v1".to_vec();
    put_field(&mut out, format!("{source_kind:?}").as_bytes());
    for part in parts {
        put_field(&mut out, part.as_bytes());
    }
    put_field(&mut out, if directory { b"directory" } else { b"file" });
    // Raw provenance is retained intact: splitting 0x5c in a multibyte encoding is unsafe.
    if let Some(raw) = raw {
        put_field(&mut out, raw);
    }
    out
}
// ZIP structural separators are ASCII slash. Do not interpret raw 0x5c as a separator.
fn raw_prefixes(entry: &PathEntry, parts: &[String]) -> Option<Vec<Vec<u8>>> {
    if !matches!(
        entry.source_kind,
        SourceNameKind::ZipCentralDirectory | SourceNameKind::ZipUnicodeExtra
    ) || entry.source.contains('\\')
    {
        return None;
    }
    let raw = entry.raw_name.as_deref()?;
    let components: Vec<_> = raw
        .split(|b| *b == b'/')
        .filter(|p| !p.is_empty() && *p != b".")
        .collect();
    if components.len() != parts.len() {
        return None;
    }
    let mut prefix = Vec::new();
    let mut result = Vec::new();
    for component in components {
        if !prefix.is_empty() {
            prefix.push(b'/');
        }
        prefix.extend_from_slice(component);
        result.push(prefix.clone());
    }
    Some(result)
}
fn comparison_key(name: &str, policy: &TargetPathPolicy) -> String {
    let name = if !policy.comparison.normalization_sensitive || policy.mode == PathMode::Portable {
        name.nfc().collect()
    } else {
        name.to_owned()
    };
    if policy.mode == PathMode::Portable {
        // Conservative Unicode caseless screening includes expansions (ß/SS, ligatures,
        // final sigma). Exclusive creation remains the filesystem authority.
        name.to_uppercase().to_lowercase().nfc().collect()
    } else if !policy.comparison.case_sensitive {
        name.to_lowercase()
    } else {
        name
    }
}
pub fn windows_reserved(name: &str) -> bool {
    let base = name
        .split('.')
        .next()
        .unwrap_or_default()
        .trim_end_matches([' ', '.'])
        .to_uppercase();
    matches!(
        base.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) || ["COM", "LPT"].iter().any(|prefix| {
        base.strip_prefix(prefix).is_some_and(|n| {
            matches!(
                n,
                "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
            )
        })
    })
}
fn clean(name: &str, policy: &TargetPathPolicy) -> (String, Vec<PathMappingReason>) {
    let mut reasons = Vec::new();
    let windows = policy.windows_names || policy.mode == PathMode::Portable;
    let mut s: String = name
        .chars()
        .map(|c| {
            if c == '\0'
                || c == '/'
                || c == '\\'
                || (windows && (c < ' ' || "<>:\"|?*".contains(c)))
            {
                '_'
            } else {
                c
            }
        })
        .collect();
    if s != name {
        reasons.push(PathMappingReason::InvalidCharacters);
    }
    if windows {
        let trimmed = s.trim_end_matches([' ', '.']).to_owned();
        if s != trimmed {
            reasons.push(PathMappingReason::TrailingCharacters);
            s = trimmed;
        }
        if windows_reserved(&s) {
            reasons.push(PathMappingReason::ReservedName);
            s.insert(0, '_');
        }
    }
    if s.is_empty() || s == "." || s == ".." {
        s = "_".into();
        reasons.push(PathMappingReason::InvalidCharacters);
    }
    (s, reasons)
}
fn base32(bytes: &[u8], bits: usize) -> String {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz234567";
    let mut out = String::new();
    for start in (0..bits).step_by(5) {
        let mut value = 0;
        for offset in 0..5 {
            let bit = start + offset;
            value = (value << 1)
                | if bit < bits {
                    (bytes[bit / 8] >> (7 - bit % 8)) & 1
                } else {
                    0
                };
        }
        out.push(ALPHABET[value as usize] as char);
    }
    out
}
fn extension(name: &str, is_dir: bool) -> (&str, &str) {
    if !is_dir {
        if let Some(i) = name.rfind('.') {
            if i != 0 {
                return (&name[..i], &name[i..]);
            }
        }
    }
    (name, "")
}
fn prefix_fitting(s: &str, suffix: &str, policy: &TargetPathPolicy) -> String {
    let mut result = String::new();
    for g in s.graphemes(true) {
        let candidate = format!("{result}{g}{suffix}");
        if !policy.fits_component(&candidate) {
            break;
        }
        result.push_str(g);
    }
    result
}
fn candidate(
    name: &str,
    is_dir: bool,
    hash: &[u8; 32],
    bits: usize,
    policy: &TargetPathPolicy,
) -> Result<(String, bool)> {
    let hash = base32(hash, bits);
    if !policy.fits_component(&hash) {
        return Err(failure(
            PathConstraintReason::NameTooLong,
            "component cannot hold a deterministic hash",
            policy,
        ));
    }
    let marker = format!("~{hash}");
    if !policy.fits_component(&marker) {
        return Ok((hash, !extension(name, is_dir).1.is_empty()));
    }
    let (stem, ext) = extension(name, is_dir);
    let first_stem = stem.graphemes(true).next().unwrap_or("");
    let extension_reserve = format!("{first_stem}{marker}");
    let mut kept_ext = prefix_fitting(ext, &extension_reserve, policy);
    if policy.windows_names || policy.mode == PathMode::Portable {
        kept_ext = kept_ext.trim_end_matches([' ', '.']).to_owned();
    }
    let suffix = format!("{marker}{kept_ext}");
    let kept_stem = prefix_fitting(stem, &suffix, policy);
    // Pure hashes avoid a punctuation-only stem when no complete grapheme fits.
    if kept_stem.is_empty() {
        return Ok((hash, !ext.is_empty()));
    }
    Ok((format!("{kept_stem}{suffix}"), kept_ext != ext))
}
pub fn map_component(
    name: &str,
    is_dir: bool,
    identity: &[u8],
    policy: &TargetPathPolicy,
) -> Result<String> {
    let (cleaned, reasons) = clean(name, policy);
    if reasons.is_empty() && policy.fits_component(&cleaned) {
        return Ok(cleaned);
    }
    let mut framed = node_identity(&[name.to_owned()], is_dir, None, SourceNameKind::Decoded);
    put_field(&mut framed, b"layout_identity");
    put_field(&mut framed, identity);
    candidate(
        &cleaned,
        is_dir,
        blake3::hash(&framed).as_bytes(),
        80,
        policy,
    )
    .map(|x| x.0)
}
/// Existing-output collision suffixes reserve all digits and preserve the last extension.
pub fn collision_name(
    name: &str,
    is_dir: bool,
    sequence: usize,
    policy: &TargetPathPolicy,
) -> Result<String> {
    let (cleaned, _) = clean(name, policy);
    let (stem, ext) = extension(&cleaned, is_dir);
    let marker = format!("_collided_{sequence}");
    if !policy.fits_component(&marker) {
        return Err(failure(
            PathConstraintReason::NameTooLong,
            "collision suffix exceeds component budget",
            policy,
        ));
    }
    let mut kept_ext = prefix_fitting(ext, &marker, policy);
    if policy.windows_names || policy.mode == PathMode::Portable {
        kept_ext = kept_ext.trim_end_matches([' ', '.']).to_owned();
    }
    let suffix = format!("{marker}{kept_ext}");
    let mut stem = prefix_fitting(stem, &suffix, policy);
    if stem.is_empty() {
        stem = prefix_fitting("_", &suffix, policy);
    }
    let result = format!("{stem}{suffix}");
    if windows_reserved(&result) || result.is_empty() {
        return Err(failure(
            PathConstraintReason::InvalidName,
            "invalid collision name",
            policy,
        ));
    }
    Ok(result)
}
pub fn refresh_mapping_digest(report: &mut PathMappingReport) {
    // Digest excludes itself and tentative execution state, covers complete final composition.
    report.refresh_digest();
}
pub fn plan_paths(entries: &[PathEntry], policy: &TargetPathPolicy) -> Result<PathMappingReport> {
    plan_paths_with_hash(entries, policy, |bytes| *blake3::hash(bytes).as_bytes())
}
/// Hash injection exists to exercise collision expansion, never used for production randomness.
pub fn plan_paths_with_hash(
    entries: &[PathEntry],
    policy: &TargetPathPolicy,
    hash: impl Fn(&[u8]) -> [u8; 32],
) -> Result<PathMappingReport> {
    plan_paths_internal(entries, policy, hash, &BTreeSet::new())
}
pub fn plan_paths_forced(
    entries: &[PathEntry],
    policy: &TargetPathPolicy,
    force_original_paths: &BTreeSet<String>,
) -> Result<PathMappingReport> {
    plan_paths_internal(
        entries,
        policy,
        |bytes| *blake3::hash(bytes).as_bytes(),
        force_original_paths,
    )
}
fn plan_paths_internal(
    entries: &[PathEntry],
    policy: &TargetPathPolicy,
    hash: impl Fn(&[u8]) -> [u8; 32],
    force_original_paths: &BTreeSet<String>,
) -> Result<PathMappingReport> {
    if entries.len() > 100_000
        || entries
            .iter()
            .try_fold(0usize, |n, e| {
                n.checked_add(e.source.len())
                    .and_then(|n| n.checked_add(e.raw_name.as_ref().map_or(0, Vec::len)))
            })
            .is_none_or(|n| n > 64 * 1024 * 1024)
    {
        return Err(SmartZipError::ResourceLimit {
            detail: "path manifest exceeds entry/name budget".into(),
        });
    }
    let mut ids = BTreeSet::new();
    let mut root = Node {
        directory: true,
        ..Node::default()
    };
    let mut parsed = Vec::with_capacity(entries.len());
    let mut node_count = 0usize;
    let mut identity_bytes = 0usize;
    for (index, entry) in entries.iter().enumerate() {
        if !ids.insert(entry.id) {
            return Err(failure(
                PathConstraintReason::NameCollision,
                "duplicate member identity",
                policy,
            ));
        }
        let parts = if entry.is_dir && is_root_directory_alias(&entry.source) {
            Vec::new()
        } else {
            logical_components(&entry.source)?
        };
        let raw_prefixes = raw_prefixes(entry, &parts);
        let mut current = &mut root;
        for (depth, part) in parts.iter().enumerate() {
            let directory = depth + 1 < parts.len() || entry.is_dir;
            current = current
                .children
                .entry(part.clone())
                .or_insert_with(|| Node {
                    directory,
                    ..{
                        node_count += 1;
                        Node::default()
                    }
                });
            if node_count > 200_000 {
                return Err(SmartZipError::ResourceLimit {
                    detail: "path trie exceeds implicit-node budget".into(),
                });
            }
            if current.directory != directory {
                return Err(failure(
                    PathConstraintReason::NameCollision,
                    "file/directory structural conflict",
                    policy,
                ));
            }
            let identity = node_identity(
                &parts[..=depth],
                directory,
                raw_prefixes
                    .as_ref()
                    .map(|prefixes| prefixes[depth].as_slice())
                    .or_else(|| {
                        if depth + 1 == parts.len() {
                            entry.raw_name.as_deref()
                        } else {
                            None
                        }
                    }),
                entry.source_kind,
            );
            if current.identity.is_empty() || identity < current.identity {
                identity_bytes = identity_bytes - current.identity.len() + identity.len();
                if identity_bytes > 64 * 1024 * 1024 {
                    return Err(SmartZipError::ResourceLimit {
                        detail: "path trie exceeds identity budget".into(),
                    });
                }
                current.identity = identity;
            }
        }
        if !entry.is_dir && !current.entries.is_empty() {
            return Err(failure(
                PathConstraintReason::NameCollision,
                "duplicate_entry: normalized file path",
                policy,
            ));
        }
        current.entries.push(index);
        parsed.push(parts);
    }
    map_children(&mut root, policy, &hash, "", force_original_paths)?;
    shorten_full_paths(&mut root, policy, &hash, force_original_paths)?;
    let mut mapped = BTreeMap::new();
    if !root.entries.is_empty() {
        mapped.insert(String::new(), (String::new(), Vec::new()));
    }
    collect(&root, "", false, &mut mapped)?;
    let mut result = Vec::with_capacity(entries.len());
    for (i, e) in entries.iter().enumerate() {
        let (path, reasons) = mapped
            .get(&parsed[i].join("/"))
            .expect("trie preserves every member");
        if let Some(limit) = policy.full_path_limit {
            let full = format!("{}/{}", policy.target.canonical_root, path);
            let measured = match policy.access {
                PathAccessStrategy::WindowsVerbatim => full.encode_utf16().count() + 64,
                PathAccessStrategy::PosixDirRelative => full.len() + 64,
            };
            if measured > limit {
                let mut d = PathDiagnostic::new(
                    PathConstraintReason::PathTooLong,
                    PathStage::Preflight,
                    "current extraction/scan API chain exceeds full path budget",
                );
                d.scope = "path".into();
                d.entry_id = Some(e.id);
                d.policy = Some(policy.clone());
                d.measured = Some(measured);
                d.limit = Some(limit);
                d.candidate = Some(path.clone());
                return Err(d.error());
            }
        }
        result.push(PathMappingEntry {
            id: e.id,
            source: e.source.clone(),
            display_name: e.source.clone(),
            raw_name: e.raw_name.clone(),
            source_kind: e.source_kind,
            staging_relative: path.clone(),
            final_relative: path.clone(),
            is_dir: e.is_dir,
            reasons: reasons.clone(),
        });
    }
    result.sort_by(|a, b| a.source.cmp(&b.source).then(a.id.cmp(&b.id)));
    let mut report = PathMappingReport {
        version: PATH_MAPPING_VERSION,
        digest: String::new(),
        policy: policy.clone(),
        entries: result,
        tentative: true,
        manifest_digest: String::new(),
        archive_identity: String::new(),
        adapter_id: String::new(),
        archive_path: String::new(),
        node_id: None,
        generation: None,
    };
    refresh_mapping_digest(&mut report);
    Ok(report)
}
fn map_children(
    node: &mut Node,
    policy: &TargetPathPolicy,
    hash: &impl Fn(&[u8]) -> [u8; 32],
    prefix: &str,
    force: &BTreeSet<String>,
) -> Result<()> {
    let mut levels = BTreeMap::<String, usize>::new();
    for (name, child) in &mut node.children {
        let (cleaned, mut reasons) = clean(name, policy);
        if !policy.fits_component(&cleaned) {
            reasons.push(PathMappingReason::NameTooLong);
        }
        let source_path = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        if force.contains(&source_path) {
            reasons.push(PathMappingReason::NameCollision);
        }
        if child.full_shortened {
            reasons.push(PathMappingReason::FullPathShortened);
        }
        child.mapped = cleaned;
        child.reasons = reasons;
        levels.insert(name.clone(), usize::from(!child.reasons.is_empty()));
    }
    // Each node has five states: original, 80, 96, 128, 256 bits. No unbounded retries.
    for _ in 0..6 {
        for (name, child) in &mut node.children {
            let level = levels[name];
            if level > 0 {
                let (cleaned, _) = clean(name, policy);
                let (mapped, extension_shortened) = if child.full_shortened {
                    let mapped = base32(&hash(&child.identity), [80, 96, 128, 256][level - 1]);
                    if !policy.fits_component(&mapped) {
                        return Err(failure(
                            if level > 1 {
                                PathConstraintReason::NameCollision
                            } else {
                                PathConstraintReason::NameTooLong
                            },
                            "full-path shortening hash exceeds component budget",
                            policy,
                        ));
                    }
                    (mapped, !extension(&cleaned, child.directory).1.is_empty())
                } else {
                    candidate(
                        &cleaned,
                        child.directory,
                        &hash(&child.identity),
                        [80, 96, 128, 256][level - 1],
                        policy,
                    )
                    .map_err(|mut error| {
                        if level > 1 {
                            if let SmartZipError::PathConstraint { diagnostic, .. } = &mut error {
                                if diagnostic.reason == PathConstraintReason::NameTooLong {
                                    diagnostic.reason = PathConstraintReason::NameCollision;
                                    diagnostic.detail =
                                        "collision hash expansion cannot fit component budget"
                                            .into();
                                }
                            }
                        }
                        error
                    })?
                };
                child.mapped = mapped;
                if extension_shortened
                    && !child
                        .reasons
                        .contains(&PathMappingReason::ExtensionShortened)
                {
                    child.reasons.push(PathMappingReason::ExtensionShortened);
                }
            }
        }
        let mut groups = BTreeMap::<String, Vec<String>>::new();
        for (name, child) in &node.children {
            groups
                .entry(comparison_key(&child.mapped, policy))
                .or_default()
                .push(name.clone());
        }
        let collisions: Vec<_> = groups.into_values().filter(|g| g.len() > 1).collect();
        if collisions.is_empty() {
            for (name, child) in &mut node.children {
                let source_path = if prefix.is_empty() {
                    name.clone()
                } else {
                    format!("{prefix}/{name}")
                };
                map_children(child, policy, hash, &source_path, force)?;
            }
            return Ok(());
        }
        for group in collisions {
            let all_original = group.iter().all(|n| levels[n] == 0);
            for name in group {
                let level = levels.get_mut(&name).expect("all nodes have hash state");
                if all_original || *level > 0 {
                    if *level == 4 {
                        return Err(failure(
                            PathConstraintReason::NameCollision,
                            "full-length deterministic hash collides",
                            policy,
                        ));
                    }
                    *level += 1;
                    let reasons = &mut node.children.get_mut(&name).unwrap().reasons;
                    if !reasons.contains(&PathMappingReason::NameCollision) {
                        reasons.push(PathMappingReason::NameCollision);
                    }
                }
            }
        }
    }
    Err(failure(
        PathConstraintReason::NameCollision,
        "deterministic collision resolution exhausted",
        policy,
    ))
}
fn shorten_full_paths(
    root: &mut Node,
    policy: &TargetPathPolicy,
    hash: &impl Fn(&[u8]) -> [u8; 32],
    force: &BTreeSet<String>,
) -> Result<()> {
    let Some(limit) = policy.full_path_limit else {
        return Ok(());
    };
    let measure = |name: &str| match policy.access {
        PathAccessStrategy::WindowsVerbatim => name.encode_utf16().count(),
        PathAccessStrategy::PosixDirRelative => name.len(),
    };
    let root_cost = measure(&policy.target.canonical_root) + 65;
    fn visit(
        node: &Node,
        logical: &str,
        mapped: &str,
        ancestors: &mut Vec<(String, usize)>,
        root_cost: usize,
        limit: usize,
        measure: &impl Fn(&str) -> usize,
        selected: &mut BTreeSet<String>,
        first_over: &mut Option<(String, usize)>,
    ) -> bool {
        let mut unshrinkable = false;
        for (name, child) in &node.children {
            let logical = if logical.is_empty() {
                name.clone()
            } else {
                format!("{logical}/{name}")
            };
            let mapped = if mapped.is_empty() {
                child.mapped.clone()
            } else {
                format!("{mapped}/{}", child.mapped)
            };
            let saving = if child.full_shortened {
                0
            } else {
                measure(&child.mapped).saturating_sub(16)
            };
            ancestors.push((logical.clone(), saving));
            let total = root_cost.saturating_add(measure(&mapped));
            if !child.entries.is_empty() && total > limit {
                if first_over.is_none() {
                    *first_over = Some((mapped.clone(), total));
                }
                if let Some((path, _)) = ancestors
                    .iter()
                    .filter(|(_, saving)| *saving > 0)
                    .max_by(|(a, sa), (b, sb)| sa.cmp(sb).then_with(|| b.cmp(a)))
                {
                    selected.insert(path.clone());
                } else {
                    unshrinkable = true;
                }
            }
            unshrinkable |= visit(
                child, &logical, &mapped, ancestors, root_cost, limit, measure, selected,
                first_over,
            );
            ancestors.pop();
        }
        unshrinkable
    }
    fn mark(node: &mut Node, logical: &str, selected: &BTreeSet<String>) {
        for (name, child) in &mut node.children {
            let path = if logical.is_empty() {
                name.clone()
            } else {
                format!("{logical}/{name}")
            };
            if selected.contains(&path) {
                child.full_shortened = true;
            }
            mark(child, &path, selected);
        }
    }
    // Each round shortens at least one available ancestor of every over-budget leaf.
    // Component depth is bounded by parsing; no output ancestor outside this trie is touched.
    for _ in 0..=256 {
        let mut selected = BTreeSet::new();
        let mut first_over = None;
        let unshrinkable = visit(
            root,
            "",
            "",
            &mut Vec::new(),
            root_cost,
            limit,
            &measure,
            &mut selected,
            &mut first_over,
        );
        let Some((candidate, measured)) = first_over else {
            return Ok(());
        };
        if unshrinkable || selected.is_empty() {
            let mut d = PathDiagnostic::new(
                PathConstraintReason::PathTooLong,
                PathStage::Preflight,
                "owned ancestor tree cannot fit the current complete-path API budget",
            );
            d.scope = "path".into();
            d.policy = Some(policy.clone());
            d.measured = Some(measured);
            d.limit = Some(limit);
            d.metric = Some(match policy.access {
                PathAccessStrategy::WindowsVerbatim => LengthMetric::Utf16Units,
                PathAccessStrategy::PosixDirRelative => LengthMetric::Utf8Bytes,
            });
            d.candidate = Some(candidate);
            return Err(d.error());
        }
        mark(root, "", &selected);
        map_children(root, policy, hash, "", force)?;
    }
    Err(failure(
        PathConstraintReason::PathTooLong,
        "owned ancestor shortening exhausted bounded passes",
        policy,
    ))
}
fn collect(
    node: &Node,
    source: &str,
    ancestor_changed: bool,
    out: &mut BTreeMap<String, (String, Vec<PathMappingReason>)>,
) -> Result<()> {
    fn walk(
        node: &Node,
        source: &str,
        mapped: &str,
        ancestor_changed: bool,
        out: &mut BTreeMap<String, (String, Vec<PathMappingReason>)>,
        bytes: &mut usize,
    ) -> Result<()> {
        for (name, child) in &node.children {
            let source = if source.is_empty() {
                name.clone()
            } else {
                format!("{source}/{name}")
            };
            let mapped = if mapped.is_empty() {
                child.mapped.clone()
            } else {
                format!("{mapped}/{}", child.mapped)
            };
            *bytes = bytes
                .saturating_add(source.len())
                .saturating_add(mapped.len());
            if *bytes > 128 * 1024 * 1024 {
                return Err(SmartZipError::ResourceLimit {
                    detail: "complete path report exceeds mapping byte budget".into(),
                });
            }
            let mut reasons = child.reasons.clone();
            if ancestor_changed {
                reasons.push(PathMappingReason::ChangedAncestor);
            }
            out.insert(source.clone(), (mapped.clone(), reasons));
            walk(
                child,
                &source,
                &mapped,
                ancestor_changed || name != &child.mapped,
                out,
                bytes,
            )?;
        }
        Ok(())
    }
    walk(node, source, "", ancestor_changed, out, &mut 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn policy(limit: usize, metric: LengthMetric) -> TargetPathPolicy {
        TargetPathPolicy {
            version: 1,
            mode: PathMode::Native,
            target: TargetIdentity {
                canonical_root: "/output".into(),
                volume_id: "test".into(),
                root_id: "test".into(),
            },
            fs_kind: "test".into(),
            component: ComponentBudget {
                limit,
                metric,
                source: "test".into(),
                confidence: PolicyConfidence::Known,
            },
            access: PathAccessStrategy::PosixDirRelative,
            comparison: NameComparison {
                case_sensitive: true,
                normalization_sensitive: true,
                confidence: PolicyConfidence::Known,
            },
            windows_names: false,
            full_path_limit: None,
        }
    }
    fn file(id: u64, name: &str) -> PathEntry {
        PathEntry {
            id,
            source: name.into(),
            raw_name: None,
            source_kind: SourceNameKind::BackendText,
            is_dir: false,
        }
    }
    #[test]
    fn byte_and_utf16_budgets_and_portable_intersection() {
        let name = format!("{}.txt", "中".repeat(90));
        let bytes = policy(255, LengthMetric::Utf8Bytes);
        let units = policy(255, LengthMetric::Utf16Units);
        assert_ne!(map_component(&name, false, b"id", &bytes).unwrap(), name);
        assert_eq!(map_component(&name, false, b"id", &units).unwrap(), name);
        let mut portable = units;
        portable.mode = PathMode::Portable;
        let mapped = map_component(&name, false, b"id", &portable).unwrap();
        assert!(mapped.len() <= 255);
        assert!(mapped.ends_with(".txt"));
    }
    #[test]
    fn grapheme_clusters_are_not_split_and_extension_is_bounded() {
        let p = policy(32, LengthMetric::Utf8Bytes);
        let g = "👨‍👩‍👧‍👦";
        let mapped = map_component(&format!("{g}{g}.txt"), false, b"id", &p).unwrap();
        assert!(mapped.is_ascii());
        assert!(p.fits_component(&mapped));
        let mapped = map_component(&format!("x.{}", "中".repeat(400)), false, b"id", &p).unwrap();
        assert!(p.fits_component(&mapped));
        let mapped = map_component(&format!("x.{}", "z".repeat(400)), false, b"id", &p).unwrap();
        assert!(mapped.starts_with("x~"));
        assert!(mapped.contains('.'));
        assert!(p.fits_component(&mapped));
        let p = policy(16, LengthMetric::Utf8Bytes);
        let mapped = map_component(&"x".repeat(20), false, b"id", &p).unwrap();
        assert_eq!(mapped.len(), 16);
        assert!(mapped.chars().all(|c| c.is_ascii_alphanumeric()));
        assert!(map_component(
            &"x".repeat(20),
            false,
            b"id",
            &policy(15, LengthMetric::Utf8Bytes)
        )
        .is_err());
    }
    #[test]
    fn windows_reserved_ads_and_trailing_names() {
        let mut p = policy(255, LengthMetric::Utf16Units);
        p.windows_names = true;
        for name in [
            "COM¹.txt",
            "LPT²",
            "nul.ZIP",
            "CON",
            "name:stream",
            "a. ",
            "<>*?.txt",
        ] {
            let mapped = map_component(name, false, name.as_bytes(), &p).unwrap();
            assert_ne!(mapped, name);
            assert!(!windows_reserved(&mapped));
            assert!(!mapped.ends_with([' ', '.']));
            assert!(!mapped.contains([':', '<', '>', '*', '?']));
        }
    }
    #[test]
    fn unsafe_paths_and_structural_duplicates_fail() {
        let p = policy(255, LengthMetric::Utf8Bytes);
        for name in ["/root", "../x", "a/../b", "C:/x", "\\\\server\\x", "a\0b"] {
            assert!(matches!(
                plan_paths(&[file(1, name)], &p),
                Err(SmartZipError::UnsafeArchivePath { .. })
            ));
        }
        for names in [["a", "a/b"], ["a/b", "a"], ["a", "./a"]] {
            assert!(matches!(
                plan_paths(&[file(1, names[0]), file(2, names[1])], &p),
                Err(SmartZipError::PathConstraint { .. })
            ));
        }
    }
    #[test]
    fn conflicting_directories_stay_distinct_and_reordering_is_stable() {
        let mut p = policy(255, LengthMetric::Utf8Bytes);
        p.comparison.case_sensitive = false;
        let entries = vec![file(1, "A/x.txt"), file(2, "a/y.txt")];
        let report = plan_paths(&entries, &p).unwrap();
        assert!(report
            .entries
            .iter()
            .all(|e| e.reasons.contains(&PathMappingReason::ChangedAncestor)));
        assert_ne!(
            report.entries[0].staging_relative.split('/').next(),
            report.entries[1].staging_relative.split('/').next()
        );
        let mut reversed = entries;
        reversed.reverse();
        assert_eq!(report, plan_paths(&reversed, &p).unwrap());
    }
    #[test]
    fn nfc_nfd_collision_and_generated_candidates_are_rechecked() {
        let mut p = policy(255, LengthMetric::Utf8Bytes);
        p.mode = PathMode::Portable;
        let report = plan_paths(&[file(1, "é"), file(2, "e\u{301}")], &p).unwrap();
        assert!(report
            .entries
            .iter()
            .all(|e| e.reasons.contains(&PathMappingReason::NameCollision)));
        let p = policy(32, LengthMetric::Utf8Bytes);
        let long = "x".repeat(100);
        let first = plan_paths(&[file(1, &long)], &p).unwrap().entries[0]
            .staging_relative
            .clone();
        let report = plan_paths(&[file(1, &long), file(2, &first)], &p).unwrap();
        assert_eq!(
            report
                .entries
                .iter()
                .find(|e| e.id == 2)
                .unwrap()
                .staging_relative,
            first
        );
        assert_ne!(
            report.entries[0].staging_relative,
            report.entries[1].staging_relative
        );
    }
    #[test]
    fn artificial_hash_collision_expands_then_fails_without_overwrite() {
        let mut p = policy(255, LengthMetric::Utf8Bytes);
        p.comparison.case_sensitive = false;
        let entries = [file(1, "A"), file(2, "a")];
        let hash = |input: &[u8]| {
            let mut result = [0u8; 32];
            result[11] = if input.contains(&b'A') { 1 } else { 2 };
            result
        };
        let report = plan_paths_with_hash(&entries, &p, hash).unwrap();
        assert!(report
            .entries
            .iter()
            .all(|e| e.staging_relative.split('~').last().unwrap().len() == 20));
        assert!(matches!(
            plan_paths_with_hash(&entries, &p, |_| [0u8; 32]),
            Err(SmartZipError::PathConstraint { .. })
        ));
    }
    #[test]
    fn collision_rename_reserves_suffix_and_extension() {
        let p = policy(32, LengthMetric::Utf8Bytes);
        let name = collision_name(&format!("{}.txt", "x".repeat(30)), false, 1234, &p).unwrap();
        assert!(name.ends_with("_collided_1234.txt"));
        assert!(name.len() <= 32);
    }
    #[test]
    fn raw_multibyte_separator_is_not_split_and_parent_hash_uses_prefix() {
        let long = "中".repeat(100);
        let mut first = file(1, &format!("{long}/a"));
        first.source_kind = SourceNameKind::ZipCentralDirectory;
        first.raw_name = Some([long.as_bytes(), b"/a"].concat());
        let p = policy(255, LengthMetric::Utf8Bytes);
        let before = plan_paths(&[first.clone()], &p).unwrap().entries[0]
            .staging_relative
            .clone();
        let mut second = file(2, &format!("{long}/b"));
        second.source_kind = SourceNameKind::ZipCentralDirectory;
        second.raw_name = Some([long.as_bytes(), b"/b"].concat());
        let after = plan_paths(&[first, second], &p).unwrap();
        assert_eq!(
            before,
            after
                .entries
                .iter()
                .find(|e| e.id == 1)
                .unwrap()
                .staging_relative
        );
        let mut multibyte = file(3, "表/x");
        multibyte.source_kind = SourceNameKind::ZipCentralDirectory;
        multibyte.raw_name = Some(vec![0x95, 0x5c, b'/', b'x']);
        let prefixes =
            raw_prefixes(&multibyte, &logical_components(&multibyte.source).unwrap()).unwrap();
        assert_eq!(prefixes[0], vec![0x95, 0x5c]);
        assert_eq!(prefixes.len(), 2);
    }
    #[test]
    fn alias_feedback_forces_both_implicit_directories() {
        let p = policy(255, LengthMetric::Utf8Bytes);
        let entries = [file(1, "Alpha/x"), file(2, "Beta/y")];
        let forced = BTreeSet::from(["Alpha".to_string(), "Beta".to_string()]);
        let report = plan_paths_forced(&entries, &p, &forced).unwrap();
        assert!(report
            .entries
            .iter()
            .all(|e| e.staging_relative.contains('~')
                && e.reasons.contains(&PathMappingReason::ChangedAncestor)));
    }
    #[test]
    fn portable_conservative_case_expansions_conflict_as_a_group() {
        let mut p = policy(255, LengthMetric::Utf8Bytes);
        p.mode = PathMode::Portable;
        for names in [["Straße", "STRASSE"], ["ς", "σ"], ["ﬀ", "ff"]] {
            let report = plan_paths(&[file(1, names[0]), file(2, names[1])], &p).unwrap();
            assert!(report
                .entries
                .iter()
                .all(|e| e.reasons.contains(&PathMappingReason::NameCollision)));
            assert_ne!(
                report.entries[0].staging_relative,
                report.entries[1].staging_relative
            );
        }
    }
    #[test]
    fn total_path_budget_shortens_owned_ancestors_without_flattening() {
        for access in [
            PathAccessStrategy::PosixDirRelative,
            PathAccessStrategy::WindowsVerbatim,
        ] {
            let mut p = policy(255, LengthMetric::Utf8Bytes);
            p.access = access;
            p.full_path_limit = Some(300);
            let a = "a".repeat(180);
            let b = "中".repeat(50);
            let entries = [
                file(1, &format!("{a}/{b}/leaf.txt")),
                file(2, &format!("{a}/other.txt")),
            ];
            let report = plan_paths(&entries, &p).unwrap();
            let item = report.entries.iter().find(|e| e.id == 1).unwrap();
            assert_eq!(item.staging_relative.split('/').count(), 3);
            assert_eq!(item.staging_relative.split('/').next().unwrap().len(), 16);
            assert!(item.reasons.contains(&PathMappingReason::ChangedAncestor));
            let mut reversed = entries.to_vec();
            reversed.reverse();
            assert_eq!(report, plan_paths(&reversed, &p).unwrap());
        }
    }
    #[test]
    fn total_path_minimum_tree_failure_is_explicit() {
        let mut p = policy(255, LengthMetric::Utf8Bytes);
        p.full_path_limit = Some(100);
        let entries = [file(
            1,
            "a/b/c/d/e/f/g/h/i/j/k/l/m/n/o/p/q/r/s/t/u/v/w/x/y/z/file",
        )];
        match plan_paths(&entries, &p) {
            Err(SmartZipError::PathConstraint { diagnostic, .. }) => {
                assert_eq!(diagnostic.reason, PathConstraintReason::PathTooLong)
            }
            other => panic!("{other:?}"),
        }
    }
    #[test]
    fn hash_collision_with_no_expansion_room_is_collision_error() {
        let mut p = policy(16, LengthMetric::Utf8Bytes);
        p.comparison.case_sensitive = false;
        match plan_paths_with_hash(&[file(1, "A"), file(2, "a")], &p, |_| [0; 32]) {
            Err(SmartZipError::PathConstraint { diagnostic, .. }) => {
                assert_eq!(diagnostic.reason, PathConstraintReason::NameCollision)
            }
            other => panic!("{other:?}"),
        }
    }
    #[test]
    fn layout_component_hash_is_framed_by_domain_kind_and_identity() {
        let p = policy(32, LengthMetric::Utf8Bytes);
        let name = "x".repeat(80);
        let mapped = map_component(&name, false, b"container", &p).unwrap();
        let mut framed = node_identity(&[name.clone()], false, None, SourceNameKind::Decoded);
        put_field(&mut framed, b"layout_identity");
        put_field(&mut framed, b"container");
        assert_eq!(
            mapped,
            candidate(&name, false, blake3::hash(&framed).as_bytes(), 80, &p)
                .unwrap()
                .0
        );
        assert_ne!(
            mapped,
            map_component(&name, true, b"container", &p).unwrap()
        );
        assert_ne!(mapped, map_component(&name, false, b"other", &p).unwrap());
    }
    #[test]
    fn explicit_root_directory_aliases_are_preserved_without_renaming() {
        let p = policy(255, LengthMetric::Utf8Bytes);
        let entries: Vec<_> = [".", "./", "././"]
            .into_iter()
            .enumerate()
            .map(|(i, name)| PathEntry {
                id: i as u64,
                source: name.into(),
                raw_name: Some(name.as_bytes().to_vec()),
                source_kind: SourceNameKind::ZipCentralDirectory,
                is_dir: true,
            })
            .collect();
        let report = plan_paths(&entries, &p).unwrap();
        assert_eq!(report.entries.len(), 3);
        assert_eq!(report.changed_count(), 0);
        for entry in &report.entries {
            assert!(entry.staging_relative.is_empty());
            assert!(entry.final_relative.is_empty());
            assert!(entry.reasons.is_empty());
            assert!(entry.raw_name.is_some());
        }
        for source in [".", "./", "././"] {
            assert!(plan_paths(&[file(42, source)], &p).is_err());
        }
        for source in ["", "/", "/./", "..", "../", "./../", "C:/.", "\\.", "a/."] {
            assert!(!is_root_directory_alias(source));
        }
        let mut reversed = entries;
        reversed.reverse();
        assert_eq!(report, plan_paths(&reversed, &p).unwrap());
    }
}
