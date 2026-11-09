//! One write boundary for names, admission, link safety and the output manifest.
use crate::{member::ReadExtent, types::*};
use smartzip_core::{Result, SmartZipError};
use smartzip_sevenzip::Entry;
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
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
impl Target {
    pub(super) fn check_catalog(&self, id: &str) -> Result<()> {
        let refs = match self {
            Self::Read { entry, .. } => std::slice::from_ref(entry),
            Self::Extract { selection, .. } => selection.as_deref().unwrap_or_default(),
            Self::None => &[],
        };
        if refs.iter().any(|entry| entry.catalog_id != id) {
            return Err(SmartZipError::BackendProtocolError {
                backend: "native-plan".into(),
                detail: "member catalog is stale; list the source again".into(),
            });
        }
        Ok(())
    }
}

pub(super) struct ControlledOutput {
    target: Target,
    budget: Option<Arc<dyn smartzip_core::OutputBudget>>,
    files: BTreeMap<u32, PlannedFile>,
    directories: BTreeMap<PathBuf, Metadata>,
    pub(super) memory: Vec<u8>,
    manifest: OutputManifest,
}

#[derive(Default)]
struct Metadata {
    mode: Option<u32>,
    modified: Option<u64>,
}
struct PlannedFile {
    path: PathBuf,
    metadata: Metadata,
    kind: FileKind,
}
enum FileKind {
    Regular {
        handle: Option<std::fs::File>,
        size: Option<u64>,
    },
    Link {
        hard: bool,
        bytes: Vec<u8>,
        target: Option<String>,
    },
}

fn is_link(entry: &Entry) -> bool {
    entry.link.is_some()
        || entry.hard_link.is_some()
        || entry.unix_mode.is_some_and(|m| m & 0o170000 == 0o120000)
}

impl ControlledOutput {
    pub(super) fn new(
        target: Target,
        budget: Option<Arc<dyn smartzip_core::OutputBudget>>,
    ) -> Self {
        Self {
            target,
            budget,
            files: BTreeMap::new(),
            directories: BTreeMap::new(),
            memory: vec![],
            manifest: OutputManifest::default(),
        }
    }

    pub(super) fn is_extract(&self) -> bool {
        matches!(self.target, Target::Extract { .. })
    }

