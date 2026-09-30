//! Detect member aliases using the filesystem receiving extraction output.
use crate::ArchiveEntry;
use smartzip_core::{Result, SmartZipError};
use std::{
    collections::HashSet,
    fs, io,
    path::{Path, PathBuf},
};
use tokio_util::sync::CancellationToken;

// Match the archive member/directory record budget. A probe consumes no member
// data, but its temporary inode count and path metadata must also remain bounded.
const MAX_COMPONENTS: usize = 100_000;

pub(crate) async fn validate_async(
    entries: Vec<ArchiveEntry>,
    output: PathBuf,
    token: &CancellationToken,
) -> Result<()> {
    let token = token.child_token();
    let _cancel_on_drop = token.clone().drop_guard();
    tokio::task::spawn_blocking(move || validate(&entries, &output, &token))
        .await
        .map_err(|e| SmartZipError::BackendProtocolError {
            backend: "member-name-validator".into(),
            detail: e.to_string(),
        })?
}

pub(crate) fn validate(
    entries: &[ArchiveEntry],
    output: &Path,
    token: &CancellationToken,
) -> Result<()> {
    if token.is_cancelled() {
        return Err(SmartZipError::Cancelled);
    }
    fs::create_dir_all(output).map_err(|e| SmartZipError::io(Some(output.into()), e))?;
    // A sibling on another volume would give the wrong equivalence rules.
    let probe = tempfile::Builder::new()
        .prefix(".smartzip-name-check-")
        .tempdir_in(output)
        .map_err(|e| SmartZipError::io(Some(output.into()), e))?;
    let result = check(entries, probe.path(), token);
    // A failed cleanup must stop extraction as well; drop remains a backup.
    probe
        .close()
        .map_err(|e| SmartZipError::io(Some(output.into()), e))?;
    result
}

