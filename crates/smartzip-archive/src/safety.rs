use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};

const FALLBACK_COMPONENT_BYTES: usize = 200;

/// Reject Windows drive-letter paths and UNC/network paths in a
/// platform-independent way. Operates on the **normalized** string
/// (backslashes already converted to forward slashes).
///
/// Cases rejected:
/// - `C:\foo`, `C:/foo`, `C:foo` — any `letter:` prefix
/// - `\\server\share` → `//server/share` after normalization
/// - Bare `//` (double slash without scheme, e.g. `//etc/passwd`)
fn has_windows_prefix(s: &str) -> bool {
    let b = s.as_bytes();
    // UNC / double-slash: //server/share (already normalized from \\)
    if b.len() >= 2 && b[0] == b'/' && b[1] == b'/' {
        return true;
    }
    // Drive letter: C: — covers C:\foo, C:/foo, and C:foo (drive-relative)
    if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' {
        return true;
    }
    false
}

/// Validate and sanitize a ZIP entry name into a safe relative path.
///
/// Rejects:
/// - Absolute paths (`/foo`)
/// - Parent directory traversal (`../foo`)
/// - Windows drive paths (`C:\foo`, `C:/foo`, `C:foo`) — all forms
/// - UNC / double-slash paths (`\\server\share`, `//etc/passwd`)
/// - NUL bytes
/// - Empty paths
pub fn safe_entry_path(raw_name: &[u8]) -> Option<PathBuf> {
    if raw_name.contains(&0) {
        return None;
    }
    let name = String::from_utf8_lossy(raw_name);
    let normalized = name.replace('\\', "/");

    if has_windows_prefix(&normalized) {
        return None;
    }

    let mut path = PathBuf::new();
    for component in std::path::Path::new(&normalized).components() {
        match component {
            Component::Normal(part) => path.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    (!path.as_os_str().is_empty()).then_some(path)
}

/// Produce a stable fallback path for a filesystem that rejected an archive
/// entry with `ENAMETOOLONG`.
///
/// Callers must first try the original path. Keeping this conversion on the
/// error path avoids silently renaming valid filenames on filesystems with
/// larger limits.
pub(crate) fn shorten_overlong_components(path: &Path) -> PathBuf {
    path.components()
        .map(|component| match component {
            Component::Normal(part) => shorten_component(part),
            _ => component.as_os_str().to_os_string(),
        })
        .collect()
}

fn shorten_component(component: &OsStr) -> std::ffi::OsString {
    let name = component.to_string_lossy();
    if name.len() <= FALLBACK_COMPONENT_BYTES {
        return component.to_os_string();
    }

    let hash = stable_hash(name.as_bytes());
    let extension = Path::new(name.as_ref())
        .extension()
        .and_then(OsStr::to_str)
        .filter(|extension| extension.len() <= 32)
        .map(|extension| format!(".{extension}"))
        .unwrap_or_default();
    let suffix = format!("~{hash:016x}{extension}");
    let prefix_budget = FALLBACK_COMPONENT_BYTES.saturating_sub(suffix.len());
    let mut prefix_end = prefix_budget.min(name.len());
    while !name.is_char_boundary(prefix_end) {
        prefix_end -= 1;
    }

    format!("{}{}", &name[..prefix_end], suffix).into()
}

fn stable_hash(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_safe_relative_paths() {
        assert_eq!(
            safe_entry_path(b"foo/bar.txt"),
            Some(PathBuf::from("foo/bar.txt"))
        );
        assert_eq!(
            safe_entry_path(b"foo\\bar.txt"),
            Some(PathBuf::from("foo/bar.txt"))
        );
    }

    #[test]
    fn rejects_absolute_paths() {
        assert_eq!(safe_entry_path(b"/etc/passwd"), None);
    }

    #[test]
    fn rejects_parent_traversal() {
        assert_eq!(safe_entry_path(b"../escape.txt"), None);
        assert_eq!(safe_entry_path(b"foo/../../escape.txt"), None);
    }

    #[test]
    fn rejects_nul_bytes() {
        assert_eq!(safe_entry_path(b"foo\0bar.txt"), None);
    }

    #[test]
    fn rejects_empty() {
        assert_eq!(safe_entry_path(b""), None);
        assert_eq!(safe_entry_path(b"/"), None);
    }

    #[test]
    fn rejects_windows_drive_letter_backslash() {
        assert_eq!(safe_entry_path(b"C:\\Windows\\system32\\evil.txt"), None);
        assert_eq!(safe_entry_path(b"D:\\data\\file.txt"), None);
    }

    #[test]
    fn rejects_windows_drive_letter_forward_slash() {
        assert_eq!(safe_entry_path(b"C:/Windows/system32/evil.txt"), None);
        assert_eq!(safe_entry_path(b"D:/data/file.txt"), None);
    }

    #[test]
    fn rejects_windows_drive_relative() {
        assert_eq!(safe_entry_path(b"C:evil.txt"), None);
        assert_eq!(safe_entry_path(b"Z:folder/file.txt"), None);
    }

    #[test]
    fn rejects_unc_paths() {
        assert_eq!(safe_entry_path(b"\\\\server\\share\\file.txt"), None);
        assert_eq!(safe_entry_path(b"\\\\192.168.1.1\\c$\\evil.txt"), None);
    }

    #[test]
    fn rejects_double_slash() {
        assert_eq!(safe_entry_path(b"//etc/passwd"), None);
    }

    #[test]
    fn rejects_backslash_traversal() {
        assert_eq!(safe_entry_path(b"foo\\..\\..\\etc\\passwd"), None);
    }

    #[test]
    fn normal_components_are_not_shortened() {
        let path = Path::new("folder/ordinary-name.txt");
        assert_eq!(shorten_overlong_components(path), path);
    }

    #[test]
    fn overlong_components_are_shortened_stably_and_keep_extension() {
        let name = format!("{}.txt", "文".repeat(100));
        let path = PathBuf::from("folder").join(&name);

        let shortened = shorten_overlong_components(&path);
        let shortened_name = shortened.file_name().unwrap().to_string_lossy();

        assert!(shortened_name.len() <= FALLBACK_COMPONENT_BYTES);
        assert!(shortened_name.ends_with(".txt"));
        assert_eq!(shortened, shorten_overlong_components(&path));
        assert_ne!(shortened, path);
    }

    #[test]
    fn overlong_names_with_same_prefix_do_not_collide() {
        let first = PathBuf::from(format!("{}-first.txt", "a".repeat(240)));
        let second = PathBuf::from(format!("{}-second.txt", "a".repeat(240)));

        assert_ne!(
            shorten_overlong_components(&first),
            shorten_overlong_components(&second)
        );
    }
}
