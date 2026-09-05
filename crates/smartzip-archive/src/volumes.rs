//! Read-only volume discovery. Numbering is a naming observation, not proof of
//! archive health or of the original byte lengths of a damaged split stream.
use crate::integrity::PhysicalRange;
use serde::{Deserialize, Serialize};
use smartzip_core::ArchiveFormat;
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

const MAX_VOLUMES: u32 = 4096;
const MAX_DIRECTORY_ENTRIES: usize = 100_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VolumeFamily {
    Single,
    RarPart,
    RarLegacy,
    ByteSplit,
    ZipSplit,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VolumeMember {
    pub path: PathBuf,
    pub number: u32,
    pub size: u64,
    pub modified_ns: Option<u128>,
    pub identity: Option<(u64, u64)>,
}

impl VolumeMember {
    fn snapshot(path: PathBuf, number: u32) -> io::Result<Self> {
        let metadata = fs::metadata(&path)?;
        if !metadata.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "volume is not a file",
            ));
        }
        #[cfg(unix)]
        let identity = {
            use std::os::unix::fs::MetadataExt;
            Some((metadata.dev(), metadata.ino()))
        };
        #[cfg(not(unix))]
        let identity = None;
        Ok(Self {
            path,
            number,
            size: metadata.len(),
            modified_ns: metadata
                .modified()
                .ok()
                .and_then(|m| m.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_nanos()),
            identity,
        })
    }

    pub fn unchanged(&self) -> bool {
        Self::snapshot(self.path.clone(), self.number).is_ok_and(|current| current == *self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VolumeSet {
    pub family: VolumeFamily,
    pub format: Option<ArchiveFormat>,
    pub entrypoint: PathBuf,
    pub members: Vec<VolumeMember>,
    pub missing: Vec<PathBuf>,
    pub unreadable: Vec<PathBuf>,
    pub issues: Vec<String>,
    pub ambiguous: bool,
}

#[derive(Debug, Clone)]
struct Naming {
    family: VolumeFamily,
    stem: String,
    width: usize,
    number: u32,
}

impl Naming {
    fn member_name(&self, number: u32) -> String {
        match self.family {
            VolumeFamily::RarPart => format!(
                "{}.part{:0width$}.rar",
                self.stem,
                number,
                width = self.width
            ),
            VolumeFamily::RarLegacy if number == 0 => format!("{}.rar", self.stem),
            VolumeFamily::RarLegacy => format!("{}.r{:02}", self.stem, number - 1),
            VolumeFamily::ByteSplit => {
                format!("{}.{:0width$}", self.stem, number, width = self.width)
            }
            VolumeFamily::ZipSplit if number == u32::MAX => format!("{}.zip", self.stem),
            VolumeFamily::ZipSplit => format!("{}.z{:02}", self.stem, number),
            VolumeFamily::Single => self.stem.clone(),
        }
    }

    fn start(&self) -> u32 {
        if self.family == VolumeFamily::RarLegacy {
            0
        } else {
            1
        }
    }
}

fn digits(s: &str) -> Option<u32> {
    (!s.is_empty() && s.bytes().all(|c| c.is_ascii_digit()))
        .then(|| s.parse().ok())
        .flatten()
}

fn parse_name(name: &str) -> Option<Naming> {
    let lower = name.to_ascii_lowercase();
    if lower.ends_with(".rar") {
        let body = &name[..name.len() - 4];
        if let Some(pos) = body.to_ascii_lowercase().rfind(".part") {
            let suffix = &body[pos + 5..];
            if let Some(number) = digits(suffix).filter(|n| *n > 0) {
                return Some(Naming {
                    family: VolumeFamily::RarPart,
                    stem: body[..pos].into(),
                    width: suffix.len(),
                    number,
                });
            }
        }
        return Some(Naming {
            family: VolumeFamily::RarLegacy,
            stem: body.into(),
            width: 2,
            number: 0,
        });
    }
    let (stem, extension) = name.rsplit_once('.')?;
    let ext = extension.to_ascii_lowercase();
    if ext == "zip" {
        return Some(Naming {
            family: VolumeFamily::ZipSplit,
            stem: stem.into(),
            width: 2,
            number: u32::MAX,
        });
    }
    if let Some(number) = ext.strip_prefix('r').and_then(digits) {
        return Some(Naming {
            family: VolumeFamily::RarLegacy,
            stem: stem.into(),
            width: 2,
            number: number.checked_add(1)?,
        });
    }
    if let Some(number) = ext.strip_prefix('z').and_then(digits).filter(|n| *n > 0) {
        return Some(Naming {
            family: VolumeFamily::ZipSplit,
            stem: stem.into(),
            width: 2,
            number,
        });
    }
    if extension.len() >= 3 {
        if let Some(number) = digits(extension).filter(|n| *n > 0) {
            return Some(Naming {
                family: VolumeFamily::ByteSplit,
                stem: stem.into(),
                width: extension.len(),
                number,
            });
        }
    }
    None
}

fn absolute_name(path: &Path) -> io::Result<PathBuf> {
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let parent = path.parent().unwrap_or(Path::new("/"));
    let parent = parent.canonicalize()?;
    Ok(parent.join(path.file_name().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "expected an archive filename")
    })?))
}

