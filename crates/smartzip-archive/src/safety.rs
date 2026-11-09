use std::{borrow::Cow, path::PathBuf};

/// Normalize archive separators and reject paths that can escape the output
/// directory when joined. Validate each component before pushing it.
pub fn safe_entry_path(raw_name: &[u8]) -> Option<PathBuf> {
    if raw_name.contains(&0) {
        return None;
    }
    let name = String::from_utf8_lossy(raw_name);
    let normalized = archive_separators(&name);
    if normalized.starts_with('/') {
        return None;
    }
    let mut path = PathBuf::new();
    for part in normalized.split('/') {
        match part {
            "" | "." => {}
            ".." => return None,
            part if !safe_component(part, path.as_os_str().is_empty()) => return None,
            part => path.push(part),
        }
    }
    (!path.as_os_str().is_empty()).then_some(path)
}

pub(crate) fn archive_separators(name: &str) -> Cow<'_, str> {
    if name.contains('\\') {
        Cow::Owned(name.replace('\\', "/"))
    } else {
        Cow::Borrowed(name)
    }
}

pub(crate) fn safe_component(part: &str, first: bool) -> bool {
    // On Windows a colon also addresses an alternate data stream.
    if cfg!(windows) && part.contains(':') {
        return false;
    }
    let bytes = part.as_bytes();
    !(first && bytes.get(1) == Some(&b':') && bytes[0].is_ascii_alphabetic())
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
        assert_eq!(safe_entry_path(b"./Z:folder/file.txt"), None);
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
}
