use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

const EOCD_SIGNATURE: [u8; 4] = [0x50, 0x4b, 0x05, 0x06];

/// Detect the end of a ZIP archive within a file starting at `zip_start_offset`.
///
/// Searches backwards from the end of the file for the ZIP End of Central Directory
/// record (EOCD, signature `0x06054b50`), parses the comment length, and returns
/// the absolute byte offset where the ZIP archive ends.
///
/// Returns `Ok(None)` if no valid EOCD is found or the computed end exceeds file length.
pub fn detect_zip_end(path: &Path, zip_start_offset: u64) -> std::io::Result<Option<u64>> {
    let mut file = File::open(path)?;
    let file_len = file.seek(SeekFrom::End(0))?;

    if file_len <= zip_start_offset {
        return Ok(None);
    }

    let search_start = file_len.saturating_sub(65557).max(zip_start_offset);
    let mut tail = vec![0u8; (file_len - search_start) as usize];
    file.seek(SeekFrom::Start(search_start))?;
    file.read_exact(&mut tail)?;

    for i in (0..tail.len().saturating_sub(21)).rev() {
        if tail[i..i + 4] == EOCD_SIGNATURE {
            let comment_len = u16::from_le_bytes([tail[i + 20], tail[i + 21]]) as u64;
            let eocd_end = search_start + i as u64 + 22 + comment_len;
            if eocd_end <= file_len {
                return Ok(Some(eocd_end));
            }
        }
    }

    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn make_eocd(comment: &[u8]) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(&EOCD_SIGNATURE);
        buf.extend_from_slice(&[0u8; 16]); // disk number through CD offset
        let comment_len = comment.len() as u16;
        buf.extend_from_slice(&comment_len.to_le_bytes());
        buf.extend_from_slice(comment);
        buf
    }

    #[test]
    fn detect_zip_end_finds_eocd() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.bin");
        let mut f = File::create(&path).unwrap();
        f.write_all(&[0xFF; 128]).unwrap(); // garbage prefix
        f.write_all(&make_eocd(b"")).unwrap(); // EOCD at offset 128
        f.write_all(&[0xAB; 64]).unwrap(); // trailing garbage
        f.flush().unwrap();

        let result = detect_zip_end(&path, 0).unwrap();
        // EOCD starts at 128, comment_len=0, so end = 128 + 22 = 150
        assert_eq!(result, Some(150));
    }

    #[test]
    fn detect_zip_end_returns_none_when_no_eocd() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.bin");
        let mut f = File::create(&path).unwrap();
        f.write_all(&[0xAA; 200]).unwrap();
        f.flush().unwrap();

        let result = detect_zip_end(&path, 0).unwrap();
        assert_eq!(result, None);
    }

    #[test]
    fn detect_zip_end_with_comment() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.bin");
        let comment = b"this is a zip comment";
        let mut f = File::create(&path).unwrap();
        f.write_all(&[0xFF; 50]).unwrap(); // prefix
        let eocd_start = 50u64;
        f.write_all(&make_eocd(comment)).unwrap();
        f.write_all(&[0xCC; 30]).unwrap(); // trailing garbage
        f.flush().unwrap();

        let result = detect_zip_end(&path, 0).unwrap();
        // EOCD at 50, comment_len=21, end = 50 + 22 + 21 = 93
        assert_eq!(result, Some(eocd_start + 22 + comment.len() as u64));
    }

    #[test]
    fn detect_zip_end_respects_zip_start_offset() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.bin");
        let mut f = File::create(&path).unwrap();
        f.write_all(&[0xFF; 50]).unwrap(); // garbage before ZIP
        f.write_all(&make_eocd(b"")).unwrap(); // EOCD at 50
        f.flush().unwrap();

        // Searching from offset 60 should NOT find EOCD at 50
        let result = detect_zip_end(&path, 60).unwrap();
        assert_eq!(result, None);

        // Searching from offset 0 should find it
        let result = detect_zip_end(&path, 0).unwrap();
        assert_eq!(result, Some(72)); // 50 + 22 + 0
    }
}
