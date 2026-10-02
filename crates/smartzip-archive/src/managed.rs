//! Frozen inventories and bounded content producers. Output names belong to the engine.
use crate::{
    backend::{ManagedSink, PreparedExtraction},
    types::*,
};
use async_trait::async_trait;
use smartzip_core::{
    EncodingMode, PathConstraintReason, PathDiagnostic, PathStage, Result, SmartZipError,
    TaskExecutionContext,
};
use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio_util::sync::CancellationToken;

const MAX_NAME_BYTES: usize = 16 * 1024 * 1024;
const BUFFER: usize = 64 * 1024;

pub(crate) fn unsupported(detail: impl Into<String>) -> SmartZipError {
    PathDiagnostic::new(
        PathConstraintReason::PathRemapUnsupported,
        PathStage::Preflight,
        detail,
    )
    .error()
}
fn protocol(detail: impl Into<String>) -> SmartZipError {
    SmartZipError::BackendProtocolError {
        backend: "managed-extraction".into(),
        detail: detail.into(),
    }
}
fn collision(detail: impl Into<String>) -> SmartZipError {
    PathDiagnostic::new(
        PathConstraintReason::NameCollision,
        PathStage::Preflight,
        detail,
    )
    .error()
}
fn io(path: &Path, error: std::io::Error) -> SmartZipError {
    SmartZipError::io(Some(path.into()), error)
}
fn zip_error(error: zip::result::ZipError, path: &Path) -> SmartZipError {
    match error {
        zip::result::ZipError::UnsupportedArchive(zip::result::ZipError::PASSWORD_REQUIRED) => {
            SmartZipError::PasswordRequired { path: path.into() }
        }
        zip::result::ZipError::InvalidPassword => {
            SmartZipError::WrongPassword { path: path.into() }
        }
        zip::result::ZipError::UnsupportedArchive(reason) => SmartZipError::UnsupportedCodec {
            backend: "native-zip-managed".into(),
            path: path.into(),
            codec: Some(reason.into()),
        },
        error => SmartZipError::CorruptedArchive {
            path: path.into(),
            detail: error.to_string(),
        },
    }
}
fn hash_file(file: &mut File, path: &Path, token: &CancellationToken) -> Result<String> {
    file.rewind().map_err(|e| io(path, e))?;
    let mut hash = blake3::Hasher::new();
    let mut buffer = [0; BUFFER];
    loop {
        if token.is_cancelled() {
            return Err(SmartZipError::Cancelled);
        }
        let count = file.read(&mut buffer).map_err(|e| io(path, e))?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    Ok(hash.finalize().to_hex().to_string())
}
async fn hash_path(path: PathBuf, token: CancellationToken) -> Result<String> {
    tokio::task::spawn_blocking(move || {
        let mut file = File::open(&path).map_err(|e| io(&path, e))?;
        hash_file(&mut file, &path, &token)
    })
    .await
    .map_err(|e| protocol(e.to_string()))?
}
fn changed() -> SmartZipError {
    protocol("archive contents changed after inventory was frozen")
}
fn make_manifest(
    entries: Vec<ExtractionEntry>,
    request: &ExtractArchiveRequest,
    adapter: String,
    adapter_identity: String,
    source: String,
) -> Result<ExtractionManifest> {
    let bytes = serde_json::to_vec(&(
        &entries,
        &adapter,
        &adapter_identity,
        &source,
        &request.encoding,
    ))
    .map_err(|e| protocol(e.to_string()))?;
    Ok(ExtractionManifest {
        entries,
        digest: blake3::hash(&bytes).to_hex().to_string(),
        adapter_id: adapter,
        adapter_identity,
        source_identity: source,
        encoding: request.encoding.clone(),
    })
}
fn check_entries(entries: &[ExtractionEntry], limits: &ExtractionLimits) -> Result<()> {
    if entries.len() > limits.max_entries {
        return Err(SmartZipError::ResourceLimit {
            detail: "extraction inventory entry limit exceeded".into(),
        });
    }
    let mut names = 0usize;
    let mut total = 0u64;
    let mut directories = BTreeMap::new();
    for entry in entries {
        if entry.is_dir {
            if entry.size != 0 {
                return Err(unsupported(
                    "directory entries with payloads cannot be safely represented",
                ));
            }
            if let Some(path) = if smartzip_core::is_root_directory_alias(&entry.display_name) {
                Some(PathBuf::new())
            } else {
                crate::safety::safe_entry_path(entry.display_name.as_bytes())
            } {
                if let Some(previous) = directories.insert(path, (entry.size, &entry.metadata)) {
                    if previous != (entry.size, &entry.metadata) {
                        return Err(collision(
                            "incompatible metadata on repeated directory declarations",
                        ));
                    }
                }
            }
        }
        names = names
            .checked_add(
                entry
                    .raw_name
                    .as_ref()
                    .map_or(entry.display_name.len(), Vec::len),
            )
            .ok_or_else(|| protocol("name inventory overflow"))?;
        total = total
            .checked_add(entry.size)
            .ok_or_else(|| protocol("size inventory overflow"))?;
        if names > MAX_NAME_BYTES
            || entry.size > limits.max_single_entry_bytes
            || total > limits.max_total_output_bytes
        {
            return Err(SmartZipError::ResourceLimit {
                detail: "extraction inventory name/size budget exceeded".into(),
            });
        }
    }
    Ok(())
}

struct ZipSession {
    request: ExtractArchiveRequest,
    manifest: ExtractionManifest,
    file: File,
    limits: ExtractionLimits,
    managed: bool,
    encrypted: bool,
    token: CancellationToken,
}

/// Read the physical central name fields, not zip::name_raw (which may contain Unicode extras).
fn central_names(
    file: &mut File,
    archive: &mut zip::ZipArchive<File>,
    request: &ExtractArchiveRequest,
    token: &CancellationToken,
    limits: &ExtractionLimits,
) -> Result<Vec<ExtractionEntry>> {
    let mut position = archive.central_directory_start();
    let mut entries = Vec::new();
    let mut names = HashSet::new();
    let mut names_bytes = 0usize;
    loop {
        if token.is_cancelled() {
            return Err(SmartZipError::Cancelled);
        }
        file.seek(SeekFrom::Start(position))
            .map_err(|e| io(&request.archive, e))?;
        let mut signature = [0; 4];
        file.read_exact(&mut signature)
            .map_err(|e| io(&request.archive, e))?;
        if signature != *b"PK\x01\x02" {
            break;
        }
        if entries.len() >= limits.max_entries {
            return Err(SmartZipError::ResourceLimit {
                detail: "ZIP inventory entry limit exceeded".into(),
            });
        }
        let mut header = [0; 42];
        file.read_exact(&mut header)
            .map_err(|e| io(&request.archive, e))?;
        let flags = u16::from_le_bytes([header[4], header[5]]);
        let name_len = u16::from_le_bytes([header[24], header[25]]) as usize;
        let extra_len = u16::from_le_bytes([header[26], header[27]]) as usize;
        let comment_len = u16::from_le_bytes([header[28], header[29]]) as usize;
        names_bytes = names_bytes
            .checked_add(name_len)
            .ok_or_else(|| protocol("ZIP names overflow"))?;
        if names_bytes > MAX_NAME_BYTES {
            return Err(SmartZipError::ResourceLimit {
                detail: "ZIP name inventory exceeds 16 MiB".into(),
            });
        }
        let mut raw = vec![0; name_len];
        let mut extra = vec![0; extra_len];
        file.read_exact(&mut raw)
            .map_err(|e| io(&request.archive, e))?;
        file.read_exact(&mut extra)
            .map_err(|e| io(&request.archive, e))?;
        position = position
            .checked_add(46 + (name_len + extra_len + comment_len) as u64)
            .ok_or_else(|| protocol("ZIP central directory offset overflow"))?;
        if !names.insert(raw.clone()) {
            return Err(collision("duplicate physical ZIP entry"));
        }
        let index = entries.len();
        let member = archive
            .by_index_raw(index)
            .map_err(|e| zip_error(e, &request.archive))?;
        if member.central_header_start()
            != position - 46 - (name_len + extra_len + comment_len) as u64
        {
            return Err(collision(
                "ZIP reader cannot preserve physical central directory identity",
            ));
        }
        if member.compressed_size() > 0
            && member.size() / member.compressed_size() > limits.max_compression_ratio as u64
        {
            return Err(SmartZipError::ResourceLimit {
                detail: "ZIP compression ratio exceeds extraction budget".into(),
            });
        }
        let mode = member.unix_mode();
        if mode.is_some_and(|m| m & 0o170000 == 0o120000) {
            return Err(unsupported(
                "managed ZIP symbolic-link target rewriting is not implemented",
            ));
        }
        if mode.is_some_and(|m| !matches!(m & 0o170000, 0 | 0o040000 | 0o100000)) {
            return Err(SmartZipError::UnsafeArchivePath {
                entry: member.name().into(),
            });
        }
        let mut unicode = None;
        let mut unix_modified = None;
        let mut fields = extra.as_slice();
        while fields.len() >= 4 {
            let id = u16::from_le_bytes([fields[0], fields[1]]);
            let len = u16::from_le_bytes([fields[2], fields[3]]) as usize;
            if fields.len() < len + 4 {
                return Err(protocol("truncated ZIP central extra field"));
            }
            let value = &fields[4..4 + len];
            if id == 0x5455 && value.len() >= 5 && value[0] & 1 != 0 {
                unix_modified = Some(u32::from_le_bytes(
                    value[1..5]
                        .try_into()
                        .map_err(|_| protocol("ZIP Unix timestamp"))?,
                ) as i64);
            }
            if id == 0x000a
                && value.len() >= 32
                && value[4..6] == 1u16.to_le_bytes()
                && value[6..8] == 24u16.to_le_bytes()
            {
                let filetime = u64::from_le_bytes(
                    value[8..16]
                        .try_into()
                        .map_err(|_| protocol("ZIP NTFS timestamp"))?,
                );
                unix_modified = Some((filetime / 10_000_000) as i64 - 11_644_473_600);
            }
            if id == 0x7075
                && value.len() >= 5
                && value[0] == 1
                && u32::from_le_bytes(
                    value[1..5]
                        .try_into()
                        .map_err(|_| protocol("ZIP Unicode CRC"))?,
                ) == crc32fast::hash(&raw)
            {
                if unicode.is_some() {
                    return Err(protocol("duplicate ZIP Unicode name extra"));
                }
                unicode = Some(
                    std::str::from_utf8(&value[5..])
                        .map_err(|_| protocol("invalid UTF-8 ZIP Unicode extra"))?
                        .to_owned(),
                );
            }
            fields = &fields[4 + len..];
        }
        if !fields.is_empty() {
            return Err(protocol("trailing ZIP extra field bytes"));
        }
        let (name, source_kind) = if flags & 0x800 != 0 {
            (
                std::str::from_utf8(&raw)
                    .map_err(|_| protocol("invalid UTF-8 flagged ZIP name"))?
                    .into(),
                ExtractionNameSource::ZipCentralDirectory,
            )
        } else if let Some(name) = unicode {
            (name, ExtractionNameSource::ZipUnicodeExtra)
        } else {
            let name = match &request.encoding {
                EncodingMode::Override(label) => smartzip_encoding::decode_name(&raw, label)
                    .ok_or_else(|| protocol(format!("ZIP name cannot be decoded as {label}")))?,
                EncodingMode::Auto => member.name().to_owned(), // ZIP's specified CP437 default.
            };
            (name, ExtractionNameSource::ZipCentralDirectory)
        };
        entries.push(ExtractionEntry {
            id: index as u64,
            is_dir: member.is_dir(),
            display_name: name,
            raw_name: Some(raw),
            source_kind,
            size: member.size(),
            metadata: ExtractionMetadata {
                crc32: if flags & 1 != 0
                    && header[6..8] == 99u16.to_le_bytes()
                    && member.crc32() == 0
                {
                    None
                } else {
                    Some(member.crc32())
                },
                unix_mode: mode,
                modified_unix_seconds: unix_modified.or_else(|| {
                    member
                        .last_modified()
                        .and_then(|date| time::PrimitiveDateTime::try_from(date).ok())
                        .and_then(local_timestamp)
                }),
            },
        });
    }
    if entries.len() != archive.len() {
        return Err(collision(
            "ZIP library omitted duplicate or ambiguous central members",
        ));
    }
    check_entries(&entries, limits)?;
    Ok(entries)
}
/// Bound allocation before zip's central-directory parser allocates its index.
fn zip_entry_count(file: &mut File, path: &Path, limits: &ExtractionLimits) -> Result<()> {
    let length = file.metadata().map_err(|e| io(path, e))?.len();
    let tail_len = length.min(65_557) as usize;
    file.seek(SeekFrom::End(-(tail_len as i64)))
        .map_err(|e| io(path, e))?;
    let mut tail = vec![0; tail_len];
    file.read_exact(&mut tail).map_err(|e| io(path, e))?;
    let found = (0..tail.len().saturating_sub(21)).rev().find(|&i| {
        tail[i..].starts_with(b"PK\x05\x06")
            && i + 22 + u16::from_le_bytes([tail[i + 20], tail[i + 21]]) as usize == tail.len()
    });
    let i = found.ok_or_else(|| SmartZipError::CorruptedArchive {
        path: path.into(),
        detail: "ZIP end-of-central-directory is absent or ambiguous".into(),
    })?;
    let count = u16::from_le_bytes([tail[i + 10], tail[i + 11]]) as usize;
    if count == 65_535 {
        return Err(unsupported(
            "ZIP64 entry-count inventories require a bounded native directory parser",
        ));
    }
    if count > limits.max_entries {
        return Err(SmartZipError::ResourceLimit {
            detail: "ZIP entry count exceeds extraction budget".into(),
        });
    }
    let central_size = u32::from_le_bytes(
        tail[i + 12..i + 16]
            .try_into()
            .map_err(|_| protocol("ZIP central size"))?,
    ) as u64;
    if central_size == u32::MAX as u64 {
        return Err(unsupported(
            "ZIP64 central-directory sizes need a bounded native directory parser",
        ));
    }
    if central_size > 64 * 1024 * 1024 {
        return Err(SmartZipError::ResourceLimit {
            detail: "ZIP central metadata exceeds 64 MiB inventory budget".into(),
        });
    }

    Ok(())
}
pub(crate) async fn prepare_zip(
    request: ExtractArchiveRequest,
    context: Arc<TaskExecutionContext>,
    managed: bool,
) -> Result<Box<dyn PreparedExtraction>> {
    let token = context.cancellation_token();
    let session = tokio::task::spawn_blocking(move || {
        let limits = ExtractionLimits::default();
        let mut file = File::open(&request.archive).map_err(|e| io(&request.archive, e))?;
        let source = hash_file(&mut file, &request.archive, &token)?;
        zip_entry_count(&mut file, &request.archive, &limits)?;
        let mut archive =
            zip::ZipArchive::new(file.try_clone().map_err(|e| io(&request.archive, e))?)
                .map_err(|e| zip_error(e, &request.archive))?;
        let entries = central_names(&mut file, &mut archive, &request, &token, &limits)?;
        let mut encrypted = false;
        for index in 0..archive.len() {
            let member = archive
                .by_index_raw(index)
                .map_err(|e| zip_error(e, &request.archive))?;
            let member_encrypted = member.encrypted();
            let directory = member.is_dir();
            encrypted |= member_encrypted;
            drop(member);
            if !directory {
                if let Some(password) = request.password.as_deref() {
                    archive
                        .by_index_decrypt(index, password.as_bytes())
                        .map_err(|e| zip_error(e, &request.archive))?;
                } else if !member_encrypted {
                    archive
                        .by_index(index)
                        .map_err(|e| zip_error(e, &request.archive))?;
                }
            }
        }
        if source != hash_file(&mut file, &request.archive, &token)? {
            return Err(changed());
        }
        let manifest = make_manifest(
            entries,
            &request,
            "native-zip-managed".into(),
            "native-zip-managed-v1".into(),
            source,
        )?;
        Ok(ZipSession {
            request,
            manifest,
            file,
            limits,
            managed,
            encrypted,
            token,
        })
    })
    .await
    .map_err(|e| protocol(e.to_string()))??;
    Ok(Box::new(session))
}
#[async_trait]
impl PreparedExtraction for ZipSession {
    fn manifest(&self) -> &ExtractionManifest {
        &self.manifest
    }
    fn supports_managed(&self) -> bool {
        self.managed
    }
    async fn verify_source(&self) -> Result<()> {
        let mut file = self
            .file
            .try_clone()
            .map_err(|e| io(&self.request.archive, e))?;
        let path = self.request.archive.clone();
        let expected = self.manifest.source_identity.clone();
        let token = self.token.clone();
        tokio::task::spawn_blocking(move || {
            if hash_file(&mut file, &path, &token)? != expected {
                return Err(changed());
            }
            let mut reopened = File::open(&path).map_err(|e| io(&path, e))?;
            if hash_file(&mut reopened, &path, &token)? != expected {
                return Err(changed());
            }
            Ok(())
        })
        .await
        .map_err(|e| protocol(e.to_string()))?
    }
    async fn execute(
        self: Box<Self>,
        sink: Arc<dyn ManagedSink>,
        context: Arc<TaskExecutionContext>,
    ) -> Result<ExtractArchiveResult> {
        if !self.managed {
            return Err(unsupported(
                "forced adapter cannot execute ZIP index-based path remapping",
            ));
        }
        let token = context.cancellation_token().child_token();
        let _guard = token.clone().drop_guard();
        tokio::task::spawn_blocking(move || {
            let mut session = *self;
            if hash_file(&mut session.file, &session.request.archive, &token)?
                != session.manifest.source_identity
            {
                return Err(changed());
            }
            let mut archive = zip::ZipArchive::new(
                session
                    .file
                    .try_clone()
                    .map_err(|e| io(&session.request.archive, e))?,
            )
            .map_err(|e| zip_error(e, &session.request.archive))?;
            let mut total = 0;
            let mut buffer = [0; BUFFER];
            for entry in &session.manifest.entries {
                if token.is_cancelled() {
                    return Err(SmartZipError::Cancelled);
                }
                if entry.is_dir {
                    continue;
                }
                let mut member = match session.request.password.as_deref() {
                    Some(password) => {
                        archive.by_index_decrypt(entry.id as usize, password.as_bytes())
                    }
                    None => archive.by_index(entry.id as usize),
                }
                .map_err(|e| zip_error(e, &session.request.archive))?;
                let mut output = sink.open_file(entry.id)?;
                let mut count = 0;
                let mut crc = crc32fast::Hasher::new();
                loop {
                    if token.is_cancelled() {
                        return Err(SmartZipError::Cancelled);
                    }
                    let size = member.read(&mut buffer).map_err(|e| {
                        if session.encrypted {
                            SmartZipError::PasswordIndeterminate {
                                path: session.request.archive.clone(),
                            }
                        } else {
                            SmartZipError::CorruptedArchive {
                                path: session.request.archive.clone(),
                                detail: e.to_string(),
                            }
                        }
                    })?;
                    if size == 0 {
                        break;
                    }
                    budget(size, &mut count, &mut total, entry.size, &session.limits)?;
                    crc.update(&buffer[..size]);
                    output
                        .write_all(&buffer[..size])
                        .map_err(|e| SmartZipError::io(None, e))?;
                }
                if count != entry.size
                    || entry
                        .metadata
                        .crc32
                        .is_some_and(|expected| expected != crc.finalize())
                {
                    return Err(SmartZipError::CorruptedArchive {
                        path: session.request.archive.clone(),
                        detail: "managed ZIP size/CRC mismatch".into(),
                    });
                }
                output.flush().map_err(|e| SmartZipError::io(None, e))?;
                drop(output);
            }
            if hash_file(&mut session.file, &session.request.archive, &token)?
                != session.manifest.source_identity
            {
                return Err(changed());
            }
            Ok(ExtractArchiveResult {
                output_dir: session.request.output_dir,
                encrypted: Some(session.encrypted),
            })
        })
        .await
        .map_err(|e| protocol(e.to_string()))?
    }
}
fn budget(
    size: usize,
    count: &mut u64,
    total: &mut u64,
    declared: u64,
    limits: &ExtractionLimits,
) -> Result<()> {
    *count = count
        .checked_add(size as u64)
        .ok_or_else(|| protocol("member size overflow"))?;
    *total = total
        .checked_add(size as u64)
        .ok_or_else(|| protocol("output size overflow"))?;
    if *count > declared
        || *count > limits.max_single_entry_bytes
        || *total > limits.max_total_output_bytes
    {
        return Err(SmartZipError::ResourceLimit {
            detail: "actual extraction bytes exceed declared size/output budget".into(),
        });
    }
    Ok(())
}

struct SevenSession {
    request: ExtractArchiveRequest,
    manifest: ExtractionManifest,
    executable: PathBuf,
    executable_identity: String,
    encrypted: bool,
    limits: ExtractionLimits,
    managed: bool,
    sources: Vec<PathBuf>,
    stats: Vec<SourceStat>,
    token: CancellationToken,
}
#[derive(PartialEq, Eq)]
struct SourceStat {
    length: u64,
    modified: std::time::SystemTime,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
}
fn source_stats(paths: &[PathBuf]) -> Result<Vec<SourceStat>> {
    paths
        .iter()
        .map(|path| {
            let metadata = std::fs::metadata(path).map_err(|e| io(path, e))?;
            #[cfg(unix)]
            use std::os::unix::fs::MetadataExt;
            Ok(SourceStat {
                length: metadata.len(),
                modified: metadata.modified().map_err(|e| io(path, e))?,
                #[cfg(unix)]
                device: metadata.dev(),
                #[cfg(unix)]
                inode: metadata.ino(),
            })
        })
        .collect()
}
async fn hash_sources(paths: &[PathBuf], token: &CancellationToken) -> Result<String> {
    let mut hash = blake3::Hasher::new();
    hash.update(b"smartzip-input-volumes-v1");
    for path in paths {
        let label = path
            .to_str()
            .ok_or_else(|| unsupported("external input volume names must be UTF-8"))?;
        hash.update(&(label.len() as u64).to_le_bytes());
        hash.update(label.as_bytes());
        hash.update(hash_path(path.clone(), token.clone()).await?.as_bytes());
    }
    Ok(hash.finalize().to_hex().to_string())
}
fn selector(name: &str) -> Result<()> {
    if name.is_empty()
        || name.starts_with('@')
        || name.starts_with('-')
        || name.chars().any(|c| c.is_control() || c == '\\')
    {
        return Err(unsupported(
            "7-Zip member selector is not unambiguously representable",
        ));
    }
    Ok(())
}
fn permission_mode(attributes: &str) -> Result<Option<u32>> {
    let Some(symbolic) = attributes.split_whitespace().find(|part| part.len() == 10) else {
        return Ok(None);
    };
    let kind = match symbolic.as_bytes()[0] {
        b'd' => 0o040000,
        b'-' => 0o100000,
        _ => {
            return Err(unsupported(
                "managed extraction rejects links and special file types",
            ))
        }
    };
    let mut mode = kind;
    for (index, byte) in symbolic.as_bytes()[1..].iter().enumerate() {
        let bit = 1 << (8 - index);
        match byte {
            b'r' | b'w' | b'x' => mode |= bit,
            b'-' => {}
            b's' | b't' => mode |= bit, // Do not propagate setuid/setgid/sticky bits.
            b'S' | b'T' => {}
            _ => return Err(unsupported("unsupported 7-Zip permission metadata")),
        }
    }
    Ok(Some(mode))
}
// Both DOS ZIP timestamps and 7-Zip's SLT text represent local civil time.
fn local_timestamp(value: time::PrimitiveDateTime) -> Option<i64> {
    #[cfg(unix)]
    {
        let mut local: libc::tm = unsafe { std::mem::zeroed() };
        local.tm_year = value.year() - 1900;
        local.tm_mon = value.month() as i32 - 1;
        local.tm_mday = value.day() as i32;
        local.tm_hour = value.hour() as i32;
        local.tm_min = value.minute() as i32;
        local.tm_sec = value.second() as i32;
        local.tm_isdst = -1;
        let seconds = unsafe { libc::mktime(&mut local) };
        (seconds != -1).then_some(seconds as i64)
    }
    #[cfg(not(unix))]
    {
        let _ = value;
        None
    }
}
fn modified(value: &str) -> Option<i64> {
    let date: Vec<u16> = value
        .split(['-', ' ', ':', '.'])
        .take(6)
        .map(str::parse)
        .collect::<std::result::Result<_, _>>()
        .ok()?;
    if date.len() != 6 {
        return None;
    }
    time::Date::from_calendar_date(
        date[0] as i32,
        time::Month::try_from(date[1] as u8).ok()?,
        date[2] as u8,
    )
    .ok()?
    .with_hms(date[3] as u8, date[4] as u8, date[5] as u8)
    .ok()
    .and_then(local_timestamp)
}
/// `-ba -slt` is a strict record protocol. Unknown/duplicate fields or continuation
/// lines are capability failures, never a partial inventory. Exact selector probes
/// below additionally detect member-name newlines that could forge whole records.
#[cfg(test)]
fn seven_entries(text: &str, limits: &ExtractionLimits) -> Result<(Vec<ExtractionEntry>, bool)> {
    seven_entries_for_format(text, limits, None)
}
fn seven_entries_for_format(
    text: &str,
    limits: &ExtractionLimits,
    format: Option<&smartzip_core::ArchiveFormat>,
) -> Result<(Vec<ExtractionEntry>, bool)> {
    const FIELDS: &[&str] = &[
        "Path",
        "Size",
        "Packed Size",
        "Modified",
        "Created",
        "Accessed",
        "Attributes",
        "CRC",
        "Encrypted",
        "Method",
        "Block",
        "Folder",
        "Host OS",
        "Version",
        "Volume Index",
        "Offset",
        "Solid",
        "Comment",
        "Characteristics",
        "Symbolic Link",
        "Hard Link",
        "Mode",
        "User",
        "Group",
        "Alternate Stream",
        "Anti",
        "Commented",
        "Split Before",
        "Split After",
        "User ID",
        "Group ID",
        "Device Major",
        "Device Minor",
    ];
    let mut entries = Vec::new();
    let mut seen = HashSet::new();
    let mut encrypted = false;
    for block in text.trim_end_matches(['\r', '\n']).split("\n\n") {
        if block.trim_matches(['\r', '\n']).is_empty() {
            continue;
        }
        let mut fields = BTreeMap::new();
        for (index, line) in block.split_terminator('\n').enumerate() {
            if line.contains('\r') {
                return Err(unsupported(
                    "7-Zip listing contains a carriage-return name or ambiguous line delimiter",
                ));
            }
            let (key, value) = line
                .split_once(" = ")
                .ok_or_else(|| unsupported("7-Zip listing contains ambiguous continuation text"))?;
            if (index == 0 && key != "Path")
                || !FIELDS.contains(&key)
                || fields.insert(key, value).is_some()
            {
                return Err(unsupported(
                    "7-Zip listing contains unknown/duplicate fields or injected member records",
                ));
            }
        }
        let name = *fields
            .get("Path")
            .ok_or_else(|| unsupported("7-Zip inventory has no member path"))?;
        selector(name)?;
        if !seen.insert(name.to_owned()) {
            return Err(collision("duplicate 7-Zip member selector"));
        }
        for key in [
            "Symbolic Link",
            "Hard Link",
            "Comment",
            "Alternate Stream",
            "Device Major",
            "Device Minor",
        ] {
            if fields
                .get(key)
                .is_some_and(|value| !value.is_empty() && *value != "-")
            {
                return Err(unsupported(format!(
                    "managed extraction does not support 7-Zip {key} metadata"
                )));
            }
        }
        if fields.get("Anti").is_some_and(|value| *value == "+") {
            return Err(unsupported(
                "7-Zip anti-items are not ordinary file members",
            ));
        }
        let attributes = fields
            .get("Attributes")
            .or_else(|| fields.get("Mode"))
            .copied()
            .unwrap_or("");
        let mode = permission_mode(attributes)?;
        let is_dir = fields.get("Folder").is_some_and(|value| *value == "+")
            || attributes
                .split_whitespace()
                .any(|part| part == "D" || part.starts_with('D') || part.starts_with('d'));
        if !fields.contains_key("Folder")
            && !fields.contains_key("Attributes")
            && !matches!(
                format,
                Some(
                    smartzip_core::ArchiveFormat::Gzip
                        | smartzip_core::ArchiveFormat::Bzip2
                        | smartzip_core::ArchiveFormat::Xz
                        | smartzip_core::ArchiveFormat::Zstd
                )
            )
        {
            return Err(unsupported(
                "7-Zip inventory cannot distinguish directories and files",
            ));
        }
        let size = fields
            .get("Size")
            .ok_or_else(|| unsupported("7-Zip inventory omits member size"))?
            .parse()
            .map_err(|_| unsupported("invalid 7-Zip member size"))?;
        if is_dir && size != 0 {
            return Err(unsupported(
                "directory with nonzero payload cannot be safely mapped",
            ));
        }
        let crc32 = fields
            .get("CRC")
            .filter(|value| !value.is_empty())
            .map(|value| {
                u32::from_str_radix(value, 16)
                    .map_err(|_| unsupported("invalid 7-Zip CRC metadata"))
            })
            .transpose()?;
        encrypted |= fields.get("Encrypted").is_some_and(|value| *value == "+");
        let modified_unix_seconds = fields
            .get("Modified")
            .filter(|value| !value.is_empty())
            .and_then(|value| modified(value));
        entries.push(ExtractionEntry {
            id: entries.len() as u64,
            display_name: name.into(),
            raw_name: None,
            source_kind: ExtractionNameSource::BackendText,
            is_dir,
            size,
            metadata: ExtractionMetadata {
                crc32,
                unix_mode: mode,
                modified_unix_seconds,
            },
        });
        if entries.len() > limits.max_entries {
            return Err(SmartZipError::ResourceLimit {
                detail: "7-Zip inventory entry limit exceeded".into(),
            });
        }
    }
    check_entries(&entries, limits)?;
    Ok((entries, encrypted))
}
fn seven_args(operation: &str, request: &ExtractArchiveRequest) -> Vec<String> {
    let mut args = vec![
        operation.into(),
        "-sccUTF-8".into(),
        "-spd".into(),
        "-ssc".into(),
        "-r-".into(),
        format!("-p{}", request.password.as_deref().unwrap_or_default()),
    ];
    if let Some(encoding) = crate::SevenZipBackend::encoding_arg(&request.encoding) {
        args.push(encoding);
    }
    args
}
fn seven_failure(
    output: &BackendCommandOutput,
    id: &str,
    request: &ExtractArchiveRequest,
) -> SmartZipError {
    if let Some(error) = crate::test_output::password_error(
        output,
        "7z",
        request.password.as_deref(),
        &request.archive,
    ) {
        return error;
    }
    let diagnostic =
        crate::test_output::diagnostic_text(&format!("{}\n{}", output.stdout, output.stderr), "7z");
    if diagnostic.contains("crc")
        || diagnostic.contains("data error")
        || diagnostic.contains("headers error")
        || diagnostic.contains("unexpected end")
    {
        return SmartZipError::CorruptedArchive {
            path: request.archive.clone(),
            detail: "7-Zip rejected managed member integrity".into(),
        };
    }
    if diagnostic.contains("is not archive")
        || diagnostic.contains("as archive")
        || diagnostic.contains("unsupported archive")
        || diagnostic
            .lines()
            .any(|line| line == "can not open the file as [7z] archive")
    {
        return SmartZipError::UnsupportedContainer {
            backend: id.into(),
            path: request.archive.clone(),
            container: None,
        };
    }
    SmartZipError::BackendFailed {
        backend: id.into(),
        exit_code: output.status,
        stderr: "managed member process failed (backend diagnostics redacted)".into(),
    }
}
async fn seven_listing(
    executable: &Path,
    id: &str,
    request: &ExtractArchiveRequest,
    member: Option<&str>,
    token: &CancellationToken,
) -> Result<String> {
    let mut args = seven_args("l", request);
    args.extend([
        "-slt".into(),
        "-ba".into(),
        "--".into(),
        request
            .archive
            .to_str()
            .ok_or_else(|| unsupported("external backend requires a UTF-8 archive input path"))?
            .into(),
    ]);
    if let Some(member) = member {
        args.push(member.into());
    }
    let (output, truncated) =
        crate::process::run_bounded(executable, id, &args, token, crate::process::Mode::Ordinary)
            .await?;
    if truncated {
        return Err(unsupported("7-Zip listing was truncated"));
    }
    if output.status != Some(0) {
        // p7zip suppresses split-container open diagnostics with -ba. Recover
        // trusted diagnostic lines on failure so volume-group retries retain
        // the same error categories as the ordinary backend.
        args.retain(|arg| arg != "-ba");
        let (diagnostic, _) = crate::process::run_bounded(
            executable,
            id,
            &args,
            token,
            crate::process::Mode::Ordinary,
        )
        .await?;
        return Err(seven_failure(
            if diagnostic.status != Some(0) {
                &diagnostic
            } else {
                &output
            },
            id,
            request,
        ));
    }
    if output.stdout.contains('\u{fffd}') {
        return Err(unsupported("7-Zip listing is not lossless UTF-8"));
    }
    #[cfg(windows)]
    {
        Ok(output.stdout.replace("\r\n", "\n"))
    }
    #[cfg(not(windows))]
    {
        Ok(output.stdout)
    }
}
pub(crate) async fn prepare_seven(
    mut request: ExtractArchiveRequest,
    executable: PathBuf,
    adapter_id: String,
    context: Arc<TaskExecutionContext>,
) -> Result<Box<dyn PreparedExtraction>> {
    let token = context.cancellation_token();
    if token.is_cancelled() {
        return Err(SmartZipError::Cancelled);
    }
    let volumes = crate::volumes::VolumeSet::collect(&request.archive)
        .map_err(|e| io(&request.archive, e))?;
    let single_volume =
        volumes.family == crate::volumes::VolumeFamily::Single && volumes.members.len() == 1;
    let mut sources: Vec<PathBuf> = volumes
        .members
        .iter()
        .map(|member| std::fs::canonicalize(&member.path).map_err(|e| io(&member.path, e)))
        .collect::<Result<_>>()?;
    sources.sort();
    let stats = source_stats(&sources)?;
    // Keep the entrypoint alias: its sibling names identify materialized volumes.
    // Canonical source paths above are only for input identity verification.
    request.archive = std::path::absolute(&request.archive).map_err(|e| io(&request.archive, e))?;
    let executable = std::fs::canonicalize(executable).map_err(|e| SmartZipError::io(None, e))?;
    let executable_identity = format!("7z:{}", hash_path(executable.clone(), token.clone()).await?);
    let source = hash_sources(&sources, &token).await?;
    let limits = ExtractionLimits::default();
    let listing = seven_listing(&executable, &adapter_id, &request, None, &token).await?;
    let (entries, encrypted) =
        seven_entries_for_format(&listing, &limits, request.format.as_ref())?;
    verify_seven_counts(&executable, &adapter_id, &request, &entries, &token).await?;
    verify_seven_selectors(
        &executable,
        &adapter_id,
        &request,
        &entries,
        &limits,
        &token,
    )
    .await?;
    if source != hash_sources(&sources, &token).await?
        || executable_identity
            != format!("7z:{}", hash_path(executable.clone(), token.clone()).await?)
    {
        return Err(changed());
    }
    let manifest = make_manifest(
        entries,
        &request,
        adapter_id,
        executable_identity.clone(),
        source,
    )?;
    Ok(Box::new(SevenSession {
        request,
        manifest,
        executable,
        executable_identity,
        encrypted,
        limits,
        managed: single_volume,
        sources,
        stats,
        token,
    }))
}

#[async_trait]
impl PreparedExtraction for SevenSession {
    fn supports_managed(&self) -> bool {
        self.managed
    }
    fn supports_bulk_original(&self) -> bool {
        self.executable_identity.starts_with("7z:")
            && !self.manifest.entries.iter().any(|entry| {
                entry.is_dir && smartzip_core::is_root_directory_alias(&entry.display_name)
            })
    }
    async fn execute_bulk(
        mut self: Box<Self>,
        context: Arc<TaskExecutionContext>,
    ) -> Result<ExtractArchiveResult> {
        if !self.supports_bulk_original() {
            return Err(unsupported(
                "prepared adapter does not support bulk-original extraction",
            ));
        }
        self.token = context.cancellation_token();
        self.verify_source().await?;
        let backend = crate::SevenZipBackend::new(self.executable.clone())
            .with_id(self.manifest.adapter_id.clone());
        let result =
            crate::ArchiveAdapter::extract_with_context(&backend, self.request.clone(), context)
                .await?;
        self.verify_source().await?;
        Ok(result)
    }
    fn manifest(&self) -> &ExtractionManifest {
        &self.manifest
    }
    async fn verify_source(&self) -> Result<()> {
        let token = self.token.clone();
        let current = crate::volumes::VolumeSet::collect(&self.request.archive)
            .map_err(|e| io(&self.request.archive, e))?;
        let mut paths: Vec<_> = current
            .members
            .iter()
            .map(|member| std::fs::canonicalize(&member.path).map_err(|e| io(&member.path, e)))
            .collect::<Result<_>>()?;
        paths.sort();
        if paths != self.sources {
            return Err(changed());
        }
        if self.manifest.source_identity != hash_sources(&self.sources, &token).await?
            || self.executable_identity.rsplit(':').next()
                != Some(hash_path(self.executable.clone(), token).await?.as_str())
        {
            return Err(changed());
        }
        Ok(())
    }
    async fn execute(
        mut self: Box<Self>,
        sink: Arc<dyn ManagedSink>,
        context: Arc<TaskExecutionContext>,
    ) -> Result<ExtractArchiveResult> {
        if !self.managed {
            return Err(unsupported(
                "selected adapter cannot execute mapped member streams",
            ));
        }
        let token = context.cancellation_token();
        self.token = token.clone();
        self.verify_source().await?;
        let mut total = 0;
        for entry in &self.manifest.entries {
            if token.is_cancelled() {
                return Err(SmartZipError::Cancelled);
            }
            if entry.is_dir {
                continue;
            }
            if source_stats(&self.sources)? != self.stats {
                return Err(changed());
            }
            stream_seven(&self, entry, sink.clone(), &token, &mut total).await?;
            if source_stats(&self.sources)? != self.stats {
                return Err(changed());
            }
        }
        self.verify_source().await?;
        Ok(ExtractArchiveResult {
            output_dir: self.request.output_dir.clone(),
            encrypted: Some(self.encrypted),
        })
    }
}

async fn diagnostic_drain(
    mut stream: impl tokio::io::AsyncRead + Unpin,
) -> std::io::Result<(Vec<u8>, bool)> {
    use tokio::io::AsyncReadExt;
    let mut retained = Vec::new();
    let mut buffer = [0; 16 * 1024];
    let mut truncated = false;
    loop {
        let count = stream.read(&mut buffer).await?;
        if count == 0 {
            break;
        }
        let keep = count.min((64 * 1024usize).saturating_sub(retained.len()));
        retained.extend_from_slice(&buffer[..keep]);
        truncated |= keep != count;
    }
    Ok((retained, truncated))
}

async fn stream_seven(
    session: &SevenSession,
    entry: &ExtractionEntry,
    sink: Arc<dyn ManagedSink>,
    token: &CancellationToken,
    total: &mut u64,
) -> Result<()> {
    use process_wrap::tokio::{CommandWrap, KillOnDrop};
    use std::process::Stdio;
    use tokio::io::AsyncReadExt;
    let mut args = seven_args("x", &session.request);
    args.extend([
        "-so".into(),
        "-y".into(),
        "-bsp0".into(),
        "--".into(),
        session
            .request
            .archive
            .to_str()
            .ok_or_else(|| unsupported("non-UTF-8 archive path"))?
            .into(),
        entry.display_name.clone(),
    ]);
    let output = sink.open_file(entry.id)?;
    let mut command = CommandWrap::with_new(&session.executable, |command| {
        command
            .args(&args)
            .env("LC_ALL", "C")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
    });
    #[cfg(unix)]
    command.wrap(process_wrap::tokio::ProcessGroup::leader());
    #[cfg(windows)]
    command.wrap(process_wrap::tokio::JobObject);
    command.wrap(KillOnDrop);
    let mut child = command.spawn().map_err(|e| io(&session.executable, e))?;
    let initial_pid = child.id();
    let mut stdout = child
        .stdout()
        .take()
        .ok_or_else(|| protocol("missing managed stdout pipe"))?;
    let stderr = child
        .stderr()
        .take()
        .ok_or_else(|| protocol("missing managed stderr pipe"))?;
    // At most one queued 64 KiB chunk. The blocking writer is always joined,
    // including after cancellation, before the engine may clean staging.
    let (sender, mut receiver) = tokio::sync::mpsc::channel::<Vec<u8>>(1);
    let writer_token = token.clone();
    let writer = tokio::task::spawn_blocking(move || {
        let mut output = output;
        while let Some(bytes) = receiver.blocking_recv() {
            if writer_token.is_cancelled() {
                return Err(SmartZipError::Cancelled);
            }
            output
                .write_all(&bytes)
                .map_err(|e| SmartZipError::io(None, e))?;
        }
        output.flush().map_err(|e| SmartZipError::io(None, e))
    });
    let mut count = 0;
    let mut crc = crc32fast::Hasher::new();
    let completed = async {
        let consume = async {
            let mut buffer = [0; BUFFER];
            loop {
                let size = stdout
                    .read(&mut buffer)
                    .await
                    .map_err(|e| SmartZipError::io(None, e))?;
                if size == 0 {
                    break;
                }
                budget(size, &mut count, total, entry.size, &session.limits)?;
                crc.update(&buffer[..size]);
                sender
                    .send(buffer[..size].to_vec())
                    .await
                    .map_err(|_| protocol("managed writer stopped before producer EOF"))?;
            }
            Ok::<(), SmartZipError>(())
        };
        // Retain at most 64 KiB for classification; never expose raw fragments.
        let drain = async {
            diagnostic_drain(stderr)
                .await
                .map_err(|e| SmartZipError::io(None, e))
        };
        let (_, (diagnostic, truncated), status) = tokio::try_join!(consume, drain, async {
            child.wait().await.map_err(|e| io(&session.executable, e))
        })?;
        if !status.success() {
            let output = BackendCommandOutput {
                status: status.code(),
                stdout: String::new(),
                stderr: if truncated {
                    String::new()
                } else {
                    String::from_utf8_lossy(&diagnostic).into_owned()
                },
            };
            return Err(seven_failure(
                &output,
                &session.manifest.adapter_id,
                &session.request,
            ));
        }
        Ok(())
    };
    let result = tokio::select! { biased; _ = token.cancelled() => Err(SmartZipError::Cancelled), result = completed => result };
    if result.is_err() {
        #[cfg(unix)]
        if let Some(pid) = initial_pid {
            unsafe {
                libc::kill(-(pid as i32), libc::SIGKILL);
            }
        }
        let _ = child.start_kill();
        let _ = child.wait().await;
    }
    drop(sender);
    let written = writer.await.map_err(|e| protocol(e.to_string()))?;
    written?;
    result?;
    if count != entry.size
        || entry
            .metadata
            .crc32
            .is_some_and(|expected| expected != crc.finalize())
    {
        return Err(SmartZipError::CorruptedArchive {
            path: session.request.archive.clone(),
            detail: "managed 7-Zip member size/CRC mismatch".into(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    struct TestSink {
        root: PathBuf,
        closed: Arc<AtomicBool>,
        cancel: Option<CancellationToken>,
    }
    struct TestWriter {
        file: File,
        closed: Arc<AtomicBool>,
        cancel: Option<CancellationToken>,
    }
    impl Write for TestWriter {
        fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
            let count = self.file.write(data)?;
            if let Some(token) = &self.cancel {
                token.cancel();
            }
            Ok(count)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            self.file.flush()
        }
    }
    impl Drop for TestWriter {
        fn drop(&mut self) {
            self.closed.store(true, Ordering::SeqCst);
        }
    }
    impl ManagedSink for TestSink {
        fn open_file(&self, id: u64) -> Result<Box<dyn Write + Send>> {
            Ok(Box::new(TestWriter {
                file: File::options()
                    .create_new(true)
                    .write(true)
                    .open(self.root.join(id.to_string()))
                    .map_err(|e| SmartZipError::io(None, e))?,
                closed: self.closed.clone(),
                cancel: self.cancel.clone(),
            }))
        }
    }
    fn context() -> Arc<TaskExecutionContext> {
        Arc::new(TaskExecutionContext::detached())
    }
    fn request(path: &Path, output: &Path) -> ExtractArchiveRequest {
        ExtractArchiveRequest {
            archive: path.into(),
            output_dir: output.into(),
            format: Some(smartzip_core::ArchiveFormat::Zip),
            password: None,
            encoding: EncodingMode::Auto,
        }
    }
    fn sink(root: &Path) -> Arc<TestSink> {
        Arc::new(TestSink {
            root: root.into(),
            closed: Arc::new(AtomicBool::new(false)),
            cancel: None,
        })
    }
    // Build the archive directly; long and illegal source names never touch a filesystem.
    fn raw_zip(path: &Path, members: &[(&[u8], &[u8], u16, Vec<u8>)]) {
        fn word(bytes: &mut Vec<u8>, value: u16) {
            bytes.extend(value.to_le_bytes());
        }
        fn dword(bytes: &mut Vec<u8>, value: u32) {
            bytes.extend(value.to_le_bytes());
        }
        let mut data = Vec::new();
        let mut central = Vec::new();
        for (name, payload, flags, extra) in members {
            let offset = data.len() as u32;
            let crc = crc32fast::hash(payload);
            data.extend(b"PK\x03\x04");
            word(&mut data, 20);
            word(&mut data, *flags);
            word(&mut data, 0);
            word(&mut data, 0);
            word(&mut data, 0x21);
            dword(&mut data, crc);
            dword(&mut data, payload.len() as u32);
            dword(&mut data, payload.len() as u32);
            word(&mut data, name.len() as u16);
            word(&mut data, extra.len() as u16);
            data.extend(*name);
            data.extend(extra);
            data.extend(*payload);
            central.extend(b"PK\x01\x02");
            word(&mut central, 20);
            word(&mut central, 20);
            word(&mut central, *flags);
            word(&mut central, 0);
            word(&mut central, 0);
            word(&mut central, 0x21);
            dword(&mut central, crc);
            dword(&mut central, payload.len() as u32);
            dword(&mut central, payload.len() as u32);
            word(&mut central, name.len() as u16);
            word(&mut central, extra.len() as u16);
            word(&mut central, 0);
            word(&mut central, 0);
            word(&mut central, 0);
            dword(&mut central, if name.ends_with(b"/") { 0x10 } else { 0 });
            dword(&mut central, offset);
            central.extend(*name);
            central.extend(extra);
        }
        let offset = data.len() as u32;
        let size = central.len() as u32;
        data.extend(central);
        data.extend(b"PK\x05\x06");
        word(&mut data, 0);
        word(&mut data, 0);
        word(&mut data, members.len() as u16);
        word(&mut data, members.len() as u16);
        dword(&mut data, size);
        dword(&mut data, offset);
        word(&mut data, 0);
        std::fs::write(path, data).unwrap();
    }

    #[tokio::test]
    async fn managed_zip_streams_long_unicode_collisions_and_illegal_names_by_index() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("fixture.zip");
        let long = format!("{}/{}.txt", "中文目录".repeat(90), "👩‍💻".repeat(90));
        let names = [
            long.as_str(),
            "Case.txt",
            "case.txt",
            "é.txt",
            "e\u{301}.txt",
            "invalid:name?.txt",
            "empty/",
        ];
        let payloads: Vec<Vec<u8>> = (0..names.len())
            .map(|index| {
                if index + 1 == names.len() {
                    Vec::new()
                } else {
                    vec![index as u8; 200_001]
                }
            })
            .collect();
        let members: Vec<_> = names
            .iter()
            .zip(&payloads)
            .map(|(name, payload)| (name.as_bytes(), payload.as_slice(), 0x800, Vec::new()))
            .collect();
        raw_zip(&path, &members);
        let session = prepare_zip(request(&path, root.path()), context(), true)
            .await
            .unwrap();
        let manifest = session.manifest().clone();
        assert_eq!(manifest.entries.len(), names.len());
        assert_eq!(
            manifest.entries[0].raw_name.as_deref(),
            Some(long.as_bytes())
        );
        assert!(manifest.entries.last().unwrap().is_dir);
        let sink = sink(root.path());
        session.execute(sink.clone(), context()).await.unwrap();
        assert!(sink.closed.load(Ordering::SeqCst));
        for (index, payload) in payloads[..6].iter().enumerate() {
            assert_eq!(
                blake3::hash(&std::fs::read(root.path().join(index.to_string())).unwrap()),
                blake3::hash(payload)
            );
        }
        assert!(!root.path().join("Case.txt").exists());
    }

    #[tokio::test]
    async fn zip_unicode_extra_does_not_replace_true_raw_identity() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("extra.zip");
        let raw = b"old.txt";
        let mut value = vec![1];
        value.extend(crc32fast::hash(raw).to_le_bytes());
        value.extend("中文.txt".as_bytes());
        let mut extra = Vec::new();
        extra.extend(0x7075u16.to_le_bytes());
        extra.extend((value.len() as u16).to_le_bytes());
        extra.extend(value);
        raw_zip(&path, &[(raw, b"contents", 0, extra)]);
        let session = prepare_zip(request(&path, root.path()), context(), true)
            .await
            .unwrap();
        let entry = &session.manifest().entries[0];
        assert_eq!(
            crate::NativeZipBackend::new().raw_entries(&path).unwrap()[0].raw_name,
            raw
        );
        assert_eq!(entry.display_name, "中文.txt");
        assert_eq!(entry.raw_name.as_deref(), Some(raw.as_slice()));
        assert_eq!(entry.source_kind, ExtractionNameSource::ZipUnicodeExtra);
        session.execute(sink(root.path()), context()).await.unwrap();
        assert_eq!(std::fs::read(root.path().join("0")).unwrap(), b"contents");
    }

    #[tokio::test]
    async fn zip_override_decodes_before_handling_multibyte_separator_bytes() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("sjis.zip");
        // Shift-JIS 表 contains 0x5c as its second byte.
        raw_zip(&path, &[(b"\x95\x5c.txt", b"data", 0, Vec::new())]);
        let mut req = request(&path, root.path());
        req.encoding = EncodingMode::Override("Shift_JIS".into());
        let session = prepare_zip(req, context(), true).await.unwrap();
        assert_eq!(session.manifest().entries[0].display_name, "表.txt");
        assert_eq!(
            session.manifest().entries[0].raw_name.as_deref(),
            Some(b"\x95\x5c.txt".as_slice())
        );
    }

    #[tokio::test]
    async fn duplicate_zip_members_are_rejected_before_library_can_hide_them() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("duplicate.zip");
        raw_zip(
            &path,
            &[
                (b"same", b"first", 0, Vec::new()),
                (b"same", b"second", 0, Vec::new()),
            ],
        );
        assert!(
            matches!(prepare_zip(request(&path, root.path()), context(), true).await, Err(SmartZipError::PathConstraint { diagnostic, .. }) if diagnostic.reason == PathConstraintReason::NameCollision)
        );
        assert!(!root.path().join("0").exists());
    }

    #[tokio::test]
    async fn cancelled_zip_closes_writer_before_returning() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("cancel.zip");
        let data = vec![5; 3 * BUFFER];
        raw_zip(&path, &[(b"content", &data, 0, Vec::new())]);
        let session = prepare_zip(request(&path, root.path()), context(), true)
            .await
            .unwrap();
        let token = CancellationToken::new();
        let ctx = Arc::new(TaskExecutionContext::detached().with_cancellation(token.clone()));
        let sink = Arc::new(TestSink {
            root: root.path().into(),
            closed: Arc::new(AtomicBool::new(false)),
            cancel: Some(token),
        });
        assert!(matches!(
            session.execute(sink.clone(), ctx).await,
            Err(SmartZipError::Cancelled)
        ));
        assert!(sink.closed.load(Ordering::SeqCst));
        assert!(std::fs::metadata(root.path().join("0")).unwrap().len() < data.len() as u64);
    }

    #[tokio::test]
    async fn corrupt_zip_and_input_replacement_cannot_report_success() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("corrupt.zip");
        raw_zip(&path, &[(b"content", b"unique-payload", 0, Vec::new())]);
        let mut bytes = std::fs::read(&path).unwrap();
        let position = bytes
            .windows(14)
            .position(|window| window == b"unique-payload")
            .unwrap();
        bytes[position] ^= 1;
        std::fs::write(&path, bytes).unwrap();
        let session = prepare_zip(request(&path, root.path()), context(), true)
            .await
            .unwrap();
        assert!(session.execute(sink(root.path()), context()).await.is_err());
        raw_zip(&path, &[(b"content", b"original", 0, Vec::new())]);
        let session = prepare_zip(request(&path, root.path()), context(), false)
            .await
            .unwrap();
        let replacement = root.path().join("replacement.zip");
        raw_zip(&replacement, &[(b"content", b"different", 0, Vec::new())]);
        std::fs::rename(replacement, &path).unwrap();
        assert!(session.verify_source().await.is_err());
    }

    #[tokio::test]
    async fn zip_links_are_rejected_before_output() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("link.zip");
        let mut writer = zip::ZipWriter::new(File::create(&path).unwrap());
        writer
            .add_symlink("link", "target", zip::write::SimpleFileOptions::default())
            .unwrap();
        writer.finish().unwrap();
        assert!(
            matches!(prepare_zip(request(&path, root.path()), context(), true).await, Err(SmartZipError::PathConstraint { diagnostic, .. }) if diagnostic.reason == PathConstraintReason::PathRemapUnsupported)
        );
    }

    #[test]
    fn slt_rejects_duplicate_and_injected_records_and_links() {
        let entry = "Path = name\nSize = 1\nAttributes = A -rw-r--r--\nCRC = 00000000\n";
        assert!(seven_entries(&format!("{entry}\n{entry}"), &ExtractionLimits::default()).is_err());
        assert!(seven_entries(
            "Path = name\nSize = 0\nSize = 1\nAttributes = A\n",
            &ExtractionLimits::default()
        )
        .is_err());
        assert!(seven_entries(
            "Path = first\ncontinuation\nSize = 0\nAttributes = A\n",
            &ExtractionLimits::default()
        )
        .is_err());
        assert!(seven_entries(
            "Path = link\nSize = 0\nAttributes = A lrwxrwxrwx\n",
            &ExtractionLimits::default()
        )
        .is_err());
    }

    fn seven_tool() -> Option<PathBuf> {
        crate::SevenZipLocator::default().locate()
    }
    #[tokio::test]
    async fn real_seven_streams_ordinary_solid_members_to_managed_ids() {
        let Some(executable) = seven_tool() else {
            eprintln!("SKIP real 7z fixture: 7-Zip is unavailable");
            return;
        };
        let root = tempfile::tempdir().unwrap();
        let inputs = root.path().join("inputs");
        std::fs::create_dir(&inputs).unwrap();
        std::fs::create_dir(inputs.join("empty")).unwrap();
        std::fs::write(inputs.join("中文.txt"), vec![42; 200_001]).unwrap();
        std::fs::write(inputs.join("emoji👩‍💻.txt"), b"second payload").unwrap();
        let archive = root.path().join("fixture.7z");
        let status = std::process::Command::new(&executable)
            .args(["a", "-ms=on"])
            .arg(&archive)
            .arg(&inputs)
            .output()
            .unwrap();
        assert!(status.status.success());
        let mut req = request(&archive, root.path());
        req.format = Some(smartzip_core::ArchiveFormat::SevenZip);
        let session = prepare_seven(req, executable, "fixture-seven".into(), context())
            .await
            .unwrap();
        let manifest = session.manifest().clone();
        assert!(manifest
            .entries
            .iter()
            .any(|entry| entry.is_dir && entry.display_name.ends_with("empty")));
        session.execute(sink(root.path()), context()).await.unwrap();
        for entry in manifest.entries.iter().filter(|entry| !entry.is_dir) {
            let original = std::fs::read(root.path().join(&entry.display_name)).unwrap();
            let written = std::fs::read(root.path().join(entry.id.to_string())).unwrap();
            assert_eq!(blake3::hash(&written), blake3::hash(&original));
        }
    }

    #[tokio::test]
    async fn real_seven_rejects_newline_names_even_if_they_forge_full_slt_records() {
        let Some(executable) = seven_tool() else {
            return;
        };
        let root = tempfile::tempdir().unwrap();
        let inputs = root.path().join("inputs");
        std::fs::create_dir(&inputs).unwrap();
        std::fs::write(
            inputs.join("first\n\nPath = forged\nSize = 0\nAttributes = A\n"),
            b"payload",
        )
        .unwrap();
        let path = root.path().join("newlines.7z");
        assert!(std::process::Command::new(&executable)
            .arg("a")
            .arg(&path)
            .arg(&inputs)
            .output()
            .unwrap()
            .status
            .success());
        let mut req = request(&path, root.path());
        req.format = Some(smartzip_core::ArchiveFormat::SevenZip);
        assert!(
            prepare_seven(req, executable, "fixture-seven".into(), context())
                .await
                .is_err()
        );
        assert!(!root.path().join("0").exists());
    }
    #[tokio::test]
    async fn real_seven_long_name_stream_never_creates_the_original_name() {
        let Some(executable) = seven_tool() else {
            return;
        };
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("long.7z");
        let source = root.path().join("payload");
        let payload = vec![71; 200_001];
        std::fs::write(&source, &payload).unwrap();
        let name = format!("{}.txt", "中文👩‍💻".repeat(70));
        let status = std::process::Command::new(&executable)
            .args(["a", "-t7z"])
            .arg(format!("-si{name}"))
            .arg(&path)
            .stdin(std::process::Stdio::from(File::open(&source).unwrap()))
            .output()
            .unwrap();
        assert!(status.status.success());
        let mut req = request(&path, root.path());
        req.format = Some(smartzip_core::ArchiveFormat::SevenZip);
        let session = prepare_seven(req, executable, "fixture-seven".into(), context())
            .await
            .unwrap();
        assert_eq!(session.manifest().entries[0].display_name, name);
        assert!(session.supports_bulk_original());
        session.execute(sink(root.path()), context()).await.unwrap();
        assert_eq!(
            blake3::hash(&std::fs::read(root.path().join("0")).unwrap()),
            blake3::hash(&payload)
        );
        assert!(!root.path().join(name).exists());
    }

    #[tokio::test]
    async fn real_rar_regular_members_and_forced_unrar_inventory_are_complete() {
        let Some(executable) = seven_tool() else {
            return;
        };
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("fixture.rar");
        std::fs::write(&path, include_bytes!("../tests/fixtures/rar-ordinary.rar")).unwrap();
        let mut req = request(&path, root.path());
        req.format = Some(smartzip_core::ArchiveFormat::Rar);
        let session = prepare_seven(req.clone(), executable, "fixture-seven".into(), context())
            .await
            .unwrap();
        let manifest = session.manifest().clone();
        assert_eq!(manifest.entries.len(), 4);
        assert_eq!(
            manifest.entries.iter().filter(|entry| entry.is_dir).count(),
            2
        );
        session.execute(sink(root.path()), context()).await.unwrap();
        for entry in manifest.entries.iter().filter(|entry| !entry.is_dir) {
            assert_eq!(
                std::fs::read(root.path().join(entry.id.to_string())).unwrap(),
                b"test text document\r\n"
            );
        }
        if let Some(unrar) = crate::UnrarLocator::default().locate() {
            let session = prepare_unrar(req, unrar, "forced-unrar".into(), context())
                .await
                .unwrap();
            assert!(!session.supports_managed());
            assert!(!session.supports_bulk_original());
            assert_eq!(session.manifest().entries.len(), 4);
            session.verify_source().await.unwrap();
        }
    }

    #[tokio::test]
    async fn real_seven_multivolume_freezes_all_parts_and_retains_bulk() {
        let Some(executable) = seven_tool() else {
            return;
        };
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("payload");
        std::fs::write(&source, vec![14; 6000]).unwrap();
        let path = root.path().join("fixture.7z");
        assert!(std::process::Command::new(&executable)
            .args(["a", "-mx=0", "-v1k"])
            .arg(&path)
            .arg(&source)
            .output()
            .unwrap()
            .status
            .success());
        let first = root.path().join("fixture.7z.001");
        let mut req = request(&first, root.path());
        req.format = Some(smartzip_core::ArchiveFormat::SevenZip);
        let session = prepare_seven(req, executable, "fixture-seven".into(), context())
            .await
            .unwrap();
        assert!(!session.supports_managed());
        assert!(session.supports_bulk_original());
        session.verify_source().await.unwrap();
        let second = root.path().join("fixture.7z.002");
        let mut bytes = std::fs::read(&second).unwrap();
        bytes[0] ^= 1;
        std::fs::write(second, bytes).unwrap();
        assert!(session.verify_source().await.is_err());
    }

    #[tokio::test]
    async fn real_seven_cancel_waits_for_writer_close() {
        let Some(executable) = seven_tool() else {
            return;
        };
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("payload");
        std::fs::write(&source, vec![18; 10 * BUFFER]).unwrap();
        let path = root.path().join("fixture.7z");
        assert!(std::process::Command::new(&executable)
            .arg("a")
            .arg(&path)
            .arg(&source)
            .output()
            .unwrap()
            .status
            .success());
        let mut req = request(&path, root.path());
        req.format = Some(smartzip_core::ArchiveFormat::SevenZip);
        let session = prepare_seven(req, executable, "fixture-seven".into(), context())
            .await
            .unwrap();
        let token = CancellationToken::new();
        let ctx = Arc::new(TaskExecutionContext::detached().with_cancellation(token.clone()));
        let sink = Arc::new(TestSink {
            root: root.path().into(),
            closed: Arc::new(AtomicBool::new(false)),
            cancel: Some(token),
        });
        assert!(matches!(
            session.execute(sink.clone(), ctx).await,
            Err(SmartZipError::Cancelled)
        ));
        assert!(sink.closed.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn aes_zip_crc_zero_and_password_errors_keep_their_classification() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("aes.zip");
        let mut writer = zip::ZipWriter::new(File::create(&path).unwrap());
        writer
            .start_file(
                "内容.txt",
                zip::write::SimpleFileOptions::default()
                    .with_aes_encryption(zip::AesMode::Aes256, "fixture-secret"),
            )
            .unwrap();
        writer.write_all(b"protected payload").unwrap();
        writer.finish().unwrap();
        let mut req = request(&path, root.path());
        req.password = Some("fixture-secret".into());
        let session = prepare_zip(req.clone(), context(), true).await.unwrap();
        session.execute(sink(root.path()), context()).await.unwrap();
        assert_eq!(
            std::fs::read(root.path().join("0")).unwrap(),
            b"protected payload"
        );
        req.password = Some("wrong-secret".into());
        assert!(matches!(
            prepare_zip(req, context(), true).await,
            Err(SmartZipError::WrongPassword { .. })
        ));
    }

    #[tokio::test]
    async fn seven_bulk_and_exact_name_selection_keep_all_regular_contents() {
        let Some(executable) = seven_tool() else {
            return;
        };
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("names.zip");
        raw_zip(
            &path,
            &[
                (b"same", b"root", 0, Vec::new()),
                (b"dir/same", b"nested", 0, Vec::new()),
                (b"Case", b"upper", 0, Vec::new()),
                (b"case", b"lower", 0, Vec::new()),
            ],
        );
        let session = prepare_seven(
            request(&path, root.path()),
            executable.clone(),
            "fixture-seven".into(),
            context(),
        )
        .await
        .unwrap();
        let entries = session.manifest().entries.clone();
        session.execute(sink(root.path()), context()).await.unwrap();
        for (entry, payload) in entries.iter().zip([
            b"root".as_slice(),
            b"nested".as_slice(),
            b"upper".as_slice(),
            b"lower".as_slice(),
        ]) {
            assert_eq!(
                std::fs::read(root.path().join(entry.id.to_string())).unwrap(),
                payload
            );
        }
        let output = root.path().join("bulk");
        std::fs::create_dir(&output).unwrap();
        // A case-sensitive bulk fixture uses distinct names on the real target FS.
        let source = root.path().join("bulk-source");
        std::fs::write(&source, b"bulk payload").unwrap();
        let archive = root.path().join("ordinary.7z");
        assert!(std::process::Command::new(&executable)
            .arg("a")
            .arg(&archive)
            .arg(&source)
            .output()
            .unwrap()
            .status
            .success());
        let mut req = request(&archive, &output);
        req.format = Some(smartzip_core::ArchiveFormat::SevenZip);
        let session = prepare_seven(req, executable, "fixture-seven".into(), context())
            .await
            .unwrap();
        assert!(session.supports_bulk_original());
        session.execute_bulk(context()).await.unwrap();
        assert_eq!(
            std::fs::read(output.join("bulk-source")).unwrap(),
            b"bulk payload"
        );
    }

    #[test]
    fn repeated_directory_aliases_require_compatible_metadata() {
        let mut entries = vec![ExtractionEntry {
            id: 0,
            display_name: "dir/".into(),
            raw_name: None,
            source_kind: ExtractionNameSource::BackendText,
            is_dir: true,
            size: 0,
            metadata: ExtractionMetadata::default(),
        }];
        let mut alias = entries[0].clone();
        alias.id = 1;
        alias.display_name = "./dir/".into();
        entries.push(alias);
        check_entries(&entries, &ExtractionLimits::default()).unwrap();
        entries[1].metadata.unix_mode = Some(0o040700);
        assert!(
            matches!(check_entries(&entries,&ExtractionLimits::default()),Err(SmartZipError::PathConstraint {diagnostic,..}) if diagnostic.reason==PathConstraintReason::NameCollision)
        );
    }

    #[test]
    fn root_directory_aliases_preserve_manifest_and_require_matching_metadata() {
        let entry = ExtractionEntry {
            id: 0,
            display_name: "./".into(),
            raw_name: Some(b"./".to_vec()),
            source_kind: ExtractionNameSource::ZipCentralDirectory,
            is_dir: true,
            size: 0,
            metadata: ExtractionMetadata::default(),
        };
        let mut alias = entry.clone();
        alias.id = 1;
        alias.display_name = "././".into();
        alias.raw_name = Some(b"././".to_vec());
        let mut entries = vec![entry, alias];
        check_entries(&entries, &ExtractionLimits::default()).unwrap();
        let manifest = make_manifest(
            entries.clone(),
            &request(Path::new("fixture.zip"), Path::new("output")),
            "fixture".into(),
            "v1".into(),
            "input".into(),
        )
        .unwrap();
        assert_eq!(manifest.entries, entries);
        entries[1].metadata.unix_mode = Some(0o040700);
        assert!(
            matches!(check_entries(&entries,&ExtractionLimits::default()),Err(SmartZipError::PathConstraint {diagnostic,..}) if diagnostic.reason==PathConstraintReason::NameCollision)
        );
    }

    #[test]
    fn selector_probe_keeps_leaf_directory_with_trailing_slash() {
        let entries: Vec<_> = ["./", "leaf/", "parent/", "parent/file"]
            .into_iter()
            .enumerate()
            .map(|(id, name)| ExtractionEntry {
                id: id as u64,
                display_name: name.into(),
                raw_name: None,
                source_kind: ExtractionNameSource::BackendText,
                is_dir: name.ends_with('/'),
                size: 0,
                metadata: ExtractionMetadata::default(),
            })
            .collect();
        assert_eq!(
            seven_selector_entries(&entries)
                .iter()
                .map(|entry| entry.display_name.as_str())
                .collect::<Vec<_>>(),
            vec!["leaf/", "parent/file"]
        );
    }

    #[tokio::test]
    async fn tar_and_gzip_use_managed_content_when_sizes_are_available() {
        let Some(executable) = seven_tool() else {
            return;
        };
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("fixture.tar");
        let mut header = [0u8; 512];
        header[..12].copy_from_slice(b"dir/file.txt");
        header[100..108].copy_from_slice(b"0000644\0");
        header[108..116].copy_from_slice(b"0000000\0");
        header[116..124].copy_from_slice(b"0000000\0");
        header[124..136].copy_from_slice(b"00000000007\0");
        header[136..148].copy_from_slice(b"11320641600\0");
        header[148..156].fill(b' ');
        header[156] = b'0';
        header[257..263].copy_from_slice(b"ustar\0");
        header[263..265].copy_from_slice(b"00");
        let checksum: u32 = header.iter().map(|byte| *byte as u32).sum();
        header[148..156].copy_from_slice(format!("{checksum:06o}\0 ").as_bytes());
        let mut tar = header.to_vec();
        tar.extend(b"payload");
        tar.resize(2048, 0);
        std::fs::write(&path, tar).unwrap();
        let mut req = request(&path, root.path());
        req.format = Some(smartzip_core::ArchiveFormat::Tar);
        let session = prepare_seven(req, executable.clone(), "fixture-seven".into(), context())
            .await
            .unwrap();
        assert_eq!(session.manifest().entries[0].display_name, "dir/file.txt");
        session.execute(sink(root.path()), context()).await.unwrap();
        assert_eq!(std::fs::read(root.path().join("0")).unwrap(), b"payload");
        std::fs::remove_file(root.path().join("0")).unwrap();
        let path = root.path().join("fixture.gz");
        let mut encoder = flate2::GzBuilder::new()
            .filename("payload.txt")
            .write(File::create(&path).unwrap(), flate2::Compression::default());
        encoder.write_all(b"gzip payload").unwrap();
        encoder.finish().unwrap();
        let mut req = request(&path, root.path());
        req.format = Some(smartzip_core::ArchiveFormat::Gzip);
        let session = prepare_seven(req, executable, "fixture-seven".into(), context())
            .await
            .unwrap();
        session.execute(sink(root.path()), context()).await.unwrap();
        assert_eq!(
            std::fs::read(root.path().join("0")).unwrap(),
            b"gzip payload"
        );
    }
}

