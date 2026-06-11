use std::path::{Component, PathBuf};

/// Validate and sanitize a ZIP entry name into a safe relative path.
///
/// Rejects:
/// - Absolute paths (`/foo`)
/// - Parent directory traversal (`../foo`)
/// - Windows drive prefixes (`C:\foo`)
/// - UNC paths (`\\server\share`)
/// - NUL bytes
/// - Empty paths
pub fn safe_entry_path(raw_name: &[u8]) -> Option<PathBuf> {
    if raw_name.contains(&0) {
        return None;
    }
    let name = String::from_utf8_lossy(raw_name);
    let normalized = name.replace('\\', "/");
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
}
