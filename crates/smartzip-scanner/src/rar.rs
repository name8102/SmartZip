//! Validate the first RAR header without needing the archive's distant EOF.
//! This establishes a detection candidate, not archive integrity or its size.

pub(crate) fn has_checked_initial_header(data: &[u8]) -> bool {
    checked_initial_header(data).unwrap_or(false)
}

fn checked_initial_header(data: &[u8]) -> Option<bool> {
    if let Some(header) = data.strip_prefix(b"Rar!\x1a\x07\x01\x00") {
        let expected_crc = u32::from_le_bytes(header.get(..4)?.try_into().ok()?);
        let mut position = 4;
        let size = usize::try_from(vint(header, &mut position)?).ok()?;
        let end = position.checked_add(size)?;
        let complete = header.get(..end)?;
        let kind = vint(complete, &mut position)?;
        // A main header or encryption header must immediately follow the marker.
        if !matches!(kind, 1 | 4) {
            return Some(false);
        }
        vint(complete, &mut position)?; // General header flags must also exist.
        Some(crc32fast::hash(complete.get(4..)?) == expected_crc)
    } else if let Some(header) = data.strip_prefix(b"Rar!\x1a\x07\x00") {
        let expected_crc = u16::from_le_bytes(header.get(..2)?.try_into().ok()?);
        let size = u16::from_le_bytes(header.get(5..7)?.try_into().ok()?) as usize;
        if size < 13 || *header.get(2)? != 0x73 {
            return Some(false);
        }
        Some(crc32fast::hash(header.get(2..size)?) as u16 == expected_crc)
    } else {
        Some(false)
    }
}

pub(super) fn vint(data: &[u8], position: &mut usize) -> Option<u64> {
    let mut value = 0u64;
    for shift in (0..64).step_by(7) {
        let byte = *data.get(*position)?;
        *position += 1;
        if shift == 63 && byte > 1 {
            return None;
        }
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Some(value);
        }
    }
    None
}

#[cfg(test)]
mod tests {

    fn block(body: &[u8]) -> Vec<u8> {
        assert!(body.len() < 128);
        let mut header = vec![body.len() as u8];
        header.extend_from_slice(body);
        let mut result = crc32fast::hash(&header).to_le_bytes().to_vec();
        result.extend(header);
        result
    }

    #[test]
    fn rar5_stored_eof_bytes_do_not_end_the_archive() {
        let end = block(&[5, 4, 0]);
        let payload = [b"begin".as_slice(), &end, b"payload-after-fake-EOF"].concat();
        let mut file_header = vec![2, 2, payload.len() as u8, 4, payload.len() as u8, 0];
        file_header.extend_from_slice(&crc32fast::hash(&payload).to_le_bytes());
        file_header.extend_from_slice(&[0, 1, 5]);
        file_header.extend_from_slice(b"a.bin");
        let archive = [
            b"Rar!\x1a\x07\x01\x00".as_slice(),
            &block(&[1, 0, 0]),
            &block(&file_header),
            &payload,
            &end,
        ]
        .concat();
        let carrier = [b"jpeg-prefix".as_slice(), &archive].concat();
        let scanner = crate::EmbeddedScanner::new(crate::ScannerConfig {
            mode: crate::ScanMode::Deep,
            max_scan_bytes: None,
            ..Default::default()
        });
        let findings = scanner.scan_bytes(&carrier);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].offset, 11);
        assert_eq!(findings[0].size, Some(archive.len() as u64));
    }
}
