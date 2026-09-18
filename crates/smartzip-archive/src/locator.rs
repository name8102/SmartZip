use std::path::{Path, PathBuf};
#[cfg(any(target_os = "macos", test))]
use std::{ffi::OsStr, path::Component};

pub(crate) fn usable_executable(path: &Path) -> Option<PathBuf> {
    if !path.is_file() {
        return None;
    }
    #[cfg(unix)]
    if std::os::unix::fs::MetadataExt::mode(&std::fs::metadata(path).ok()?) & 0o111 == 0 {
        return None;
    }
    Some(std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()))
}

pub(crate) fn ordered_executables(
    path_candidates: impl IntoIterator<Item = PathBuf>,
    fallback_candidates: impl IntoIterator<Item = PathBuf>,
) -> Vec<PathBuf> {
    let mut result = Vec::new();
    for candidate in path_candidates.into_iter().chain(fallback_candidates) {
        if let Some(candidate) = usable_executable(&candidate) {
            if !result.contains(&candidate) {
                result.push(candidate);
            }
        }
    }
    result
}

#[cfg(target_os = "macos")]
pub(crate) fn known_macos_executables(candidate: &str) -> Vec<PathBuf> {
    let mut dirs = vec![
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
        PathBuf::from("/opt/local/bin"),
        PathBuf::from("/run/current-system/sw/bin"),
        PathBuf::from("/nix/var/nix/profiles/default/bin"),
    ];
    if let Some(home) = std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
    {
        dirs.push(home.join(".nix-profile/bin"));
    }
    if let Some(user) = std::env::var_os("USER").or_else(|| std::env::var_os("USERNAME")) {
        if valid_user_component(&user) {
            dirs.push(
                PathBuf::from("/etc/profiles/per-user")
                    .join(user)
                    .join("bin"),
            );
        }
    }
    dirs.into_iter().map(|dir| dir.join(candidate)).collect()
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn known_macos_executables(_candidate: &str) -> Vec<PathBuf> {
    Vec::new()
}

#[cfg(any(target_os = "macos", test))]
fn valid_user_component(user: &OsStr) -> bool {
    let mut components = Path::new(user).components();
    matches!(components.next(), Some(Component::Normal(_))) && components.next().is_none()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    #[cfg(unix)]
    #[test]
    fn orders_path_before_fallback_and_deduplicates_real_files() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("path-tool");
        let fallback = temp.path().join("fallback-tool");
        let non_executable = temp.path().join("non-executable");
        let directory = temp.path().join("directory");
        std::fs::write(&path, b"path").unwrap();
        std::fs::write(&fallback, b"fallback").unwrap();
        std::fs::write(&non_executable, b"no").unwrap();
        std::fs::create_dir(&directory).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::set_permissions(&fallback, std::fs::Permissions::from_mode(0o755)).unwrap();
        let link = temp.path().join("same-tool");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        let result = ordered_executables(
            vec![path.clone(), temp.path().join("missing"), link],
            vec![
                non_executable,
                directory,
                fallback.clone(),
                path.clone(),
                temp.path().join("directory"),
            ],
        );
        assert_eq!(
            result,
            vec![
                std::fs::canonicalize(path).unwrap(),
                std::fs::canonicalize(fallback).unwrap()
            ]
        );
    }

    #[test]
    fn rejects_unsafe_user_components() {
        assert!(!valid_user_component(OsStr::new("a/b")));
        assert!(!valid_user_component(OsStr::new("/absolute")));
        assert!(!valid_user_component(OsStr::new("..")));
        assert!(valid_user_component(OsStr::new("charl")));
    }
}
