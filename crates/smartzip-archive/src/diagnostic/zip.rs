use super::*;
use crate::volumes::{VolumeFamily, VolumeMember, VolumeReader};
use std::fs::File;

const SOURCE: &str = "zip-local-v1";

struct Layout<'a> {
    set: &'a VolumeSet,
    last_disk: u32,
}
impl Layout<'_> {
    fn member(&self, disk: u32) -> Option<&VolumeMember> {
        if self.set.family != VolumeFamily::ZipSplit {
            return (disk == 0).then(|| self.set.members.first()).flatten();
        }
        self.set.members.iter().find(|m| {
            m.number
                == if disk == self.last_disk {
                    u32::MAX
                } else {
                    disk + 1
                }
        })
    }

    fn ranges(&self, disk: u32, offset: u64, length: u64) -> io::Result<Vec<PhysicalRange>> {
        if self.set.family == VolumeFamily::ByteSplit {
            if disk != 0 {
                return Err(invalid(
                    "ZIP byte-split stream contains native disk references",
                ));
            }
            return self
                .set
                .observed_ranges(offset, length)
                .ok_or_else(|| invalid("ZIP byte-split range unavailable"));
        }
        if disk > self.last_disk {
            return Err(invalid("ZIP references an invalid disk number"));
        }
        let mut disk = disk;
        let mut offset = offset;
        let mut left = length;
        let mut ranges = Vec::new();
        while left > 0 {
            let m = self
                .member(disk)
                .ok_or_else(|| invalid("ZIP range depends on a missing disk"))?;
            if offset > m.size {
                return Err(invalid("ZIP offset exceeds the observed disk length"));
            }
            let count = left.min(m.size - offset);
            if count > 0 {
                ranges.push(PhysicalRange {
                    volume: m.path.clone(),
                    offset,
                    length: count,
                });
            }
            left -= count;
            disk += 1;
            offset = 0;
            if disk > self.last_disk && left > 0 {
                return Err(invalid("ZIP range extends beyond the final disk"));
            }
        }
        Ok(ranges)
    }

    fn read(&self, disk: u32, offset: u64, size: usize) -> io::Result<Vec<u8>> {
        if size > MAX_METADATA {
            return Err(invalid("ZIP metadata budget exceeded"));
        }
        let ranges = self.ranges(disk, offset, size as u64)?;
        let mut reader = RangeReader::new(ranges);
        let mut buffer = vec![0; size];
        reader.read_exact(&mut buffer)?;
        Ok(buffer)
    }

    fn advance(&self, disk: u32, offset: u64, length: u64) -> io::Result<(u32, u64)> {
        if self.set.family == VolumeFamily::ByteSplit {
            return Ok((
                0,
                offset
                    .checked_add(length)
                    .ok_or_else(|| invalid("ZIP offset overflow"))?,
            ));
        }
        let mut disk = disk;
        let mut offset = offset
            .checked_add(length)
            .ok_or_else(|| invalid("ZIP offset overflow"))?;
        loop {
            let member = self
                .member(disk)
                .ok_or_else(|| invalid("ZIP metadata crosses a missing disk"))?;
            if offset < member.size || offset == member.size && disk == self.last_disk {
                return Ok((disk, offset));
            }
            offset -= member.size;
            disk += 1;
            if disk > self.last_disk {
                return Err(invalid("ZIP offset extends beyond the final disk"));
            }
        }
    }
}

