//! Bounded random-access metadata parsing. Search coverage is independent of RAM.
use crate::{binwalk_name_to_format, Confidence, EmbeddedArchiveFinding, EmbeddedScanner};
use std::{
    fs::File,
    io::{self, Read, Seek, SeekFrom},
};

const CHUNK: usize = 1024 * 1024;
const MAX_HEADER: usize = 64 * 1024 * 1024;

pub(super) struct Input<'a> {
    file: File,
    pub end: u64,
    cancelled: &'a dyn Fn() -> bool,
    error: Option<io::Error>,
}
impl Input<'_> {
    pub fn read(&mut self, offset: u64, size: usize) -> Option<Vec<u8>> {
        if (self.cancelled)() {
            self.error = Some(io::Error::new(io::ErrorKind::Interrupted, "scan cancelled"));
            return None;
        }
        if self.error.is_some() || size > MAX_HEADER || offset.checked_add(size as u64)? > self.end
        {
            return None;
        }
        let mut bytes = vec![0; size];
        let result = (|| {
            self.file.seek(SeekFrom::Start(offset))?;
            for chunk in bytes.chunks_mut(64 * 1024) {
                if (self.cancelled)() {
                    return Err(io::Error::new(io::ErrorKind::Interrupted, "scan cancelled"));
                }
                self.file.read_exact(chunk)?;
            }
            Ok(())
        })();
        if let Err(error) = result {
            self.error = Some(error);
            return None;
        }
        Some(bytes)
    }
    pub fn prefix(&mut self, offset: u64, max: usize) -> Option<Vec<u8>> {
        self.read(
            offset,
            self.end.checked_sub(offset)?.min(max as u64) as usize,
        )
    }
    pub fn find(
        &mut self,
        magic: &[u8],
        mut from: u64,
        until: u64,
        mut accept: impl FnMut(&mut Self, u64) -> Option<u64>,
    ) -> Option<u64> {
        let matcher = aho_corasick::AhoCorasick::new([magic]).ok()?;
        while from < until {
            let end = from.saturating_add(CHUNK as u64).min(until);
            let bytes = self.read(
                from,
                (end.saturating_add(magic.len() as u64 - 1).min(until) - from) as usize,
            )?;
            for found in matcher.find_iter(&bytes) {
                let position = from + found.start() as u64;
                if position >= end {
                    break;
                }
                if let Some(value) = accept(self, position) {
                    return Some(value);
                }
                if self.error.is_some() {
                    return None;
                }
            }
            from = end;
        }
        None
    }
}

impl EmbeddedScanner {
    pub(super) fn scan_file(
        &self,
        file: File,
        cancelled: &dyn Fn() -> bool,
    ) -> io::Result<Vec<EmbeddedArchiveFinding>> {
        let end = file
            .metadata()?
            .len()
            .min(self.scan_limit().unwrap_or(u64::MAX));
        let mut input = Input {
            file,
            end,
            cancelled,
            error: None,
        };
        let matcher =
            aho_corasick::AhoCorasick::new(&self.binwalk.patterns).map_err(io::Error::other)?;
        let overlap = self
            .binwalk
            .patterns
            .iter()
            .map(Vec::len)
            .max()
            .unwrap_or(1)
            - 1;
        let mut cursor = 0;
        let mut accepted_end = 0;
        let mut findings = Vec::new();
        while cursor < end && findings.len() < self.config.max_findings {
            let window_end = cursor.saturating_add(CHUNK as u64).min(end);
            let Some(bytes) = input.read(
                cursor,
                (window_end.saturating_add(overlap as u64).min(end) - cursor) as usize,
            ) else {
                break;
            };
            let mut next = window_end;
            for magic in matcher.find_overlapping_iter(&bytes) {
                let offset = cursor + magic.start() as u64;
                if offset >= window_end {
                    break;
                }
                let signature = &self.binwalk.pattern_signature_table[&magic.pattern().as_usize()];
                let Some(format) = binwalk_name_to_format(&signature.name) else {
                    continue;
                };
                let Some(finding) = parse(&mut input, offset, format, signature) else {
                    if input.error.is_some() {
                        break;
                    }
                    continue;
                };
                if finding.offset < accepted_end
                    || !self.config.include_formats.contains(&finding.format)
                    || finding.confidence < self.config.min_confidence
                {
                    continue;
                }
                next = finding
                    .offset
                    .saturating_add(finding.size.unwrap_or(1))
                    .max(offset + 1);
                accepted_end = next;
                findings.push(finding);
                break;
            }
            if input.error.is_some() {
                break;
            }
            cursor = next;
        }
        if let Some(error) = input.error {
            Err(error)
        } else if cancelled() {
            Err(io::Error::new(io::ErrorKind::Interrupted, "scan cancelled"))
        } else {
            Ok(findings)
        }
    }
}