pub fn signature_format(path: &Path) -> Option<ArchiveFormat> {
    let mut header = [0u8; 8];
    let count = File::open(path).ok()?.read(&mut header).ok()?;
    let bytes = &header[..count];
    if bytes.starts_with(b"7z\xbc\xaf\x27\x1c") {
        Some(ArchiveFormat::SevenZip)
    } else if bytes.starts_with(b"Rar!\x1a\x07") {
        Some(ArchiveFormat::Rar)
    } else if bytes.starts_with(b"PK\x03\x04")
        || bytes.starts_with(b"PK\x05\x06")
        || bytes.starts_with(b"PK\x07\x08")
    {
        Some(ArchiveFormat::Zip)
    } else {
        None
    }
}

/// A bounded naming hint, not proof that the ZIP directory is intact.
pub(crate) fn zip_last_disk_hint(path: &Path) -> Option<u32> {
    let mut file = File::open(path).ok()?;
    let length = file.metadata().ok()?.len();
    let start = length.saturating_sub(65535 + 22 + 20);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut bytes = vec![0; (length - start) as usize];
    file.read_exact(&mut bytes).ok()?;
    let pos = (0..bytes.len().saturating_sub(21)).rev().find(|i| {
        bytes[*i..].starts_with(b"PK\x05\x06")
            && *i + 22 + u16::from_le_bytes([bytes[*i + 20], bytes[*i + 21]]) as usize
                == bytes.len()
    })?;
    let disk = u16::from_le_bytes([bytes[pos + 4], bytes[pos + 5]]);
    if disk == u16::MAX {
        let locator = pos.checked_sub(20)?;
        if !bytes[locator..].starts_with(b"PK\x06\x07") {
            return None;
        }
        let total = u32::from_le_bytes(bytes.get(locator + 16..locator + 20)?.try_into().ok()?);
        total.checked_sub(1).filter(|disk| *disk < MAX_VOLUMES)
    } else {
        Some(u32::from(disk))
    }
}

