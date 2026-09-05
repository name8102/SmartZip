use super::*;
use crate::volumes::{VolumeFamily, VolumeMember};
use std::fs::File;
use std::io;

const SOURCE: &str = "rar5-local-v1";

fn range(member: &VolumeMember, offset: u64, length: u64) -> PhysicalRange {
    PhysicalRange {
        volume: member.path.clone(),
        offset,
        length,
    }
}

fn failure(
    report: &mut DiagnosticReport,
    member: &VolumeMember,
    pass_id: u32,
    kind: EvidenceKind,
    offset: u64,
    size: u64,
    summary: &str,
) {
    report.evidence(TestEvidence {
        id: String::new(),
        kind,
        strength: EvidenceStrength::Confirmed,
        source: SOURCE.into(),
        pass_id,
        ranges: vec![range(member, offset, size)],
        reference_ranges: Vec::new(),
        metadata_trust: MetadataTrust::Structural,
        affected_entries: Vec::new(),
        summary: summary.into(),
    });
}

pub(super) fn inspect(
    set: &VolumeSet,
    control: &DiagnosticControl,
    pass_id: u32,
    report: &mut DiagnosticReport,
) -> io::Result<()> {
    if set.family == VolumeFamily::ByteSplit {
        report.stop("RAR byte-split stream has no independent physical RAR volume headers");
        return Ok(());
    }
    for member in &set.members {
        control.check()?;
        match inspect_member(set, member, control, pass_id, report) {
            Ok(()) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::Interrupted | io::ErrorKind::TimedOut
                ) =>
            {
                return Err(error)
            }
            Err(error) => report.stop(format!("{}: {error}", member.path.display())),
        }
    }
    Ok(())
}