struct RangeReader {
    ranges: Vec<PhysicalRange>,
    index: usize,
    consumed: u64,
    file: Option<File>,
}
impl RangeReader {
    fn new(ranges: Vec<PhysicalRange>) -> Self {
        Self {
            ranges,
            index: 0,
            consumed: 0,
            file: None,
        }
    }
}
impl Read for RangeReader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        while let Some(range) = self.ranges.get(self.index) {
            if self.consumed == range.length {
                self.index += 1;
                self.consumed = 0;
                self.file = None;
                continue;
            }
            if self.file.is_none() {
                let mut file = File::open(&range.volume)?;
                file.seek(SeekFrom::Start(range.offset))?;
                self.file = Some(file);
            }
            let length = (range.length - self.consumed).min(buffer.len() as u64) as usize;
            let count = self
                .file
                .as_mut()
                .ok_or_else(|| invalid("missing ZIP descriptor"))?
                .read(&mut buffer[..length])?;
            if count == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "ZIP volume changed while reading",
                ));
            }
            self.consumed += count as u64;
            return Ok(count);
        }
        Ok(0)
    }
}

struct DirectoryEntry {
    raw_name: Vec<u8>,
    name: String,
    flags: u16,
    method: u16,
    crc: u32,
    packed: u64,
    unpacked: u64,
    disk: u32,
    offset: u64,
    zip64: bool,
}

fn central(bytes: &[u8]) -> io::Result<DirectoryEntry> {
    let mut r = Bytes::new(bytes);
    if r.u32()? != 0x02014b50 {
        return Err(invalid("ZIP central-directory signature mismatch"));
    }
    r.take(4)?;
    let flags = r.u16()?;
    let method = r.u16()?;
    r.take(4)?;
    let crc = r.u32()?;
    let mut packed = u64::from(r.u32()?);
    let mut unpacked = u64::from(r.u32()?);
    let name_length = r.u16()? as usize;
    let extra_length = r.u16()? as usize;
    let comment_length = r.u16()? as usize;
    let mut disk = u32::from(r.u16()?);
    r.take(6)?;
    let mut offset = u64::from(r.u32()?);
    let raw_name = r.take(name_length)?.to_vec();
    let mut extra = Bytes::new(r.take(extra_length)?);
    let zip64 = packed == u32::MAX as u64
        || unpacked == u32::MAX as u64
        || offset == u32::MAX as u64
        || disk == u16::MAX as u32;
    let mut resolved = !zip64;
    while extra.remaining() > 0 {
        let kind = extra.u16()?;
        let size = extra.u16()? as usize;
        let mut values = Bytes::new(extra.take(size)?);
        if kind == 1 {
            if unpacked == u32::MAX as u64 {
                unpacked = values.u64()?;
            }
            if packed == u32::MAX as u64 {
                packed = values.u64()?;
            }
            if offset == u32::MAX as u64 {
                offset = values.u64()?;
            }
            if disk == u16::MAX as u32 {
                disk = values.u32()?;
            }
            resolved = true;
        }
    }
    r.take(comment_length)?;
    if !resolved {
        return Err(invalid("ZIP64 extra values are missing"));
    }
    Ok(DirectoryEntry {
        name: String::from_utf8_lossy(&raw_name).into_owned(),
        raw_name,
        flags,
        method,
        crc,
        packed,
        unpacked,
        disk,
        offset,
        zip64,
    })
}

fn add_failure(
    report: &mut DiagnosticReport,
    pass_id: u32,
    entry: &DirectoryEntry,
    ranges: Vec<PhysicalRange>,
    mut references: Vec<PhysicalRange>,
    summary: &str,
    independent: bool,
) {
    // ZIP's reference CRC has no enclosing metadata CRC. Retain its physical
    // directory/header location in the candidate set, even if data is local.
    let mut candidates = ranges.clone();
    candidates.extend(references.iter().cloned());
    let strength = if independent && paths(&candidates).len() == 1 {
        EvidenceStrength::Confirmed
    } else {
        EvidenceStrength::Suspect
    };
    references.sort_by(|a, b| a.volume.cmp(&b.volume).then(a.offset.cmp(&b.offset)));
    references.dedup();
    report.evidence(TestEvidence {
        id: String::new(),
        kind: if independent {
            EvidenceKind::DataChecksum
        } else {
            EvidenceKind::EntryChecksum
        },
        strength,
        source: SOURCE.into(),
        pass_id,
        ranges,
        reference_ranges: references,
        metadata_trust: MetadataTrust::Structural,
        affected_entries: vec![entry.name.clone()],
        summary: summary.into(),
    });
}