fn parse(
    input: &mut Input<'_>,
    offset: u64,
    format: smartzip_core::ArchiveFormat,
    signature: &binwalk::signatures::common::Signature,
) -> Option<EmbeddedArchiveFinding> {
    use smartzip_core::ArchiveFormat::*;
    let prefix = input.prefix(offset, 64 * 1024)?;
    let mut start = offset;
    let (size, confidence) = match format {
        Zip => {
            binwalk::structures::zip::parse_zip_header(&prefix).ok()?;
            let size = crate::zip::checked_file_size(input, offset);
            (
                size,
                if size.is_some() {
                    Confidence::High
                } else {
                    Confidence::Medium
                },
            )
        }
        SevenZip => (Some(seven_zip_size(input, offset)?), Confidence::High),
        Rar => {
            if !crate::rar::has_checked_initial_header(&prefix) {
                return None;
            }
            (
                rar_size(input, offset, prefix.starts_with(b"Rar!\x1a\x07\x01\x00")),
                Confidence::Medium,
            )
        }
        // Stream compressed frames with fixed output buffers and cancellation.
        Gzip => {
            binwalk::structures::gzip::parse_gzip_header(&prefix).ok()?;
            let mut decoder = flate2::bufread::GzDecoder::new(Stream::new(input, offset));
            let mut output = [0; 64 * 1024];
            while decoder.read(&mut output).ok()? != 0 {}
            (
                Some(decoder.into_inner().position() - offset),
                Confidence::High,
            )
        }
        Bzip2 => {
            if !prefix.starts_with(b"BZh")
                || !matches!(prefix.get(3), Some(b'1'..=b'9'))
                || !matches!(
                    prefix.get(4..10),
                    Some(b"\x31\x41\x59\x26\x53\x59" | b"\x17\x72\x45\x38\x50\x90")
                )
            {
                return None;
            }
            let mut decoder = bzip2::read::BzDecoder::new(Stream::new(input, offset));
            let mut output = [0; 64 * 1024];
            while decoder.read(&mut output).ok()? != 0 {}
            (Some(decoder.total_in()), Confidence::High)
        }
        Xz | Lzma => {
            if format == Xz {
                binwalk::structures::xz::parse_xz_header(&prefix).ok()?;
            } else {
                binwalk::structures::lzma::parse_lzma_header(&prefix).ok()?;
            }
            let mut decoder = if format == Xz {
                xz2::stream::Stream::new_stream_decoder(MAX_HEADER as u64, 0)
            } else {
                xz2::stream::Stream::new_lzma_decoder(MAX_HEADER as u64)
            }
            .ok()?;
            let mut reader = Stream::new(input, offset);
            let mut output = [0; 64 * 1024];
            let mut complete = false;
            loop {
                use io::BufRead;
                let bytes = reader.fill_buf().ok()?;
                let before_in = decoder.total_in();
                let before_out = decoder.total_out();
                let status = decoder.process(
                    bytes,
                    &mut output,
                    if bytes.is_empty() {
                        xz2::stream::Action::Finish
                    } else {
                        xz2::stream::Action::Run
                    },
                );
                reader.consume((decoder.total_in() - before_in) as usize);
                match status {
                    Ok(xz2::stream::Status::StreamEnd) => {
                        complete = true;
                        break;
                    }
                    Err(_) => break,
                    _ if decoder.total_in() == before_in && decoder.total_out() == before_out => {
                        break
                    }
                    _ => {}
                }
            }
            let size = complete.then(|| decoder.total_in());
            (
                size,
                if complete {
                    Confidence::High
                } else {
                    Confidence::Medium
                },
            )
        }
        Zstd => {
            let header = binwalk::structures::zstd::parse_zstd_header(&prefix).ok()?;
            let content = match header.frame_content_flag {
                0 => usize::from(header.single_segment_flag),
                1 => 2,
                2 => 4,
                _ => 8,
            };
            let dictionary = [0, 1, 2, 4][header.dictionary_id_flag];
            let mut cursor = offset
                + (header.fixed_header_size
                    + usize::from(!header.single_segment_flag)
                    + dictionary
                    + content) as u64;
            loop {
                let block =
                    binwalk::structures::zstd::parse_block_header(&input.read(cursor, 3)?).ok()?;
                cursor = cursor.checked_add(3 + block.block_size as u64)?;
                if cursor > input.end {
                    return None;
                }
                if block.last_block {
                    break;
                }
            }
            if header.content_checksum_present {
                input.read(cursor, 4)?;
                cursor += 4;
            }
            (Some(cursor - offset), Confidence::High)
        }
        Lz4 => {
            let header = binwalk::structures::lz4::parse_lz4_file_header(&prefix).ok()?;
            let mut cursor = offset + header.header_size as u64;
            loop {
                let block = binwalk::structures::lz4::parse_lz4_block_header(
                    &input.read(cursor, 4)?,
                    header.block_checksum_present,
                )
                .ok()?;
                cursor += 4;
                if block.last_block {
                    break;
                }
                cursor = cursor.checked_add((block.data_size + block.checksum_size) as u64)?;
                if cursor > input.end {
                    return None;
                }
            }
            if header.content_checksum_present {
                input.read(cursor, 4)?;
                cursor += 4;
            }
            (Some(cursor - offset), Confidence::High)
        }
        Tar => {
            start = offset.checked_sub(257)?;
            (Some(tar_size(input, start)?), Confidence::High)
        }
        _ => {
            // Remaining binwalk parsers inspect container metadata. Give them
            // bounded context for signatures such as ISO's volume descriptor.
            let base = offset.saturating_sub(64 * 1024);
            let data = input.prefix(base, MAX_HEADER)?;
            let result =
                std::panic::catch_unwind(|| (signature.parser)(&data, (offset - base) as usize))
                    .ok()?
                    .ok()?;
            start = base.checked_add(result.offset as u64)?;
            let size = (result.size > 0).then_some(result.size as u64);
            if start.checked_add(size.unwrap_or(0))? > input.end {
                return None;
            }
            (size, Confidence::from_binwalk(result.confidence))
        }
    };
    let description = match (&format, size) {
        (Rar, Some(size)) => {
            format!("RAR archive; checked block boundaries; total size: {size} bytes")
        }
        (Rar, None) => "RAR archive; initial header checksum verified; archive size unknown".into(),
        (Zip, Some(size)) => {
            format!("ZIP archive; checked central directory; total size: {size} bytes")
        }
        (Zip, None) => "ZIP archive; directory boundary unknown".into(),
        _ => format!(
            "{format:?} archive; metadata checked; {}",
            size.map(|s| format!("total size: {s} bytes"))
                .unwrap_or_else(|| "archive size unknown".into())
        ),
    };
    Some(EmbeddedArchiveFinding {
        offset: start,
        size,
        format,
        confidence,
        description,
    })
}

