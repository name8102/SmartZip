//! ZIP central-directory names only; payload decoding belongs to the native library.
use smartzip_core::{ArchiveInput, Result, SmartZipError};
use std::{
    io::{Read, Seek, SeekFrom},
    path::Path,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZipRawEntry {
    pub raw_name: Vec<u8>,
    pub unicode: bool,
    pub unicode_name: Option<String>,
    pub encrypted: bool,
    pub is_dir: bool,
    pub compressed_size: u64,
    pub uncompressed_size: u64,
}
#[derive(Debug, Clone, Copy, Default)]
pub struct NativeZipBackend;
impl NativeZipBackend {
    pub fn new() -> Self {
        Self
    }
    pub fn raw_entries(&self, path: &Path) -> Result<Vec<ZipRawEntry>> {
        self.raw_entries_with_input(&ArchiveInput {
            path: path.into(),
            offset: 0,
            length: None,
            volumes: Default::default(),
        })
    }
    pub fn has_encrypted_entries(&self, path: &Path) -> Result<bool> {
        Ok(self.raw_entries(path)?.iter().any(|e| e.encrypted))
    }
    pub fn raw_entries_with_input(&self, input: &ArchiveInput) -> Result<Vec<ZipRawEntry>> {
        let parse = || -> std::io::Result<Vec<ZipRawEntry>> {
            let mut reader = crate::source::InputReader::open(input)?;
            let size = reader.seek(SeekFrom::End(0))?;
            let tail_len = size.min(22 + u16::MAX as u64) as usize;
            reader.seek(SeekFrom::End(-(tail_len as i64)))?;
            let mut tail = vec![0; tail_len];
            reader.read_exact(&mut tail)?;
            let end = (0..tail.len().saturating_sub(21))
                .rev()
                .find(|&i| {
                    tail[i..].starts_with(b"PK\x05\x06")
                        && i + 22 + u16v(&tail[i + 20..]) as usize == tail.len()
                })
                .ok_or_else(|| invalid("ZIP end directory unavailable"))?;
            let end_pos = size - tail_len as u64 + end as u64;
            let eocd = &tail[end..];
            let mut count = u16v(&eocd[10..]) as u64;
            let mut cd_size = u32v(&eocd[12..]) as u64;
            let mut cd_offset = u32v(&eocd[16..]) as u64;
            let mut cd_end = end_pos;
            if count == u16::MAX as u64
                || cd_size == u32::MAX as u64
                || cd_offset == u32::MAX as u64
            {
                if end_pos < 20 {
                    return Err(invalid("ZIP64 locator unavailable"));
                }
                reader.seek(SeekFrom::Start(end_pos - 20))?;
                let mut locator = [0; 20];
                reader.read_exact(&mut locator)?;
                if !locator.starts_with(b"PK\x06\x07") {
                    return Err(invalid("ZIP64 locator unavailable"));
                }
                let offset = u64v(&locator[8..]);
                reader.seek(SeekFrom::Start(offset))?;
                let mut record = [0; 56];
                reader.read_exact(&mut record)?;
                if !record.starts_with(b"PK\x06\x06") {
                    return Err(invalid("ZIP64 directory unavailable"));
                }
                count = u64v(&record[32..]);
                cd_size = u64v(&record[40..]);
                cd_offset = u64v(&record[48..]);
                cd_end = offset;
            }
            reader.seek(SeekFrom::Start(cd_offset))?;
            let mut signature = [0; 4];
            reader.read_exact(&mut signature)?;
            if signature != *b"PK\x01\x02" && count != 0 {
                cd_offset = cd_end
                    .checked_sub(cd_size)
                    .ok_or_else(|| invalid("ZIP central-directory bounds"))?;
            }
            reader.seek(SeekFrom::Start(cd_offset))?;
            let mut entries = Vec::new();
            for _ in 0..count {
                let mut header = [0; 46];
                reader.read_exact(&mut header)?;
                if !header.starts_with(b"PK\x01\x02") {
                    return Err(invalid("invalid central-directory entry"));
                }
                let flags = u16v(&header[8..]);
                let mut compressed_size = u32v(&header[20..]) as u64;
                let mut uncompressed_size = u32v(&header[24..]) as u64;
                let mut raw_name = vec![0; u16v(&header[28..]) as usize];
                reader.read_exact(&mut raw_name)?;
                let mut extra = vec![0; u16v(&header[30..]) as usize];
                reader.read_exact(&mut extra)?;
                reader.seek(SeekFrom::Current(u16v(&header[32..]) as i64))?;
                let mut unicode_name = None;
                let mut rest = extra.as_slice();
                while rest.len() >= 4 {
                    let id = u16v(rest);
                    let len = u16v(&rest[2..]) as usize;
                    let data = rest
                        .get(4..4 + len)
                        .ok_or_else(|| invalid("invalid ZIP extra field"))?;
                    if id == 0x7075
                        && data.len() >= 5
                        && data[0] == 1
                        && u32v(&data[1..]) == crc32fast::hash(&raw_name)
                    {
                        unicode_name = std::str::from_utf8(&data[5..]).ok().map(str::to_owned);
                    }
                    if id == 1 {
                        let mut data = data;
                        if uncompressed_size == u32::MAX as u64 {
                            uncompressed_size =
                                u64v(data.get(..8).ok_or_else(|| invalid("ZIP64 size missing"))?);
                            data = &data[8..];
                        }
                        if compressed_size == u32::MAX as u64 {
                            compressed_size =
                                u64v(data.get(..8).ok_or_else(|| invalid("ZIP64 size missing"))?);
                        }
                    }
                    rest = &rest[4 + len..];
                }
                entries.push(ZipRawEntry {
                    is_dir: raw_name.ends_with(b"/")
                        || (u32v(&header[38..]) >> 16) & 0o170000 == 0o040000,
                    raw_name,
                    unicode: flags & 0x800 != 0 || unicode_name.is_some(),
                    unicode_name,
                    encrypted: flags & 1 != 0,
                    compressed_size,
                    uncompressed_size,
                });
            }
            Ok(entries)
        };
        parse().map_err(|e| SmartZipError::io(Some(input.path.clone()), e))
    }
}
fn invalid(message: &str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, message)
}
fn u16v(bytes: &[u8]) -> u16 {
    u16::from_le_bytes(bytes[..2].try_into().unwrap())
}
fn u32v(bytes: &[u8]) -> u32 {
    u32::from_le_bytes(bytes[..4].try_into().unwrap())
}
fn u64v(bytes: &[u8]) -> u64 {
    u64::from_le_bytes(bytes[..8].try_into().unwrap())
}

