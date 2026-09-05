//! Folder dependency mapping from CRC-verified 7z metadata. Packed offsets
//! refer to the compressed stream, never to a file's uncompressed offset.
use super::*;
use crate::volumes::{VolumeFamily, VolumeReader};
use std::io::Cursor;

const SOURCE: &str = "7z-local-v1";

#[derive(Default)]
struct Streams {
    pack_pos: u64,
    sizes: Vec<u64>,
    digests: Vec<Option<u32>>,
    folders: Vec<Folder>,
}
struct Coder {
    id: Vec<u8>,
    properties: Vec<u8>,
    inputs: usize,
    outputs: usize,
}
struct Folder {
    coders: Vec<Coder>,
    packed_count: usize,
    output_count: usize,
    final_output: usize,
    unpack_sizes: Vec<u64>,
    digest: Option<u32>,
    substreams: usize,
}

fn bits(bytes: &mut Bytes<'_>, count: usize) -> io::Result<Vec<bool>> {
    if count > MAX_RECORDS {
        return Err(invalid("7z bitset budget exceeded"));
    }
    let data = bytes.take(count.div_ceil(8))?;
    Ok((0..count)
        .map(|i| data[i / 8] & (0x80 >> (i % 8)) != 0)
        .collect())
}

fn digests(bytes: &mut Bytes<'_>, count: usize) -> io::Result<Vec<Option<u32>>> {
    let defined = if bytes.byte()? != 0 {
        vec![true; count]
    } else {
        bits(bytes, count)?
    };
    defined
        .into_iter()
        .map(|yes| if yes { bytes.u32().map(Some) } else { Ok(None) })
        .collect()
}

fn expect(bytes: &mut Bytes<'_>, expected: u8) -> io::Result<()> {
    if bytes.byte()? == expected {
        Ok(())
    } else {
        Err(invalid("unexpected 7z metadata property"))
    }
}

fn folder(bytes: &mut Bytes<'_>) -> io::Result<Folder> {
    let count = bytes.count()?;
    if count == 0 || count > 64 {
        return Err(invalid("7z coder count unsupported"));
    }
    let mut coders = Vec::new();
    let mut inputs = 0usize;
    let mut outputs = 0usize;
    for _ in 0..count {
        let flags = bytes.byte()?;
        if flags & 0xc0 != 0 || flags & 15 == 0 {
            return Err(invalid("unsupported 7z coder flags"));
        }
        let id = bytes.take((flags & 15) as usize)?.to_vec();
        let (num_in, num_out) = if flags & 16 != 0 {
            (bytes.count()?, bytes.count()?)
        } else {
            (1, 1)
        };
        if num_in == 0 || num_out == 0 {
            return Err(invalid("empty 7z coder"));
        }
        inputs = inputs
            .checked_add(num_in)
            .filter(|n| *n <= MAX_RECORDS)
            .ok_or_else(|| invalid("7z stream budget exceeded"))?;
        outputs = outputs
            .checked_add(num_out)
            .filter(|n| *n <= MAX_RECORDS)
            .ok_or_else(|| invalid("7z stream budget exceeded"))?;
        let properties = if flags & 32 != 0 {
            let size = bytes.count()?;
            bytes.take(size)?.to_vec()
        } else {
            Vec::new()
        };
        coders.push(Coder {
            id,
            properties,
            inputs: num_in,
            outputs: num_out,
        });
    }
    let bindings = outputs
        .checked_sub(1)
        .ok_or_else(|| invalid("7z folder has no output"))?;
    let packed_count = inputs
        .checked_sub(bindings)
        .filter(|n| *n > 0)
        .ok_or_else(|| invalid("invalid 7z binding graph"))?;
    let mut bound_in = vec![false; inputs];
    let mut bound_out = vec![false; outputs];
    for _ in 0..bindings {
        let input = bytes.count()?;
        let output = bytes.count()?;
        if input >= inputs || output >= outputs || bound_in[input] || bound_out[output] {
            return Err(invalid("invalid 7z binding index"));
        }
        bound_in[input] = true;
        bound_out[output] = true;
    }
    if packed_count > 1 {
        for _ in 0..packed_count {
            let input = bytes.count()?;
            if input >= inputs || bound_in[input] {
                return Err(invalid("invalid 7z packed input index"));
            }
            bound_in[input] = true;
        }
    }
    let final_output = bound_out
        .iter()
        .position(|bound| !bound)
        .ok_or_else(|| invalid("7z folder has no final output"))?;
    Ok(Folder {
        coders,
        packed_count,
        output_count: outputs,
        final_output,
        unpack_sizes: Vec::new(),
        digest: None,
        substreams: 1,
    })
}

