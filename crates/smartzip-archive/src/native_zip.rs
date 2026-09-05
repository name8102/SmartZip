use smartzip_core::{Result, SmartZipError};
use std::fs::File;
use std::path::Path;
use zip::ZipArchive;

/// Raw ZIP entry as stored in central directory, without SmartZip decoding.
///
/// This is the only data NativeZip is allowed to expose: the exact byte
/// sequence of the entry filename as it appears on disk. Encoding detection
/// must happen in `smartzip-encoding`, not here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZipRawEntry {
    pub raw_name: Vec<u8>,
    pub is_dir: bool,
    pub compressed_size: u64,
    pub uncompressed_size: u64,
}

/// Narrow helper for ZIP filename encoding detection.
///
/// Previously this type pretended to be a full `ArchiveAdapter` (probe/list/
/// test/extract/compress) which polluted routing. It now only reads the
/// central-directory filename bytes.
#[derive(Debug, Clone)]
pub struct NativeZipBackend {
    id: String,
}

impl Default for NativeZipBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl NativeZipBackend {
    pub fn new() -> Self {
        Self {
            id: "native-zip".into(),
        }
    }

    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.id = id.into();
        self
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    fn open_archive_read(path: &Path) -> Result<ZipArchive<File>> {
        let file = File::open(path)
            .map_err(|source| SmartZipError::io(Some(path.to_path_buf()), source))?;
        ZipArchive::new(file).map_err(|source| map_zip_error(source, path))
    }

    /// Read the raw central-directory entries of a ZIP file.
    ///
    /// Returns the exact filename bytes (`name_raw`) for every entry, plus
    /// `is_dir`. No decoding, no extraction, no materialization.
    pub fn raw_entries(&self, path: &Path) -> Result<Vec<ZipRawEntry>> {
        let mut archive =
            Self::open_archive_read(path).map_err(|e| with_backend_identity(e, &self.id))?;
        let mut entries = Vec::with_capacity(archive.len());
        for i in 0..archive.len() {
            let entry = archive
                .by_index_raw(i)
                .map_err(|source| map_zip_error(source, path))
                .map_err(|e| with_backend_identity(e, &self.id))?;
            entries.push(ZipRawEntry {
                raw_name: entry.name_raw().to_vec(),
                is_dir: entry.is_dir(),
                compressed_size: entry.compressed_size(),
                uncompressed_size: entry.size(),
            });
        }
        Ok(entries)
    }

    /// Check whether any entry in the ZIP is encrypted (without decrypting).
    pub fn has_encrypted_entries(&self, path: &Path) -> Result<bool> {
        let mut archive =
            Self::open_archive_read(path).map_err(|e| with_backend_identity(e, &self.id))?;
        Ok((0..archive.len()).any(|i| archive.by_index_raw(i).ok().is_some_and(|e| e.encrypted())))
    }
}

// Keep the old helper for tests that want to verify raw bytes are preserved
// exactly as stored. This is intentionally *not* exposed as a backend.
fn with_backend_identity(error: SmartZipError, backend_id: &str) -> SmartZipError {
    match error {
        SmartZipError::UnsupportedContainer {
            path, container, ..
        } => SmartZipError::UnsupportedContainer {
            backend: backend_id.to_owned(),
            path,
            container,
        },
        SmartZipError::UnsupportedCodec { path, codec, .. } => SmartZipError::UnsupportedCodec {
            backend: backend_id.to_owned(),
            path,
            codec,
        },
        SmartZipError::BackendFailed {
            exit_code, stderr, ..
        } => SmartZipError::BackendFailed {
            backend: backend_id.to_owned(),
            exit_code,
            stderr,
        },
        other => other,
    }
}

fn map_zip_error(source: zip::result::ZipError, path: &Path) -> SmartZipError {
    match source {
        zip::result::ZipError::UnsupportedArchive(feature) => SmartZipError::UnsupportedCodec {
            backend: "native-zip".into(),
            path: path.to_path_buf(),
            codec: Some(feature.to_string()),
        },
        zip::result::ZipError::Io(source) => SmartZipError::io(Some(path.to_path_buf()), source),
        zip::result::ZipError::InvalidArchive(detail) => SmartZipError::CorruptedArchive {
            path: path.to_path_buf(),
            detail: detail.to_string(),
        },
        zip::result::ZipError::InvalidPassword => SmartZipError::WrongPassword {
            path: path.to_path_buf(),
        },
        other => SmartZipError::BackendFailed {
            backend: "native-zip".into(),
            exit_code: None,
            stderr: other.to_string(),
        },
    }
}

// ---------------------------------------------------------------------------
// Tests: only raw-filename preservation and detection metadata. No extract,
// probe, list, test, compress. Those operations are intentionally not
// supported via this type any more; they must go through SevenZip/Unrar.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;
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
