//! Actual-directory naming evidence and a no-follow, exclusive controlled writer.
use smartzip_core::path_policy::*;
use smartzip_core::{Result, SmartZipError};
use std::path::{Path, PathBuf};

fn io_failure(stage: PathStage, relative: &str, error: std::io::Error) -> SmartZipError {
    let reason = match error.raw_os_error() {
        #[cfg(unix)]
        Some(libc::ENAMETOOLONG) => PathConstraintReason::NameTooLong,
        #[cfg(unix)]
        Some(libc::EEXIST) => PathConstraintReason::NameCollision,
        #[cfg(unix)]
        Some(libc::ELOOP) => PathConstraintReason::InvalidName,
        _ => PathConstraintReason::PathConstraintUnknown,
    };
    let mut diagnostic =
        PathDiagnostic::new(reason, stage, "controlled filesystem operation failed");
    diagnostic.candidate = Some(relative.to_owned());
    diagnostic.io_error(error)
}
fn annotate_error(mut error: SmartZipError, report: &PathMappingReport) -> SmartZipError {
    if let SmartZipError::PathConstraint { diagnostic, .. } = &mut error {
        diagnostic.policy = Some(report.policy.clone());
        if let Some(candidate) = &diagnostic.candidate {
            if let Some(entry) = report.entries.iter().find(|entry| {
                entry.staging_relative == *candidate
                    || entry
                        .staging_relative
                        .strip_prefix(candidate)
                        .is_some_and(|suffix| suffix.starts_with('/'))
            }) {
                diagnostic.entry_id = Some(entry.id);
                diagnostic.display_name = Some(entry.display_name.clone());
                diagnostic.raw_name = entry.raw_name.clone();
                diagnostic.source_kind = Some(entry.source_kind);
            }
            if diagnostic.scope == "component" {
                let name = candidate.rsplit('/').next().unwrap_or(candidate);
                diagnostic.component_index = Some(candidate.split('/').count().saturating_sub(1));
                diagnostic.measured = Some(report.policy.measure(name));
                diagnostic.limit = Some(report.policy.component.limit);
                diagnostic.metric = Some(report.policy.component.metric);
            }
        }
    }
    error
}
fn validate_relative(relative: &str) -> Result<Vec<&str>> {
    if relative.is_empty()
        || relative.contains(['\0', '\\'])
        || relative.starts_with('/')
        || relative
            .split('/')
            .any(|s| s.is_empty() || s == "." || s == "..")
        || relative.as_bytes().get(1) == Some(&b':')
    {
        return Err(SmartZipError::UnsafeArchivePath {
            entry: relative.into(),
        });
    }
    Ok(relative.split('/').collect())
}
/// This guard reflects current scans, budgets and external processes, not the writer's deeper capability.
pub fn validate_path_budget(root: &Path, report: &PathMappingReport) -> Result<()> {
    if let Some(limit) = report.policy.full_path_limit {
        for entry in &report.entries {
            let full = root.join(&entry.staging_relative);
            let measured = match report.policy.access {
                PathAccessStrategy::WindowsVerbatim => {
                    full.as_os_str().to_string_lossy().encode_utf16().count() + 16
                }
                PathAccessStrategy::PosixDirRelative => {
                    #[cfg(unix)]
                    {
                        use std::os::unix::ffi::OsStrExt;
                        full.as_os_str().as_bytes().len() + 1
                    }
                    #[cfg(not(unix))]
                    {
                        full.to_string_lossy().len() + 1
                    }
                }
            };
            if measured > limit {
                let mut d = PathDiagnostic::new(
                    PathConstraintReason::PathTooLong,
                    PathStage::Preflight,
                    "path exceeds current full-path API boundary",
                );
                d.scope = "path".into();
                d.entry_id = Some(entry.id);
                d.measured = Some(measured);
                d.limit = Some(limit);
                d.policy = Some(report.policy.clone());
                d.candidate = Some(entry.staging_relative.clone());
                return Err(d.error());
            }
        }
    }
    Ok(())
}
fn recovery_directories(directories: &[String]) -> Result<Vec<String>> {
    let mut all = std::collections::BTreeSet::new();
    for relative in directories {
        if relative.is_empty() {
            continue;
        }
        let parts = validate_relative(relative)?;
        for count in 1..=parts.len() {
            all.insert(parts[..count].join("/"));
        }
        if all.len() > 200_000 {
            return Err(SmartZipError::ResourceLimit {
                detail: "staging recovery exceeds directory budget".into(),
            });
        }
    }
    let mut all: Vec<_> = all.into_iter().collect();
    all.sort_by(|a, b| {
        a.split('/')
            .count()
            .cmp(&b.split('/').count())
            .then(a.cmp(b))
    });
    Ok(all)
}