fn streams(bytes: &mut Bytes<'_>) -> io::Result<Streams> {
    let mut streams = Streams::default();
    let mut seen = [false; 3];
    loop {
        match bytes.byte()? {
            0 => break,
            6 if !seen[0] => {
                seen[0] = true;
                streams.pack_pos = bytes.seven()?;
                let count = bytes.count()?;
                expect(bytes, 9)?;
                streams.sizes = (0..count)
                    .map(|_| bytes.seven())
                    .collect::<io::Result<_>>()?;
                let next = bytes.byte()?;
                streams.digests = if next == 10 {
                    digests(bytes, count)?
                } else {
                    vec![None; count]
                };
                if next == 10 {
                    expect(bytes, 0)?;
                } else if next != 0 {
                    return Err(invalid("unsupported 7z pack property"));
                }
            }
            7 if !seen[1] => {
                seen[1] = true;
                expect(bytes, 11)?;
                let count = bytes.count()?;
                if bytes.byte()? != 0 {
                    return Err(invalid("external 7z folders are not locally mapped"));
                }
                streams.folders = (0..count)
                    .map(|_| folder(bytes))
                    .collect::<io::Result<_>>()?;
                expect(bytes, 12)?;
                for f in &mut streams.folders {
                    f.unpack_sizes = (0..f.output_count)
                        .map(|_| bytes.seven())
                        .collect::<io::Result<_>>()?;
                }
                let next = bytes.byte()?;
                if next == 10 {
                    for (f, crc) in streams.folders.iter_mut().zip(digests(bytes, count)?) {
                        f.digest = crc;
                    }
                    expect(bytes, 0)?;
                } else if next != 0 {
                    return Err(invalid("unsupported 7z unpack property"));
                }
            }
            8 if !seen[2] => {
                seen[2] = true;
                let mut next = bytes.byte()?;
                if next == 13 {
                    for f in &mut streams.folders {
                        f.substreams = bytes.count()?;
                    }
                    next = bytes.byte()?;
                }
                let count = streams
                    .folders
                    .iter()
                    .try_fold(0usize, |n, f| n.checked_add(f.substreams))
                    .filter(|n| *n <= MAX_RECORDS)
                    .ok_or_else(|| invalid("7z substream budget exceeded"))?;
                if next == 9 {
                    for f in &streams.folders {
                        let mut sum = 0u64;
                        for _ in 1..f.substreams {
                            sum = sum
                                .checked_add(bytes.seven()?)
                                .ok_or_else(|| invalid("7z substream length overflow"))?;
                        }
                        if sum > f.unpack_sizes[f.final_output] {
                            return Err(invalid("7z substreams exceed folder output"));
                        }
                    }
                    next = bytes.byte()?;
                } else if streams.folders.iter().any(|f| f.substreams > 1) {
                    return Err(invalid("missing 7z substream sizes"));
                }
                if next == 10 {
                    let inherited = streams
                        .folders
                        .iter()
                        .filter(|f| f.substreams == 1 && f.digest.is_some())
                        .count();
                    digests(
                        bytes,
                        count
                            .checked_sub(inherited)
                            .ok_or_else(|| invalid("invalid 7z substream digests"))?,
                    )?;
                    next = bytes.byte()?;
                }
                if next != 0 {
                    return Err(invalid("unsupported 7z substream property"));
                }
            }
            _ => return Err(invalid("unsupported or duplicate 7z streams property")),
        }
    }
    let packed = streams
        .folders
        .iter()
        .try_fold(0usize, |n, f| n.checked_add(f.packed_count))
        .ok_or_else(|| invalid("7z packed stream count overflow"))?;
    if packed != streams.sizes.len() {
        return Err(invalid("7z folder and packed-stream counts disagree"));
    }
    Ok(streams)
}