async fn verify_seven_counts(
    executable: &Path,
    id: &str,
    request: &ExtractArchiveRequest,
    entries: &[ExtractionEntry],
    token: &CancellationToken,
) -> Result<()> {
    let mut args = seven_args("l", request);
    args.extend([
        "--".into(),
        request
            .archive
            .to_str()
            .ok_or_else(|| unsupported("non-UTF-8 archive input"))?
            .into(),
    ]);
    let (output, _) =
        crate::process::run_bounded(executable, id, &args, token, crate::process::Mode::Ordinary)
            .await?;
    if output.status != Some(0) {
        return Err(seven_failure(&output, id, request));
    }
    let footer = output
        .stdout
        .lines()
        .rev()
        .find(|line| {
            line.split_whitespace()
                .any(|word| matches!(word.trim_end_matches(','), "files" | "folders"))
        })
        .ok_or_else(|| unsupported("7-Zip inventory has no physical member-count footer"))?;
    let words: Vec<_> = footer.split_whitespace().collect();
    let count = |label: &str| -> Result<usize> {
        if let Some(index) = words
            .iter()
            .position(|word| word.trim_end_matches(',') == label)
        {
            return words
                .get(index.saturating_sub(1))
                .ok_or_else(|| unsupported("ambiguous 7-Zip inventory footer"))?
                .parse()
                .map_err(|_| unsupported("invalid 7-Zip inventory count"));
        }
        Ok(0)
    };
    let files = count("files")?;
    let folders = count("folders")?;
    if !words
        .iter()
        .any(|word| matches!(word.trim_end_matches(','), "files" | "folders"))
        || files != entries.iter().filter(|entry| !entry.is_dir).count()
        || folders != entries.iter().filter(|entry| entry.is_dir).count()
    {
        return Err(unsupported(
            "technical listing is incomplete or contains injected member records",
        ));
    }
    Ok(())
}

