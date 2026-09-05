//! Associate EOCD records with the current archive, not an entry's inner ZIP.

use aho_corasick::AhoCorasick;

pub(crate) fn checked_size(data: &[u8]) -> Option<usize> {
    let matcher = AhoCorasick::new([b"PK\x05\x06"]).ok()?;
    for eocd in matcher.find_iter(data) {
        if let Some(end) = check_directory(data, eocd.start()) {
            return Some(end);
        }
    }
    None
}

fn check_directory(data: &[u8], eocd: usize) -> Option<usize> {
    let header = data.get(eocd..eocd.checked_add(22)?)?;
    if u16_at(header, 4)? != 0 || u16_at(header, 6)? != 0 {
        return None; // Volume sets have a separate resolver.
    }
    let end = eocd.checked_add(22 + u16_at(header, 20)? as usize)?;
    data.get(..end)?;
    let count = u16_at(header, 10)? as u64;
    let size = u32_at(header, 12)? as u64;
    let offset = u32_at(header, 16)? as u64;
    let (count, size, offset, directory_end) =
        if count == u16::MAX as u64 || size == u32::MAX as u64 || offset == u32::MAX as u64 {
            zip64_directory(data, eocd)?
        } else {
            if u16_at(header, 8)? as u64 != count {
                return None;
            }
            (count, size, offset, eocd)
        };
    if count == 0 || count > size / 46 {
        return None;
    }
    let directory_start = directory_end.checked_sub(usize::try_from(size).ok()?)?;
    // Both appended ZIPs (offsets relative to ZIP) and ZIP writers opened after
    // an existing prefix (offsets include prefix) are common. Normalize from
    // the actual directory position, allowing a negative adjustment after carve.
    let adjustment = directory_start as i128 - offset as i128;
    let mut cursor = directory_start;
    let mut references_start = false;
    for _ in 0..count {
        let entry = data.get(cursor..cursor.checked_add(46)?)?;
        if !entry.starts_with(b"PK\x01\x02") {
            return None;
        }
        let name_size = u16_at(entry, 28)? as usize;
        let extra_size = u16_at(entry, 30)? as usize;
        let comment_size = u16_at(entry, 32)? as usize;
        let record_end = cursor.checked_add(46 + name_size + extra_size + comment_size)?;
        if record_end > directory_end {
            return None;
        }
        let name = data.get(cursor + 46..cursor + 46 + name_size)?;
        let extra = data.get(cursor + 46 + name_size..cursor + 46 + name_size + extra_size)?;
        let declared = local_offset(entry, extra)?;
        let local = usize::try_from(declared as i128 + adjustment).ok()?;
        if local >= directory_start {
            return None;
        }
        let local_header = data.get(local..local.checked_add(30)?)?;
        if !local_header.starts_with(b"PK\x03\x04")
            || u16_at(local_header, 26)? as usize != name_size
            || data.get(local + 30..local + 30 + name_size)? != name
        {
            return None;
        }
        references_start |= local == 0;
        cursor = record_end;
    }
    // A directory must actually reference the candidate header at offset zero.
    // archive-offset arithmetic alone can associate an inner ZIP with its parent.
    (references_start && cursor == directory_end).then_some(end)
}

fn local_offset(entry: &[u8], mut extra: &[u8]) -> Option<u64> {
    let offset = u32_at(entry, 42)?;
    if offset != u32::MAX {
        return Some(offset as u64);
    }
    while !extra.is_empty() {
        let id = u16_at(extra, 0)?;
        let size = u16_at(extra, 2)? as usize;
        let value = extra.get(4..4 + size)?;
        if id == 1 {
            let position = usize::from(u32_at(entry, 24)? == u32::MAX) * 8
                + usize::from(u32_at(entry, 20)? == u32::MAX) * 8;
            return u64_at(value, position);
        }
        extra = extra.get(4 + size..)?;
    }
    None
}