fn files(bytes: &mut Bytes<'_>) -> io::Result<Vec<(String, bool)>> {
    let count = bytes.count()?;
    let mut names = Vec::new();
    let mut empty = vec![false; count];
    loop {
        let kind = bytes.byte()?;
        if kind == 0 {
            break;
        }
        let size = usize::try_from(bytes.seven()?).map_err(|_| invalid("7z property too large"))?;
        let mut value = Bytes::new(bytes.take(size)?);
        match kind {
            14 => empty = bits(&mut value, count)?,
            17 => {
                if value.byte()? != 0 {
                    return Err(invalid("external 7z filenames are not locally mapped"));
                }
                for _ in 0..count {
                    let mut name = Vec::new();
                    loop {
                        let code = value.u16()?;
                        if code == 0 {
                            break;
                        }
                        name.push(code);
                    }
                    names.push(String::from_utf16_lossy(&name));
                }
                if value.remaining() != 0 {
                    return Err(invalid("extra 7z filename data"));
                }
            }
            _ => {}
        }
    }
    if names.len() != count {
        return Err(invalid("7z filenames unavailable"));
    }
    if names
        .iter()
        .any(|name: &String| name.chars().any(char::is_control))
    {
        return Err(invalid(
            "7z filenames containing control characters cannot be matched to text diagnostics",
        ));
    }
    Ok(names.into_iter().zip(empty).collect())
}

fn parse_header(bytes: &[u8]) -> io::Result<(Streams, Vec<(String, bool)>)> {
    let mut reader = Bytes::new(bytes);
    expect(&mut reader, 1)?;
    let mut main = Streams::default();
    let mut names = Vec::new();
    let mut got_main = false;
    loop {
        match reader.byte()? {
            0 => break,
            2 => loop {
                if reader.byte()? == 0 {
                    break;
                }
                let size = usize::try_from(reader.seven()?)
                    .map_err(|_| invalid("7z property too large"))?;
                reader.take(size)?;
            },
            3 => return Err(invalid(
                "7z additional streams require external metadata; candidate range remains unknown",
            )),
            4 if !got_main => {
                main = streams(&mut reader)?;
                got_main = true;
            }
            5 => names = files(&mut reader)?,
            _ => return Err(invalid("unsupported 7z header property")),
        }
    }
    if reader.remaining() != 0 {
        return Err(invalid("extra 7z header bytes"));
    }
    Ok((main, names))
}

fn decode_header(
    input: &[u8],
    folder: &Folder,
    control: &DiagnosticControl,
) -> io::Result<Vec<u8>> {
    if folder.coders.len() != 1 || folder.packed_count != 1 {
        return Err(invalid(
            "complex encoded 7z header codec is not locally supported",
        ));
    }
    let coder = &folder.coders[0];
    if coder.inputs != 1 || coder.outputs != 1 {
        return Err(invalid(
            "multi-stream encoded 7z header is not locally supported",
        ));
    }
    let size = folder.unpack_sizes[folder.final_output];
    if size > MAX_METADATA as u64 {
        return Err(invalid("decoded 7z header size budget exceeded"));
    }
    let mut decoder: Box<dyn Read + '_> = match coder.id.as_slice() {
        [0] => Box::new(Cursor::new(input)),
        [3, 1, 1] if coder.properties.len() == 5 => {
            let mut props = Bytes::new(&coder.properties);
            let property = props.byte()?;
            let dictionary = props.u32()?;
            if dictionary > MAX_METADATA as u32 {
                return Err(invalid("7z header dictionary budget exceeded"));
            }
            Box::new(lzma_rust2::LzmaReader::new_with_props(
                Cursor::new(input),
                size,
                property,
                dictionary,
                None,
            )?)
        }
        [0x21] if coder.properties.len() == 1 && coder.properties[0] < 40 => {
            let prop = coder.properties[0];
            let dictionary = (2u64 | u64::from(prop & 1)) << (u32::from(prop) / 2 + 11);
            if dictionary > MAX_METADATA as u64 {
                return Err(invalid("7z header dictionary budget exceeded"));
            }
            Box::new(lzma_rust2::Lzma2Reader::new(
                Cursor::new(input),
                dictionary as u32,
                None,
            ))
        }
        [6, 0xf1, 7, 1] => return Err(invalid("7z encrypted metadata cannot be locally mapped")),
        _ => return Err(invalid("encoded 7z header codec is not locally supported")),
    };
    let mut output = Vec::with_capacity(size as usize);
    let mut chunk = [0; 8192];
    loop {
        control.check()?;
        let count = decoder.read(&mut chunk)?;
        if count == 0 {
            break;
        }
        if output.len() + count > size as usize {
            return Err(invalid("7z header expands beyond declared size"));
        }
        output.extend_from_slice(&chunk[..count]);
    }
    if output.len() != size as usize {
        return Err(invalid("decoded 7z header length mismatch"));
    }
    if folder
        .digest
        .is_none_or(|crc| crc != crc32fast::hash(&output))
    {
        return Err(invalid("encoded 7z header lacks a valid decoded checksum"));
    }
    Ok(output)
}