fn check_entry(
    layout: &Layout<'_>,
    entry: &DirectoryEntry,
    central_ranges: Vec<PhysicalRange>,
    failed_files: &[String],
    control: &DiagnosticControl,
    pass_id: u32,
    report: &mut DiagnosticReport,
) -> io::Result<()> {
    let local = layout.read(entry.disk, entry.offset, 30)?;
    let mut r = Bytes::new(&local);
    if r.u32()? != 0x04034b50 {
        return Err(invalid(
            "ZIP local header cannot be located from the directory",
        ));
    }
    r.u16()?;
    let flags = r.u16()?;
    let method = r.u16()?;
    r.take(4)?;
    let local_crc = r.u32()?;
    r.take(8)?;
    let name_len = r.u16()? as usize;
    let extra_len = r.u16()? as usize;
    let header_size = 30 + name_len + extra_len;
    let header = layout.read(entry.disk, entry.offset, header_size)?;
    if header[30..30 + name_len] != entry.raw_name || flags != entry.flags || method != entry.method
    {
        return Err(invalid(
            "ZIP local header and directory disagree; candidate offsets are untrusted",
        ));
    }
    let mut references = central_ranges;
    references.extend(layout.ranges(entry.disk, entry.offset, header_size as u64)?);
    let (data_disk, data_offset) = layout.advance(entry.disk, entry.offset, header_size as u64)?;
    let ranges = layout.ranges(data_disk, data_offset, entry.packed)?;
    if entry.flags & 8 != 0 {
        let (disk, offset) = layout.advance(data_disk, data_offset, entry.packed)?;
        let prefix = layout.read(disk, offset, 4)?;
        let signed = prefix == b"PK\x07\x08";
        let size = if entry.zip64 { 20 } else { 12 } + if signed { 4 } else { 0 };
        let data = layout.read(disk, offset, size)?;
        let mut descriptor = Bytes::new(&data[if signed { 4 } else { 0 }..]);
        let crc = descriptor.u32()?;
        let packed = if entry.zip64 {
            descriptor.u64()?
        } else {
            u64::from(descriptor.u32()?)
        };
        let unpacked = if entry.zip64 {
            descriptor.u64()?
        } else {
            u64::from(descriptor.u32()?)
        };
        references.extend(layout.ranges(disk, offset, size as u64)?);
        if crc != entry.crc || packed != entry.packed || unpacked != entry.unpacked {
            add_failure(
                report,
                pass_id,
                entry,
                ranges,
                references,
                "ZIP descriptor and directory disagree; either metadata location may be damaged",
                false,
            );
            return Ok(());
        }
    } else if local_crc != entry.crc {
        add_failure(
            report,
            pass_id,
            entry,
            ranges,
            references,
            "ZIP local and central reference CRCs disagree",
            false,
        );
        return Ok(());
    }
    if entry.flags & (1 | 64) != 0 {
        report.encrypted = Some(true);
        if failed_files.contains(&entry.name) {
            report.stop(
                "encrypted ZIP entry cannot be locally checked without password-dependent decoding",
            );
        }
        return Ok(());
    }
    if entry.unpacked > 10 * 1024 * 1024 * 1024
        || entry.packed > 0 && entry.unpacked / entry.packed > 10_000
    {
        return Err(invalid("ZIP diagnostic expansion budget exceeded"));
    }
    let mut decoder: Box<dyn Read> = match entry.method {
        0 => Box::new(RangeReader::new(ranges.clone())),
        8 => Box::new(flate2::read::DeflateDecoder::new(RangeReader::new(
            ranges.clone(),
        ))),
        _ => {
            if failed_files.contains(&entry.name) {
                add_failure(
                    report,
                    pass_id,
                    entry,
                    ranges,
                    references,
                    "backend failed entry depends on its packed data and ZIP checksum metadata",
                    false,
                );
            }
            report.stop(format!(
                "ZIP method {} uses backend verification only",
                entry.method
            ));
            return Ok(());
        }
    };
    let mut buffer = [0; 64 * 1024];
    let mut crc = crc32fast::Hasher::new();
    let mut unpacked = 0u64;
    let mut decode_error = None;
    loop {
        control.check()?;
        match decoder.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => {
                unpacked = unpacked
                    .checked_add(count as u64)
                    .ok_or_else(|| invalid("ZIP output size overflow"))?;
                if unpacked > entry.unpacked {
                    decode_error = Some("ZIP decoded data exceeds its declared size".to_owned());
                    break;
                }
                crc.update(&buffer[..count]);
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::PermissionDenied | io::ErrorKind::NotFound
                ) =>
            {
                return Err(error)
            }
            Err(error) => {
                decode_error = Some(error.to_string());
                break;
            }
        }
    }
    let passed =
        decode_error.is_none() && unpacked == entry.unpacked && crc.finalize() == entry.crc;
    report.checked(CheckedScope {
        source: SOURCE.into(),
        pass_id,
        description: format!("ZIP entry CRC32: {}", entry.name),
        ranges: ranges.clone(),
        passed,
    });
    if !passed {
        add_failure(
            report,
            pass_id,
            entry,
            ranges,
            references,
            "ZIP entry CRC32/decode failure; packed data and its reference metadata are candidates",
            true,
        );
    }
    Ok(())
}