fn inspect_member(
    set: &VolumeSet,
    member: &VolumeMember,
    control: &DiagnosticControl,
    pass_id: u32,
    report: &mut DiagnosticReport,
) -> io::Result<()> {
    let mut file = match File::open(&member.path) {
        Ok(file) => file,
        Err(error) => {
            report.unreadable.push(member.path.clone());
            return Err(error);
        }
    };
    let signature = read_at(&mut file, 0, 8)?;
    if signature != b"Rar!\x1a\x07\x01\x00" {
        report.stop(format!(
            "{}: RAR5 local checker requires an intact RAR5 signature (RAR4 uses backend evidence)",
            member.path.display()
        ));
        return Ok(());
    }
    let mut offset = 8u64;
    let mut blocks = 0;
    while offset < member.size {
        control.check()?;
        blocks += 1;
        if blocks > MAX_RECORDS {
            return Err(invalid("RAR header-count budget exceeded"));
        }
        let prefix_len = (member.size - offset).min(7) as usize;
        let prefix = read_at(&mut file, offset, prefix_len)?;
        if prefix.len() < 5 {
            failure(
                report,
                member,
                pass_id,
                EvidenceKind::StructuralTruncation,
                offset,
                prefix.len() as u64,
                "RAR5 block header is truncated at a verified boundary",
            );
            return Ok(());
        }
        let mut header = Bytes::new(&prefix);
        let expected_crc = header.u32()?;
        let header_size = match header.vint() {
            Ok(size) if size <= 2 * 1024 * 1024 => size,
            _ => {
                failure(
                    report,
                    member,
                    pass_id,
                    EvidenceKind::HeaderChecksum,
                    offset,
                    prefix.len() as u64,
                    "invalid RAR5 header-size field at a verified block boundary",
                );
                return Ok(());
            }
        };
        let prefix_size = header.pos as u64;
        let total = prefix_size
            .checked_add(header_size)
            .ok_or_else(|| invalid("RAR header size overflow"))?;
        if total > member.size - offset {
            failure(
                report,
                member,
                pass_id,
                EvidenceKind::StructuralTruncation,
                offset,
                member.size - offset,
                "RAR5 header extends beyond this physical volume; header or volume is truncated",
            );
            return Ok(());
        }
        let bytes = read_at(&mut file, offset, total as usize)?;
        if crc32fast::hash(&bytes[4..]) != expected_crc {
            failure(
                report,
                member,
                pass_id,
                EvidenceKind::HeaderChecksum,
                offset,
                total,
                "RAR5 header CRC32 mismatch; checksum and header are in this volume",
            );
            return Ok(());
        }
        report.checked(CheckedScope {
            source: SOURCE.into(),
            pass_id,
            description: "RAR5 header CRC32".into(),
            ranges: vec![range(member, offset, total)],
            passed: true,
        });
        let mut block = Bytes::new(&bytes[prefix_size as usize..]);
        let kind = block.vint()?;
        let flags = block.vint()?;
        let extra_size = if flags & 1 != 0 { block.vint()? } else { 0 };
        let data_size = if flags & 2 != 0 { block.vint()? } else { 0 };
        let data_offset = offset + total;
        if data_size > member.size - data_offset {
            failure(
                report,
                member,
                pass_id,
                EvidenceKind::StructuralTruncation,
                data_offset,
                member.size - data_offset,
                "CRC-verified RAR5 header requires packed bytes missing from this physical volume",
            );
            return Ok(());
        }
        if kind == 4 {
            report.encrypted = Some(true);
            report.stop("RAR5 encrypted headers: local checks stop after the encryption header");
            return Ok(());
        }
        if kind == 1 {
            let archive_flags = block.vint()?;
            let number = if archive_flags & 2 != 0 {
                block.vint()?
            } else {
                0
            };
            let named = if set.family == VolumeFamily::RarPart {
                member.number.saturating_sub(1)
            } else {
                member.number
            };
            if archive_flags & 1 != 0 && number != u64::from(named) {
                report.stop(format!("{}: RAR5 header volume number disagrees with filename; set may contain a foreign member", member.path.display()));
                report.evidence(TestEvidence {
                    id: String::new(),
                    kind: EvidenceKind::AmbiguousSequence,
                    strength: EvidenceStrength::Observation,
                    source: SOURCE.into(),
                    pass_id,
                    ranges: vec![range(member, offset, total)],
                    reference_ranges: Vec::new(),
                    metadata_trust: MetadataTrust::ChecksumVerified,
                    affected_entries: Vec::new(),
                    summary: "volume header number and filename disagree".into(),
                });
            }
        }
        if kind == 2 || kind == 3 {
            let file_flags = block.vint()?;
            let _unpacked = block.vint()?;
            block.vint()?; // attributes
            if file_flags & 2 != 0 {
                block.u32()?;
            }
            let crc_pos = offset + prefix_size + block.pos as u64;
            let checksum = if file_flags & 4 != 0 {
                Some(block.u32()?)
            } else {
                None
            };
            let compression = block.vint()?;
            block.vint()?; // host OS
            let name_size =
                usize::try_from(block.vint()?).map_err(|_| invalid("RAR filename too large"))?;
            let name = String::from_utf8_lossy(block.take(name_size)?).into_owned();
            let extra_size =
                usize::try_from(extra_size).map_err(|_| invalid("RAR extra area too large"))?;
            if extra_size > block.remaining() {
                return Err(invalid("invalid RAR extra area boundary"));
            }
            let mut extra = Bytes::new(&bytes[bytes.len() - extra_size..]);
            let mut encrypted = false;
            while extra.remaining() > 0 {
                let size = usize::try_from(extra.vint()?)
                    .map_err(|_| invalid("RAR extra record too large"))?;
                let mut record = Bytes::new(extra.take(size)?);
                if record.vint()? == 1 {
                    encrypted = true;
                }
            }
            if encrypted {
                report.encrypted = Some(true);
            }
            // Non-final split CRCs cover this volume's packed bytes. A final
            // CRC covers the entire decoded file, so cannot blame its volume.
            // A non-split stored entry can also be verified without decoding.
            let local_checksum =
                flags & 0x10 != 0 || flags & 0x18 == 0 && compression & 0x0380 == 0;
            if let Some(expected) = checksum.filter(|_| local_checksum && !encrypted) {
                let actual = crc_range(&mut file, data_offset, data_size, control)?;
                let passed = actual == expected;
                report.checked(CheckedScope {
                    source: SOURCE.into(),
                    pass_id,
                    description: format!("RAR5 packed segment: {name}"),
                    ranges: vec![range(member, data_offset, data_size)],
                    passed,
                });
                if !passed {
                    report.evidence(TestEvidence { id: String::new(), kind: EvidenceKind::PackedChecksum, strength: EvidenceStrength::Confirmed, source: SOURCE.into(), pass_id,
                        ranges: vec![range(member, data_offset, data_size)], reference_ranges: vec![range(member, crc_pos, 4)],
                        metadata_trust: MetadataTrust::ChecksumVerified, affected_entries: vec![name], summary: "RAR5 local packed-data CRC32 mismatch (not a final-part whole-file checksum)".into() });
                }
            }
        }
        if kind == 5 {
            let end_flags = block.vint()?;
            if end_flags & 1 != 0 {
                if let Some(next) = set.next_named_member(member) {
                    if !next.exists() && !report.missing.contains(&next) {
                        report.missing.push(next);
                    }
                } else {
                    report.stop("RAR5 end header requires another volume, but its filename cannot be inferred");
                }
            }
            return Ok(());
        }
        offset = data_offset + data_size;
    }
    failure(
        report,
        member,
        pass_id,
        EvidenceKind::StructuralTruncation,
        offset,
        0,
        "RAR5 volume ends without the required end-of-archive header",
    );
    Ok(())
}
