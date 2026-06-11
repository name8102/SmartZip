use std::path::{Component, PathBuf};

/// Reject Windows drive-letter paths (`C:\...` or `C:/...`) and UNC paths (`\\...`).
/// This check is platform-independent — it operates on the raw string before
/// `Path::components()` normalizes it, so it works on Linux/macOS where
/// `C:foo` would not produce a `Component::Prefix`.
fn has_windows_prefix(s: &str) -> bool {
    let b = s.as_bytes();
    // UNC: \\server\share
    if b.len() >= 2 && b[0] == b'\\' && b[1] == b'\\' {
        return true;
    }
    // Drive letter: C:\ or C:/
    if b.len() >= 3
        && b[0].is_ascii_alphabetic()
        && b[1] == b':'
        && (b[2] == b'/' || b[2] == b'\\')
    {
        return true;
    }
    false
}

/// Validate and sanitize a ZIP entry name into a safe relative path.
///
/// Rejects:
/// - Absolute paths (`/foo`)
/// - Parent directory traversal (`../foo`)
/// - Windows drive prefixes (`C:\foo`, `C:/foo`) — platform-independent
/// - UNC paths (`\\server\share`) — platform-independent
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
    fn rejects_unc_paths() {
        assert_eq!(safe_entry_path(b"\\\\server\\share\\file.txt"), None);
        assert_eq!(safe_entry_path(b"\\\\192.168.1.1\\c$\\evil.txt"), None);
    }

    #[test]
    fn rejects_backslash_traversal() {
        assert_eq!(safe_entry_path(b"foo\\..\\..\\etc\\passwd"), None);
    }
}