pub(super) fn inspect(
    set: &VolumeSet,
    failed_files: &[String],
    control: &DiagnosticControl,
    pass_id: u32,
    report: &mut DiagnosticReport,
) -> io::Result<()> {
    if !matches!(
        set.family,
        VolumeFamily::Single | VolumeFamily::ByteSplit | VolumeFamily::ZipSplit
    ) {
        return Err(invalid("unsupported ZIP volume family"));
    }
    let (mut tail_reader, tail_size): (Box<dyn ReadSeek>, u64) =
        if set.family == VolumeFamily::ZipSplit {
            let member = set
                .members
                .iter()
                .find(|m| m.path == set.entrypoint)
                .ok_or_else(|| invalid("ZIP final disk is missing"))?;
            (Box::new(File::open(&member.path)?), member.size)
        } else {
            (
                Box::new(VolumeReader::new(set)?),
                set.byte_len().ok_or_else(|| invalid("ZIP size overflow"))?,
            )
        };
    let tail_offset = tail_size.saturating_sub(65535 + 22 + 20);
    let tail = read_at(
        &mut tail_reader,
        tail_offset,
        (tail_size - tail_offset) as usize,
    )?;
    let index = (0..tail.len().saturating_sub(21))
        .rev()
        .find(|i| {
            tail[*i..].starts_with(b"PK\x05\x06")
                && *i + 22 + u16::from_le_bytes([tail[*i + 20], tail[*i + 21]]) as usize
                    == tail.len()
        })
        .ok_or_else(|| {
            invalid("ZIP end directory is missing/truncated; physical localization unavailable")
        })?;
    let mut end = Bytes::new(&tail[index + 4..]);
    let mut last_disk = u32::from(end.u16()?);
    let mut cd_disk = u32::from(end.u16()?);
    end.u16()?;
    let mut count = u64::from(end.u16()?);
    let mut cd_size = u64::from(end.u32()?);
    let mut cd_offset = u64::from(end.u32()?);
    let zip64 = last_disk == 65535
        || cd_disk == 65535
        || count == 65535
        || cd_size == u32::MAX as u64
        || cd_offset == u32::MAX as u64;
    if zip64 {
        let locator_pos = index
            .checked_sub(20)
            .ok_or_else(|| invalid("ZIP64 locator unavailable"))?;
        let mut locator = Bytes::new(&tail[locator_pos..index]);
        if locator.u32()? != 0x07064b50 {
            return Err(invalid("ZIP64 locator missing"));
        }
        let disk = locator.u32()?;
        let offset = locator.u64()?;
        let total = locator.u32()?;
        last_disk = total
            .checked_sub(1)
            .filter(|n| *n < 4096)
            .ok_or_else(|| invalid("ZIP64 disk count exceeds budget"))?;
        let layout = Layout { set, last_disk };
        let header = layout.read(disk, offset, 56)?;
        let mut h = Bytes::new(&header);
        if h.u32()? != 0x06064b50 {
            return Err(invalid("ZIP64 end-directory signature mismatch"));
        }
        let length = h.u64()?;
        if length < 44 || length > MAX_METADATA as u64 {
            return Err(invalid("ZIP64 end-directory size unsupported"));
        }
        h.take(4)?;
        if h.u32()? != last_disk {
            return Err(invalid("ZIP64 locator and end-directory disks disagree"));
        }
        cd_disk = h.u32()?;
        h.u64()?;
        count = h.u64()?;
        cd_size = h.u64()?;
        cd_offset = h.u64()?;
    }
    if last_disk >= 4096 || count > MAX_RECORDS as u64 || cd_size > MAX_METADATA as u64 {
        return Err(invalid("ZIP directory budget exceeded"));
    }
    if set.family != VolumeFamily::ZipSplit && last_disk != 0 {
        return Err(invalid(
            "ZIP directory requires native split disks absent from the volume set",
        ));
    }
    let layout = Layout { set, last_disk };
    if set.family == VolumeFamily::ZipSplit {
        let stem = set
            .entrypoint
            .file_stem()
            .ok_or_else(|| invalid("ZIP basename unavailable"))?
            .to_string_lossy();
        for disk in 0..last_disk {
            if layout.member(disk).is_none() {
                let path = set
                    .entrypoint
                    .with_file_name(format!("{stem}.z{:02}", disk + 1));
                if !report.missing.contains(&path) {
                    report.missing.push(path);
                }
            }
        }
        if set
            .members
            .iter()
            .any(|m| m.number != u32::MAX && m.number > last_disk)
        {
            return Err(invalid(
                "ZIP filename sequence and directory disk count disagree",
            ));
        }
    }
    let directory = layout.read(cd_disk, cd_offset, cd_size as usize)?;
    let mut offset = 0usize;
    report.encrypted = Some(false);
    let mut checked_ranges = std::collections::HashSet::new();
    for _ in 0..count {
        control.check()?;
        let fixed = directory
            .get(offset..offset + 46)
            .ok_or_else(|| invalid("truncated ZIP directory entry"))?;
        let size = 46
            + u16::from_le_bytes([fixed[28], fixed[29]]) as usize
            + u16::from_le_bytes([fixed[30], fixed[31]]) as usize
            + u16::from_le_bytes([fixed[32], fixed[33]]) as usize;
        let bytes = directory
            .get(offset..offset + size)
            .ok_or_else(|| invalid("truncated ZIP directory entry fields"))?;
        let entry = central(bytes)?;
        let (disk, pos) = layout.advance(cd_disk, cd_offset, offset as u64)?;
        let references = layout.ranges(disk, pos, size as u64)?;
        offset += size;
        if !checked_ranges.insert((entry.disk, entry.offset)) {
            report.stop("duplicate ZIP local-header reference; range was checked once");
            continue;
        }
        match check_entry(
            &layout,
            &entry,
            references,
            failed_files,
            control,
            pass_id,
            report,
        ) {
            Ok(()) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::Interrupted | io::ErrorKind::TimedOut
                ) =>
            {
                return Err(error)
            }
            Err(error) => report.stop(format!("{}: {error}", entry.name)),
        }
    }
    Ok(())
}

trait ReadSeek: Read + Seek {}
impl<T: Read + Seek> ReadSeek for T {}
