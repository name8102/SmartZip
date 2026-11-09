//! Seekable source views; embedded archives and byte splits need no copy.
use smartzip_core::ArchiveInput;
use std::{
    fs::File,
    io::{self, Read, Seek, SeekFrom},
    path::PathBuf,
};

pub struct InputReader {
    parts: Vec<Part>,
    position: u64,
    length: u64,
}
struct Part {
    file: File,
    offset: u64,
    start: u64,
    length: u64,
}
impl InputReader {
    pub fn open(input: &ArchiveInput) -> io::Result<Self> {
        let paths = byte_split_paths(input);
        let mut parts = Vec::new();
        let mut length: u64 = 0;
        for path in paths {
            let file = File::open(path)?;
            let size = file.metadata()?.len();
            parts.push(Part {
                file,
                offset: 0,
                start: length,
                length: size,
            });
            length = length
                .checked_add(size)
                .ok_or_else(|| io::Error::other("source length overflow"))?;
        }
        let end = input
            .length
            .map(|n| {
                input
                    .offset
                    .checked_add(n)
                    .ok_or_else(|| io::Error::other("source range overflow"))
            })
            .transpose()?
            .unwrap_or(length);
        if input.offset > length || end > length {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "source range exceeds input",
            ));
        }
        let mut sliced = Vec::new();
        let mut start = 0;
        for mut part in parts {
            let lo = part.start.max(input.offset);
            let hi = (part.start + part.length).min(end);
            if hi > lo {
                part.offset = lo - part.start;
                part.start = start;
                part.length = hi - lo;
                start += part.length;
                sliced.push(part);
            }
        }
        Ok(Self {
            parts: sliced,
            position: 0,
            length: end - input.offset,
        })
    }
}
fn byte_split_paths(input: &ArchiveInput) -> Vec<PathBuf> {
    let name = input
        .volumes
        .iter()
        .find(|(_, p)| *p == &input.path)
        .map(|(n, _)| n.as_str())
        .or_else(|| input.path.file_name()?.to_str())
        .unwrap_or("");
    if let Some((base, ext)) = name.rsplit_once('.') {
        if !ext.is_empty() && ext.bytes().all(|c| c.is_ascii_digit()) {
            let mut numbered: Vec<_> = input
                .volumes
                .iter()
                .filter_map(|(n, p)| {
                    let (b, e) = n.rsplit_once('.')?;
                    (b == base)
                        .then(|| e.parse::<u64>().ok().map(|n| (n, p.clone())))
                        .flatten()
                })
                .collect();
            numbered.sort_by_key(|(n, _)| *n);
            if !numbered.is_empty() {
                return numbered.into_iter().map(|(_, p)| p).collect();
            }
        }
    }
    vec![input.path.clone()]
}
impl Read for InputReader {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        if bytes.is_empty() || self.position >= self.length {
            return Ok(0);
        }
        let part = self
            .parts
            .iter_mut()
            .find(|p| self.position >= p.start && self.position < p.start + p.length)
            .ok_or_else(|| io::Error::other("source mapping missing"))?;
        let local = self.position - part.start;
        part.file.seek(SeekFrom::Start(part.offset + local))?;
        let count = (bytes.len() as u64).min(part.length - local) as usize;
        let n = part.file.read(&mut bytes[..count])?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "source changed or truncated",
            ));
        }
        self.position += n as u64;
        Ok(n)
    }
}
impl Seek for InputReader {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        let pos = match from {
            SeekFrom::Start(n) => n as i128,
            SeekFrom::Current(n) => self.position as i128 + n as i128,
            SeekFrom::End(n) => self.length as i128 + n as i128,
        };
        self.position = u64::try_from(pos).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "negative or overflowing seek")
        })?;
        Ok(self.position)
    }
}

