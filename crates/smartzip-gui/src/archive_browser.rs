use std::{collections::BTreeMap, sync::Arc};

#[derive(Debug)]
pub(crate) struct ArchiveView {
    pub(crate) input: smartzip_core::ArchiveInput,
    pub(crate) path: std::path::PathBuf,
    pub(crate) adapter: String,
    pub(crate) router: smartzip_archive::BackendRouter,
    pub(crate) browser: ArchiveBrowser,
    pub(crate) count: usize,
}

impl ArchiveView {
    pub(crate) fn new(
        result: smartzip_engine::ListArchiveResult,
        router: smartzip_archive::BackendRouter,
    ) -> Self {
        use smartzip_core::{RouteEvent, TaskEventKind};
        let adapter = result
            .events
            .into_iter()
            .rev()
            .find_map(|event| match event.kind {
                TaskEventKind::Route(RouteEvent::BackendSelected { adapter_id }) => {
                    Some(adapter_id)
                }
                _ => None,
            })
            .unwrap_or_default();
        let count = result.entries.len();
        let browser = ArchiveBrowser::new(result.entries.into_iter().map(|entry| {
            ArchiveEntry {
                path: entry
                    .path
                    .into_os_string()
                    .into_string()
                    .unwrap_or_else(|path| path.to_string_lossy().into_owned()),
                reference: entry.native.map(|native| native.reference),
                is_dir: entry.is_dir,
                size: entry.uncompressed_size,
            }
        }));
        Self {
            input: result.input,
            path: result.path,
            adapter,
            router,
            browser,
            count,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ArchiveEntry {
    pub(crate) path: String,
    pub(crate) reference: Option<smartzip_archive::EntryRef>,
    pub(crate) is_dir: bool,
    pub(crate) size: Option<u64>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ArchiveChild {
    pub(crate) path: String,
    pub(crate) reference: Option<smartzip_archive::EntryRef>,
    pub(crate) name: String,
    pub(crate) is_dir: bool,
    pub(crate) size: Option<u64>,
}
#[derive(Clone, Debug, Default)]
pub(crate) struct ArchiveBrowser {
    directories: BTreeMap<Vec<String>, Arc<[ArchiveChild]>>,
}

impl ArchiveBrowser {
    pub(crate) fn new(entries: impl IntoIterator<Item = ArchiveEntry>) -> Self {
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
                        reference: if is_dir {
                            None
                        } else {
                            entry.reference.clone()
                        },
                        name,
                        is_dir,
                        size: if is_dir { None } else { entry.size },
                    });
                }
            }
        }
        Self {
            directories: directories
                .into_iter()
                .map(|(path, grouped)| {
                    let mut children: Vec<_> = grouped.into_values().flatten().collect();
                    children.sort_by_cached_key(|child| (!child.is_dir, child.name.to_lowercase()));
                    (path, children.into())
                })
                .collect(),
        }
    }
    pub(crate) fn children(&self, directory: &[String]) -> Arc<[ArchiveChild]> {
        self.directories.get(directory).cloned().unwrap_or_default()
    }
    pub(crate) fn path_parts(path: &str) -> Option<Vec<String>> {
        member_parts(path).map(|parts| parts.into_iter().map(str::to_owned).collect())
    }
}

// This is a read-only display hierarchy, not a filesystem path. Keep the original
// display path on files; reads use the catalog reference rather than this label.
fn member_parts(path: &str) -> Option<Vec<&str>> {
    let parts: Vec<_> = path
        .split('/')
        .filter(|part| !part.is_empty() && *part != ".")
        .collect();
    (!parts.is_empty()).then_some(parts)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn entry(path: &str, is_dir: bool) -> ArchiveEntry {
        ArchiveEntry {
            path: path.into(),
            reference: None,
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
        ]);
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
    fn read_only_browser_preserves_unusual_display_paths() {
        for path in [
            "../secret",
            "/absolute",
            "a/../b",
            "a\\b",
            "a//b",
            "C:/archive.txt",
        ] {
            let browser = ArchiveBrowser::new(vec![entry(path, false)]);
            let parts = ArchiveBrowser::path_parts(path).unwrap();
            assert_eq!(browser.children(&parts[..parts.len() - 1])[0].path, path);
        }
    }
    #[test]
    fn accepts_tar_dot_prefix_and_preserves_file_path() {
        let browser = ArchiveBrowser::new(vec![
            entry("./", true),
            entry("./folder/file name.txt", false),
        ]);
        assert_eq!(browser.children(&[])[0].name, "folder");
        assert_eq!(
            browser.children(&["folder".into()])[0].path,
            "./folder/file name.txt"
        );
    }
    #[test]
    fn duplicate_names_keep_distinct_catalog_indices() {
        let mut first = entry("same", false);
        first.reference = Some(smartzip_archive::EntryRef {
            catalog_id: "catalog".into(),
            index: 1,
        });
        let mut second = first.clone();
        second.reference.as_mut().unwrap().index = 4;
        let browser = ArchiveBrowser::new(vec![first, second]);
        let children = browser.children(&[]);
        assert_eq!(children.len(), 2);
        assert_eq!(children[0].reference.as_ref().unwrap().index, 1);
        assert_eq!(children[1].reference.as_ref().unwrap().index, 4);
    }
    #[test]
    fn merges_directory_and_preserves_duplicate_files() {
        let browser = ArchiveBrowser::new(vec![
            entry("same", false),
            entry("same", false),
            entry("same/child", false),
        ]);
        assert_eq!(browser.children(&[]).len(), 3);
        assert!(browser.children(&[]).iter().any(|child| child.is_dir));
        assert_eq!(browser.children(&["same".into()])[0].path, "same/child");
    }
}