impl VolumeSet {
    pub fn collect(input: &Path) -> io::Result<Self> {
        let input = absolute_name(input)?;
        let parent = input
            .parent()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no parent directory"))?;
        let mut naming = input
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(parse_name);
        let mut found = Vec::new();
        let mut issues = Vec::new();
        let mut ambiguous = false;
        if let Some(pattern) = &naming {
            for (index, entry) in fs::read_dir(parent)?.enumerate() {
                if index >= MAX_DIRECTORY_ENTRIES {
                    issues.push("volume discovery directory-entry budget reached".into());
                    ambiguous = true;
                    break;
                }
                let entry = entry?;
                let Some(other) = entry.file_name().to_str().and_then(parse_name) else {
                    continue;
                };
                if other.family == pattern.family && other.stem == pattern.stem {
                    if other.number != u32::MAX && other.number >= MAX_VOLUMES {
                        issues.push("volume number exceeds discovery budget".into());
                        ambiguous = true;
                        continue;
                    }
                    found.push((other.number, entry.path()));
                }
            }
            // A plain ZIP/RAR with no sibling volumes is still a single file.
            let zip_disks = if pattern.family == VolumeFamily::ZipSplit {
                zip_last_disk_hint(&parent.join(pattern.member_name(u32::MAX)))
            } else {
                None
            };
            if zip_disks == Some(0) && found.iter().any(|(number, _)| *number != u32::MAX) {
                ambiguous = true;
                issues.push("ZIP directory says single disk but split-named siblings exist".into());
            }
            if pattern.number == pattern.start()
                && pattern.family == VolumeFamily::RarLegacy
                && found.len() <= 1
                || pattern.family == VolumeFamily::ZipSplit
                    && pattern.number == u32::MAX
                    && found.len() <= 1
                    && zip_disks.is_none_or(|disk| disk == 0)
            {
                naming = None;
                found.clear();
            }
        }
        let mut family = naming
            .as_ref()
            .map(|n| n.family)
            .unwrap_or(VolumeFamily::Single);
        let mut entrypoint = naming
            .as_ref()
            .map(|n| {
                parent.join(n.member_name(if family == VolumeFamily::ZipSplit {
                    u32::MAX
                } else {
                    n.start()
                }))
            })
            .unwrap_or_else(|| input.clone());
        if let Some((_, actual)) = found.iter().find(|(n, _)| {
            *n == if family == VolumeFamily::ZipSplit {
                u32::MAX
            } else {
                naming.as_ref().map(|p| p.start()).unwrap_or(0)
            }
        }) {
            entrypoint = actual.clone();
        }
        let mut format = signature_format(&entrypoint);
        // Unknown *.001 data is not promoted based on its suffix alone. Known
        // archive extensions retain a corrupt/missing-header diagnostic path.
        if family == VolumeFamily::ByteSplit && format.is_none() {
            format = naming.as_ref().and_then(|n| {
                match Path::new(&n.stem)
                    .extension()?
                    .to_str()?
                    .to_ascii_lowercase()
                    .as_str()
                {
                    "7z" => Some(ArchiveFormat::SevenZip),
                    "zip" => Some(ArchiveFormat::Zip),
                    "rar" => Some(ArchiveFormat::Rar),
                    _ => None,
                }
            });
            if format.is_none() {
                family = VolumeFamily::Single;
                naming = None;
                entrypoint = input.clone();
                found.clear();
            }
        }
        if format.is_none() {
            format = match family {
                VolumeFamily::RarPart | VolumeFamily::RarLegacy => Some(ArchiveFormat::Rar),
                VolumeFamily::ZipSplit => Some(ArchiveFormat::Zip),
                _ => None,
            };
        }
        if naming.is_none() {
            found.push((0, input.clone()));
        }
        found.sort();
        for pair in found.windows(2) {
            if pair[0].0 == pair[1].0 {
                issues.push(format!("duplicate volume number {}", pair[0].0));
                ambiguous = true;
            }
        }
        let mut missing = Vec::new();
        if let Some(pattern) = &naming {
            let mut max = found
                .iter()
                .map(|(n, _)| *n)
                .filter(|n| *n != u32::MAX)
                .max()
                .unwrap_or(pattern.start());
            if family == VolumeFamily::ZipSplit {
                if let Some(disk) = zip_last_disk_hint(&entrypoint) {
                    max = max.max(disk);
                }
            }
            for number in pattern.start()..=max.min(MAX_VOLUMES - 1) {
                if !found.iter().any(|(n, _)| *n == number) {
                    missing.push(parent.join(pattern.member_name(number)));
                }
            }
            if !found.iter().any(|(_, p)| *p == entrypoint) && !missing.contains(&entrypoint) {
                missing.push(entrypoint.clone());
            }
        }
        let mut members = Vec::new();
        let mut unreadable = Vec::new();
        for (number, path) in found {
            match VolumeMember::snapshot(path.clone(), number) {
                Ok(member) => {
                    if File::open(&path).is_err() {
                        unreadable.push(path.clone());
                    }
                    members.push(member);
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    if !missing.contains(&path) {
                        missing.push(path);
                    }
                }
                Err(_) => unreadable.push(path),
            }
        }
        // RAR members each carry their own signature. A different recognized
        // container is evidence of an ambiguous set, never of a specific CRC.
        if matches!(family, VolumeFamily::RarPart | VolumeFamily::RarLegacy)
            && members
                .iter()
                .any(|m| signature_format(&m.path).is_some_and(|f| f != ArchiveFormat::Rar))
        {
            ambiguous = true;
            issues.push("volume name and container signature disagree".into());
        }
        Ok(Self {
            family,
            format,
            entrypoint,
            members,
            missing,
            unreadable,
            issues,
            ambiguous,
        })
    }