pub(crate) fn resolve_input(
    path: &std::path::Path,
    input: Option<&ArchiveInput>,
) -> io::Result<ArchiveInput> {
    if let Some(input) = input {
        return Ok(input.clone());
    }
    let set = crate::volumes::VolumeSet::collect(path)?;
    Ok(ArchiveInput {
        path: set.entrypoint,
        offset: 0,
        length: None,
        volumes: set
            .members
            .into_iter()
            .filter_map(|m| Some((m.path.file_name()?.to_str()?.to_owned(), m.path)))
            .collect(),
    })
}

pub(crate) fn catalog_id(input: &ArchiveInput, library: &std::path::Path) -> io::Result<String> {
    let mut hash = blake3::Hasher::new();
    hash.update(library.as_os_str().as_encoded_bytes());
    hash.update(&input.offset.to_le_bytes());
    hash.update(&input.length.unwrap_or(u64::MAX).to_le_bytes());
    for name in input.volumes.keys() {
        hash.update(&(name.len() as u64).to_le_bytes());
        hash.update(name.as_bytes());
    }
    for path in std::iter::once(&input.path).chain(input.volumes.values()) {
        let metadata = std::fs::metadata(path)?;
        hash.update(path.as_os_str().as_encoded_bytes());
        hash.update(&metadata.len().to_le_bytes());
        if let Ok(time) = metadata.modified().and_then(|t| {
            t.duration_since(std::time::UNIX_EPOCH)
                .map_err(io::Error::other)
        }) {
            hash.update(&time.as_nanos().to_le_bytes());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            hash.update(&metadata.dev().to_le_bytes());
            hash.update(&metadata.ino().to_le_bytes());
        }
    }
    Ok(hash.finalize().to_hex().to_string())
}

/// Only path-only adapters request this representation. Native execution never does.
pub(crate) struct MaterializedInput {
    pub path: PathBuf,
    _keep: Option<tempfile::TempDir>,
}
impl MaterializedInput {
    pub fn new(
        request_path: &std::path::Path,
        input: Option<&ArchiveInput>,
    ) -> smartzip_core::Result<Self> {
        use smartzip_core::SmartZipError;
        let Some(input) = input else {
            return Ok(Self {
                path: request_path.into(),
                _keep: None,
            });
        };
        if input.offset == 0 && input.length.is_none() && input.volumes.is_empty() {
            return Ok(Self {
                path: input.path.clone(),
                _keep: None,
            });
        }
        let dir = tempfile::tempdir().map_err(|e| SmartZipError::io(None, e))?;
        let name = request_path
            .file_name()
            .unwrap_or(std::ffi::OsStr::new("archive.rar"));
        let path = dir.path().join(name);
        if !input.volumes.is_empty() && input.offset == 0 && input.length.is_none() {
            for (logical, physical) in &input.volumes {
                if std::path::Path::new(logical).file_name() != Some(std::ffi::OsStr::new(logical))
                {
                    return Err(SmartZipError::UnsafeArchivePath {
                        entry: logical.clone(),
                    });
                }
                let destination = dir.path().join(logical);
                #[cfg(unix)]
                std::os::unix::fs::symlink(
                    std::fs::canonicalize(physical)
                        .map_err(|e| SmartZipError::io(Some(physical.clone()), e))?,
                    &destination,
                )
                .map_err(|e| SmartZipError::io(Some(destination), e))?;
                #[cfg(not(unix))]
                std::fs::hard_link(physical, &destination)
                    .or_else(|_| std::fs::copy(physical, &destination).map(|_| ()))
                    .map_err(|e| SmartZipError::io(Some(destination), e))?;
            }
        } else {
            let mut source = InputReader::open(input)
                .map_err(|e| SmartZipError::io(Some(input.path.clone()), e))?;
            let mut dest =
                File::create(&path).map_err(|e| SmartZipError::io(Some(path.clone()), e))?;
            std::io::copy(&mut source, &mut dest)
                .map_err(|e| SmartZipError::io(Some(path.clone()), e))?;
        }
        Ok(Self {
            path,
            _keep: Some(dir),
        })
    }
}
