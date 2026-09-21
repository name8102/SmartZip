//! Narrow path for ZIP names that the external backend cannot decode faithfully.
//! Ordinary UTF-8 archives continue through the configured backend.
use crate::{ArchiveEntry, ExtractArchiveRequest};
use smartzip_core::{EncodingMode, Result, SmartZipError};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
};
use tokio_util::sync::CancellationToken;

fn zip_error(error: zip::result::ZipError, path: &Path) -> SmartZipError {
    match error {
        zip::result::ZipError::UnsupportedArchive(zip::result::ZipError::PASSWORD_REQUIRED) => {
            SmartZipError::PasswordRequired { path: path.into() }
        }
        zip::result::ZipError::InvalidPassword => {
            SmartZipError::WrongPassword { path: path.into() }
        }
        zip::result::ZipError::UnsupportedArchive(reason) => SmartZipError::UnsupportedCodec {
            backend: "zip-name-decoder".into(),
            path: path.into(),
            codec: Some(reason.into()),
        },
        error => SmartZipError::CorruptedArchive {
            path: path.into(),
            detail: error.to_string(),
        },
    }
}

pub(crate) fn entries(path: &Path, encoding: &str) -> Result<(Vec<ArchiveEntry>, bool)> {
    let mut headers = File::open(path).map_err(|e| SmartZipError::io(Some(path.into()), e))?;
    let mut archive = zip::ZipArchive::new(
        File::open(path).map_err(|e| SmartZipError::io(Some(path.into()), e))?,
    )
    .map_err(|e| zip_error(e, path))?;
    let mut result = Vec::with_capacity(archive.len());
    let mut legacy = false;
    for index in 0..archive.len() {
        let entry = archive
            .by_index_raw(index)
            .map_err(|e| zip_error(e, path))?;
        headers
            .seek(SeekFrom::Start(entry.central_header_start() + 8))
            .map_err(|e| SmartZipError::io(Some(path.into()), e))?;
        let mut flags = [0; 2];
        headers
            .read_exact(&mut flags)
            .map_err(|e| SmartZipError::io(Some(path.into()), e))?;
        let utf8 = u16::from_le_bytes(flags) & 0x800 != 0;
        // zip validates Unicode path extras and exposes their name in name_raw.
        let unicode_extra = entry.extra_data().is_some_and(|mut extra| {
            while extra.len() >= 4 {
                let id = u16::from_le_bytes([extra[0], extra[1]]);
                let len = u16::from_le_bytes([extra[2], extra[3]]) as usize;
                if extra.len() < 4 + len {
                    break;
                }
                if id == 0x7075 {
                    return true;
                }
                extra = &extra[4 + len..];
            }
            false
        });
        let name = if utf8 || unicode_extra {
            std::str::from_utf8(entry.name_raw())
                .ok()
                .map(str::to_owned)
        } else {
            legacy |= !entry.name_raw().is_ascii();
            smartzip_encoding::decode_name(entry.name_raw(), encoding)
        }
        .ok_or_else(|| SmartZipError::BackendProtocolError {
            backend: "zip-name-decoder".into(),
            detail: format!("ZIP name cannot be decoded as {encoding}"),
        })?;
        result.push(ArchiveEntry {
            path: name.into(),
            raw_name: entry.name_raw().to_vec(),
            compressed_size: Some(entry.compressed_size()),
            uncompressed_size: Some(entry.size()),
            is_dir: entry.is_dir(),
        });
    }
    Ok((result, legacy))
}

pub(crate) async fn extract_if_needed(
    request: &ExtractArchiveRequest,
    listed_count: usize,
    token: &CancellationToken,
) -> Result<bool> {
    let EncodingMode::Override(encoding) = &request.encoding else {
        return Ok(false);
    };
    // Non-ZIP containers and unresolved split ZIPs retain the external path.
    let file = File::open(&request.archive)
        .map_err(|e| SmartZipError::io(Some(request.archive.clone()), e))?;
    if zip::ZipArchive::new(file).is_err() {
        return Ok(false);
    }
    let (entries, legacy) = entries(&request.archive, encoding)?;
    if !legacy {
        return Ok(false);
    }
    if entries.len() != listed_count {
        return Err(SmartZipError::BackendProtocolError {
            backend: "zip-name-decoder".into(),
            detail: "ambiguous ZIP directory".into(),
        });
    }
    let request = request.clone();
    let token = token.child_token();
    let _cancel_on_drop = token.clone().drop_guard();
    tokio::task::spawn_blocking(move || extract(request, entries, &token))
        .await
        .map_err(|e| SmartZipError::BackendProtocolError {
            backend: "zip-name-decoder".into(),
            detail: e.to_string(),
        })??;
    Ok(true)
}