struct Stream<'a, 'b> {
    input: &'a mut Input<'b>,
    cursor: u64,
    buffer: Vec<u8>,
    used: usize,
}
impl<'a, 'b> Stream<'a, 'b> {
    fn new(input: &'a mut Input<'b>, cursor: u64) -> Self {
        Self {
            input,
            cursor,
            buffer: Vec::new(),
            used: 0,
        }
    }
    fn position(&self) -> u64 {
        self.cursor - (self.buffer.len() - self.used) as u64
    }
}
impl io::BufRead for Stream<'_, '_> {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        if (self.input.cancelled)() {
            self.input.error = Some(io::Error::new(io::ErrorKind::Interrupted, "scan cancelled"));
            return Err(io::Error::other("scan cancelled"));
        }
        if self.used == self.buffer.len() {
            self.buffer = self
                .input
                .prefix(self.cursor, 64 * 1024)
                .ok_or_else(|| io::Error::other("scan read failed"))?;
            self.cursor += self.buffer.len() as u64;
            self.used = 0;
        }
        Ok(&self.buffer[self.used..])
    }
    fn consume(&mut self, amount: usize) {
        self.used += amount.min(self.buffer.len() - self.used);
    }
}
impl Read for Stream<'_, '_> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        use io::BufRead;
        let buffer = self.fill_buf()?;
        let count = buffer.len().min(output.len());
        output[..count].copy_from_slice(&buffer[..count]);
        self.consume(count);
        Ok(count)
    }
}