    pub fn paths(&self) -> Vec<PathBuf> {
        self.members.iter().map(|m| m.path.clone()).collect()
    }

    pub(crate) fn next_named_member(&self, member: &VolumeMember) -> Option<PathBuf> {
        let naming = parse_name(member.path.file_name()?.to_str()?)?;
        if !matches!(
            naming.family,
            VolumeFamily::RarPart | VolumeFamily::RarLegacy
        ) {
            return None;
        }
        let next = naming.number.checked_add(1)?;
        if next >= MAX_VOLUMES {
            return None;
        }
        Some(member.path.parent()?.join(naming.member_name(next)))
    }

    pub fn byte_len(&self) -> Option<u64> {
        self.members
            .iter()
            .try_fold(0u64, |sum, m| sum.checked_add(m.size))
    }

    /// Maps the observed concatenation only. Callers must independently
    /// validate archive offsets before using it as an original-stream map.
    pub fn observed_ranges(&self, offset: u64, length: u64) -> Option<Vec<PhysicalRange>> {
        if !self.missing.is_empty() || self.ambiguous {
            return None;
        }
        let end = offset.checked_add(length)?;
        if end > self.byte_len()? {
            return None;
        }
        let mut base = 0u64;
        let mut ranges = Vec::new();
        for m in &self.members {
            let next = base.checked_add(m.size)?;
            let start = base.max(offset);
            let stop = next.min(end);
            if start < stop {
                ranges.push(PhysicalRange {
                    volume: m.path.clone(),
                    offset: start - base,
                    length: stop - start,
                });
            }
            base = next;
        }
        Some(ranges)
    }
}

/// A seekable view of existing contiguous byte-split members. Keeps at most
/// one input descriptor open and never materializes a joined archive.
pub struct VolumeReader {
    set: VolumeSet,
    position: u64,
    current: Option<(usize, File)>,
}

impl VolumeReader {
    pub fn new(set: &VolumeSet) -> io::Result<Self> {
        if !set.missing.is_empty()
            || !set.unreadable.is_empty()
            || set.ambiguous
            || set.byte_len().is_none()
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "volume sequence is incomplete or ambiguous",
            ));
        }
        Ok(Self {
            set: set.clone(),
            position: 0,
            current: None,
        })
    }
}

