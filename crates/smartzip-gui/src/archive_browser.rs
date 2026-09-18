use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ArchiveEntry {
    pub(crate) path: String,
    pub(crate) is_dir: bool,
    pub(crate) size: Option<u64>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ArchiveChild {
    pub(crate) path: String,
    pub(crate) name: String,
    pub(crate) is_dir: bool,
    pub(crate) size: Option<u64>,
}
#[derive(Clone, Debug, Default)]
pub(crate) struct ArchiveBrowser {
    entries: Vec<ArchiveEntry>,
}

impl ArchiveBrowser {
    pub(crate) fn new(entries: Vec<ArchiveEntry>) -> Result<Self, String> {
        for entry in &entries {
            if entry.is_dir && matches!(entry.path.as_str(), "." | "./") {
                continue;
            }
            validate_member_path(&entry.path)?;
        }
        Ok(Self { entries })
    }
    pub(crate) fn children(&self, directory: &[String]) -> Vec<ArchiveChild> {
        let mut grouped: BTreeMap<String, Vec<ArchiveChild>> = BTreeMap::new();
        for entry in &self.entries {
            let Some(parts) = member_parts(&entry.path) else {
                continue;
            };
            if parts.len() <= directory.len() || parts[..directory.len()] != directory[..] {
                continue;
            }
            let name = parts[directory.len()].to_owned();
            let child_path = parts[..directory.len() + 1].join("/");
            let is_dir = parts.len() > directory.len() + 1 || entry.is_dir;
            let child = ArchiveChild {
                path: if is_dir {
                    child_path.clone()
                } else {
                    entry.path.clone()
                },
                name,
                is_dir,
                size: (!is_dir).then_some(entry.size).flatten(),
            };
            let same = grouped.entry(child_path).or_default();
            if child.is_dir {
                if !same.iter().any(|existing| existing.is_dir) {
                    same.push(child);
                }
            } else {
                same.push(child);
            }
        }
        let mut children: Vec<_> = grouped.into_values().flatten().collect();
        children.sort_by_key(|child| (!child.is_dir, child.name.to_ascii_lowercase()));
        children
    }
    pub(crate) fn path_parts(path: &str) -> Option<Vec<String>> {
        member_parts(path).map(|parts| parts.into_iter().map(str::to_owned).collect())
    }
}

fn validate_member_path(path: &str) -> Result<(), String> {
    if member_parts(path).is_some() {
        Ok(())
    } else {
        Err(format!("unsafe archive member path: {path:?}"))
    }
}
fn member_parts(path: &str) -> Option<Vec<&str>> {
    if path.is_empty() || path.contains('\0') || path.contains('\\') {
        return None;
    }
    let mut path = path;
    while let Some(stripped) = path.strip_prefix("./") {
        path = stripped;
    }
    let path = path.strip_suffix('/').unwrap_or(path);
    if path.is_empty() || path.starts_with('/') {
        return None;
    }
    let parts: Vec<_> = path.split('/').collect();
    if parts.first().is_some_and(|part| part.contains(':'))
        || parts
            .iter()
            .any(|part| part.is_empty() || *part == ".." || *part == ".")
    {
        return None;
    }
    Some(parts)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn entry(path: &str, is_dir: bool) -> ArchiveEntry {
        ArchiveEntry {
            path: path.into(),
            is_dir,
            size: Some(7),
        }
    }
    #[test]
    fn shows_direct_children_with_implicit_directories_first() {
        let browser = ArchiveBrowser::new(vec![
            entry("z.txt", false),
            entry("nested/file.txt", false),
            entry("nested/deep/image.png", false),
            entry("empty/", true),
        ])
        .unwrap();
        let root = browser.children(&[]);
        assert_eq!(
            root.iter()
                .map(|child| child.name.as_str())
                .collect::<Vec<_>>(),
            ["empty", "nested", "z.txt"]
        );
        assert_eq!(
            browser.children(&["nested".into()])[1].path,
            "nested/file.txt"
        );
    }
    #[test]
    fn rejects_unsafe_paths_without_normalizing_them() {
        for path in [
            "../secret",
            "/absolute",
            "a/../b",
            "a\\b",
            "a//b",
            "",
            "C:/archive.txt",
        ] {
            assert!(
                ArchiveBrowser::new(vec![entry(path, false)]).is_err(),
                "{path}"
            );
        }
    }
    #[test]
    fn accepts_tar_dot_prefix_and_preserves_file_path() {
        let browser = ArchiveBrowser::new(vec![
            entry("./", true),
            entry("./folder/file name.txt", false),
        ])
        .unwrap();
        assert_eq!(browser.children(&[])[0].name, "folder");
        assert_eq!(
            browser.children(&["folder".into()])[0].path,
            "./folder/file name.txt"
        );
    }
    #[test]
    fn merges_directory_and_preserves_duplicate_files() {
        let browser = ArchiveBrowser::new(vec![
            entry("same", false),
            entry("same", false),
            entry("same/child", false),
        ])
        .unwrap();
        assert_eq!(browser.children(&[]).len(), 3);
        assert!(browser.children(&[]).iter().any(|child| child.is_dir));
        assert_eq!(browser.children(&["same".into()])[0].path, "same/child");
    }
}