fn seven_zip_size(input: &mut Input<'_>, start: u64) -> Option<u64> {
    let header = input.read(start, 32)?;
    if crc32fast::hash(&header[12..]) != u32::from_le_bytes(header[8..12].try_into().ok()?) {
        return None;
    }
    let offset = u64::from_le_bytes(header[12..20].try_into().ok()?);
    let length = u64::from_le_bytes(header[20..28].try_into().ok()?);
    let size = 32u64.checked_add(offset)?.checked_add(length)?;
    if start.checked_add(size)? > input.end {
        return None;
    }
    let mut cursor = start + 32 + offset;
    let mut hash = crc32fast::Hasher::new();
    while cursor < start + size {
        let count = (start + size - cursor).min(CHUNK as u64) as usize;
        hash.update(&input.read(cursor, count)?);
        cursor += count as u64;
    }
    (hash.finalize() == u32::from_le_bytes(header[28..32].try_into().ok()?)).then_some(size)
}

fn rar_size(input: &mut Input<'_>, start: u64, rar5: bool) -> Option<u64> {
    let mut cursor = start.checked_add(if rar5 { 8 } else { 7 })?;
    while cursor < input.end {
        let header = input.prefix(cursor, 64)?;
        let (size, packed, last, encrypted) = if rar5 {
            let mut position = 4;
            let size = usize::try_from(crate::rar::vint(&header, &mut position)?)
                .ok()?
                .checked_add(position)?;
            let block = input.read(cursor, size)?;
            if crc32fast::hash(&block[4..]) != u32::from_le_bytes(block[..4].try_into().ok()?) {
                return None;
            }
            let kind = crate::rar::vint(&block, &mut position)?;
            let flags = crate::rar::vint(&block, &mut position)?;
            if flags & 1 != 0 {
                crate::rar::vint(&block, &mut position)?;
            }
            let packed = if flags & 2 != 0 {
                crate::rar::vint(&block, &mut position)?
            } else {
                0
            };
            (size, packed, kind == 5, kind == 4)
        } else {
            let kind = *header.get(2)?;
            let flags = u16::from_le_bytes(header.get(3..5)?.try_into().ok()?);
            let size = u16::from_le_bytes(header.get(5..7)?.try_into().ok()?) as usize;
            if size < 7 {
                return None;
            }
            let block = input.read(cursor, size)?;
            if crc32fast::hash(&block[2..]) as u16
                != u16::from_le_bytes(block[..2].try_into().ok()?)
            {
                return None;
            }
            let mut packed = if flags & 0x8000 != 0 {
                u32::from_le_bytes(block.get(7..11)?.try_into().ok()?) as u64
            } else {
                0
            };
            if kind == 0x74 && flags & 0x100 != 0 {
                packed |= (u32::from_le_bytes(block.get(32..36)?.try_into().ok()?) as u64) << 32;
            }
            (
                size,
                packed,
                kind == 0x7b,
                kind == 0x73 && flags & 0x80 != 0,
            )
        };
        cursor = cursor.checked_add(size as u64)?;
        if last {
            return Some(cursor - start);
        }
        if encrypted {
            return None;
        }
        cursor = cursor.checked_add(packed)?;
    }
    None
}