fn extract(
    request: ExtractArchiveRequest,
    entries: Vec<ArchiveEntry>,
    token: &CancellationToken,
) -> Result<()> {
    let mut archive = zip::ZipArchive::new(
        File::open(&request.archive)
            .map_err(|e| SmartZipError::io(Some(request.archive.clone()), e))?,
    )
    .map_err(|e| zip_error(e, &request.archive))?;
    let mut names = std::collections::HashSet::new();
    let mut safe = Vec::with_capacity(entries.len());
    // Validate every decoded name and file type before creating output.
    for (index, entry) in entries.iter().enumerate() {
        if token.is_cancelled() {
            return Err(SmartZipError::Cancelled);
        }
        let name = entry.path.to_string_lossy();
        let path = crate::safety::safe_entry_path(name.as_bytes()).ok_or_else(|| {
            SmartZipError::UnsafeArchivePath {
                entry: name.to_string(),
            }
        })?;
        let raw = archive
            .by_index_raw(index)
            .map_err(|e| zip_error(e, &request.archive))?;
        if raw
            .unix_mode()
            .is_some_and(|mode| !matches!(mode & 0o170000, 0 | 0o040000 | 0o100000 | 0o120000))
        {
            return Err(SmartZipError::UnsafeArchivePath {
                entry: name.to_string(),
            });
        }
        if !names.insert(path.clone()) {
            return Err(SmartZipError::UnsafeArchivePath {
                entry: format!("duplicate decoded ZIP name: {name}"),
            });
        }
        safe.push(path);
    }
    let mut directories = std::collections::HashSet::new();
    let mut links = Vec::new();
    for (index, relative) in safe.iter().enumerate() {
        if token.is_cancelled() {
            return Err(SmartZipError::Cancelled);
        }
        let mut entry = match request.password.as_deref() {
            Some(password) => archive.by_index_decrypt(index, password.as_bytes()),
            None => archive.by_index(index),
        }
        .map_err(|e| zip_error(e, &request.archive))?;
        let target = request.output_dir.join(relative);
        let parent = if entry.is_dir() {
            target.as_path()
        } else {
            target.parent().unwrap_or(&request.output_dir)
        };
        if directories.insert(parent.to_path_buf()) {
            std::fs::create_dir_all(parent)
                .map_err(|e| SmartZipError::io(Some(parent.into()), e))?;
        }
        if entry.is_dir() {
            continue;
        }
        if entry
            .unix_mode()
            .is_some_and(|mode| mode & 0o170000 == 0o120000)
        {
            let mut destination = Vec::new();
            entry
                .read_to_end(&mut destination)
                .map_err(|e| SmartZipError::io(Some(target.clone()), e))?;
            links.push((target, destination));
            continue;
        }
        // Links are created after regular files, so output writes cannot follow them.
        // create_new also refuses decoded name collisions.
        let mut output = File::options()
            .write(true)
            .create_new(true)
            .open(&target)
            .map_err(|e| SmartZipError::io(Some(target.clone()), e))?;
        let encrypted = entry.encrypted();
        let mut buffer = [0; 64 * 1024];
        loop {
            if token.is_cancelled() {
                return Err(SmartZipError::Cancelled);
            }
            let count = entry.read(&mut buffer).map_err(|e| {
                if encrypted {
                    SmartZipError::PasswordIndeterminate {
                        path: request.archive.clone(),
                    }
                } else {
                    SmartZipError::CorruptedArchive {
                        path: request.archive.clone(),
                        detail: e.to_string(),
                    }
                }
            })?;
            if count == 0 {
                break;
            }
            output
                .write_all(&buffer[..count])
                .map_err(|e| SmartZipError::io(Some(target.clone()), e))?;
        }
    }
    for (target, destination) in links {
        if token.is_cancelled() {
            return Err(SmartZipError::Cancelled);
        }
        create_symlink(&destination, &target).map_err(|e| SmartZipError::io(Some(target), e))?;
    }
    Ok(())
}

fn create_symlink(destination: &[u8], target: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        std::os::unix::fs::symlink(std::ffi::OsStr::from_bytes(destination), target)
    }
    #[cfg(windows)]
    {
        let destination = std::str::from_utf8(destination)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let resolved = target.parent().unwrap_or(Path::new(".")).join(destination);
        if resolved.is_dir() {
            std::os::windows::fs::symlink_dir(destination, target)
        } else {
            std::os::windows::fs::symlink_file(destination, target)
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (destination, target);
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "symlinks are unsupported",
        ))
    }
}

