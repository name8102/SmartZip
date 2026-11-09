//! One write boundary for names, admission, link safety and the output manifest.
use crate::{member::ReadExtent, types::*};
use smartzip_core::{Result, SmartZipError};
use smartzip_sevenzip::Entry;
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::Arc,
};

pub(super) enum Target {
    None,
    Extract {
        root: PathBuf,
        selection: Option<Vec<EntryRef>>,
    },
    Read {
        entry: EntryRef,
        extent: ReadExtent,
    },
}
pub(super) struct ControlledOutput {
    target: Target,
    budget: Option<Arc<dyn smartzip_core::OutputBudget>>,
    paths: BTreeMap<u32, PathBuf>,
    metadata: BTreeMap<u32, (Option<u32>, Option<u64>)>,
    handles: BTreeMap<u32, std::fs::File>,
    sizes: BTreeMap<u32, u64>,
    directories: BTreeSet<PathBuf>,
    links: BTreeMap<u32, (PathBuf, bool, Vec<u8>, Option<String>)>,
    pub(super) selection: Option<Vec<u32>>,
    pub(super) memory: Vec<u8>,
    manifest: OutputManifest,
}
impl ControlledOutput {
    pub(super) fn new(
        target: Target,
        budget: Option<Arc<dyn smartzip_core::OutputBudget>>,
    ) -> Self {
        Self {
            target,
            budget,
            paths: Default::default(),
            metadata: Default::default(),
            handles: Default::default(),
            sizes: Default::default(),
            directories: Default::default(),
            links: Default::default(),
            selection: None,
            memory: vec![],
            manifest: Default::default(),
        }
    }
    pub(super) fn prepare(
        &mut self,
        id: &str,
        entries: &[ArchiveEntry],
        native: &[Entry],
    ) -> Result<()> {
        let check = |r: &EntryRef| {
            if r.catalog_id != id || !native.iter().any(|e| e.index == r.index) {
                Err(SmartZipError::BackendProtocolError {
                    backend: "native-plan".into(),
                    detail: "member catalog is stale; list the source again".into(),
                })
            } else {
                Ok(())
            }
        };
        match &self.target {
            Target::None => {}
            Target::Read { entry, .. } => {
                check(entry)?;
                let found = native.iter().find(|e| e.index == entry.index).unwrap();
                if found.is_dir
                    || found.link.is_some()
                    || found.hard_link.is_some()
                    || found.unix_mode.is_some_and(|m| m & 0o170000 == 0o120000)
                {
                    return Err(SmartZipError::UnsupportedFormat {
                        path: PathBuf::new(),
                        format: Some("directory or link preview".into()),
                    });
                }
                self.selection = Some(vec![entry.index]);
            }
            Target::Extract { root, selection } => {
                if let Some(refs) = selection {
                    for r in refs {
                        check(r)?;
                    }
                    self.selection = Some(refs.iter().map(|r| r.index).collect());
                }
                let mut used = BTreeSet::new();
                let mut directories = BTreeSet::new();
                for (entry, raw) in entries.iter().zip(native) {
                    if self
                        .selection
                        .as_ref()
                        .is_some_and(|s| !s.contains(&raw.index))
                    {
                        continue;
                    }
                    let name = entry.path.to_string_lossy();
                    let mut path =
                        crate::safety::safe_entry_path(name.as_bytes()).ok_or_else(|| {
                            SmartZipError::UnsafeArchivePath {
                                entry: name.into_owned(),
                            }
                        })?;
                    if raw.unix_mode.is_some_and(|m| {
                        !matches!(m & 0o170000, 0 | 0o040000 | 0o100000 | 0o120000)
                    }) {
                        return Err(SmartZipError::UnsafeArchivePath {
                            entry: path.display().to_string(),
                        });
                    }
                    if !raw.is_dir {
                        let base = path.clone();
                        let mut n = 2;
                        while used.contains(&path) {
                            path = duplicate_path(&base, n);
                            n += 1;
                        }
                    }
                    used.insert(path.clone());
                    if raw.is_dir {
                        self.directories.insert(path.clone());
                        directories.insert(path.clone());
                    } else if let Some(parent) = path.parent() {
                        if !parent.as_os_str().is_empty() {
                            directories.insert(parent.to_path_buf());
                        }
                    }
                    self.paths.insert(raw.index, path.clone());
                    self.metadata
                        .insert(raw.index, (raw.unix_mode, raw.modified));
                    if raw.link.is_some()
                        || raw.hard_link.is_some()
                        || raw.unix_mode.is_some_and(|m| m & 0o170000 == 0o120000)
                    {
                        self.links.insert(
                            raw.index,
                            (
                                path,
                                raw.hard_link.is_some(),
                                Vec::new(),
                                raw.hard_link.clone().or(raw.link.clone()),
                            ),
                        );
                    }
                }
                // Ancestor lookup is linear in path depth, not quadratic in archive size.
                let non_directories: BTreeSet<_> = self
                    .paths
                    .iter()
                    .filter(|(_, p)| !self.directories.contains(*p))
                    .map(|(_, p)| p.clone())
                    .collect();
                for path in self.paths.values() {
                    if path
                        .ancestors()
                        .skip(1)
                        .any(|parent| non_directories.contains(parent))
                    {
                        return Err(SmartZipError::UnsafeArchivePath {
                            entry: path.display().to_string(),
                        });
                    }
                }
                let mut all_dirs = BTreeSet::new();
                for path in directories {
                    for parent in path.ancestors() {
                        if !parent.as_os_str().is_empty() {
                            all_dirs.insert(parent.to_path_buf());
                        }
                    }
                }
                std::fs::create_dir_all(root)
                    .map_err(|e| SmartZipError::io(Some(root.clone()), e))?;
                for path in all_dirs {
                    self.admit(1, 0)?;
                    let directory = root.join(&path);
                    match std::fs::create_dir(&directory) {
                        Ok(()) => {}
                        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                            let metadata = std::fs::symlink_metadata(&directory)
                                .map_err(|e| SmartZipError::io(Some(directory.clone()), e))?;
                            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                                return Err(SmartZipError::UnsafeArchivePath {
                                    entry: path.display().to_string(),
                                });
                            }
                        }
                        Err(error) => return Err(SmartZipError::io(Some(directory), error)),
                    }
                    self.manifest.entries += 1;
                }
            }
        }
        Ok(())
    }
    fn admit(&self, files: u64, bytes: u64) -> Result<()> {
        if let Some(b) = &self.budget {
            b.reserve(files, bytes)?;
        }
        Ok(())
    }
    pub(super) fn write(&mut self, index: u32, bytes: &[u8]) -> Result<bool> {
        match &self.target {
            Target::None => {
                return Err(SmartZipError::BackendProtocolError {
                    backend: "native".into(),
                    detail: "unexpected payload in inspect/test".into(),
                })
            }
            Target::Read { entry, extent } => {
                if index != entry.index {
                    return Err(SmartZipError::BackendProtocolError {
                        backend: "native".into(),
                        detail: "unexpected member stream".into(),
                    });
                }
                match *extent {
                    ReadExtent::Prefix { bytes: limit } => {
                        let keep = limit.saturating_sub(self.memory.len()).min(bytes.len());
                        self.memory.extend_from_slice(&bytes[..keep]);
                        return Ok(self.memory.len() == limit);
                    }
                    ReadExtent::Complete { max_bytes } => {
                        if max_bytes != 0
                            && bytes.len() > max_bytes.saturating_sub(self.memory.len())
                        {
                            return Err(SmartZipError::ResourceLimit {
                                detail: format!("member exceeds {max_bytes} bytes"),
                            });
                        }
                        self.memory.extend_from_slice(bytes);
                    }
                }
            }
            Target::Extract { root, .. } => {
                let path = self
                    .paths
                    .get(&index)
                    .ok_or_else(|| SmartZipError::BackendProtocolError {
                        backend: "native".into(),
                        detail: format!("unselected output index {index}"),
                    })?
                    .clone();
                if self.links.contains_key(&index) {
                    self.links
                        .get_mut(&index)
                        .unwrap()
                        .2
                        .extend_from_slice(bytes);
                    return Ok(false);
                }
                if !self.handles.contains_key(&index) {
                    self.admit(1, 0)?;
                    let file = std::fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(root.join(&path))
                        .map_err(|e| SmartZipError::io(Some(root.join(&path)), e))?;
                    self.handles.insert(index, file);
                    self.manifest.entries += 1;
                }
                self.admit(0, bytes.len() as u64)?;
                self.handles
                    .get_mut(&index)
                    .unwrap()
                    .write_all(bytes)
                    .map_err(|e| SmartZipError::io(Some(root.join(&path)), e))?;
                self.manifest.bytes += bytes.len() as u64;
                *self.sizes.entry(index).or_default() += bytes.len() as u64;
            }
        }
        Ok(false)
    }
    pub(super) fn close(&mut self, index: u32) -> Result<()> {
        if let Some(mut file) = self.handles.remove(&index) {
            file.flush().map_err(|e| SmartZipError::io(None, e))?;
        }
        Ok(())
    }
    pub(super) fn finish(&mut self) -> Result<Option<OutputManifest>> {
        let Target::Extract { root, .. } = &self.target else {
            return Ok(None);
        };
        // Native GetStream may write no bytes for an empty file; materialize it here.
        for (index, path) in &self.paths {
            if self.links.contains_key(index) || self.directories.contains(path) {
                continue;
            }
            if !self.sizes.contains_key(index) {
                self.admit(1, 0)?;
                let file = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(root.join(path))
                    .map_err(|e| SmartZipError::io(Some(root.join(path)), e))?;
                drop(file);
                self.manifest.entries += 1;
            }
            let size = self.sizes.get(index).copied().unwrap_or(0);
            self.manifest.files.push(OutputFile {
                relative_path: path.clone(),
                size,
            });
        }
        self.handles.clear();
        for (path, hard, bytes, metadata_target) in self.links.values() {
            let target = if bytes.is_empty() {
                metadata_target.as_deref().unwrap_or("").as_bytes()
            } else {
                bytes.as_slice()
            };
            self.admit(1, 0)?;
            let target =
                std::str::from_utf8(target).map_err(|_| SmartZipError::UnsafeArchivePath {
                    entry: path.display().to_string(),
                })?;
            let resolved = link_target(path, target, *hard).ok_or_else(|| {
                SmartZipError::UnsafeArchivePath {
                    entry: path.display().to_string(),
                }
            })?;
            if *hard {
                std::fs::hard_link(root.join(resolved), root.join(path))
                    .map_err(|e| SmartZipError::io(Some(root.join(path)), e))?;
                let size = std::fs::metadata(root.join(path))
                    .map_err(|e| SmartZipError::io(Some(root.join(path)), e))?
                    .len();
                self.manifest.files.push(OutputFile {
                    relative_path: path.clone(),
                    size,
                });
            } else {
                #[cfg(unix)]
                std::os::unix::fs::symlink(target, root.join(path))
                    .map_err(|e| SmartZipError::io(Some(root.join(path)), e))?;
                #[cfg(windows)]
                std::os::windows::fs::symlink_file(target, root.join(path))
                    .map_err(|e| SmartZipError::io(Some(root.join(path)), e))?;
            }
            self.manifest.entries += 1;
        }
        // Apply directory metadata last, so a read-only directory never blocks children.
        let mut metadata: Vec<_> = self
            .paths
            .iter()
            .filter(|(index, _)| !self.links.contains_key(index))
            .collect();
        metadata.sort_by_key(|(_, path)| std::cmp::Reverse(path.components().count()));
        for (index, path) in metadata {
            let destination = root.join(path);
            let (mode, modified) = self.metadata[index];
            if let Some(ticks) = modified {
                let nanos = (ticks as i128 - 116444736000000000i128) * 100;
                let duration = std::time::Duration::new(
                    (nanos.unsigned_abs() / 1_000_000_000) as u64,
                    (nanos.unsigned_abs() % 1_000_000_000) as u32,
                );
                let time = if nanos >= 0 {
                    std::time::UNIX_EPOCH.checked_add(duration)
                } else {
                    std::time::UNIX_EPOCH.checked_sub(duration)
                };
                if let Some(time) = time {
                    let file = if self.directories.contains(path) {
                        #[cfg(unix)]
                        {
                            Some(std::fs::File::open(&destination))
                        }
                        #[cfg(not(unix))]
                        {
                            None
                        }
                    } else {
                        Some(std::fs::OpenOptions::new().write(true).open(&destination))
                    };
                    if let Some(file) = file {
                        file.and_then(|file| {
                            file.set_times(std::fs::FileTimes::new().set_modified(time))
                        })
                        .map_err(|e| SmartZipError::io(Some(destination.clone()), e))?;
                    }
                }
            }
            #[cfg(unix)]
            if let Some(mode) = mode {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(
                    &destination,
                    std::fs::Permissions::from_mode(mode & 0o777),
                )
                .map_err(|e| SmartZipError::io(Some(destination.clone()), e))?;
            }
            #[cfg(not(unix))]
            let _ = mode;
        }
        Ok(Some(std::mem::take(&mut self.manifest)))
    }
}
fn duplicate_path(path: &Path, n: u32) -> PathBuf {
    let stem = path.file_stem().unwrap_or_default().to_string_lossy();
    let ext = path
        .extension()
        .map(|s| format!(".{}", s.to_string_lossy()))
        .unwrap_or_default();
    path.with_file_name(format!("{stem} ({n}){ext}"))
}
fn link_target(path: &Path, target: &str, hard: bool) -> Option<PathBuf> {
    if target.starts_with('/')
        || target.starts_with('\\')
        || target.contains('\0')
        || target.as_bytes().get(1) == Some(&b':')
    {
        return None;
    }
    let mut resolved = if hard {
        PathBuf::new()
    } else {
        path.parent()?.into()
    };
    for part in target.replace('\\', "/").split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if !resolved.pop() {
                    return None;
                }
            }
            s => resolved.push(s),
        }
    }
    Some(resolved)
}
