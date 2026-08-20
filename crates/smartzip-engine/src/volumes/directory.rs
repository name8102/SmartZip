use std::collections::HashMap;
use std::path::{Path, PathBuf};
use unicode_normalization::UnicodeNormalization;

use super::sequence::OrdinalToken;

#[derive(Debug, Clone)]
pub struct DirectoryFile {
    pub path: PathBuf,
    pub normalized_name: String,
    pub filename_ordinals: Vec<OrdinalToken>,
}

#[derive(Debug, Clone)]
pub struct DirectoryVolumeIndex {
    pub directory: PathBuf,
    pub files: Vec<DirectoryFile>,
    // Cache normalized analysis strings, original PathBuf unchanged.
}

impl DirectoryVolumeIndex {
    pub fn from_directory(dir: &Path) -> std::io::Result<Self> {
        let mut files = Vec::new();
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            let meta = std::fs::symlink_metadata(&path)?;
            if !meta.is_file() {
                continue;
            }
            let file_name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default();
            let normalized = file_name.nfkc().collect::<String>();
            let ordinals = super::sequence::parse_ordinal_tokens(&normalized);
            files.push(DirectoryFile {
                path,
                normalized_name: normalized,
                filename_ordinals: ordinals,
            });
        }
        // Deterministic order for tests
        files.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(Self {
            directory: dir.to_path_buf(),
            files,
        })
    }

    pub fn find_file(&self, path: &Path) -> Option<&DirectoryFile> {
        self.files.iter().find(|f| f.path == path)
    }
}

/// Task-scoped cache to avoid repeated read_dir for same directory.
#[derive(Debug, Default)]
pub struct DirectoryIndexCache {
    cache: HashMap<PathBuf, DirectoryVolumeIndex>,
}

impl DirectoryIndexCache {
    pub fn get_or_index(&mut self, dir: &Path) -> std::io::Result<&DirectoryVolumeIndex> {
        let key = dir.to_path_buf();
        if !self.cache.contains_key(&key) {
            let idx = DirectoryVolumeIndex::from_directory(dir)?;
            self.cache.insert(key.clone(), idx);
        }
        Ok(self.cache.get(&key).unwrap())
    }

    pub fn get(&self, dir: &Path) -> Option<&DirectoryVolumeIndex> {
        self.cache.get(dir)
    }
}
