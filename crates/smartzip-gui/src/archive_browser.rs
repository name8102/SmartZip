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
    directories: BTreeMap<Vec<String>, Vec<ArchiveChild>>,
}

impl ArchiveBrowser {
    pub(crate) fn new(entries: Vec<ArchiveEntry>) -> Result<Self, String> {
        for entry in &entries {
            if entry.is_dir && matches!(entry.path.as_str(), "." | "./") {
                continue;
            }
            validate_member_path(&entry.path)?;
        }
        type Siblings = BTreeMap<(String, bool), Vec<ArchiveChild>>;
        let mut directories: BTreeMap<Vec<String>, Siblings> = BTreeMap::new();
        for entry in entries {
            let Some(parts) = member_parts(&entry.path) else {
                continue;
            };
            for depth in 0..parts.len() {
                let directory = parts[..depth].iter().map(|s| s.to_string()).collect();
                let name = parts[depth].to_owned();
                let is_dir = depth + 1 < parts.len() || entry.is_dir;
                let path = if is_dir {
                    parts[..=depth].join("/")
                } else {
                    entry.path.clone()
                };
                let siblings = directories
                    .entry(directory)
                    .or_default()
                    .entry((name.clone(), is_dir))
                    .or_default();
                if !is_dir || siblings.is_empty() {
                    siblings.push(ArchiveChild {
                        path,
                        name,
                        is_dir,
                        size: if is_dir { None } else { entry.size },
                    });
                }
            }
        }
        Ok(Self {
            directories: directories
                .into_iter()
                .map(|(path, grouped)| {
                    let mut children: Vec<_> = grouped.into_values().flatten().collect();
                    children.sort_by_key(|child| (!child.is_dir, child.name.to_lowercase()));
                    (path, children)
                })
                .collect(),
        })
    }
    pub(crate) fn children(&self, directory: &[String]) -> Vec<ArchiveChild> {
        self.directories.get(directory).cloned().unwrap_or_default()
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