pub(crate) async fn prepare_unrar(
    mut request: ExtractArchiveRequest,
    executable: PathBuf,
    adapter_id: String,
    context: Arc<TaskExecutionContext>,
) -> Result<Box<dyn PreparedExtraction>> {
    let token = context.cancellation_token();
    let volumes = crate::volumes::VolumeSet::collect(&request.archive)
        .map_err(|e| io(&request.archive, e))?;
    let mut sources: Vec<_> = volumes
        .members
        .iter()
        .map(|member| std::fs::canonicalize(&member.path).map_err(|e| io(&member.path, e)))
        .collect::<Result<_>>()?;
    sources.sort();
    let stats = source_stats(&sources)?;
    let source = hash_sources(&sources, &token).await?;
    let executable = std::fs::canonicalize(executable).map_err(|e| SmartZipError::io(None, e))?;
    let executable_identity = format!(
        "unrar:{}",
        hash_path(executable.clone(), token.clone()).await?
    );
    // UnRAR also resolves sibling volumes through the canonical-name aliases.
    request.archive = std::path::absolute(&request.archive).map_err(|e| io(&request.archive, e))?;
    let run = |operation: &str| {
        let args = vec![
            operation.into(),
            "-cfg-".into(),
            "-c-".into(),
            if let Some(password) = &request.password {
                format!("-p{password}")
            } else {
                "-p-".into()
            },
            "--".into(),
            request.archive.to_string_lossy().into_owned(),
        ];
        let executable = executable.clone();
        let adapter_id = adapter_id.clone();
        let token = token.clone();
        async move {
            crate::process::run_bounded(
                &executable,
                &adapter_id,
                &args,
                &token,
                crate::process::Mode::Diagnostic,
            )
            .await
            .map(|(output, _)| output)
        }
    };
    let technical = run("lt").await?;
    if technical.status != Some(0) {
        return Err(crate::test_output::password_error(
            &technical,
            "unrar",
            request.password.as_deref(),
            &request.archive,
        )
        .unwrap_or_else(|| SmartZipError::BackendFailed {
            backend: adapter_id.clone(),
            exit_code: technical.status,
            stderr: "Unrar inventory failed (diagnostics redacted)".into(),
        }));
    }
    #[cfg(windows)]
    let technical_text = technical.stdout.replace("\r\n", "\n");
    #[cfg(not(windows))]
    let technical_text = technical.stdout;
    if technical_text.contains('\u{fffd}') || technical_text.contains('\r') {
        return Err(unsupported("Unrar inventory is not unambiguous UTF-8 text"));
    }
    let mut entries = Vec::new();
    let mut current = BTreeMap::new();
    let mut encrypted = false;
    fn entry(fields: &BTreeMap<&str, &str>, id: u64) -> Result<ExtractionEntry> {
        let name = fields
            .get("Name")
            .ok_or_else(|| unsupported("Unrar inventory omitted name"))?;
        let kind = fields
            .get("Type")
            .ok_or_else(|| unsupported("Unrar inventory omitted type"))?;
        let is_dir = match *kind {
            "File" => false,
            "Directory" => true,
            _ => return Err(unsupported("Unrar link/special member cannot be mapped")),
        };
        let size = match fields.get("Size") {
            Some(size) => size
                .parse()
                .map_err(|_| unsupported("invalid Unrar inventory size"))?,
            None if is_dir => 0,
            None => return Err(unsupported("Unrar file size missing")),
        };
        let mode = permission_mode(fields.get("Attributes").copied().unwrap_or(""))?;
        let crc32 = fields
            .get("CRC32")
            .map(|value| {
                u32::from_str_radix(value, 16).map_err(|_| unsupported("invalid Unrar CRC32"))
            })
            .transpose()?;
        Ok(ExtractionEntry {
            id,
            display_name: (*name).into(),
            raw_name: None,
            source_kind: ExtractionNameSource::BackendText,
            is_dir,
            size,
            metadata: ExtractionMetadata {
                crc32,
                unix_mode: mode,
                modified_unix_seconds: fields
                    .get("mtime")
                    .and_then(|value| modified(&value.replace(',', "."))),
            },
        })
    }
    for line in technical_text.split_terminator('\n') {
        let line = line.trim_start_matches(' ').trim_end_matches('\r');
        if line.is_empty() {
            if !current.is_empty() {
                entries.push(entry(&current, entries.len() as u64)?);
                current.clear();
            }
            continue;
        }
        if current.is_empty() && !line.starts_with("Name: ") {
            continue;
        }
        let (key, value) = line
            .split_once(": ")
            .ok_or_else(|| unsupported("Unrar listing has ambiguous continuation text"))?;
        if ![
            "Name",
            "Type",
            "Size",
            "Packed size",
            "Ratio",
            "mtime",
            "ctime",
            "atime",
            "Attributes",
            "CRC32",
            "Host OS",
            "Compression",
            "Flags",
        ]
        .contains(&key)
            || current.insert(key, value).is_some()
        {
            return Err(unsupported(
                "Unrar listing contains injected or unknown fields",
            ));
        }
        encrypted |= key == "Flags" && value.to_ascii_lowercase().contains("encrypted");
    }
    if !current.is_empty() {
        entries.push(entry(&current, entries.len() as u64)?);
    }
    let bare = run("lb").await?;
    if bare.status != Some(0) {
        return Err(unsupported("Unrar cannot corroborate its member inventory"));
    }
    #[cfg(windows)]
    let bare_text = bare.stdout.replace("\r\n", "\n");
    #[cfg(not(windows))]
    let bare_text = bare.stdout;
    if bare_text.contains('\r') || bare_text.contains('\u{fffd}') {
        return Err(unsupported(
            "Unrar bare inventory contains ambiguous control/UTF-8 text",
        ));
    }
    let mut bare: Vec<_> = bare_text
        .split_terminator('\n')
        .map(str::to_owned)
        .collect();
    let mut expected: Vec<_> = entries
        .iter()
        .map(|entry| entry.display_name.clone())
        .collect();
    bare.sort();
    expected.sort();
    if bare != expected || expected.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(unsupported(
            "Unrar bare/technical inventories disagree or contain duplicate members",
        ));
    }
    let limits = ExtractionLimits::default();
    check_entries(&entries, &limits)?;
    if hash_sources(&sources, &token).await? != source {
        return Err(changed());
    }
    let manifest = make_manifest(
        entries,
        &request,
        adapter_id,
        executable_identity.clone(),
        source,
    )?;
    Ok(Box::new(SevenSession {
        request,
        manifest,
        executable,
        executable_identity,
        encrypted,
        limits,
        managed: false,
        sources,
        stats,
        token,
    }))
}

