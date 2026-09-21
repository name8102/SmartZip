use smartzip_core::ArchiveFormat;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// RAII staging object for resolved volume sets.
/// Creates canonical symbolic links without copying or renaming source volumes.
pub struct MaterializedVolumeSet {
    pub staging_dir: PathBuf,
    pub canonical_entrypoint: PathBuf,
    pub canonical_members: Vec<PathBuf>,
    // Keep temp dir handle for cleanup if we created via tempfile? We'll manage manually.
    _temp_handle: Option<tempfile::TempDir>,
}

impl Drop for MaterializedVolumeSet {
    fn drop(&mut self) {
        // Best-effort cleanup. If we used TempDir, it auto-deletes. Otherwise manual.
        if self._temp_handle.is_none() {
            let _ = fs::remove_dir_all(&self.staging_dir);
        }
    }
}

#[derive(Debug, Clone)]
pub struct VolumeSetForMaterialize {
    pub format: ArchiveFormat,
    pub members: Vec<super::VolumeMember>,
}

pub fn materialize_volume_set(set: &super::VolumeSet) -> io::Result<MaterializedVolumeSet> {
    if set.members.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "empty volume set",
        ));
    }
    let source_dir = set.members[0]
        .path
        .parent()
        .unwrap_or_else(|| Path::new("."));
    // Keep relative links next to the originals when the source allows it.
    // A local link directory handles read-only shares or unsupported links.
    if let Ok(staging) = tempfile::Builder::new()
        .prefix(".smartzip-volume-")
        .tempdir_in(source_dir)
    {
        if let Ok(materialized) = materialize_with_staging(set, staging) {
            return Ok(materialized);
        }
    }
    let staging = tempfile::Builder::new()
        .prefix(".smartzip-volume-")
        .tempdir()?;
    materialize_with_staging(set, staging)
}

fn materialize_with_staging(
    set: &super::VolumeSet,
    staging: tempfile::TempDir,
) -> io::Result<MaterializedVolumeSet> {
    let staging_path = staging.path().to_path_buf();
    let mut canonical_members = Vec::new();
    // Use a stable stem for canonical naming: derive from first member's stem without ordinal?
    // Simplify: use "payload" or original stem? Design example uses "payload". We'll use first member's file stem without numbers?
    // Let's derive canonical stem as e.g., "archive" -> strip volume suffix? For now use "payload".
    let canonical_stem = "payload";

    // For ZIP, use resolver-determined split mechanism (Spanned vs Raw) rather than re-guessing via filename.
    let zip_is_raw_split = matches!(set.zip_kind, Some(crate::volumes::ZipSplitKind::Raw));
    let total = set.members.len();
    for (idx, member) in set.members.iter().enumerate() {
        let seq = idx + 1; // 1-based canonical order
        let canonical_name = if set.format == ArchiveFormat::Zip {
            if zip_is_raw_split {
                format!("{canonical_stem}.zip.{:03}", seq)
            } else if seq == total {
                format!("{canonical_stem}.zip")
            } else {
                format!("{canonical_stem}.z{:02}", seq)
            }
        } else {
            canonical_name_for(&set.format, canonical_stem, seq, member.logical_index)
        };
        let dest = staging_path.join(&canonical_name);
        link_input(&member.path, &dest)?;
        canonical_members.push(dest);
    }

    // Entrypoint depends on format and split style: ZIP spanned last contains EOCD, ZIP raw split first is entry, 7z/RAR first.
    let entry_idx = match set.format {
        ArchiveFormat::Zip => {
            if zip_is_raw_split {
                0
            } else {
                canonical_members.len().saturating_sub(1)
            }
        }
        _ => 0,
    };
    let canonical_entrypoint = canonical_members[entry_idx].clone();

    // Keep staging handle for RAII; TempDir will delete on drop.
    let materialized = MaterializedVolumeSet {
        staging_dir: staging_path,
        canonical_entrypoint,
        canonical_members,
        _temp_handle: Some(staging),
    };
    Ok(materialized)
}