fn zip64_directory(data: &[u8], eocd: usize) -> Option<(u64, u64, u64, usize)> {
    let locator = eocd.checked_sub(20)?;
    if data.get(locator..locator + 4)? != b"PK\x06\x07" {
        return None;
    }
    let matcher = AhoCorasick::new([b"PK\x06\x06"]).ok()?;
    for record in matcher.find_iter(data.get(..locator)?) {
        let header = data.get(record.start()..locator)?;
        let Some(size) = u64_at(header, 4).and_then(|size| usize::try_from(size).ok()) else {
            continue;
        };
        if size.checked_add(12) != Some(header.len()) || header.len() < 56 {
            continue;
        }
        if u32_at(header, 16)? != 0
            || u32_at(header, 20)? != 0
            || u64_at(header, 24)? != u64_at(header, 32)?
        {
            return None;
        }
        return Some((
            u64_at(header, 32)?,
            u64_at(header, 40)?,
            u64_at(header, 48)?,
            record.start(),
        ));
    }
    None
}

fn u16_at(data: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_le_bytes(
        data.get(offset..offset + 2)?.try_into().ok()?,
    ))
}
fn u32_at(data: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        data.get(offset..offset + 4)?.try_into().ok()?,
    ))
}
fn u64_at(data: &[u8], offset: usize) -> Option<u64> {
    Some(u64::from_le_bytes(
        data.get(offset..offset + 8)?.try_into().ok()?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Write};

    fn archive(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut writer = ::zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, contents) in entries {
            writer
                .start_file(
                    *name,
                    ::zip::write::SimpleFileOptions::default()
                        .compression_method(::zip::CompressionMethod::Stored),
                )
                .unwrap();
            writer.write_all(contents).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    #[test]
    fn prefixed_writer_offsets_are_normalized_after_carving() {
        let prefix = vec![0; 317];
        let mut cursor = Cursor::new(prefix.clone());
        cursor.set_position(prefix.len() as u64);
        let mut writer = ::zip::ZipWriter::new(cursor);
        writer
            .start_file("file.txt", ::zip::write::SimpleFileOptions::default())
            .unwrap();
        writer.write_all(b"payload").unwrap();
        let data = writer.finish().unwrap().into_inner();
        assert_eq!(
            checked_size(&data[prefix.len()..]),
            Some(data.len() - prefix.len())
        );
    }

    #[test]
    fn inner_directory_with_shifted_offsets_cannot_end_outer_archive() {
        let mut inner = archive(&[("inside.txt", b"inside")]);
        let central = inner.windows(4).position(|w| w == b"PK\x01\x02").unwrap();
        let eocd = inner.windows(4).position(|w| w == b"PK\x05\x06").unwrap();
        // Legal prefix-adjusted inner offsets can make naive archive-offset
        // arithmetic claim that this inner directory belongs to the outer ZIP.
        let prefix = 30 + "inner.zip".len();
        inner[central + 42..central + 46].copy_from_slice(&(prefix as u32).to_le_bytes());
        inner[eocd + 16..eocd + 20].copy_from_slice(&((central + prefix) as u32).to_le_bytes());
        let outer = archive(&[("inner.zip", &inner), ("sibling.txt", b"sibling")]);
        assert_eq!(checked_size(&outer), Some(outer.len()));
    }

    #[test]
    fn stored_inner_zip_is_not_the_outer_end_and_concatenated_zip_is_separate() {
        let inner = archive(&[("inner.txt", b"inner")]);
        let outer = archive(&[("inner.zip", &inner), ("sibling.txt", b"sibling")]);
        let mut joined = outer.clone();
        joined.extend_from_slice(&inner);
        assert_eq!(checked_size(&joined), Some(outer.len()));
        assert_eq!(checked_size(&joined[outer.len()..]), Some(inner.len()));
        let scanner = crate::EmbeddedScanner::new(crate::ScannerConfig {
            mode: crate::ScanMode::Deep,
            max_scan_bytes: None,
            ..Default::default()
        });
        let findings = scanner.scan_bytes(&joined);
        assert_eq!(findings.len(), 2);
        assert_eq!(findings[0].size, Some(outer.len() as u64));
        assert_eq!(findings[1].offset, outer.len() as u64);
    }
}