/// Restore ordinary permissions and a supported modification timestamp through the held file.
/// Privileged bits are intentionally masked. Unsupported timestamp ranges are explicit errors.
pub fn apply_file_metadata(
    file: &std::fs::File,
    mode: Option<u32>,
    modified_unix: Option<i64>,
) -> Result<()> {
    #[cfg(unix)]
    if let Some(mode) = mode {
        use std::os::fd::AsRawFd;
        if unsafe { libc::fchmod(file.as_raw_fd(), (mode & 0o777) as libc::mode_t) } != 0 {
            return Err(io_failure(
                PathStage::Create,
                "metadata",
                std::io::Error::last_os_error(),
            ));
        }
    }
    #[cfg(not(unix))]
    let _ = mode;
    if let Some(seconds) = modified_unix {
        let duration = std::time::Duration::from_secs(seconds.unsigned_abs());
        let time = if seconds >= 0 {
            std::time::UNIX_EPOCH.checked_add(duration)
        } else {
            std::time::UNIX_EPOCH.checked_sub(duration)
        }
        .ok_or_else(|| {
            PathDiagnostic::new(
                PathConstraintReason::PathRemapUnsupported,
                PathStage::Create,
                "timestamp outside platform range",
            )
            .error()
        })?;
        file.set_times(std::fs::FileTimes::new().set_modified(time))
            .map_err(|e| io_failure(PathStage::Create, "metadata", e))?;
    }
    Ok(())
}
#[cfg(unix)]
mod native {
    use super::*;
    use std::collections::{BTreeMap, BTreeSet};
    use std::ffi::CString;
    use std::fs::File;
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::MetadataExt;
    use std::sync::Mutex;
    type Identity = (u64, u64);
    fn identity(file: &File) -> std::io::Result<Identity> {
        let m = file.metadata()?;
        Ok((m.dev(), m.ino()))
    }
    // libc stat field widths and signedness vary across supported Unix ABIs.
    #[allow(clippy::unnecessary_cast)]
    fn stat_identity(stat: &libc::stat) -> Identity {
        (stat.st_dev as u64, stat.st_ino as u64)
    }
    fn cstr(s: &str) -> std::io::Result<CString> {
        CString::new(s)
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "NUL in component"))
    }
    fn open_dir(parent: &File, name: &str) -> std::io::Result<File> {
        let name = cstr(name)?;
        // SAFETY: name is a live NUL-terminated component, parent is a live directory FD.
        let fd = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(unsafe { File::from_raw_fd(fd) })
        }
    }
    #[cfg(target_os = "linux")]
    unsafe fn errno_ptr() -> *mut libc::c_int {
        unsafe { libc::__errno_location() }
    }
    #[cfg(any(target_os = "macos", target_os = "freebsd", target_os = "ios"))]
    unsafe fn errno_ptr() -> *mut libc::c_int {
        unsafe { libc::__error() }
    }
    #[cfg(not(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "freebsd",
        target_os = "ios"
    )))]
    unsafe fn errno_ptr() -> *mut libc::c_int {
        unsafe { libc::__errno_location() }
    }
    pub struct ControlledRoot {
        root: File,
        canonical: PathBuf,
        id: Identity,
        directories: Mutex<BTreeMap<String, Identity>>,
        files: Mutex<BTreeMap<String, Identity>>,
    }
    impl ControlledRoot {
        pub fn open(root: impl AsRef<Path>) -> Result<Self> {
            let canonical = std::fs::canonicalize(root.as_ref())
                .map_err(|e| SmartZipError::io(Some(root.as_ref().to_path_buf()), e))?;
            let c = CString::new(canonical.as_os_str().as_bytes()).map_err(|_| {
                SmartZipError::UnsafeArchivePath {
                    entry: canonical.display().to_string(),
                }
            })?;
            // The user-selected root is resolved once, then all archive access is FD-relative.
            let fd = unsafe {
                libc::open(
                    c.as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                )
            };
            if fd < 0 {
                return Err(io_failure(
                    PathStage::Preflight,
                    "",
                    std::io::Error::last_os_error(),
                ));
            }
            let file = unsafe { File::from_raw_fd(fd) };
            let id = identity(&file).map_err(|e| io_failure(PathStage::Preflight, "", e))?;
            Ok(Self {
                root: file,
                canonical,
                id,
                directories: Mutex::new(BTreeMap::new()),
                files: Mutex::new(BTreeMap::new()),
            })
        }
        pub fn canonical_root(&self) -> &Path {
            &self.canonical
        }
        pub fn verify_identity(&self, policy: &TargetPathPolicy) -> Result<()> {
            let reopened = Self::open(&self.canonical)?;
            if reopened.id != self.id
                || self.id.0.to_string() != policy.target.volume_id
                || format!("{}:{}", self.id.0, self.id.1) != policy.target.root_id
            {
                return Err(PathDiagnostic::new(
                    PathConstraintReason::PathConstraintUnknown,
                    PathStage::Commit,
                    "target root/volume identity changed",
                )
                .error());
            }
            Ok(())
        }
        fn parent(&self, parts: &[&str]) -> Result<File> {
            let mut dir = self
                .root
                .try_clone()
                .map_err(|e| io_failure(PathStage::Create, "", e))?;
            let known = self.directories.lock().map_err(|_| {
                PathDiagnostic::new(
                    PathConstraintReason::PathConstraintUnknown,
                    PathStage::Create,
                    "directory identity lock poisoned",
                )
                .error()
            })?;
            let mut relative = String::new();
            for component in parts {
                if !relative.is_empty() {
                    relative.push('/');
                }
                relative.push_str(component);
                let next = open_dir(&dir, component)
                    .map_err(|e| io_failure(PathStage::Create, &relative, e))?;
                if known.get(&relative)
                    != Some(
                        &identity(&next)
                            .map_err(|e| io_failure(PathStage::Create, &relative, e))?,
                    )
                {
                    return Err(PathDiagnostic::new(
                        PathConstraintReason::InvalidName,
                        PathStage::Create,
                        "untracked or replaced parent directory",
                    )
                    .error());
                }
                dir = next;
            }
            Ok(dir)
        }
        pub fn create_dir(&self, relative: &str) -> Result<()> {
            let parts = validate_relative(relative)?;
            let parent = self.parent(&parts[..parts.len() - 1])?;
            let name = cstr(parts[parts.len() - 1])
                .map_err(|e| io_failure(PathStage::Create, relative, e))?;
            let rc = unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o700) };
            if rc != 0 {
                let err = std::io::Error::last_os_error();
                if err.raw_os_error() != Some(libc::EEXIST) {
                    return Err(io_failure(PathStage::Create, relative, err));
                }
                let dir = open_dir(&parent, parts[parts.len() - 1])
                    .map_err(|e| io_failure(PathStage::Create, relative, e))?;
                let found =
                    identity(&dir).map_err(|e| io_failure(PathStage::Create, relative, e))?;
                let known = self.directories.lock().unwrap_or_else(|e| e.into_inner());
                if known.get(relative) != Some(&found) {
                    let mut d = PathDiagnostic::new(
                        PathConstraintReason::NameCollision,
                        PathStage::Create,
                        "exclusive directory creation found an alias or external object",
                    );
                    d.candidate = Some(relative.into());
                    d.actual_mapping = known
                        .iter()
                        .find(|(_, id)| **id == found)
                        .map(|(path, _)| path.clone());
                    return Err(d.io_error(err));
                }
                return Ok(());
            }
            let dir = open_dir(&parent, parts[parts.len() - 1])
                .map_err(|e| io_failure(PathStage::Create, relative, e))?;
            let id = identity(&dir).map_err(|e| io_failure(PathStage::Create, relative, e))?;
            self.directories
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(relative.into(), id);
            Ok(())
        }
        pub fn create_file(&self, relative: &str) -> Result<File> {
            let parts = validate_relative(relative)?;
            let parent = self.parent(&parts[..parts.len() - 1])?;
            let name = cstr(parts[parts.len() - 1])
                .map_err(|e| io_failure(PathStage::Create, relative, e))?;
            let fd = unsafe {
                libc::openat(
                    parent.as_raw_fd(),
                    name.as_ptr(),
                    libc::O_WRONLY
                        | libc::O_CREAT
                        | libc::O_EXCL
                        | libc::O_NOFOLLOW
                        | libc::O_CLOEXEC,
                    0o600,
                )
            };
            if fd < 0 {
                Err(io_failure(
                    PathStage::Create,
                    relative,
                    std::io::Error::last_os_error(),
                ))
            } else {
                let file = unsafe { File::from_raw_fd(fd) };
                let id = identity(&file).map_err(|e| io_failure(PathStage::Create, relative, e))?;
                self.files
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(relative.into(), id);
                Ok(file)
            }
        }
        fn remove_file(&self, relative: &str, expected: Identity) -> Result<()> {
            let parts = validate_relative(relative)?;
            let parent = self.parent(&parts[..parts.len() - 1])?;
            let name = cstr(parts[parts.len() - 1])
                .map_err(|e| io_failure(PathStage::Cleanup, relative, e))?;
            let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
            let rc = unsafe {
                libc::fstatat(
                    parent.as_raw_fd(),
                    name.as_ptr(),
                    stat.as_mut_ptr(),
                    libc::AT_SYMLINK_NOFOLLOW,
                )
            };
            if rc != 0 {
                return Err(io_failure(
                    PathStage::Cleanup,
                    relative,
                    std::io::Error::last_os_error(),
                ));
            }
            let stat = unsafe { stat.assume_init() };
            if stat_identity(&stat) != expected || stat.st_mode & libc::S_IFMT != libc::S_IFREG {
                return Err(PathDiagnostic::new(
                    PathConstraintReason::InvalidName,
                    PathStage::Cleanup,
                    "placeholder replaced before removal",
                )
                .error());
            }
            if unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), 0) } != 0 {
                return Err(io_failure(
                    PathStage::Cleanup,
                    relative,
                    std::io::Error::last_os_error(),
                ));
            }
            self.files
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(relative);

            Ok(())
        }
        pub fn apply_metadata(
            &self,
            relative: &str,
            mode: Option<u32>,
            modified_unix: Option<i64>,
        ) -> Result<()> {
            if relative.is_empty() {
                let reopened = Self::open(&self.canonical)?;
                if reopened.id != self.id {
                    return Err(PathDiagnostic::new(
                        PathConstraintReason::InvalidName,
                        PathStage::Create,
                        "root identity changed before directory metadata",
                    )
                    .error());
                }
                return apply_file_metadata(&self.root, mode, modified_unix);
            }
            let parts = validate_relative(relative)?;
            let parent = self.parent(&parts[..parts.len() - 1])?;
            let name = cstr(parts[parts.len() - 1])
                .map_err(|e| io_failure(PathStage::Create, relative, e))?;
            let fd = unsafe {
                libc::openat(
                    parent.as_raw_fd(),
                    name.as_ptr(),
                    libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
                )
            };
            if fd < 0 {
                return Err(io_failure(
                    PathStage::Create,
                    relative,
                    std::io::Error::last_os_error(),
                ));
            }
            let file = unsafe { File::from_raw_fd(fd) };
            let id = identity(&file).map_err(|e| io_failure(PathStage::Create, relative, e))?;
            if self
                .directories
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(relative)
                != Some(&id)
                && self
                    .files
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .get(relative)
                    != Some(&id)
            {
                return Err(PathDiagnostic::new(
                    PathConstraintReason::InvalidName,
                    PathStage::Create,
                    "metadata target was replaced or is untracked",
                )
                .error());
            }
            apply_file_metadata(&file, mode, modified_unix)
        }
        /// Remove only empty directories whose identities were created by this root.
        /// Any injected object makes cleanup fail rather than recursively deleting it.
        /// Re-enable cleanup of a tracked directory after restrictive archive metadata.
        /// Callers restore parents before children. Only owner rwx bits are added.
        pub fn restore_directory_access(&self, relative: &str) -> Result<()> {
            if relative.is_empty() {
                let current = std::fs::symlink_metadata(&self.canonical)
                    .map_err(|e| io_failure(PathStage::Cleanup, "", e))?;
                if !current.is_dir() || (current.dev(), current.ino()) != self.id {
                    return Err(PathDiagnostic::new(
                        PathConstraintReason::InvalidName,
                        PathStage::Cleanup,
                        "root replaced before restoring directory access",
                    )
                    .error());
                }
                // The held root FD remains usable even when its directory mode is 000.
                let mode = ((current.mode() & 0o7777) | 0o700) as libc::mode_t;
                if unsafe { libc::fchmod(self.root.as_raw_fd(), mode) } != 0 {
                    return Err(io_failure(
                        PathStage::Cleanup,
                        "",
                        std::io::Error::last_os_error(),
                    ));
                }
                return Ok(());
            }
            let parts = validate_relative(relative)?;
            let parent = self.parent(&parts[..parts.len() - 1])?;
            let name = cstr(parts[parts.len() - 1])
                .map_err(|e| io_failure(PathStage::Cleanup, relative, e))?;
            let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
            if unsafe {
                libc::fstatat(
                    parent.as_raw_fd(),
                    name.as_ptr(),
                    stat.as_mut_ptr(),
                    libc::AT_SYMLINK_NOFOLLOW,
                )
            } != 0
            {
                return Err(io_failure(
                    PathStage::Cleanup,
                    relative,
                    std::io::Error::last_os_error(),
                ));
            }
            let stat = unsafe { stat.assume_init() };
            if stat.st_mode & libc::S_IFMT != libc::S_IFDIR
                || self
                    .directories
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .get(relative)
                    != Some(&stat_identity(&stat))
            {
                return Err(PathDiagnostic::new(
                    PathConstraintReason::InvalidName,
                    PathStage::Cleanup,
                    "directory replaced or untracked before access restoration",
                )
                .error());
            }
            // Parent traversal never follows links. A private staging root is the security
            // boundary; fstatat/fchmodat do not promise isolation from same-uid replacement.
            if unsafe {
                libc::fchmodat(
                    parent.as_raw_fd(),
                    name.as_ptr(),
                    (stat.st_mode & 0o7777) | 0o700,
                    0,
                )
            } != 0
            {
                return Err(io_failure(
                    PathStage::Cleanup,
                    relative,
                    std::io::Error::last_os_error(),
                ));
            }
            Ok(())
        }
        pub fn clear_verified_namespace(&self) -> Result<()> {
            let mut paths: Vec<_> = self
                .directories
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .keys()
                .cloned()
                .collect();
            paths.sort_by(|a, b| {
                b.split('/')
                    .count()
                    .cmp(&a.split('/').count())
                    .then(b.cmp(a))
            });
            for path in paths {
                let parts = validate_relative(&path)?;
                let parent = self.parent(&parts[..parts.len() - 1])?;
                let dir = open_dir(&parent, parts[parts.len() - 1])
                    .map_err(|e| io_failure(PathStage::Cleanup, &path, e))?;
                let actual =
                    identity(&dir).map_err(|e| io_failure(PathStage::Cleanup, &path, e))?;
                if self
                    .directories
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .get(&path)
                    != Some(&actual)
                {
                    return Err(PathDiagnostic::new(
                        PathConstraintReason::InvalidName,
                        PathStage::Cleanup,
                        "directory replaced during preflight cleanup",
                    )
                    .error());
                }
                let name = cstr(parts[parts.len() - 1])
                    .map_err(|e| io_failure(PathStage::Cleanup, &path, e))?;
                if unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), libc::AT_REMOVEDIR) }
                    != 0
                {
                    return Err(io_failure(
                        PathStage::Cleanup,
                        &path,
                        std::io::Error::last_os_error(),
                    ));
                }
                self.directories
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&path);
            }
            Ok(())
        }
        pub fn verify_namespace(&self, report: &PathMappingReport) -> Result<()> {
            self.verify_namespace_with_cancellation(report, || false)
        }
        pub fn verify_namespace_with_cancellation(
            &self,
            report: &PathMappingReport,
            cancelled: impl Fn() -> bool,
        ) -> Result<()> {
            self.verify_namespace_inner(report, &cancelled)
                .map_err(|error| annotate_error(error, report))
        }
        fn verify_namespace_inner(
            &self,
            report: &PathMappingReport,
            cancelled: &dyn Fn() -> bool,
        ) -> Result<()> {
            if cancelled() {
                return Err(SmartZipError::Cancelled);
            }
            validate_path_budget(&self.canonical, report)?;
            let mut dirs = BTreeSet::new();
            let mut files = BTreeSet::new();
            for entry in &report.entries {
                if entry.is_dir && entry.staging_relative.is_empty() {
                    continue;
                }
                let parts = validate_relative(&entry.staging_relative)?;
                for count in 1..=parts.len() {
                    if count < parts.len() || entry.is_dir {
                        dirs.insert(parts[..count].join("/"));
                    }
                }
                if !entry.is_dir && !files.insert(entry.staging_relative.clone()) {
                    return Err(PathDiagnostic::new(
                        PathConstraintReason::NameCollision,
                        PathStage::Create,
                        "duplicate mapped file",
                    )
                    .error());
                }
            }
            let mut dirs: Vec<_> = dirs.into_iter().collect();
            dirs.sort_by(|a, b| {
                a.split('/')
                    .count()
                    .cmp(&b.split('/').count())
                    .then(a.cmp(b))
            });
            for dir in dirs {
                if cancelled() {
                    return Err(SmartZipError::Cancelled);
                }
                self.create_dir(&dir)?;
            }
            let mut placeholders: Vec<(String, Identity)> = Vec::new();
            let created = (|| {
                for path in files {
                    if cancelled() {
                        return Err(SmartZipError::Cancelled);
                    }
                    match self.create_file(&path) {
                        Ok(file) => {
                            let id = identity(&file)
                                .map_err(|e| io_failure(PathStage::Create, &path, e))?;
                            placeholders.push((path, id));
                        }
                        Err(SmartZipError::PathConstraint {
                            mut diagnostic,
                            source,
                        }) => {
                            if diagnostic.reason == PathConstraintReason::NameCollision {
                                // fstatat follows no links; only an identity tracked in this preflight is a known alias.
                                let parts = validate_relative(&path)?;
                                let parent = self.parent(&parts[..parts.len() - 1])?;
                                let name = cstr(parts[parts.len() - 1])
                                    .map_err(|e| io_failure(PathStage::Create, &path, e))?;
                                let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
                                if unsafe {
                                    libc::fstatat(
                                        parent.as_raw_fd(),
                                        name.as_ptr(),
                                        stat.as_mut_ptr(),
                                        libc::AT_SYMLINK_NOFOLLOW,
                                    )
                                } == 0
                                {
                                    let s = unsafe { stat.assume_init() };
                                    let id = stat_identity(&s);
                                    diagnostic.actual_mapping = placeholders
                                        .iter()
                                        .find(|(_, old)| *old == id)
                                        .map(|(path, _)| path.clone());
                                }
                            }
                            return Err(SmartZipError::PathConstraint { diagnostic, source });
                        }
                        Err(error) => return Err(error),
                    }
                }
                Ok(())
            })();
            // Close every placeholder before removal. Cleanup failure always terminates an attempt.
            for (path, id) in placeholders.into_iter().rev() {
                self.remove_file(&path, id)?;
            }
            created
        }
    }
    /// Restore access only to a SmartZip-owned staging tree described by a frozen intent.
    /// The caller must verify intent ownership and its recorded source identity first.
    /// This operation never deletes objects or restores access to user backup directories.
    pub fn restore_staging_access(root: &Path, directories: &[String]) -> Result<()> {
        let directories = recovery_directories(directories)?;
        let parent_path = root
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let name = root
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| {
                PathDiagnostic::new(
                    PathConstraintReason::InvalidName,
                    PathStage::Cleanup,
                    "staging root is not a named directory",
                )
                .error()
            })?;
        validate_relative(name)?;
        let parent = ControlledRoot::open(parent_path)?;
        let component = cstr(name).map_err(|e| io_failure(PathStage::Cleanup, "", e))?;
        let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
        if unsafe {
            libc::fstatat(
                parent.root.as_raw_fd(),
                component.as_ptr(),
                stat.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } != 0
        {
            return Err(io_failure(
                PathStage::Cleanup,
                "",
                std::io::Error::last_os_error(),
            ));
        }
        let stat = unsafe { stat.assume_init() };
        if stat.st_mode & libc::S_IFMT != libc::S_IFDIR {
            return Err(PathDiagnostic::new(
                PathConstraintReason::InvalidName,
                PathStage::Cleanup,
                "staging root is a link or not a directory",
            )
            .error());
        }
        let expected = stat_identity(&stat);
        // The caller owns this private staging tree; same-uid replacement races are
        // outside this API's isolation promise. The parent descriptor is anchored.
        if unsafe {
            libc::fchmodat(
                parent.root.as_raw_fd(),
                component.as_ptr(),
                (stat.st_mode & 0o7777) | 0o700,
                0,
            )
        } != 0
        {
            return Err(io_failure(
                PathStage::Cleanup,
                "",
                std::io::Error::last_os_error(),
            ));
        }
        let controlled = ControlledRoot::open(parent.canonical.join(name))?;
        if controlled.id != expected {
            return Err(PathDiagnostic::new(
                PathConstraintReason::InvalidName,
                PathStage::Cleanup,
                "staging root changed during access recovery",
            )
            .error());
        }
        for relative in directories {
            let parts = validate_relative(&relative)?;
            let parent = controlled.parent(&parts[..parts.len() - 1])?;
            let name = cstr(parts[parts.len() - 1])
                .map_err(|e| io_failure(PathStage::Cleanup, &relative, e))?;
            let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
            if unsafe {
                libc::fstatat(
                    parent.as_raw_fd(),
                    name.as_ptr(),
                    stat.as_mut_ptr(),
                    libc::AT_SYMLINK_NOFOLLOW,
                )
            } != 0
            {
                return Err(io_failure(
                    PathStage::Cleanup,
                    &relative,
                    std::io::Error::last_os_error(),
                ));
            }
            let stat = unsafe { stat.assume_init() };
            if stat.st_mode & libc::S_IFMT != libc::S_IFDIR {
                return Err(PathDiagnostic::new(
                    PathConstraintReason::InvalidName,
                    PathStage::Cleanup,
                    "staging recovery directory is a link or wrong type",
                )
                .error());
            }
            controlled
                .directories
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(relative.clone(), stat_identity(&stat));
            controlled.restore_directory_access(&relative)?;
        }
        Ok(())
    }
    pub fn probe_target(parent: &Path, mode: PathMode) -> Result<TargetPathPolicy> {
        let root = ControlledRoot::open(parent)?;
        // POSIX distinguishes indeterminate -1 (errno remains zero) from failure.
        unsafe {
            *errno_ptr() = 0;
        }
        let limit = unsafe { libc::fpathconf(root.root.as_raw_fd(), libc::_PC_NAME_MAX) };
        let errno = unsafe { *errno_ptr() };
        let (limit, confidence, source) = if limit > 0 {
            (
                limit as usize,
                PolicyConfidence::Known,
                "fpathconf(_PC_NAME_MAX)".to_string(),
            )
        } else if limit == -1 && errno == 0 {
            (
                255,
                PolicyConfidence::Unknown,
                "fpathconf indeterminate; conservative application budget".into(),
            )
        } else {
            return Err(io_failure(
                PathStage::Preflight,
                "",
                std::io::Error::from_raw_os_error(errno),
            ));
        };
        let fs_kind = filesystem_kind(&root.root)?;
        let probe = tempfile::Builder::new()
            .prefix(".sz-name-")
            .tempdir_in(&root.canonical)
            .map_err(|e| io_failure(PathStage::Preflight, "", e))?;
        fn equivalent(root: &Path, a: &str, b: &str) -> std::io::Result<bool> {
            let _first = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(root.join(a))?;
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(root.join(b))
            {
                Ok(_) => Ok(false),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(true),
                Err(e) => Err(e),
            }
        }
        let case_sensitive = !equivalent(probe.path(), "SmartZipCase", "smartzipcase")
            .map_err(|e| io_failure(PathStage::Preflight, "case probe", e))?;
        let normalization_sensitive = !equivalent(probe.path(), "é", "e\u{301}")
            .map_err(|e| io_failure(PathStage::Preflight, "normalization probe", e))?;
        probe
            .close()
            .map_err(|e| io_failure(PathStage::Cleanup, "name probe", e))?;
        #[cfg(target_os = "macos")]
        let component = if fs_kind == "apfs" {
            ComponentBudget {
                limit: 255,
                metric: LengthMetric::Utf16Units,
                source: "APFS application conservative UTF-16 budget (creation is authoritative)"
                    .into(),
                confidence: PolicyConfidence::Conservative,
            }
        } else {
            ComponentBudget {
                limit,
                metric: LengthMetric::Utf8Bytes,
                source,
                confidence,
            }
        };
        #[cfg(not(target_os = "macos"))]
        let component = ComponentBudget {
            limit,
            metric: LengthMetric::Utf8Bytes,
            source,
            confidence,
        };
        let full_limit = unsafe { libc::fpathconf(root.root.as_raw_fd(), libc::_PC_PATH_MAX) };
        Ok(TargetPathPolicy {
            version: PATH_MAPPING_VERSION,
            mode,
            target: TargetIdentity {
                canonical_root: root.canonical.to_string_lossy().into_owned(),
                volume_id: root.id.0.to_string(),
                root_id: format!("{}:{}", root.id.0, root.id.1),
            },
            fs_kind,
            component,
            access: PathAccessStrategy::PosixDirRelative,
            comparison: NameComparison {
                case_sensitive,
                normalization_sensitive,
                confidence: PolicyConfidence::Known,
            },
            windows_names: false,
            full_path_limit: Some(if full_limit > 0 {
                full_limit as usize
            } else {
                1024
            }),
        })
    }
    fn filesystem_kind(file: &File) -> Result<String> {
        let mut stat = std::mem::MaybeUninit::<libc::statfs>::uninit();
        if unsafe { libc::fstatfs(file.as_raw_fd(), stat.as_mut_ptr()) } != 0 {
            return Err(io_failure(
                PathStage::Preflight,
                "filesystem",
                std::io::Error::last_os_error(),
            ));
        }
        let stat = unsafe { stat.assume_init() };
        #[cfg(target_os = "macos")]
        {
            Ok(
                unsafe { std::ffi::CStr::from_ptr(stat.f_fstypename.as_ptr()) }
                    .to_string_lossy()
                    .into_owned(),
            )
        }
        #[cfg(target_os = "linux")]
        {
            Ok(match stat.f_type as u64 {
                0x9123683e => "btrfs".into(),
                0xEF53 => "ext".into(),
                0x01021994 => "tmpfs".into(),
                0x6969 => "nfs".into(),
                other => format!("posix:0x{other:x}"),
            })
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            let _ = stat;
            Ok("posix:unknown".into())
        }
    }
}
#[cfg(unix)]
pub use native::{probe_target, restore_staging_access, ControlledRoot};