    pub(super) fn prepare(
        &mut self,
        entries: &[Entry],
        name: impl Fn(&Entry) -> Result<String>,
    ) -> Result<()> {
        let root = match &self.target {
            Target::None => return Ok(()),
            Target::Read { .. } => {
                if entries.iter().any(|entry| entry.is_dir || is_link(entry)) {
                    return Err(SmartZipError::UnsupportedFormat {
                        path: PathBuf::new(),
                        format: Some("directory or link preview".into()),
                    });
                }
                return Ok(());
            }
            Target::Extract { root, .. } => root,
        };

        // Reserve directory names before assigning filenames. Links are not created
        // until all ordinary files are complete and are never used as parents.
        let mut dirs = BTreeSet::new();
        for entry in entries {
            let name = name(entry)?;
            let path = crate::safety::safe_entry_path(name.as_bytes())
                .ok_or_else(|| SmartZipError::UnsafeArchivePath { entry: name })?;
            if entry
                .unix_mode
                .is_some_and(|m| !matches!(m & 0o170000, 0 | 0o040000 | 0o100000 | 0o120000))
            {
                return Err(SmartZipError::UnsafeArchivePath {
                    entry: path.display().to_string(),
                });
            }
            for dir in path.ancestors().skip(usize::from(!entry.is_dir)) {
                if dir.as_os_str().is_empty() || !dirs.insert(dir.to_path_buf()) {
                    break;
                }
            }
            let metadata = Metadata {
                mode: entry.unix_mode,
                modified: entry.modified,
            };
            if entry.is_dir {
                self.directories.insert(path, metadata);
            } else {
                let kind = if is_link(entry) {
                    FileKind::Link {
                        hard: entry.hard_link.is_some(),
                        bytes: Vec::new(),
                        target: entry.hard_link.clone().or_else(|| entry.link.clone()),
                    }
                } else {
                    FileKind::Regular {
                        handle: None,
                        size: None,
                    }
                };
                self.files.insert(
                    entry.index,
                    PlannedFile {
                        path,
                        metadata,
                        kind,
                    },
                );
            }
        }
        let mut used: HashMap<PathBuf, usize> = dirs.iter().map(|dir| (dir.clone(), 2)).collect();
        for file in self.files.values_mut() {
            if let Some(mut suffix) = used.get(&file.path).copied() {
                let candidate = loop {
                    let candidate = duplicate_path(&file.path, suffix);
                    suffix += 1;
                    if !used.contains_key(&candidate) {
                        break candidate;
                    }
                };
                *used.get_mut(&file.path).unwrap() = suffix;
                file.path = candidate;
            }
            used.insert(file.path.clone(), 2);
        }

        std::fs::create_dir_all(root).map_err(|e| SmartZipError::io(Some(root.clone()), e))?;
        for path in dirs {
            admit(&self.budget, 1, 0)?;
            let directory = root.join(&path);
            match std::fs::create_dir(&directory) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    if !std::fs::symlink_metadata(&directory)
                        .map_err(|e| SmartZipError::io(Some(directory.clone()), e))?
                        .is_dir()
                    {
                        return Err(SmartZipError::UnsafeArchivePath {
                            entry: path.display().to_string(),
                        });
                    }
                }
                Err(error) => return Err(SmartZipError::io(Some(directory), error)),
            }
            self.manifest.entries += 1;
        }
        Ok(())
    }

    pub(super) fn write(&mut self, index: u32, bytes: &[u8]) -> Result<bool> {
        match &self.target {
            Target::None => return Err(protocol("unexpected payload in inspect/test")),
            Target::Read { entry, extent } => {
                if index != entry.index {
                    return Err(protocol("unexpected member stream"));
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
                let file = self
                    .files
                    .get_mut(&index)
                    .ok_or_else(|| protocol(format!("unselected output index {index}")))?;
                match &mut file.kind {
                    FileKind::Link { bytes: target, .. } => target.extend_from_slice(bytes),
                    FileKind::Regular { handle, size } => {
                        if handle.is_none() {
                            if size.is_some() {
                                return Err(protocol(format!(
                                    "member stream {index} already closed"
                                )));
                            }
                            admit(&self.budget, 1, bytes.len() as u64)?;
                            *handle = Some(create_file(root, &file.path)?);
                            *size = Some(0);
                            self.manifest.entries += 1;
                        } else {
                            admit(&self.budget, 0, bytes.len() as u64)?;
                        }
                        handle
                            .as_mut()
                            .unwrap()
                            .write_all(bytes)
                            .map_err(|e| SmartZipError::io(Some(root.join(&file.path)), e))?;
                        self.manifest.bytes += bytes.len() as u64;
                        *size.as_mut().unwrap() += bytes.len() as u64;
                    }
                }
            }
        }
        Ok(false)
    }

    pub(super) fn close(&mut self, index: u32) -> Result<()> {
        let Target::Extract { root, .. } = &self.target else {
            return Ok(());
        };
        if let Some(file) = self.files.get_mut(&index) {
            if let FileKind::Regular { handle, .. } = &mut file.kind {
                if let Some(handle) = handle.take() {
                    apply_metadata(&handle, &file.metadata)
                        .map_err(|e| SmartZipError::io(Some(root.join(&file.path)), e))?;
                }
            }
        }
        Ok(())
    }

    pub(super) fn finish(&mut self) -> Result<Option<OutputManifest>> {
        let Target::Extract { root, .. } = &self.target else {
            return Ok(None);
        };
        let mut links = Vec::new();
        for file in std::mem::take(&mut self.files).into_values() {
            let FileKind::Regular { handle, size } = file.kind else {
                links.push(file);
                continue;
            };
            // A successful native item can have no payload (empty file).
            let handle = if size.is_none() {
                admit(&self.budget, 1, 0)?;
                self.manifest.entries += 1;
                Some(create_file(root, &file.path)?)
            } else {
                handle
            };
            if let Some(handle) = handle {
                apply_metadata(&handle, &file.metadata)
                    .map_err(|e| SmartZipError::io(Some(root.join(&file.path)), e))?;
            }
            self.manifest.files.push(OutputFile {
                relative_path: file.path,
                size: size.unwrap_or(0),
            });
        }
        // Hard link sources can only be regular files and previously created
        // hard links; there are no archive-controlled symlinks on disk yet.
        links.sort_by_key(|file| matches!(file.kind, FileKind::Link { hard: false, .. }));
        for file in links {
            let FileKind::Link {
                hard,
                bytes,
                target,
            } = file.kind
            else {
                unreachable!()
            };
            let path = file.path;
            let target = if bytes.is_empty() {
                target.as_deref().unwrap_or("").as_bytes()
            } else {
                &bytes
            };
            let unsafe_path = || SmartZipError::UnsafeArchivePath {
                entry: path.display().to_string(),
            };
            let target = std::str::from_utf8(target).map_err(|_| unsafe_path())?;
            let resolved = link_target(&path, target, hard).ok_or_else(unsafe_path)?;
            let destination = root.join(&path);
            admit(&self.budget, 1, 0)?;
            if hard {
                let source = root.join(resolved);
                let size = std::fs::metadata(&source)
                    .map_err(|e| SmartZipError::io(Some(source.clone()), e))?
                    .len();
                std::fs::hard_link(source, &destination)
                    .map_err(|e| SmartZipError::io(Some(destination), e))?;
                self.manifest.files.push(OutputFile {
                    relative_path: path,
                    size,
                });
            } else {
                let target = relative_link_target(&path, &resolved);
                #[cfg(unix)]
                let result = std::os::unix::fs::symlink(target, &destination);
                #[cfg(windows)]
                let result = std::os::windows::fs::symlink_file(target, &destination);
                result.map_err(|e| SmartZipError::io(Some(destination), e))?;
            }
            self.manifest.entries += 1;
        }

        // Reverse path order puts children before parents. Delay restrictive modes
        // until everything inside the directory has been written.
        #[cfg(unix)]
        for (path, metadata) in self.directories.iter().rev() {
            if metadata.modified.is_none() && metadata.mode.is_none() {
                continue;
            }
            let destination = root.join(path);
            std::fs::File::open(&destination)
                .and_then(|directory| apply_metadata(&directory, metadata))
                .map_err(|e| SmartZipError::io(Some(destination), e))?;
        }
        Ok(Some(std::mem::take(&mut self.manifest)))
    }
}