// One batch probe checks all regular files and leaf directories. Including their
// ancestor directories would mask sanitized control characters by recursively
// selecting unmatched descendants. Keep the listing path fixed and private.
fn seven_selector_entries(entries: &[ExtractionEntry]) -> Vec<&ExtractionEntry> {
    let names: BTreeSet<_> = entries
        .iter()
        .map(|entry| entry.display_name.clone())
        .collect();
    entries
        .iter()
        .filter(|entry| {
            if !entry.is_dir {
                return true;
            }
            if smartzip_core::is_root_directory_alias(&entry.display_name) {
                return false;
            }
            let prefix = format!("{}/", entry.display_name.trim_end_matches('/'));
            !names
                .range(prefix.clone()..)
                .find(|name| *name != &entry.display_name)
                .is_some_and(|name| name.starts_with(&prefix))
        })
        .collect()
}

async fn verify_seven_selectors(
    executable: &Path,
    id: &str,
    request: &ExtractArchiveRequest,
    entries: &[ExtractionEntry],
    limits: &ExtractionLimits,
    token: &CancellationToken,
) -> Result<()> {
    if entries.is_empty() {
        return Ok(());
    }
    let expected = seven_selector_entries(entries);
    if expected.is_empty() {
        return Ok(());
    }
    let mut list = tempfile::NamedTempFile::new().map_err(|e| SmartZipError::io(None, e))?;
    for entry in &expected {
        writeln!(list, "{}", entry.display_name).map_err(|e| SmartZipError::io(None, e))?;
    }
    list.flush().map_err(|e| SmartZipError::io(None, e))?;
    let mut args = seven_args("l", request);
    // This switch controls the selector-list encoding. Override the inherited
    // value rather than adding a duplicate switch, which 7-Zip rejects.
    args.retain(|arg| !arg.starts_with("-scs"));
    args.extend([
        "-slt".into(),
        "-ba".into(),
        "-scsUTF-8".into(),
        format!("-i@{}", list.path().to_string_lossy()),
        "--".into(),
        request.archive.to_string_lossy().into_owned(),
    ]);
    let (output, _) =
        crate::process::run_bounded(executable, id, &args, token, crate::process::Mode::Ordinary)
            .await?;
    if output.status != Some(0) {
        return Err(seven_failure(&output, id, request));
    }
    #[cfg(windows)]
    let listing = output.stdout.replace("\r\n", "\n");
    #[cfg(not(windows))]
    let listing = output.stdout;
    let (selected, _) = seven_entries_for_format(&listing, limits, request.format.as_ref())?;
    if selected.len() != expected.len() {
        return Err(unsupported(
            "7-Zip cannot uniquely address every member without sanitized-name ambiguity",
        ));
    }
    let expected: BTreeMap<_, _> = expected
        .into_iter()
        .map(|entry| (&entry.display_name, entry))
        .collect();
    for mut entry in selected {
        let Some(original) = expected.get(&entry.display_name) else {
            return Err(unsupported("7-Zip selectors selected unexpected members"));
        };
        entry.id = original.id;
        if entry != **original {
            return Err(unsupported(
                "selected member metadata differs from the frozen inventory",
            ));
        }
    }
    Ok(())
}