/// Read a decoded legacy ZIP member, with the same bounds as external previews.
pub(crate) async fn read_member_if_needed(
    request: &crate::MemberReadRequest,
    listed_count: usize,
    token: &CancellationToken,
    deadline: tokio::time::Instant,
) -> Result<Option<Vec<u8>>> {
    let EncodingMode::Override(encoding) = &request.encoding else {
        return Ok(None);
    };
    let file = File::open(&request.archive)
        .map_err(|e| SmartZipError::io(Some(request.archive.clone()), e))?;
    if zip::ZipArchive::new(file).is_err() {
        return Ok(None);
    }
    let (entries, legacy) = entries(&request.archive, encoding)?;
    if !legacy {
        return Ok(None);
    }
    if entries.len() != listed_count {
        return Err(SmartZipError::BackendProtocolError {
            backend: "zip-name-decoder".into(),
            detail: "ambiguous ZIP directory".into(),
        });
    }
    let indices: Vec<_> = entries
        .iter()
        .enumerate()
        .filter(|(_, e)| e.path.as_os_str() == request.member.as_os_str())
        .collect();
    if indices.len() != 1 {
        return Err(SmartZipError::BackendProtocolError {
            backend: "zip-name-decoder".into(),
            detail: "member is absent or ambiguous".into(),
        });
    }
    let (index, entry) = indices[0];
    if entry.is_dir
        || entry
            .uncompressed_size
            .is_some_and(|s| s > request.max_bytes as u64)
    {
        return Err(SmartZipError::ResourceLimit {
            detail: "member is a directory or exceeds preview limit".into(),
        });
    }
    let request = request.clone();
    let token = token.child_token();
    let _guard = token.clone().drop_guard();
    tokio::task::spawn_blocking(move || {
        let mut archive = zip::ZipArchive::new(
            File::open(&request.archive)
                .map_err(|e| SmartZipError::io(Some(request.archive.clone()), e))?,
        )
        .map_err(|e| zip_error(e, &request.archive))?;
        let mut entry = match request.password.as_deref() {
            Some(pw) => archive.by_index_decrypt(index, pw.as_bytes()),
            None => archive.by_index(index),
        }
        .map_err(|e| zip_error(e, &request.archive))?;
        let mut bytes = Vec::new();
        let mut buffer = [0; 64 * 1024];
        loop {
            if token.is_cancelled() {
                return Err(SmartZipError::Cancelled);
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(SmartZipError::ResourceLimit {
                    detail: "member preview exceeded 30 seconds".into(),
                });
            }
            let count = entry
                .read(&mut buffer)
                .map_err(|e| SmartZipError::CorruptedArchive {
                    path: request.archive.clone(),
                    detail: e.to_string(),
                })?;
            if count == 0 {
                break;
            }
            if count > request.max_bytes.saturating_sub(bytes.len()) {
                return Err(SmartZipError::ResourceLimit {
                    detail: "member exceeds preview limit".into(),
                });
            }
            bytes.extend_from_slice(&buffer[..count]);
        }
        Ok(Some(bytes))
    })
    .await
    .map_err(|e| SmartZipError::BackendProtocolError {
        backend: "zip-name-decoder".into(),
        detail: e.to_string(),
    })?
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn decoded_zip_preserves_links_even_when_the_target_comes_later() {
        let root = tempfile::tempdir().unwrap();
        let archive = root.path().join("links.zip");
        let mut writer = zip::ZipWriter::new(File::create(&archive).unwrap());
        let options = zip::write::SimpleFileOptions::default();
        writer
            .add_symlink("folder/shortcut", "data.txt", options)
            .unwrap();
        writer.add_symlink("dangling", "missing", options).unwrap();
        writer.start_file("folder/data.txt", options).unwrap();
        writer.write_all(b"payload").unwrap();
        writer.finish().unwrap();
        let output_dir = root.path().join("output");
        std::fs::create_dir(&output_dir).unwrap();
        let (listed, _) = entries(&archive, "UTF-8").unwrap();
        extract(
            ExtractArchiveRequest {
                archive,
                format: Some(smartzip_core::ArchiveFormat::Zip),
                output_dir: output_dir.clone(),
                password: None,
                encoding: EncodingMode::Override("UTF-8".into()),
            },
            listed,
            &CancellationToken::new(),
        )
        .unwrap();
        assert_eq!(
            std::fs::read_link(output_dir.join("folder/shortcut")).unwrap(),
            Path::new("data.txt")
        );
        assert_eq!(
            std::fs::read(output_dir.join("folder/shortcut")).unwrap(),
            b"payload"
        );
        assert_eq!(
            std::fs::read_link(output_dir.join("dangling")).unwrap(),
            Path::new("missing")
        );
    }
}