fn protocol(detail: impl Into<String>) -> SmartZipError {
    SmartZipError::BackendProtocolError {
        backend: "native".into(),
        detail: detail.into(),
    }
}

fn admit(
    budget: &Option<Arc<dyn smartzip_core::OutputBudget>>,
    files: u64,
    bytes: u64,
) -> Result<()> {
    budget
        .as_ref()
        .map_or(Ok(()), |budget| budget.reserve(files, bytes))
}

fn create_file(root: &Path, path: &Path) -> Result<std::fs::File> {
    let destination = root.join(path);
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&destination)
        .map_err(|e| SmartZipError::io(Some(destination), e))
}

fn apply_metadata(file: &std::fs::File, metadata: &Metadata) -> io::Result<()> {
    if let Some(ticks) = metadata.modified {
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
            file.set_times(std::fs::FileTimes::new().set_modified(time))?;
        }
    }
    #[cfg(unix)]
    if let Some(mode) = metadata.mode {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(mode & 0o777))?;
    }
    Ok(())
}

fn duplicate_path(path: &Path, n: usize) -> PathBuf {
    let stem = path.file_stem().unwrap_or_default().to_string_lossy();
    let ext = path
        .extension()
        .map(|s| format!(".{}", s.to_string_lossy()))
        .unwrap_or_default();
    path.with_file_name(format!("{stem} ({n}){ext}"))
}
fn link_target(path: &Path, target: &str, hard: bool) -> Option<PathBuf> {
    let target = crate::safety::archive_separators(target);
    if target.is_empty() || target.starts_with('/') || target.contains('\0') {
        return None;
    }
    let mut resolved = if hard {
        PathBuf::new()
    } else {
        path.parent()?.into()
    };
    let mut first = true;
    for part in target.split('/') {
        match part {
            "" | "." => continue,
            ".." => {
                if !resolved.pop() {
                    return None;
                }
            }
            part if crate::safety::safe_component(part, first) => {
                resolved.push(part);
                first = false;
            }
            _ => return None,
        }
    }
    Some(resolved)
}

// Emit only leading parent components. Keeping interior ".." would let a
// symlink in the target path change the effective meaning of those components.
fn relative_link_target(link: &Path, resolved: &Path) -> PathBuf {
    let parent = link.parent().unwrap();
    let shared = parent
        .components()
        .zip(resolved.components())
        .take_while(|(a, b)| a == b)
        .count();
    let mut target = PathBuf::new();
    for _ in parent.components().skip(shared) {
        target.push("..");
    }
    target.extend(resolved.components().skip(shared));
    if target.as_os_str().is_empty() {
        target.push(".");
    }
    target
}