fn tar_size(input: &mut Input<'_>, start: u64) -> Option<u64> {
    let mut cursor = start;
    loop {
        let header = input.read(cursor, 512)?;
        if header.iter().all(|&b| b == 0) {
            return input
                .read(cursor + 512, 512)?
                .iter()
                .all(|&b| b == 0)
                .then_some(cursor + 1024 - start);
        }
        let octal = |bytes: &[u8]| {
            u64::from_str_radix(
                std::str::from_utf8(bytes).ok()?.trim_matches(['\0', ' ']),
                8,
            )
            .ok()
        };
        let checksum = octal(&header[148..156])?;
        let sum: u64 = header
            .iter()
            .enumerate()
            .map(|(i, b)| {
                if (148..156).contains(&i) {
                    32
                } else {
                    *b as u64
                }
            })
            .sum();
        if checksum != sum {
            return None;
        }
        let size = if header[124] & 0x80 != 0 {
            if header[124] != 0x80 || header[125..128].iter().any(|&b| b != 0) {
                return None;
            }
            u64::from_be_bytes(header[128..136].try_into().ok()?)
        } else {
            octal(&header[124..136])?
        };
        cursor = cursor
            .checked_add(512)?
            .checked_add(size.checked_add(511)? / 512 * 512)?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ScanMode, ScannerConfig};
    use std::{cell::Cell, io::Write};

    fn scanner() -> EmbeddedScanner {
        EmbeddedScanner::new(ScannerConfig {
            mode: ScanMode::Deep,
            max_scan_bytes: None,
            ..Default::default()
        })
    }

    #[test]
    fn sparse_carrier_search_crosses_empty_windows_and_can_be_cancelled() {
        let mut zip = zip::ZipWriter::new(io::Cursor::new(Vec::new()));
        zip.start_file("a.txt", zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(b"contents").unwrap();
        let zip = zip.finish().unwrap().into_inner();
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(&zip).unwrap();
        let offset = 192 * 1024 * 1024;
        file.seek(SeekFrom::Start(offset)).unwrap();
        file.write_all(&zip).unwrap();
        let findings = scanner().scan_path(file.path()).unwrap();
        assert_eq!(
            findings
                .iter()
                .map(|f| (f.offset, f.size))
                .collect::<Vec<_>>(),
            vec![
                (0, Some(zip.len() as u64)),
                (offset, Some(zip.len() as u64))
            ]
        );
        let checks = Cell::new(0);
        let error = scanner()
            .scan_path_cancellable(file.path(), &|| {
                checks.set(checks.get() + 1);
                checks.get() > 35
            })
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
    }

    #[test]
    fn tar_header_start_may_precede_the_current_signature_window() {
        let mut header = [0u8; 512];
        header[..5].copy_from_slice(b"a.txt");
        header[124..136].copy_from_slice(b"00000000000\0");
        header[148..156].fill(b' ');
        header[156] = b'0';
        header[257..263].copy_from_slice(b"ustar\0");
        let sum: u64 = header.iter().map(|b| *b as u64).sum();
        header[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
        let mut file = tempfile::NamedTempFile::new().unwrap();
        let start = CHUNK as u64 - 100;
        file.seek(SeekFrom::Start(start)).unwrap();
        file.write_all(&header).unwrap();
        file.write_all(&[0; 1024]).unwrap();
        let findings = scanner().scan_path(file.path()).unwrap();
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].offset, start);
        assert_eq!(findings[0].size, Some(1536));
    }

    #[test]
    fn compressed_stream_sizes_stop_before_the_next_payload() {
        let content = vec![b'a'; 256 * 1024];
        let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        gzip.write_all(&content).unwrap();
        let mut bzip = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::fast());
        bzip.write_all(&content).unwrap();
        let mut xz = xz2::write::XzEncoder::new(Vec::new(), 1);
        xz.write_all(&content).unwrap();
        for data in [
            gzip.finish().unwrap(),
            bzip.finish().unwrap(),
            xz.finish().unwrap(),
        ] {
            let mut file = tempfile::NamedTempFile::new().unwrap();
            file.write_all(&[0; 17]).unwrap();
            file.write_all(&data).unwrap();
            file.write_all(&[0; 13]).unwrap();
            file.write_all(&data).unwrap();
            let findings = scanner().scan_path(file.path()).unwrap();
            assert_eq!(findings.len(), 2, "{findings:?}");
            assert_eq!(findings[0].size, Some(data.len() as u64));
            assert_eq!(findings[1].offset, 30 + data.len() as u64);
        }
    }
}