#[cfg(windows)]
mod windows {
    use super::*;
    use std::collections::{BTreeMap, BTreeSet};
    use std::fs::File;
    use std::os::windows::{
        ffi::OsStrExt,
        io::{AsRawHandle, FromRawHandle},
    };
    use std::sync::Mutex;
    use windows_sys::Win32::Foundation::{
        ERROR_ALREADY_EXISTS, ERROR_FILENAME_EXCED_RANGE, ERROR_FILE_EXISTS, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Storage::FileSystem::*;
    type Identity = (u32, u64);
    fn wide(path: &Path) -> Vec<u16> {
        path.as_os_str().encode_wide().chain(Some(0)).collect()
    }
    fn information(file: &File) -> std::io::Result<BY_HANDLE_FILE_INFORMATION> {
        let mut info = std::mem::MaybeUninit::uninit();
        if unsafe { GetFileInformationByHandle(file.as_raw_handle(), info.as_mut_ptr()) } == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(unsafe { info.assume_init() })
    }
    fn identity(file: &File) -> std::io::Result<Identity> {
        let info = information(file)?;
        Ok((
            info.dwVolumeSerialNumber,
            ((info.nFileIndexHigh as u64) << 32) | info.nFileIndexLow as u64,
        ))
    }
    fn open_directory(path: &Path) -> std::io::Result<File> {
        let handle = unsafe {
            CreateFileW(
                wide(path).as_ptr(),
                FILE_READ_ATTRIBUTES,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
                std::ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(std::io::Error::last_os_error());
        }
        let file = unsafe { File::from_raw_handle(handle) };
        let info = information(&file)?;
        if info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
            || info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY == 0
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "reparse point or non-directory in controlled path",
            ));
        }
        Ok(file)
    }
    fn win_error(stage: PathStage, path: &str, error: std::io::Error) -> SmartZipError {
        let reason = match error.raw_os_error().map(|n| n as u32) {
            Some(ERROR_ALREADY_EXISTS | ERROR_FILE_EXISTS) => PathConstraintReason::NameCollision,
            Some(ERROR_FILENAME_EXCED_RANGE) => PathConstraintReason::NameTooLong,
            _ => PathConstraintReason::PathConstraintUnknown,
        };
        let mut d = PathDiagnostic::new(reason, stage, "Windows controlled creation failed");
        d.candidate = Some(path.into());
        d.io_error(error)
    }
    pub struct ControlledRoot {
        root: File,
        canonical: PathBuf,
        id: Identity,
        directories: Mutex<BTreeMap<String, Identity>>,
        files: Mutex<BTreeMap<String, Identity>>,
    }
    impl ControlledRoot {
        pub fn open(root: impl AsRef<Path>) -> Result<Self> {
            // canonicalize provides an absolute verbatim path, resolving only the user-selected root.
            let canonical = std::fs::canonicalize(root.as_ref())
                .map_err(|e| SmartZipError::io(Some(root.as_ref().to_path_buf()), e))?;
            let file =
                open_directory(&canonical).map_err(|e| win_error(PathStage::Preflight, "", e))?;
            let id = identity(&file).map_err(|e| win_error(PathStage::Preflight, "", e))?;
            Ok(Self {
                root: file,
                canonical,
                id,
                directories: Mutex::new(BTreeMap::new()),
                files: Mutex::new(BTreeMap::new()),
            })
        }
        pub fn canonical_root(&self) -> &Path {
            &self.canonical
        }
        pub fn verify_identity(&self, policy: &TargetPathPolicy) -> Result<()> {
            let reopened = Self::open(&self.canonical)?;
            if reopened.id != self.id
                || self.id.0.to_string() != policy.target.volume_id
                || format!("{}:{}", self.id.0, self.id.1) != policy.target.root_id
            {
                return Err(PathDiagnostic::new(
                    PathConstraintReason::PathConstraintUnknown,
                    PathStage::Commit,
                    "target root identity changed",
                )
                .error());
            }
            Ok(())
        }
        fn parent(&self, parts: &[&str]) -> Result<(PathBuf, Vec<File>)> {
            let mut path = self.canonical.clone();
            let mut handles = vec![self
                .root
                .try_clone()
                .map_err(|e| win_error(PathStage::Create, "", e))?];
            let mut relative = String::new();
            let known = self.directories.lock().unwrap_or_else(|e| e.into_inner());
            for part in parts {
                path.push(part);
                if !relative.is_empty() {
                    relative.push('/');
                }
                relative.push_str(part);
                let file = open_directory(&path)
                    .map_err(|e| win_error(PathStage::Create, &relative, e))?;
                if known.get(&relative)
                    != Some(
                        &identity(&file).map_err(|e| win_error(PathStage::Create, &relative, e))?,
                    )
                {
                    return Err(PathDiagnostic::new(
                        PathConstraintReason::InvalidName,
                        PathStage::Create,
                        "untracked or replaced Windows parent directory",
                    )
                    .error());
                }
                // Handles exclude FILE_SHARE_DELETE and remain live until the child create finishes.
                handles.push(file);
            }
            Ok((path, handles))
        }
        pub fn create_dir(&self, relative: &str) -> Result<()> {
            let parts = validate_relative(relative)?;
            let (mut path, _parents) = self.parent(&parts[..parts.len() - 1])?;
            path.push(parts[parts.len() - 1]);
            if unsafe { CreateDirectoryW(wide(&path).as_ptr(), std::ptr::null()) } == 0 {
                let error = std::io::Error::last_os_error();
                if error.raw_os_error().map(|n| n as u32) != Some(ERROR_ALREADY_EXISTS) {
                    return Err(win_error(PathStage::Create, relative, error));
                }
                let file =
                    open_directory(&path).map_err(|e| win_error(PathStage::Create, relative, e))?;
                let found =
                    identity(&file).map_err(|e| win_error(PathStage::Create, relative, e))?;
                let known = self.directories.lock().unwrap_or_else(|e| e.into_inner());
                if known.get(relative) != Some(&found) {
                    let mut d = PathDiagnostic::new(
                        PathConstraintReason::NameCollision,
                        PathStage::Create,
                        "Windows directory aliases another node or an external object",
                    );
                    d.candidate = Some(relative.into());
                    d.actual_mapping = known
                        .iter()
                        .find(|(_, id)| **id == found)
                        .map(|(p, _)| p.clone());
                    return Err(d.io_error(error));
                }
                return Ok(());
            }
            let file =
                open_directory(&path).map_err(|e| win_error(PathStage::Create, relative, e))?;
            let id = identity(&file).map_err(|e| win_error(PathStage::Create, relative, e))?;
            self.directories
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(relative.into(), id);
            Ok(())
        }
        pub fn create_file(&self, relative: &str) -> Result<File> {
            let parts = validate_relative(relative)?;
            let (mut path, _parents) = self.parent(&parts[..parts.len() - 1])?;
            path.push(parts[parts.len() - 1]);
            let handle = unsafe {
                CreateFileW(
                    wide(&path).as_ptr(),
                    FILE_GENERIC_WRITE,
                    FILE_SHARE_READ,
                    std::ptr::null(),
                    CREATE_NEW,
                    FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT,
                    std::ptr::null_mut(),
                )
            };
            if handle == INVALID_HANDLE_VALUE {
                return Err(win_error(
                    PathStage::Create,
                    relative,
                    std::io::Error::last_os_error(),
                ));
            }
            let file = unsafe { File::from_raw_handle(handle) };
            if information(&file)
                .map_err(|e| win_error(PathStage::Create, relative, e))?
                .dwFileAttributes
                & FILE_ATTRIBUTE_REPARSE_POINT
                != 0
            {
                return Err(PathDiagnostic::new(
                    PathConstraintReason::InvalidName,
                    PathStage::Create,
                    "created Windows file is a reparse point",
                )
                .error());
            }
            let id = identity(&file).map_err(|e| win_error(PathStage::Create, relative, e))?;
            self.files
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(relative.into(), id);
            Ok(file)
        }
        fn remove_file(&self, relative: &str, expected: Identity) -> Result<()> {
            let parts = validate_relative(relative)?;
            let (mut path, _parents) = self.parent(&parts[..parts.len() - 1])?;
            path.push(parts[parts.len() - 1]);
            let handle = unsafe {
                CreateFileW(
                    wide(&path).as_ptr(),
                    FILE_READ_ATTRIBUTES,
                    FILE_SHARE_READ | FILE_SHARE_WRITE,
                    std::ptr::null(),
                    OPEN_EXISTING,
                    FILE_FLAG_OPEN_REPARSE_POINT,
                    std::ptr::null_mut(),
                )
            };
            if handle == INVALID_HANDLE_VALUE {
                return Err(win_error(
                    PathStage::Cleanup,
                    relative,
                    std::io::Error::last_os_error(),
                ));
            }
            let file = unsafe { File::from_raw_handle(handle) };
            let info =
                information(&file).map_err(|e| win_error(PathStage::Cleanup, relative, e))?;
            if identity(&file).map_err(|e| win_error(PathStage::Cleanup, relative, e))? != expected
                || info.dwFileAttributes & (FILE_ATTRIBUTE_REPARSE_POINT | FILE_ATTRIBUTE_DIRECTORY)
                    != 0
            {
                return Err(PathDiagnostic::new(
                    PathConstraintReason::InvalidName,
                    PathStage::Cleanup,
                    "Windows placeholder replaced before removal",
                )
                .error());
            }
            drop(file);
            if unsafe { DeleteFileW(wide(&path).as_ptr()) } == 0 {
                return Err(win_error(
                    PathStage::Cleanup,
                    relative,
                    std::io::Error::last_os_error(),
                ));
            }
            self.files
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(relative);
            Ok(())
        }
        pub fn apply_metadata(
            &self,
            relative: &str,
            mode: Option<u32>,
            modified_unix: Option<i64>,
        ) -> Result<()> {
            if relative.is_empty() {
                let reopened = Self::open(&self.canonical)?;
                if reopened.id != self.id {
                    return Err(PathDiagnostic::new(
                        PathConstraintReason::InvalidName,
                        PathStage::Create,
                        "root identity changed before directory metadata",
                    )
                    .error());
                }
                // The original root handle excludes share-delete and keeps this directory fixed.
                // Acquire only the write-attributes rights needed for metadata, then verify identity.
                let handle = unsafe {
                    CreateFileW(
                        wide(&self.canonical).as_ptr(),
                        FILE_READ_ATTRIBUTES | FILE_WRITE_ATTRIBUTES,
                        FILE_SHARE_READ | FILE_SHARE_WRITE,
                        std::ptr::null(),
                        OPEN_EXISTING,
                        FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
                        std::ptr::null_mut(),
                    )
                };
                if handle == INVALID_HANDLE_VALUE {
                    return Err(win_error(
                        PathStage::Create,
                        "",
                        std::io::Error::last_os_error(),
                    ));
                }
                let file = unsafe { File::from_raw_handle(handle) };
                if identity(&file).map_err(|e| win_error(PathStage::Create, "", e))? != self.id
                    || information(&file)
                        .map_err(|e| win_error(PathStage::Create, "", e))?
                        .dwFileAttributes
                        & FILE_ATTRIBUTE_REPARSE_POINT
                        != 0
                {
                    return Err(PathDiagnostic::new(
                        PathConstraintReason::InvalidName,
                        PathStage::Create,
                        "root metadata handle identity changed",
                    )
                    .error());
                }
                return apply_file_metadata(&file, mode, modified_unix);
            }
            let parts = validate_relative(relative)?;
            let (mut path, _parents) = self.parent(&parts[..parts.len() - 1])?;
            path.push(parts[parts.len() - 1]);
            let handle = unsafe {
                CreateFileW(
                    wide(&path).as_ptr(),
                    FILE_READ_ATTRIBUTES | FILE_WRITE_ATTRIBUTES,
                    FILE_SHARE_READ | FILE_SHARE_WRITE,
                    std::ptr::null(),
                    OPEN_EXISTING,
                    FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
                    std::ptr::null_mut(),
                )
            };
            if handle == INVALID_HANDLE_VALUE {
                return Err(win_error(
                    PathStage::Create,
                    relative,
                    std::io::Error::last_os_error(),
                ));
            }
            let file = unsafe { File::from_raw_handle(handle) };
            let id = identity(&file).map_err(|e| win_error(PathStage::Create, relative, e))?;
            if information(&file)
                .map_err(|e| win_error(PathStage::Create, relative, e))?
                .dwFileAttributes
                & FILE_ATTRIBUTE_REPARSE_POINT
                != 0
                || (self
                    .directories
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .get(relative)
                    != Some(&id)
                    && self
                        .files
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .get(relative)
                        != Some(&id))
            {
                return Err(PathDiagnostic::new(
                    PathConstraintReason::InvalidName,
                    PathStage::Create,
                    "Windows metadata target was replaced or is untracked",
                )
                .error());
            }
            apply_file_metadata(&file, mode, modified_unix)
        }
        /// Windows ignores Unix permission bits; still verify the tracked directory.
        pub fn restore_directory_access(&self, relative: &str) -> Result<()> {
            let reopened = Self::open(&self.canonical)?;
            if reopened.id != self.id {
                return Err(PathDiagnostic::new(
                    PathConstraintReason::InvalidName,
                    PathStage::Cleanup,
                    "Windows root replaced before access restoration",
                )
                .error());
            }
            if relative.is_empty() {
                return Ok(());
            }
            let parts = validate_relative(relative)?;
            let (mut path, _parents) = self.parent(&parts[..parts.len() - 1])?;
            path.push(parts[parts.len() - 1]);
            let directory =
                open_directory(&path).map_err(|e| win_error(PathStage::Cleanup, relative, e))?;
            let id =
                identity(&directory).map_err(|e| win_error(PathStage::Cleanup, relative, e))?;
            if self
                .directories
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(relative)
                != Some(&id)
            {
                return Err(PathDiagnostic::new(
                    PathConstraintReason::InvalidName,
                    PathStage::Cleanup,
                    "Windows directory replaced or untracked before access restoration",
                )
                .error());
            }
            Ok(())
        }
        pub fn clear_verified_namespace(&self) -> Result<()> {
            let mut paths: Vec<_> = self
                .directories
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .keys()
                .cloned()
                .collect();
            paths.sort_by(|a, b| {
                b.split('/')
                    .count()
                    .cmp(&a.split('/').count())
                    .then(b.cmp(a))
            });
            for relative in paths {
                let parts = validate_relative(&relative)?;
                let (mut path, _parents) = self.parent(&parts[..parts.len() - 1])?;
                path.push(parts[parts.len() - 1]);
                let dir = open_directory(&path)
                    .map_err(|e| win_error(PathStage::Cleanup, &relative, e))?;
                if self
                    .directories
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .get(&relative)
                    != Some(
                        &identity(&dir).map_err(|e| win_error(PathStage::Cleanup, &relative, e))?,
                    )
                {
                    return Err(PathDiagnostic::new(
                        PathConstraintReason::InvalidName,
                        PathStage::Cleanup,
                        "Windows directory replaced during cleanup",
                    )
                    .error());
                }
                drop(dir);
                if unsafe { RemoveDirectoryW(wide(&path).as_ptr()) } == 0 {
                    return Err(win_error(
                        PathStage::Cleanup,
                        &relative,
                        std::io::Error::last_os_error(),
                    ));
                }
                self.directories
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&relative);
            }
            Ok(())
        }
        pub fn verify_namespace(&self, report: &PathMappingReport) -> Result<()> {
            self.verify_namespace_with_cancellation(report, || false)
        }
        pub fn verify_namespace_with_cancellation(
            &self,
            report: &PathMappingReport,
            cancelled: impl Fn() -> bool,
        ) -> Result<()> {
            self.verify_namespace_inner(report, &cancelled)
                .map_err(|error| annotate_error(error, report))
        }
        fn verify_namespace_inner(
            &self,
            report: &PathMappingReport,
            cancelled: &dyn Fn() -> bool,
        ) -> Result<()> {
            if cancelled() {
                return Err(SmartZipError::Cancelled);
            }
            validate_path_budget(&self.canonical, report)?;
            let mut dirs = BTreeSet::new();
            let mut files = BTreeSet::new();
            for e in &report.entries {
                if e.is_dir && e.staging_relative.is_empty() {
                    continue;
                }
                let parts = validate_relative(&e.staging_relative)?;
                for n in 1..=parts.len() {
                    if n < parts.len() || e.is_dir {
                        dirs.insert(parts[..n].join("/"));
                    }
                }
                if !e.is_dir && !files.insert(e.staging_relative.clone()) {
                    return Err(PathDiagnostic::new(
                        PathConstraintReason::NameCollision,
                        PathStage::Create,
                        "duplicate mapped file",
                    )
                    .error());
                }
            }
            let mut dirs: Vec<_> = dirs.into_iter().collect();
            dirs.sort_by(|a, b| {
                a.split('/')
                    .count()
                    .cmp(&b.split('/').count())
                    .then(a.cmp(b))
            });
            for dir in dirs {
                if cancelled() {
                    return Err(SmartZipError::Cancelled);
                }
                self.create_dir(&dir)?;
            }
            let mut placeholders = Vec::new();
            let created = (|| {
                for path in files {
                    if cancelled() {
                        return Err(SmartZipError::Cancelled);
                    }
                    match self.create_file(&path) {
                        Ok(file) => placeholders.push((
                            path,
                            identity(&file)
                                .map_err(|e| win_error(PathStage::Create, "placeholder", e))?,
                        )),
                        Err(SmartZipError::PathConstraint {
                            mut diagnostic,
                            source,
                        }) => {
                            if diagnostic.reason == PathConstraintReason::NameCollision {
                                let parts = validate_relative(&path)?;
                                let (mut p, _parents) = self.parent(&parts[..parts.len() - 1])?;
                                p.push(parts[parts.len() - 1]);
                                let handle = unsafe {
                                    CreateFileW(
                                        wide(&p).as_ptr(),
                                        FILE_READ_ATTRIBUTES,
                                        FILE_SHARE_READ | FILE_SHARE_WRITE,
                                        std::ptr::null(),
                                        OPEN_EXISTING,
                                        FILE_FLAG_OPEN_REPARSE_POINT,
                                        std::ptr::null_mut(),
                                    )
                                };
                                if handle != INVALID_HANDLE_VALUE {
                                    let f = unsafe { File::from_raw_handle(handle) };
                                    if let Ok(id) = identity(&f) {
                                        diagnostic.actual_mapping = placeholders
                                            .iter()
                                            .find(|(_, old)| *old == id)
                                            .map(|(p, _)| p.clone());
                                    }
                                }
                            }
                            return Err(SmartZipError::PathConstraint { diagnostic, source });
                        }
                        Err(e) => return Err(e),
                    }
                }
                Ok(())
            })();
            for (path, id) in placeholders.into_iter().rev() {
                self.remove_file(&path, id)?;
            }
            created
        }
    }
    /// Verify a frozen, caller-owned staging namespace; Windows ignores Unix modes.
    /// Never use this operation to modify access to arbitrary user backups.
    pub fn restore_staging_access(root: &Path, directories: &[String]) -> Result<()> {
        use std::os::windows::fs::OpenOptionsExt;
        let directories = recovery_directories(directories)?;
        // Rust's Windows path adapter supplies verbatim paths. OPEN_REPARSE_POINT
        // lets the first held handle reject a root link before canonicalization.
        let initial = std::fs::OpenOptions::new()
            .read(true)
            .access_mode(FILE_READ_ATTRIBUTES)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(root)
            .map_err(|e| win_error(PathStage::Cleanup, "", e))?;
        let info = information(&initial).map_err(|e| win_error(PathStage::Cleanup, "", e))?;
        if info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
            || info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY == 0
        {
            return Err(PathDiagnostic::new(
                PathConstraintReason::InvalidName,
                PathStage::Cleanup,
                "Windows staging root is a reparse point or wrong type",
            )
            .error());
        }
        let expected = identity(&initial).map_err(|e| win_error(PathStage::Cleanup, "", e))?;
        let controlled = ControlledRoot::open(root)?;
        if controlled.id != expected {
            return Err(PathDiagnostic::new(
                PathConstraintReason::InvalidName,
                PathStage::Cleanup,
                "Windows staging root changed during recovery",
            )
            .error());
        }
        for relative in directories {
            let parts = validate_relative(&relative)?;
            let (mut path, _parents) = controlled.parent(&parts[..parts.len() - 1])?;
            path.push(parts[parts.len() - 1]);
            let directory =
                open_directory(&path).map_err(|e| win_error(PathStage::Cleanup, &relative, e))?;
            let id =
                identity(&directory).map_err(|e| win_error(PathStage::Cleanup, &relative, e))?;
            controlled
                .directories
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(relative.clone(), id);
            controlled.restore_directory_access(&relative)?;
        }
        Ok(())
    }
    pub fn probe_target(parent: &Path, mode: PathMode) -> Result<TargetPathPolicy> {
        let root = ControlledRoot::open(parent)?;
        let canonical = &root.canonical;
        let mut volume = vec![0u16; 32768];
        if unsafe {
            GetVolumePathNameW(
                wide(canonical).as_ptr(),
                volume.as_mut_ptr(),
                volume.len() as u32,
            )
        } == 0
        {
            return Err(win_error(
                PathStage::Preflight,
                "volume path",
                std::io::Error::last_os_error(),
            ));
        }
        let mut serial = 0u32;
        let mut max = 0u32;
        let mut flags = 0u32;
        let mut fs = vec![0u16; 256];
        if unsafe {
            GetVolumeInformationW(
                volume.as_ptr(),
                std::ptr::null_mut(),
                0,
                &mut serial,
                &mut max,
                &mut flags,
                fs.as_mut_ptr(),
                fs.len() as u32,
            )
        } == 0
        {
            return Err(win_error(
                PathStage::Preflight,
                "volume information",
                std::io::Error::last_os_error(),
            ));
        }
        // Per-directory rule evidence comes from actual exclusive creation in an inherited child.
        let probe = tempfile::Builder::new()
            .prefix(".sz-name-")
            .tempdir_in(canonical)
            .map_err(|e| win_error(PathStage::Preflight, "case probe", e))?;
        let first = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(probe.path().join("SmartZipCase"))
            .map_err(|e| win_error(PathStage::Preflight, "case probe", e))?;
        drop(first);
        let case_sensitive = match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(probe.path().join("smartzipcase"))
        {
            Ok(_) => true,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => false,
            Err(e) => return Err(win_error(PathStage::Preflight, "case probe", e)),
        };
        probe
            .close()
            .map_err(|e| win_error(PathStage::Cleanup, "case probe", e))?;
        Ok(TargetPathPolicy {
            version: PATH_MAPPING_VERSION,
            mode,
            target: TargetIdentity {
                canonical_root: canonical.to_string_lossy().into_owned(),
                volume_id: root.id.0.to_string(),
                root_id: format!("{}:{}", root.id.0, root.id.1),
            },
            fs_kind: String::from_utf16_lossy(
                &fs[..fs.iter().position(|n| *n == 0).unwrap_or(fs.len())],
            ),
            component: ComponentBudget {
                limit: max as usize,
                metric: LengthMetric::Utf16Units,
                source: "GetVolumeInformationW".into(),
                confidence: PolicyConfidence::Known,
            },
            access: PathAccessStrategy::WindowsVerbatim,
            comparison: NameComparison {
                case_sensitive,
                normalization_sensitive: true,
                confidence: PolicyConfidence::Known,
            },
            windows_names: true,
            full_path_limit: Some(32700),
        })
    }
}
#[cfg(windows)]
pub use windows::{probe_target, restore_staging_access, ControlledRoot};

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::unix::fs::symlink;
    #[test]
    fn actual_directory_probe_and_volume_identity() {
        let temp = tempfile::tempdir().unwrap();
        let policy = probe_target(temp.path(), PathMode::Native).unwrap();
        assert!(policy.component.limit > 0);
        assert!(!policy.fs_kind.is_empty());
        println!("target evidence: fs={} component={} {:?} {:?} full_path={:?} case_sensitive={} normalization_sensitive={}",policy.fs_kind,policy.component.limit,policy.component.metric,policy.component.confidence,policy.full_path_limit,policy.comparison.case_sensitive,policy.comparison.normalization_sensitive);
        ControlledRoot::open(temp.path())
            .unwrap()
            .verify_identity(&policy)
            .unwrap();
        assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 0);
    }
    #[test]
    fn controlled_creation_is_exclusive_and_never_follows_links() {
        let temp = tempfile::tempdir().unwrap();
        let root = ControlledRoot::open(temp.path()).unwrap();
        root.create_dir("d").unwrap();
        root.create_dir("d").unwrap();
        root.create_file("d/x").unwrap().write_all(b"safe").unwrap();
        assert!(root.create_file("d/x").is_err());
        let external = tempfile::tempdir().unwrap();
        symlink(external.path(), temp.path().join("link")).unwrap();
        assert!(root.create_dir("link").is_err());
        assert!(root.create_file("link/stolen").is_err());
        assert!(!external.path().join("stolen").exists());
        for path in ["../escape", "/escape", "a//x", "a/../x"] {
            assert!(root.create_file(path).is_err());
        }
    }
    #[test]
    fn preflight_removes_file_placeholders_and_cleanup_refuses_data() {
        let temp = tempfile::tempdir().unwrap();
        let policy = probe_target(temp.path(), PathMode::Native).unwrap();
        let root = ControlledRoot::open(temp.path()).unwrap();
        let report = PathMappingReport {
            version: 1,
            digest: String::new(),
            policy,
            entries: vec![PathMappingEntry {
                id: 1,
                source: "a/b".into(),
                display_name: "a/b".into(),
                raw_name: None,
                source_kind: SourceNameKind::BackendText,
                staging_relative: "a/b".into(),
                final_relative: "a/b".into(),
                is_dir: false,
                reasons: vec![],
            }],
            tentative: true,
            manifest_digest: String::new(),
            archive_identity: String::new(),
            adapter_id: String::new(),
            archive_path: String::new(),
            node_id: None,
            generation: None,
        };
        root.verify_namespace(&report).unwrap();
        assert!(temp.path().join("a").is_dir());
        assert!(!temp.path().join("a/b").exists());
        root.clear_verified_namespace().unwrap();
        assert!(!temp.path().join("a").exists());
        root.create_dir("a").unwrap();
        root.create_file("a/b")
            .unwrap()
            .write_all(b"content")
            .unwrap();
        assert!(root.clear_verified_namespace().is_err());
        assert_eq!(std::fs::read(temp.path().join("a/b")).unwrap(), b"content");
    }
    #[test]
    fn single_component_limit_keeps_actual_errno() {
        let temp = tempfile::tempdir().unwrap();
        let root = ControlledRoot::open(temp.path()).unwrap();
        let error = root.create_file(&"x".repeat(1024)).unwrap_err();
        match error {
            SmartZipError::PathConstraint { diagnostic, source } => {
                assert_eq!(diagnostic.reason, PathConstraintReason::NameTooLong);
                assert_eq!(source.unwrap().raw_os_error(), Some(libc::ENAMETOOLONG));
            }
            e => panic!("{e}"),
        }
    }
    #[test]
    fn real_alias_feedback_and_metadata_stay_bound_to_created_identities() {
        use std::os::unix::fs::MetadataExt;
        let temp = tempfile::tempdir().unwrap();
        let policy = probe_target(temp.path(), PathMode::Native).unwrap();
        let root = ControlledRoot::open(temp.path()).unwrap();
        let entries = ["é", "e\u{301}"]
            .into_iter()
            .enumerate()
            .map(|(i, name)| PathMappingEntry {
                id: i as u64,
                source: name.into(),
                display_name: name.into(),
                raw_name: None,
                source_kind: SourceNameKind::BackendText,
                staging_relative: name.into(),
                final_relative: name.into(),
                is_dir: false,
                reasons: vec![],
            })
            .collect();
        let report = PathMappingReport {
            version: 1,
            digest: String::new(),
            policy: policy.clone(),
            entries,
            tentative: true,
            manifest_digest: String::new(),
            archive_identity: String::new(),
            adapter_id: String::new(),
            archive_path: String::new(),
            node_id: None,
            generation: None,
        };
        let calls = std::cell::Cell::new(0usize);
        assert!(matches!(
            root.verify_namespace_with_cancellation(&report, || {
                calls.set(calls.get() + 1);
                calls.get() > 2
            }),
            Err(SmartZipError::Cancelled)
        ));
        assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 0);
        match root.verify_namespace(&report) {
            Ok(()) => assert!(policy.comparison.normalization_sensitive),
            Err(SmartZipError::PathConstraint { diagnostic, .. }) => {
                assert!(!policy.comparison.normalization_sensitive);
                assert_eq!(diagnostic.reason, PathConstraintReason::NameCollision);
                assert!(diagnostic.actual_mapping.is_some());
                assert!(diagnostic.policy.is_some());
            }
            Err(e) => panic!("{e}"),
        }
        assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 0);
        root.create_file("data").unwrap();
        root.apply_metadata("data", Some(0o6754), Some(1_000_000))
            .unwrap();
        let metadata = std::fs::metadata(temp.path().join("data")).unwrap();
        assert_eq!(metadata.mode() & 0o7777, 0o754);
        assert_eq!(metadata.mtime(), 1_000_000);
        std::fs::remove_file(temp.path().join("data")).unwrap();
        let external = tempfile::NamedTempFile::new().unwrap();
        symlink(external.path(), temp.path().join("data")).unwrap();
        assert!(root.apply_metadata("data", Some(0o777), None).is_err());
    }
    #[test]
    fn root_directory_alias_metadata_uses_fixed_root_and_empty_files_stay_invalid() {
        use std::os::unix::fs::MetadataExt;
        let temp = tempfile::tempdir().unwrap();
        let policy = probe_target(temp.path(), PathMode::Native).unwrap();
        let root = ControlledRoot::open(temp.path()).unwrap();
        let report = PathMappingReport {
            version: 1,
            digest: String::new(),
            policy,
            entries: vec![PathMappingEntry {
                id: 1,
                source: "./".into(),
                display_name: "./".into(),
                raw_name: Some(b"./".to_vec()),
                source_kind: SourceNameKind::ZipCentralDirectory,
                staging_relative: String::new(),
                final_relative: String::new(),
                is_dir: true,
                reasons: vec![],
            }],
            tentative: true,
            manifest_digest: String::new(),
            archive_identity: String::new(),
            adapter_id: String::new(),
            archive_path: String::new(),
            node_id: None,
            generation: None,
        };
        root.verify_namespace(&report).unwrap();
        assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 0);
        assert!(matches!(
            root.verify_namespace_with_cancellation(&report, || true),
            Err(SmartZipError::Cancelled)
        ));
        assert!(root.create_dir("").is_err());
        assert!(root.create_file("").is_err());
        root.apply_metadata("", Some(0o1700), Some(1_000_001))
            .unwrap();
        let metadata = std::fs::metadata(temp.path()).unwrap();
        assert_eq!(metadata.mode() & 0o7777, 0o700);
        assert_eq!(metadata.mtime(), 1_000_001);
        let mut invalid = report;
        invalid.entries[0].is_dir = false;
        assert!(root.verify_namespace(&invalid).is_err());
        let container = tempfile::tempdir().unwrap();
        let selected = container.path().join("selected");
        std::fs::create_dir(&selected).unwrap();
        let fixed = ControlledRoot::open(&selected).unwrap();
        std::fs::rename(&selected, container.path().join("old")).unwrap();
        std::fs::create_dir(&selected).unwrap();
        assert!(fixed.apply_metadata("", None, Some(1)).is_err());
    }
    #[test]
    fn restrictive_directory_access_is_restored_parent_first_without_touching_replacements() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let temp = tempfile::tempdir().unwrap();
        let root = ControlledRoot::open(temp.path()).unwrap();
        root.create_dir("parent").unwrap();
        root.create_dir("parent/leaf").unwrap();
        root.apply_metadata("parent/leaf", Some(0o040), None)
            .unwrap();
        root.apply_metadata("parent", Some(0), None).unwrap();
        root.apply_metadata("", Some(0), None).unwrap();
        root.restore_directory_access("").unwrap();
        root.restore_directory_access("parent").unwrap();
        root.restore_directory_access("parent/leaf").unwrap();
        assert_eq!(
            std::fs::metadata(temp.path()).unwrap().mode() & 0o7777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(temp.path().join("parent"))
                .unwrap()
                .mode()
                & 0o7777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(temp.path().join("parent/leaf"))
                .unwrap()
                .mode()
                & 0o7777,
            0o740
        );
        root.create_file("parent/leaf/restored").unwrap();
        std::fs::rename(
            temp.path().join("parent/leaf"),
            temp.path().join("parent/original"),
        )
        .unwrap();
        std::fs::create_dir(temp.path().join("parent/leaf")).unwrap();
        std::fs::set_permissions(
            temp.path().join("parent/leaf"),
            std::fs::Permissions::from_mode(0o400),
        )
        .unwrap();
        assert!(root.restore_directory_access("parent/leaf").is_err());
        assert_eq!(
            std::fs::metadata(temp.path().join("parent/leaf"))
                .unwrap()
                .mode()
                & 0o7777,
            0o400
        );
        std::fs::remove_dir(temp.path().join("parent/leaf")).unwrap();
        let external = tempfile::tempdir().unwrap();
        std::fs::set_permissions(external.path(), std::fs::Permissions::from_mode(0o400)).unwrap();
        symlink(external.path(), temp.path().join("parent/leaf")).unwrap();
        assert!(root.restore_directory_access("parent/leaf").is_err());
        assert_eq!(
            std::fs::metadata(external.path()).unwrap().mode() & 0o7777,
            0o400
        );
        std::fs::set_permissions(external.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let container = tempfile::tempdir().unwrap();
        let selected = container.path().join("selected");
        std::fs::create_dir(&selected).unwrap();
        let fixed = ControlledRoot::open(&selected).unwrap();
        std::fs::rename(&selected, container.path().join("old")).unwrap();
        std::fs::create_dir(&selected).unwrap();
        std::fs::set_permissions(&selected, std::fs::Permissions::from_mode(0o400)).unwrap();
        assert!(fixed.restore_directory_access("").is_err());
        assert_eq!(std::fs::metadata(&selected).unwrap().mode() & 0o7777, 0o400);
    }
    #[test]
    fn crashed_staging_tree_access_recovers_without_following_injected_links() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let container = tempfile::tempdir().unwrap();
        let stage = container.path().join(".smartzip-recover");
        std::fs::create_dir(&stage).unwrap();
        std::fs::create_dir_all(stage.join("parent/leaf")).unwrap();
        std::fs::write(stage.join("parent/leaf/data"), b"preserved").unwrap();
        for path in [
            stage.join("parent/leaf"),
            stage.join("parent"),
            stage.clone(),
        ] {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0)).unwrap();
        }
        restore_staging_access(&stage, &["parent/leaf".into(), String::new()]).unwrap();
        assert_eq!(
            std::fs::read(stage.join("parent/leaf/data")).unwrap(),
            b"preserved"
        );
        for path in [&stage, &stage.join("parent"), &stage.join("parent/leaf")] {
            assert_eq!(std::fs::metadata(path).unwrap().mode() & 0o700, 0o700);
        }
        let outside = tempfile::tempdir().unwrap();
        std::fs::set_permissions(outside.path(), std::fs::Permissions::from_mode(0o400)).unwrap();
        symlink(outside.path(), stage.join("injected")).unwrap();
        assert!(restore_staging_access(&stage, &["injected/child".into()]).is_err());
        assert_eq!(
            std::fs::metadata(outside.path()).unwrap().mode() & 0o7777,
            0o400
        );
        std::fs::set_permissions(outside.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let root_link = container.path().join(".smartzip-link");
        symlink(outside.path(), &root_link).unwrap();
        assert!(restore_staging_access(&root_link, &[]).is_err());
        std::fs::set_permissions(&stage, std::fs::Permissions::from_mode(0)).unwrap();
        assert!(restore_staging_access(&stage, &["../outside".into()]).is_err());
        assert_eq!(std::fs::metadata(&stage).unwrap().mode() & 0o7777, 0);
        restore_staging_access(&stage, &[]).unwrap();
    }
}