impl Read for VolumeReader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        let mut base = 0;
        for (index, member) in self.set.members.iter().enumerate() {
            if self.position < base + member.size {
                if !self.current.as_ref().is_some_and(|(i, _)| *i == index) {
                    self.current = Some((index, File::open(&member.path)?));
                }
                let Some((_, file)) = self.current.as_mut() else {
                    return Err(io::Error::other("missing volume descriptor"));
                };
                file.seek(SeekFrom::Start(self.position - base))?;
                let size = buffer
                    .len()
                    .min((base + member.size - self.position).min(usize::MAX as u64) as usize);
                let count = file.read(&mut buffer[..size])?;
                if count == 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "volume changed while reading",
                    ));
                }
                self.position += count as u64;
                return Ok(count);
            }
            base += member.size;
        }
        Ok(0)
    }
}

impl Seek for VolumeReader {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        let pos = match from {
            SeekFrom::Start(pos) => i128::from(pos),
            SeekFrom::End(offset) => {
                i128::from(self.set.byte_len().unwrap_or(0)) + i128::from(offset)
            }
            SeekFrom::Current(offset) => i128::from(self.position) + i128::from(offset),
        };
        self.position = u64::try_from(pos)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid seek"))?;
        Ok(self.position)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arbitrary_member_numerical_order_and_missing_volume() {
        let dir = tempfile::tempdir().unwrap();
        for i in [1, 3, 10] {
            fs::write(
                dir.path().join(format!("a.part{i:02}.rar")),
                b"Rar!\x1a\x07\x01\x00",
            )
            .unwrap();
        }
        let set = VolumeSet::collect(&dir.path().join("a.part03.rar")).unwrap();
        assert_eq!(
            set.members.iter().map(|m| m.number).collect::<Vec<_>>(),
            [1, 3, 10]
        );
        assert!(set.entrypoint.ends_with("a.part01.rar"));
        assert!(set.missing.iter().any(|p| p.ends_with("a.part02.rar")));
        assert_eq!(
            set,
            VolumeSet::collect(&dir.path().join("a.part10.rar")).unwrap()
        );
    }

    #[test]
    fn split_zip_uses_final_disk_and_accepts_unequal_lengths() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.z01"), b"PK\x07\x08abcd").unwrap();
        fs::write(dir.path().join("a.z02"), b"hi").unwrap();
        fs::write(dir.path().join("a.zip"), b"PK\x05\x06").unwrap();
        let set = VolumeSet::collect(&dir.path().join("a.z02")).unwrap();
        assert_eq!(set.family, VolumeFamily::ZipSplit);
        assert!(set.entrypoint.ends_with("a.zip"));
        assert!(set.missing.is_empty());
        assert!(!set.ambiguous);
    }

    #[test]
    fn byte_split_reader_crosses_physical_boundaries() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.7z.001"), b"7z\xbc\xaf\x27\x1c1").unwrap();
        fs::write(dir.path().join("a.7z.002"), b"234").unwrap();
        let set = VolumeSet::collect(&dir.path().join("a.7z.002")).unwrap();
        let mut reader = VolumeReader::new(&set).unwrap();
        reader.seek(SeekFrom::Start(6)).unwrap();
        let mut buf = [0; 4];
        reader.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"1234");
        assert_eq!(set.observed_ranges(6, 4).unwrap().len(), 2);
        fs::write(dir.path().join("random.001"), b"random bytes").unwrap();
        let random = VolumeSet::collect(&dir.path().join("random.001")).unwrap();
        assert_eq!(random.family, VolumeFamily::Single);
        assert!(random.format.is_none());
    }

    #[test]
    fn duplicate_numbers_are_ambiguous_and_missing_first_is_retained() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["a.part2.rar", "a.part02.rar"] {
            fs::write(dir.path().join(name), b"Rar!\x1a\x07\x01\x00").unwrap();
        }
        let set = VolumeSet::collect(&dir.path().join("a.part02.rar")).unwrap();
        assert!(set.ambiguous);
        assert!(set.missing.iter().any(|p| p.ends_with("a.part01.rar")));
        assert!(VolumeReader::new(&set).is_err());
    }
}
