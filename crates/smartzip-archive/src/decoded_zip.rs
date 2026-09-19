//! Narrow path for ZIP names that the external backend cannot decode faithfully.
//! Ordinary UTF-8 archives continue through the configured backend.
use crate::{ArchiveEntry, ExtractArchiveRequest};
use smartzip_core::{EncodingMode, Result, SmartZipError};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
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
            .is_some_and(|mode| !matches!(mode & 0o170000, 0 | 0o040000 | 0o100000))
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
        ensure_directories(&request.output_dir, relative, entry.is_dir())?;
        if entry.is_dir() {
            continue;
        }
        // create_new also refuses pre-existing symlinks and decoded collisions.
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
    Ok(())
}

fn ensure_directories(root: &Path, relative: &Path, is_dir: bool) -> Result<()> {
    let directories = if is_dir {
        relative
    } else {
        relative.parent().unwrap_or(Path::new(""))
    };
    let mut current = PathBuf::from(root);
    for part in std::iter::once(None).chain(directories.components().map(Some)) {
        if let Some(part) = part {
            current.push(part.as_os_str());
        }
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.is_dir() && !metadata.is_symlink() => {}
            Ok(_) => {
                return Err(SmartZipError::UnsafeArchivePath {
                    entry: current.display().to_string(),
                })
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => std::fs::create_dir(&current)
                .map_err(|e| SmartZipError::io(Some(current.clone()), e))?,
            Err(e) => return Err(SmartZipError::io(Some(current.clone()), e)),
        }
    }
    Ok(())
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