fn check(entries: &[ArchiveEntry], probe: &Path, token: &CancellationToken) -> Result<()> {
    let mut directories = HashSet::new();
    let mut components = 0usize;
    let mut metadata_bytes = 0usize;
    for entry in entries {
        if token.is_cancelled() {
            return Err(SmartZipError::Cancelled);
        }
        // Root-only TAR directories do not create a named member.
        if entry.is_dir
            && entry
                .path
                .components()
                .all(|c| c == std::path::Component::CurDir)
            && !entry.path.as_os_str().is_empty()
        {
            continue;
        }
        let name = entry.path.to_string_lossy();
        metadata_bytes = metadata_bytes.saturating_add(name.len());
        if metadata_bytes > crate::process::MAX_OUTPUT {
            return Err(SmartZipError::ResourceLimit {
                detail: "member name validation exceeded 16 MiB of path metadata".into(),
            });
        }
        let relative = crate::safety::safe_entry_path(name.as_bytes()).ok_or_else(|| {
            SmartZipError::UnsafeArchivePath {
                entry: name.to_string(),
            }
        })?;
        let parts = relative.components().collect::<Vec<_>>();
        let mut path = PathBuf::new();
        for (index, part) in parts.iter().enumerate() {
            if token.is_cancelled() {
                return Err(SmartZipError::Cancelled);
            }
            path.push(part.as_os_str());
            let directory = index + 1 < parts.len() || entry.is_dir;
            if directory && directories.contains(&path) {
                continue;
            }
            components += 1;
            if components > MAX_COMPONENTS {
                return Err(SmartZipError::ResourceLimit {
                    detail: "member name validation exceeded 100000 filesystem components".into(),
                });
            }
            let target = probe.join(&path);
            let created = if directory {
                fs::create_dir(&target)
            } else {
                fs::File::options()
                    .write(true)
                    .create_new(true)
                    .open(&target)
                    .map(|_| ())
            };
            match created {
                Ok(()) => {
                    if directory {
                        directories.insert(path.clone());
                    }
                }
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::AlreadyExists | io::ErrorKind::NotADirectory
                    ) =>
                {
                    return Err(SmartZipError::UnsafeArchivePath {
                        entry: format!("member name collides on the output filesystem: {name}"),
                    });
                }
                Err(e) => return Err(SmartZipError::io(Some(entry.path.clone()), e)),
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn entry(name: &str, is_dir: bool) -> ArchiveEntry {
        ArchiveEntry {
            path: name.into(),
            raw_name: Vec::new(),
            compressed_size: None,
            uncompressed_size: None,
            is_dir,
        }
    }
    #[test]
    fn checks_names_on_output_volume_and_removes_its_probe() {
        let output = tempfile::tempdir().unwrap();
        fs::write(output.path().join("old.txt"), "old").unwrap();
        for (a, b) in [("Readme.txt", "README.txt"), ("é.txt", "e\u{301}.txt")] {
            let control = tempfile::tempdir_in(output.path()).unwrap();
            fs::write(control.path().join(a), []).unwrap();
            let aliases = control.path().join(b).exists();
            control.close().unwrap();
            let result = validate(
                &[entry(a, false), entry(b, false)],
                output.path(),
                &CancellationToken::new(),
            );
            assert_eq!(result.is_err(), aliases, "{a:?} / {b:?}");
            if aliases {
                assert!(matches!(
                    result,
                    Err(SmartZipError::UnsafeArchivePath { .. })
                ));
            }
            assert_eq!(
                fs::read_to_string(output.path().join("old.txt")).unwrap(),
                "old"
            );
            assert_eq!(fs::read_dir(output.path()).unwrap().count(), 1);
        }
        for entries in [
            vec![entry("same", false), entry("same", false)],
            vec![entry("a", false), entry("a/b", false)],
            vec![entry("a/b", false), entry("a", false)],
        ] {
            assert!(matches!(
                validate(&entries, output.path(), &CancellationToken::new()),
                Err(SmartZipError::UnsafeArchivePath { .. })
            ));
            assert_eq!(fs::read_dir(output.path()).unwrap().count(), 1);
        }
        validate(
            &[entry("a/b", false), entry("a", true)],
            output.path(),
            &CancellationToken::new(),
        )
        .unwrap();
        let token = CancellationToken::new();
        token.cancel();
        assert!(matches!(
            validate(&[entry("new", false)], output.path(), &token),
            Err(SmartZipError::Cancelled)
        ));
        assert_eq!(fs::read_dir(output.path()).unwrap().count(), 1);
    }

    #[tokio::test]
    async fn cancellation_during_probe_cleans_every_temporary_member() {
        let output = tempfile::tempdir().unwrap();
        fs::write(output.path().join("old.txt"), "old").unwrap();
        let entries = (0..20_000)
            .map(|i| entry(&format!("file-{i}.txt"), false))
            .collect();
        let token = CancellationToken::new();
        let cancelled = token.clone();
        let path = output.path().to_path_buf();
        let pending = tokio::spawn(async move { validate_async(entries, path, &cancelled).await });
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            while fs::read_dir(output.path()).unwrap().count() == 1 {
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap();
        token.cancel();
        assert!(matches!(
            tokio::time::timeout(std::time::Duration::from_secs(3), pending)
                .await
                .unwrap()
                .unwrap(),
            Err(SmartZipError::Cancelled)
        ));
        assert_eq!(fs::read_dir(output.path()).unwrap().count(), 1);
        assert_eq!(
            fs::read_to_string(output.path().join("old.txt")).unwrap(),
            "old"
        );
    }

    #[test]
    fn metadata_budget_failure_cleans_the_probe_before_returning() {
        let output = tempfile::tempdir().unwrap();
        let entries = vec![entry(&"a".repeat(crate::process::MAX_OUTPUT + 1), false)];
        assert!(matches!(
            validate(&entries, output.path(), &CancellationToken::new()),
            Err(SmartZipError::ResourceLimit { .. })
        ));
        assert_eq!(fs::read_dir(output.path()).unwrap().count(), 0);
    }

    #[test]
    #[ignore = "filesystem cost evidence, run explicitly on the output volume"]
    fn probe_cost_for_3000_flat_members() {
        let entries = (0..3000)
            .map(|i| entry(&format!("file-{i:05}.txt"), false))
            .collect::<Vec<_>>();
        let mut milliseconds = Vec::new();
        for _ in 0..7 {
            let output = tempfile::tempdir().unwrap();
            let start = std::time::Instant::now();
            validate(&entries, output.path(), &CancellationToken::new()).unwrap();
            milliseconds.push(start.elapsed().as_secs_f64() * 1000.0);
            assert_eq!(fs::read_dir(output.path()).unwrap().count(), 0);
        }
        milliseconds.sort_by(f64::total_cmp);
        println!("3000-member validation and complete probe cleanup: samples_ms={milliseconds:?}, median_ms={}", milliseconds[3]);
    }
}