fn link_input(source: &Path, destination: &Path) -> io::Result<()> {
    let source = fs::canonicalize(source)?;
    let staging_parent = destination
        .parent()
        .and_then(Path::parent)
        .map(fs::canonicalize)
        .transpose()?;
    let link_target = if source.parent() == staging_parent.as_deref() {
        // Relative paths also work when the remote server resolves the link;
        // it need not know the client's mount point.
        Path::new("..").join(
            source
                .file_name()
                .ok_or_else(|| io::Error::other("volume has no filename"))?,
        )
    } else {
        source.clone()
    };
    #[cfg(unix)]
    std::os::unix::fs::symlink(&link_target, destination)?;
    #[cfg(windows)]
    std::os::windows::fs::symlink_file(&link_target, destination)?;
    #[cfg(not(any(unix, windows)))]
    return Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "volume aliases require symbolic link support; data copying is disabled",
    ));

    // Some remote filesystems accept link creation but cannot follow it.
    // Check access before handing the canonical name to the backend.
    let metadata = fs::metadata(destination)?;
    if !metadata.is_file() {
        return Err(io::Error::other("volume alias does not resolve to a file"));
    }
    Ok(())
}

fn canonical_name_for(
    format: &ArchiveFormat,
    stem: &str,
    seq: usize,
    _logical_index: Option<u32>,
) -> String {
    match format {
        ArchiveFormat::SevenZip => format!("{stem}.7z.{:03}", seq),
        ArchiveFormat::Rar => format!("{stem}.part{:02}.rar", seq),
        ArchiveFormat::Zip => {
            // For ZIP split: first volumes .z01, .z02 etc., last .zip
            // We will generate .zXX for all but last which is .zip
            // However materialize currently generates all members; last should be .zip
            // But we need to know if this seq is last? We'll need total count.
            // For simplicity, generate .zip.001 style? To keep deterministic, we'll generate .zXX plus final .zip handling is done by caller? For now generate generic .zip.{:03} to ensure 7zz finds them via generic split naming? It may not.
            // To preserve backend compatibility, we generate .zXX for sequential, last as .zip.
            // But we don't know total at this function call per member; we handle via seq placeholder: if format is Zip, we generate .z01 etc., but entrypoint expects .zip.
            // We'll generate .zXX for all, and later rename last to .zip if needed? Easier: generate .z01, .z02, ..., .zip for last.
            // The caller will need to map correctly. This per-member function cannot know total.
            // So we will generate placeholder; the loop above will handle collectively by checking seq.
            // We'll hack: if seq < 1000, use .z{:02} for non-last but need last detection. So we need volume set length.
            // Instead, we generate generic zip split naming .zip.{:03} as fallback – 7zz may still handle via .zip.001? Might need testing.
            // We'll currently generate zip as .zip.{:03} to keep uniform (splits use .zip.001). Many tools accept .zip.001.
            format!("{stem}.zip.{:03}", seq)
        }
        _ => format!("{stem}.vol{:03}", seq),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn volume_aliases_are_relative_links_without_copying_sources() {
        let source = tempfile::tempdir().unwrap();
        let paths: Vec<_> = (1..=2)
            .map(|n| source.path().join(format!("archive.7z.{n:03}")))
            .collect();
        for path in &paths {
            fs::write(path, b"original volume").unwrap();
        }
        let set = crate::volumes::VolumeSet {
            format: ArchiveFormat::SevenZip,
            entrypoint: paths[0].clone(),
            members: paths
                .iter()
                .enumerate()
                .map(|(n, path)| crate::volumes::VolumeMember {
                    path: path.clone(),
                    filename_ordinal: Some(n as u64 + 1),
                    logical_index: None,
                })
                .collect(),
            expected_volume_count: None,
            expected_logical_size: None,
            zip_kind: None,
        };
        let materialized = materialize_volume_set(&set).unwrap();
        assert_eq!(materialized.staging_dir.parent(), Some(source.path()));
        assert_eq!(fs::read_dir(source.path()).unwrap().count(), 3);
        for path in &materialized.canonical_members {
            assert_eq!(fs::read(path).unwrap(), b"original volume");
            assert!(fs::symlink_metadata(path).unwrap().file_type().is_symlink());
            assert!(fs::read_link(path).unwrap().starts_with(".."));
        }
        let staging = materialized.staging_dir.clone();
        drop(materialized);
        assert!(!staging.exists());
        for path in &paths {
            assert_eq!(fs::read(path).unwrap(), b"original volume");
        }

        assert_eq!(fs::read_dir(source.path()).unwrap().count(), 2);
        // A local alias fallback uses the absolute source path, with no copies.
        let local = tempfile::tempdir().unwrap();
        let fallback = materialize_with_staging(&set, local).unwrap();
        for path in &fallback.canonical_members {
            assert!(fs::symlink_metadata(path).unwrap().file_type().is_symlink());
            assert!(fs::read_link(path).unwrap().is_absolute());
            assert_eq!(fs::read(path).unwrap(), b"original volume");
        }
        let fallback_dir = fallback.staging_dir.clone();
        drop(fallback);
        assert!(!fallback_dir.exists());
    }
}