struct EvidenceInput<'a> {
    kind: EvidenceKind,
    ranges: Vec<PhysicalRange>,
    references: Vec<PhysicalRange>,
    trusted: bool,
    entries: Vec<String>,
    summary: &'a str,
    can_confirm: bool,
}

fn evidence(report: &mut DiagnosticReport, pass_id: u32, input: EvidenceInput<'_>) {
    let strength = if input.can_confirm && paths(&input.ranges).len() == 1 {
        EvidenceStrength::Confirmed
    } else {
        EvidenceStrength::Suspect
    };
    report.evidence(TestEvidence {
        id: String::new(),
        kind: input.kind,
        strength,
        source: SOURCE.into(),
        pass_id,
        ranges: input.ranges,
        reference_ranges: input.references,
        metadata_trust: if input.trusted {
            MetadataTrust::ChecksumVerified
        } else {
            MetadataTrust::Unverified
        },
        affected_entries: input.entries,
        summary: input.summary.into(),
    });
}

pub(super) fn inspect(
    set: &VolumeSet,
    failed_files: &[String],
    control: &DiagnosticControl,
    pass_id: u32,
    report: &mut DiagnosticReport,
) -> io::Result<()> {
    if !matches!(set.family, VolumeFamily::Single | VolumeFamily::ByteSplit) {
        return Err(invalid("7z requires a single file or byte-split sequence"));
    }
    let mut reader = VolumeReader::new(set)?;
    let signature = read_at(&mut reader, 0, 32)?;
    if !signature.starts_with(b"7z\xbc\xaf\x27\x1c") || signature[6] != 0 {
        return Err(invalid(
            "intact supported 7z signature required for local mapping",
        ));
    }
    let mut start = Bytes::new(&signature[8..]);
    let start_crc = start.u32()?;
    let start_ranges = set
        .observed_ranges(8, 24)
        .ok_or_else(|| invalid("7z start header range unavailable"))?;
    if crc32fast::hash(&signature[12..]) != start_crc {
        evidence(
            report,
            pass_id,
            EvidenceInput {
                kind: EvidenceKind::HeaderChecksum,
                ranges: start_ranges,
                references: Vec::new(),
                trusted: false,
                entries: Vec::new(),
                summary: "7z start-header CRC32 mismatch",
                can_confirm: true,
            },
        );
        return Ok(());
    }
    report.checked(CheckedScope {
        source: SOURCE.into(),
        pass_id,
        description: "7z start-header CRC32".into(),
        ranges: start_ranges.clone(),
        passed: true,
    });
    let next_offset = 32u64
        .checked_add(start.u64()?)
        .ok_or_else(|| invalid("7z header offset overflow"))?;
    let next_size = start.u64()?;
    let next_crc = start.u32()?;
    let next_end = next_offset
        .checked_add(next_size)
        .ok_or_else(|| invalid("7z header length overflow"))?;
    if next_end > set.byte_len().unwrap_or(0) {
        evidence(report, pass_id, EvidenceInput { kind: EvidenceKind::StructuralTruncation, ranges: whole_set(set), references: start_ranges, trusted: true, entries: Vec::new(), summary: "7z expects bytes beyond the observed stream; a missing tail or shortened member cannot be distinguished by current member sizes", can_confirm: false });
        report.stop("original byte-split boundaries are not known after truncation");
        return Ok(());
    }
    if next_size > MAX_METADATA as u64 {
        return Err(invalid("7z next-header memory budget exceeded"));
    }
    let next = read_at(&mut reader, next_offset, next_size as usize)?;
    let next_ranges = set
        .observed_ranges(next_offset, next_size)
        .ok_or_else(|| invalid("7z next-header range unavailable"))?;
    if crc32fast::hash(&next) != next_crc {
        // An earlier shortened member can make a healthy tail appear bad.
        let ranges = if set.members.len() == 1 {
            next_ranges
        } else {
            whole_set(set)
        };
        evidence(
            report,
            pass_id,
            EvidenceInput {
                kind: EvidenceKind::HeaderChecksum,
                ranges,
                references: start_ranges,
                trusted: true,
                entries: Vec::new(),
                summary:
                    "7z next-header CRC32 mismatch; split-stream offsets are not yet validated",
                can_confirm: set.members.len() == 1,
            },
        );
        return Ok(());
    }
    report.checked(CheckedScope {
        source: SOURCE.into(),
        pass_id,
        description: "7z next-header CRC32".into(),
        ranges: next_ranges.clone(),
        passed: true,
    });
    if next.is_empty() {
        return Ok(());
    }
    let mut metadata = next_ranges;
    metadata.extend(start_ranges);
    let header = if next[0] == 23 {
        let mut bytes = Bytes::new(&next[1..]);
        let encoded = streams(&mut bytes)?;
        if encoded
            .folders
            .iter()
            .flat_map(|f| &f.coders)
            .any(|c| c.id == [6, 0xf1, 7, 1])
        {
            report.encrypted = Some(true);
        }
        if encoded.sizes.len() != 1 || encoded.folders.len() != 1 {
            return Err(invalid(
                "multi-stream encoded 7z header cannot be locally decoded",
            ));
        }
        let offset = 32u64
            .checked_add(encoded.pack_pos)
            .ok_or_else(|| invalid("7z encoded header offset overflow"))?;
        let size = encoded.sizes[0];
        if offset.checked_add(size).is_none_or(|end| end > next_offset) {
            return Err(invalid(
                "encoded 7z header overlaps metadata or exceeds stream",
            ));
        }
        if size > MAX_METADATA as u64 {
            return Err(invalid("encoded 7z header memory budget exceeded"));
        }
        let packed = read_at(&mut reader, offset, size as usize)?;
        let ranges = set
            .observed_ranges(offset, size)
            .ok_or_else(|| invalid("7z encoded header range unavailable"))?;
        let decoded = match decode_header(&packed, &encoded.folders[0], control) {
            Ok(decoded) => decoded,
            Err(error) => {
                report.stop(error.to_string());
                return Ok(());
            }
        };
        metadata.extend(ranges.clone());
        report.checked(CheckedScope {
            source: SOURCE.into(),
            pass_id,
            description: "decoded 7z header CRC32".into(),
            ranges,
            passed: true,
        });
        decoded
    } else {
        next
    };
    control.check()?;
    let (main, names) = parse_header(&header)?;
    if main
        .folders
        .iter()
        .flat_map(|f| &f.coders)
        .any(|c| c.id == [6, 0xf1, 7, 1])
    {
        report.encrypted = Some(true);
    } else {
        report.encrypted = Some(false);
    }
    let mut offset = 32u64
        .checked_add(main.pack_pos)
        .ok_or_else(|| invalid("7z pack position overflow"))?;
    let mut packed_ranges = Vec::new();
    let mut packed_offsets = Vec::new();
    for size in &main.sizes {
        let end = offset
            .checked_add(*size)
            .ok_or_else(|| invalid("7z pack size overflow"))?;
        if end > next_offset {
            return Err(invalid("7z packed range overlaps next header"));
        }
        packed_offsets.push(offset);
        packed_ranges.push(
            set.observed_ranges(offset, *size)
                .ok_or_else(|| invalid("7z packed range unavailable"))?,
        );
        offset = end;
    }
    for (index, digest) in main.digests.iter().enumerate() {
        if let Some(expected) = digest {
            let actual = crc_range(
                &mut reader,
                packed_offsets[index],
                main.sizes[index],
                control,
            )?;
            report.checked(CheckedScope {
                source: SOURCE.into(),
                pass_id,
                description: format!("7z packed stream {index} CRC32"),
                ranges: packed_ranges[index].clone(),
                passed: actual == *expected,
            });
            if actual != *expected {
                evidence(
                    report,
                    pass_id,
                    EvidenceInput {
                        kind: EvidenceKind::PackedChecksum,
                        ranges: packed_ranges[index].clone(),
                        references: metadata.clone(),
                        trusted: true,
                        entries: Vec::new(),
                        summary: "7z packed-stream CRC32 mismatch",
                        can_confirm: true,
                    },
                );
            }
        }
    }
    let names: Vec<_> = names
        .into_iter()
        .filter(|(_, empty)| !*empty)
        .map(|(name, _)| name)
        .collect();
    let expected_files = main
        .folders
        .iter()
        .try_fold(0usize, |n, f| n.checked_add(f.substreams))
        .ok_or_else(|| invalid("7z file count overflow"))?;
    if names.len() != expected_files {
        return Err(invalid("7z file-to-folder mapping incomplete"));
    }
    let mut file_index = 0;
    let mut packed_index = 0;
    for (folder_index, folder) in main.folders.iter().enumerate() {
        control.check()?;
        let members: Vec<_> = packed_ranges[packed_index..packed_index + folder.packed_count]
            .iter()
            .flatten()
            .cloned()
            .collect();
        let folder_names = &names[file_index..file_index + folder.substreams];
        let failed: Vec<_> = folder_names
            .iter()
            .filter(|name| failed_files.contains(name))
            .cloned()
            .collect();
        if !failed.is_empty() {
            evidence(report, pass_id, EvidenceInput { kind: EvidenceKind::EntryChecksum, ranges: members.clone(), references: metadata.clone(), trusted: true, entries: failed, summary: "failed file depends on all packed inputs of this 7z folder, including solid predecessors", can_confirm: false });
        }
        // Stored folders can use a decoded digest directly, without decoding.
        if folder.coders.len() == 1
            && folder.coders[0].id == [0]
            && folder.packed_count == 1
            && main.digests[packed_index].is_none()
        {
            if let Some(expected) = folder
                .digest
                .filter(|_| folder.unpack_sizes[folder.final_output] == main.sizes[packed_index])
            {
                let actual = crc_range(
                    &mut reader,
                    packed_offsets[packed_index],
                    main.sizes[packed_index],
                    control,
                )?;
                report.checked(CheckedScope {
                    source: SOURCE.into(),
                    pass_id,
                    description: format!("7z stored folder {folder_index} CRC32"),
                    ranges: members.clone(),
                    passed: actual == expected,
                });
                if actual != expected {
                    evidence(
                        report,
                        pass_id,
                        EvidenceInput {
                            kind: EvidenceKind::PackedChecksum,
                            ranges: members,
                            references: metadata.clone(),
                            trusted: true,
                            entries: folder_names.to_vec(),
                            summary: "7z stored-folder CRC32 mismatch",
                            can_confirm: true,
                        },
                    );
                }
            }
        }
        packed_index += folder.packed_count;
        file_index += folder.substreams;
    }
    if failed_files.iter().any(|name| !names.contains(name)) {
        report.stop("some backend file names cannot be matched unambiguously to 7z metadata");
    }
    Ok(())
}