// ---------------------------------------------------------------------------
// Tests: only raw-filename preservation and detection metadata. No extract,
// probe, list, test, compress. Those operations are intentionally not
// supported via this type any more; they must go through SevenZip/Unrar.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Write;
    use std::path::PathBuf;
    use zip::write::SimpleFileOptions;
    use zip::ZipWriter;

    fn create_minimal_zip() -> Vec<u8> {
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut writer = ZipWriter::new(&mut buf);
            let options =
                SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
            writer.start_file("test.txt", options).unwrap();
            writer.write_all(b"hello").unwrap();
            writer.finish().unwrap();
        }
        buf.into_inner()
    }

    fn create_raw_zip_with_encoding(path: &Path, entries: &[(&[u8], &[u8])]) {
        use std::io::Seek;
        let mut f = File::create(path).unwrap();
        let mut offsets = Vec::new();

        for (fname, content) in entries {
            offsets.push(f.stream_position().unwrap());
            f.write_all(b"PK\x03\x04").unwrap();
            let flags: u16 = 0;
            let method: u16 = 0;
            let crc = crc32fast::hash(content);
            f.write_all(&20u16.to_le_bytes()).unwrap();
            f.write_all(&flags.to_le_bytes()).unwrap();
            f.write_all(&method.to_le_bytes()).unwrap();
            f.write_all(&0u16.to_le_bytes()).unwrap();
            f.write_all(&0u16.to_le_bytes()).unwrap();
            f.write_all(&crc.to_le_bytes()).unwrap();
            f.write_all(&(content.len() as u32).to_le_bytes()).unwrap();
            f.write_all(&(content.len() as u32).to_le_bytes()).unwrap();
            f.write_all(&(fname.len() as u16).to_le_bytes()).unwrap();
            f.write_all(&0u16.to_le_bytes()).unwrap();
            f.write_all(fname).unwrap();
            f.write_all(content).unwrap();
        }

        let cd_offset = f.stream_position().unwrap();
        for (i, (fname, content)) in entries.iter().enumerate() {
            f.write_all(b"PK\x01\x02").unwrap();
            let crc = crc32fast::hash(content);
            f.write_all(&20u16.to_le_bytes()).unwrap();
            f.write_all(&20u16.to_le_bytes()).unwrap();
            f.write_all(&0u16.to_le_bytes()).unwrap();
            f.write_all(&0u16.to_le_bytes()).unwrap();
            f.write_all(&0u16.to_le_bytes()).unwrap();
            f.write_all(&0u16.to_le_bytes()).unwrap();
            f.write_all(&crc.to_le_bytes()).unwrap();
            f.write_all(&(content.len() as u32).to_le_bytes()).unwrap();
            f.write_all(&(content.len() as u32).to_le_bytes()).unwrap();
            f.write_all(&(fname.len() as u16).to_le_bytes()).unwrap();
            f.write_all(&0u16.to_le_bytes()).unwrap();
            f.write_all(&0u16.to_le_bytes()).unwrap();
            f.write_all(&0u16.to_le_bytes()).unwrap();
            f.write_all(&0u16.to_le_bytes()).unwrap();
            f.write_all(&0u32.to_le_bytes()).unwrap();
            f.write_all(&(offsets[i] as u32).to_le_bytes()).unwrap();
            f.write_all(fname).unwrap();
        }

        let cd_size = (f.stream_position().unwrap() - cd_offset) as u32;
        f.write_all(b"PK\x05\x06").unwrap();
        f.write_all(&0u16.to_le_bytes()).unwrap();
        f.write_all(&0u16.to_le_bytes()).unwrap();
        f.write_all(&(entries.len() as u16).to_le_bytes()).unwrap();
        f.write_all(&(entries.len() as u16).to_le_bytes()).unwrap();
        f.write_all(&cd_size.to_le_bytes()).unwrap();
        f.write_all(&(cd_offset as u32).to_le_bytes()).unwrap();
        f.write_all(&0u16.to_le_bytes()).unwrap();
    }

    #[test]
    fn raw_entries_preserves_gbk_bytes_exactly() {
        let temp = tempfile::tempdir().unwrap();
        let archive = temp.path().join("gbk.zip");
        let gbk_name = b"\xb2\xe2\xca\xd4\xce\xc4\xbc\xfe.txt";
        create_raw_zip_with_encoding(&archive, &[(gbk_name, b"hello")]);

        let reader = NativeZipBackend::new();
        let entries = reader.raw_entries(&archive).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].raw_name, gbk_name);
        assert!(!entries[0].is_dir);
        // Verify the bytes are exactly the central-directory bytes, not decoded.
        let decoded = smartzip_encoding::decode_name(gbk_name, "gbk").unwrap();
        assert_eq!(PathBuf::from(&decoded).to_string_lossy(), decoded.clone());
    }

    #[test]
    fn raw_entries_preserves_shift_jis_bytes() {
        let temp = tempfile::tempdir().unwrap();
        let archive = temp.path().join("sjis.zip");
        let sjis_name = b"\x83\x65\x83\x58\x83\x67.txt";
        create_raw_zip_with_encoding(&archive, &[(sjis_name, b"hello")]);

        let reader = NativeZipBackend::new();
        let entries = reader.raw_entries(&archive).unwrap();
        assert_eq!(entries[0].raw_name, sjis_name);
        let decoded = smartzip_encoding::decode_name(sjis_name, "shift_jis").unwrap();
        assert_eq!(PathBuf::from(&decoded).to_string_lossy(), decoded.clone());
    }

    #[test]
    fn raw_entries_handles_invalid_utf8_and_mixed_encodings() {
        let temp = tempfile::tempdir().unwrap();
        let archive = temp.path().join("mixed.zip");
        let entry1 = b"\xff\xfe\xfd"; // invalid UTF-8
        let entry2 = b"\xb2\xe2\xca\xd4"; // GBK for 测试
        let entry3 = b"normal.txt"; // ASCII / UTF-8
        create_raw_zip_with_encoding(&archive, &[(entry1, b"a"), (entry2, b"b"), (entry3, b"c")]);

        let reader = NativeZipBackend::new();
        let entries = reader.raw_entries(&archive).unwrap();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].raw_name, entry1);
        assert_eq!(entries[1].raw_name, entry2);
        assert_eq!(entries[2].raw_name, entry3);
        // Ensure invalid UTF-8 is preserved byte-for-byte.
        assert!(String::from_utf8(entries[0].raw_name.clone()).is_err());
    }

    #[test]
    fn raw_entries_preserves_utf8_flag_entry() {
        // ZIP crate sets UTF-8 bit (0x800) when given valid UTF-8; ensure we
        // still get the raw bytes unchanged.
        let temp = tempfile::tempdir().unwrap();
        let archive = temp.path().join("utf8.zip");
        std::fs::write(&archive, create_minimal_zip()).unwrap();

        let reader = NativeZipBackend::new();
        let entries = reader.raw_entries(&archive).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].raw_name, b"test.txt");
    }

    #[test]
    fn raw_entries_preserves_order_and_is_dir_flag() {
        let temp = tempfile::tempdir().unwrap();
        let archive = temp.path().join("ordered.zip");
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut writer = ZipWriter::new(&mut buf);
            let opts =
                SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
            writer.start_file("a.txt", opts).unwrap();
            writer.write_all(b"a").unwrap();
            writer.add_directory("dir/", opts).unwrap();
            writer.start_file("b.txt", opts).unwrap();
            writer.write_all(b"b").unwrap();
            writer.finish().unwrap();
        }
        std::fs::write(&archive, buf.into_inner()).unwrap();

        let reader = NativeZipBackend::new();
        let entries = reader.raw_entries(&archive).unwrap();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].raw_name, b"a.txt");
        assert!(!entries[0].is_dir);
        assert_eq!(entries[1].raw_name, b"dir/");
        assert!(entries[1].is_dir);
        assert_eq!(entries[2].raw_name, b"b.txt");
    }
}
