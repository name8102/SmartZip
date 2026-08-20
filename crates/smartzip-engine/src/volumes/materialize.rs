use smartzip_core::ArchiveFormat;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// RAII staging object for resolved volume sets.
/// Creates canonical filenames, prefers CoW/reflink, fallback copy, never renames originals.
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

pub fn materialize_volume_set(
    set: &super::VolumeSet,
) -> io::Result<MaterializedVolumeSet> {
    if set.members.is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "empty volume set"));
    }
    let source_dir = set.members[0]
        .path
        .parent()
        .unwrap_or_else(|| Path::new("."));
    // Try to stage on same filesystem as source when practical to preserve CoW.
    let staging = create_staging_dir(source_dir)?;

    let staging_path = staging.path().to_path_buf();
    let mut canonical_members = Vec::new();
    // Use a stable stem for canonical naming: derive from first member's stem without ordinal?
    // Simplify: use "payload" or original stem? Design example uses "payload". We'll use first member's file stem without numbers?
    // Let's derive canonical stem as e.g., "archive" -> strip volume suffix? For now use "payload".
    let canonical_stem = "payload";

    // Members are already sorted by filename_ordinal.
    let total = set.members.len();
    for (idx, member) in set.members.iter().enumerate() {
        let seq = idx + 1; // 1-based canonical order
        let canonical_name = if set.format == ArchiveFormat::Zip {
            if seq == total {
                format!("{canonical_stem}.zip")
            } else {
                format!("{canonical_stem}.z{:02}", seq)
            }
        } else {
            canonical_name_for(&set.format, canonical_stem, seq, member.logical_index)
        };
        let dest = staging_path.join(&canonical_name);
        cow_copy(&member.path, &dest)?;
        canonical_members.push(dest);
    }

    // Entrypoint depends on format: for ZIP, last disk contains EOCD, so entrypoint is last.
    // For 7z and RAR, first.
    let entry_idx = match set.format {
        ArchiveFormat::Zip => canonical_members.len().saturating_sub(1),
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

fn create_staging_dir(source_dir: &Path) -> io::Result<tempfile::TempDir> {
    // Try to create temp dir inside source_dir's filesystem for CoW.
    // If source_dir is not writable or similar, fallback to temp_dir.
    match tempfile::Builder::new()
        .prefix("smartzip-volume-")
        .tempdir_in(source_dir)
    {
        Ok(dir) => Ok(dir),
        Err(_) => tempfile::Builder::new()
            .prefix("smartzip-volume-")
            .tempdir(),
    }
}

fn canonical_name_for(format: &ArchiveFormat, stem: &str, seq: usize, _logical_index: Option<u32>) -> String {
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

// Wrapper that tries CoW reflink before falling back to copy.
fn cow_copy(src: &Path, dst: &Path) -> io::Result<()> {
    // Try reflink via `reflink` crate semantics? We don't have that crate, so try manual ioctl on Linux/macOS.
    #[cfg(target_os = "linux")]
    {
        if try_reflink_linux(src, dst).is_ok() {
            return Ok(());
        }
    }
    #[cfg(target_os = "macos")]
    {
        if try_clonefile_macos(src, dst).is_ok() {
            return Ok(());
        }
    }
    // Fallback regular copy
    fs::copy(src, dst).map(|_| ())
}

#[cfg(target_os = "linux")]
fn try_reflink_linux(src: &Path, dst: &Path) -> io::Result<()> {
    use std::os::unix::io::AsRawFd;
    let src_file = fs::File::open(src)?;
    let dst_file = fs::File::create(dst)?;
    let src_fd = src_file.as_raw_fd();
    let dst_fd = dst_file.as_raw_fd();
    // FICLONE = _IOW(0x94, 9, int) = 0x40049409
    const FICLONE: libc::c_ulong = 0x40049409;
    let ret = unsafe { libc::ioctl(dst_fd, FICLONE as _, src_fd) };
    if ret == 0 {
        Ok(())
    } else {
        let err = io::Error::last_os_error();
        // Clean up failed dst
        let _ = fs::remove_file(dst);
        Err(err)
    }
}

#[cfg(target_os = "macos")]
fn try_clonefile_macos(src: &Path, dst: &Path) -> io::Result<()> {
    // Use clonefile(2) via libc if available? On macOS, clonefile is not in libc; we can use fclonefileat? Simplify: try std::fs::copy with APFS CoW may already? Just fallback.
    // Attempt via `std::process::Command::new("cp").arg("-c")`? Not.
    // We'll try using `nix`? Instead, just fail to fallback.
    Err(io::Error::new(io::ErrorKind::Unsupported, "clonefile not implemented, fallback"))
}
