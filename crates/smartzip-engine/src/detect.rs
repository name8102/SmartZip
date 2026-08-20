use smartzip_core::{ArchiveFormat, DetectionKind};
use std::fs::File;
use std::io::Read;
use std::path::Path;

const MAGIC_ZIP: &[u8] = b"PK\x03\x04";
const MAGIC_ZIP_EMPTY_CENTRAL: &[u8] = b"PK\x05\x06";
const MAGIC_RAR4: &[u8] = b"Rar!\x1a\x07\x00";
const MAGIC_RAR5: &[u8] = b"Rar!\x1a\x07\x01\x00";
const MAGIC_7Z: &[u8] = b"\x37\x7a\xbc\xaf\x27\x1c";
const MAGIC_GZIP: &[u8] = b"\x1f\x8b";
const MAGIC_BZIP2: &[u8] = b"BZ";
const MAGIC_BZIP2_FULL: &[u8] = b"BZh";
const MAGIC_XZ: &[u8] = b"\xfd\x37\x7a\x58\x5a\x00";
const MAGIC_TAR_AT_OFFSET_257: &[u8] = b"ustar\0";

fn matches_prefix(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.len() >= needle.len() && &haystack[..needle.len()] == needle
}

pub fn detect_archive_header(bytes: &[u8]) -> Option<(ArchiveFormat, u64)> {
    if bytes.len() < 6 {
        return None;
    }

    if matches_prefix(bytes, MAGIC_ZIP) || matches_prefix(bytes, MAGIC_ZIP_EMPTY_CENTRAL) {
        return Some((ArchiveFormat::Zip, 0));
    }
    if matches_prefix(bytes, MAGIC_RAR5) {
        return Some((ArchiveFormat::Rar, 0));
    }
    if matches_prefix(bytes, MAGIC_RAR4) {
        return Some((ArchiveFormat::Rar, 0));
    }
    if matches_prefix(bytes, MAGIC_7Z) {
        return Some((ArchiveFormat::SevenZip, 0));
    }
    if matches_prefix(bytes, MAGIC_GZIP) {
        return Some((ArchiveFormat::Gzip, 0));
    }
    if matches_prefix(bytes, MAGIC_BZIP2_FULL) || matches_prefix(bytes, MAGIC_BZIP2) {
        return Some((ArchiveFormat::Bzip2, 0));
    }
    if matches_prefix(bytes, MAGIC_XZ) {
        return Some((ArchiveFormat::Xz, 0));
    }
    if bytes.len() >= 263 && &bytes[257..263] == MAGIC_TAR_AT_OFFSET_257 {
        return Some((ArchiveFormat::Tar, 0));
    }

    None
}

pub fn is_archive_at_offset(bytes: &[u8], offset: usize) -> bool {
    if offset >= bytes.len() {
        return false;
    }
    detect_archive_header(&bytes[offset..]).is_some()
}

pub fn detect_non_archive_header(bytes: &[u8]) -> bool {
    if bytes.is_empty() {
        return false;
    }
    // Replace handwritten ordinary-file magic with the mature `infer` crate.
    // `infer` is used only for ordinary non-archive classification; archive
    // detection remains via SmartZip’s own `detect_archive_header` / volume
    // probes. Embedded scanning stays independent.
    let Some(kind) = infer::get(bytes) else {
        return false;
    };
    let ext = kind.extension();
    // Archive-like extensions are not ordinary files; they must not block
    // volume discovery. This prevents a disguised `.jpg` that actually
    // contains `PK`/`7z`/`Rar!` bytes from being treated as ordinary.
    // `infer` itself may report archive types for such bytes – in that
    // case we return false so the caller can proceed to archive probing.
    const ARCHIVE_EXTS: &[&str] = &[
        "zip", "rar", "7z", "tar", "gz", "tgz", "bz2", "xz", "zst", "zstd", "lz4", "lzma", "cab",
        "iso", "dmg",
    ];
    if ARCHIVE_EXTS.contains(&ext) {
        return false;
    }
    // Any other `infer` hit (jpeg, png, pdf, mp4, elf, exe, riff, etc.) is
    // strong negative evidence for cross-file volume discovery.
    true
}

pub fn classify_by_header(
    header: Option<ArchiveFormat>,
    has_non_archive_header: bool,
    file_ext_is_archive: bool,
) -> DetectionKind {
    match (header, has_non_archive_header, file_ext_is_archive) {
        (Some(_), _, _) => DetectionKind::DirectArchive,
        (None, true, true) => DetectionKind::NotArchive,
        (None, false, true) => DetectionKind::Ambiguous,
        (None, true, false) => DetectionKind::NotArchive,
        (None, false, false) => DetectionKind::NotArchive,
    }
}

const HEADER_READ_SIZE: usize = 8 * 1024;

pub fn probe_file_header(path: &Path) -> Option<(ArchiveFormat, u64)> {
    let mut file = File::open(path).ok()?;
    let mut buf = vec![0u8; HEADER_READ_SIZE];
    let n = file.read(&mut buf).ok()?;
    buf.truncate(n);
    detect_archive_header(&buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_zip_magic() {
        let data = b"PK\x03\x04rest";
        let (fmt, off) = detect_archive_header(data).unwrap();
        assert_eq!(fmt, ArchiveFormat::Zip);
        assert_eq!(off, 0);
    }

    #[test]
    fn detects_zip_empty_central() {
        let data = b"PK\x05\x06rest";
        let (fmt, off) = detect_archive_header(data).unwrap();
        assert_eq!(fmt, ArchiveFormat::Zip);
        assert_eq!(off, 0);
    }

    #[test]
    fn detects_rar4() {
        let data = b"Rar!\x1a\x07\x00rest";
        let (fmt, _) = detect_archive_header(data).unwrap();
        assert_eq!(fmt, ArchiveFormat::Rar);
    }

    #[test]
    fn detects_rar5() {
        let mut data = vec![0u8; 8];
        data[..8].copy_from_slice(b"Rar!\x1a\x07\x01\x00");
        let (fmt, _) = detect_archive_header(&data).unwrap();
        assert_eq!(fmt, ArchiveFormat::Rar);
    }

    #[test]
    fn detects_7z() {
        let mut data = vec![0u8; 6];
        data[..6].copy_from_slice(b"\x37\x7a\xbc\xaf\x27\x1c");
        let (fmt, _) = detect_archive_header(&data).unwrap();
        assert_eq!(fmt, ArchiveFormat::SevenZip);
    }

    #[test]
    fn detects_gzip() {
        let data = b"\x1f\x8b\x08rest";
        let (fmt, _) = detect_archive_header(data).unwrap();
        assert_eq!(fmt, ArchiveFormat::Gzip);
    }

    #[test]
    fn detects_bzip2() {
        let data = b"BZh9rest";
        let (fmt, _) = detect_archive_header(data).unwrap();
        assert_eq!(fmt, ArchiveFormat::Bzip2);
    }

    #[test]
    fn detects_xz() {
        let mut data = vec![0u8; 6];
        data[..6].copy_from_slice(b"\xfd\x37\x7a\x58\x5a\x00");
        let (fmt, _) = detect_archive_header(&data).unwrap();
        assert_eq!(fmt, ArchiveFormat::Xz);
    }

    #[test]
    fn detects_tar_at_offset_257() {
        let mut data = vec![0u8; 263];
        data[257..263].copy_from_slice(b"ustar\0");
        let (fmt, _) = detect_archive_header(&data).unwrap();
        assert_eq!(fmt, ArchiveFormat::Tar);
    }

    #[test]
    fn returns_none_for_too_short() {
        assert!(detect_archive_header(b"PK").is_none());
        assert!(detect_archive_header(b"").is_none());
    }

    #[test]
    fn returns_none_for_unknown() {
        assert!(detect_archive_header(b"hello world").is_none());
    }

    #[test]
    fn is_archive_at_offset_works() {
        let mut data = vec![0u8; 128];
        data[64..68].copy_from_slice(b"PK\x03\x04");
        assert!(is_archive_at_offset(&data, 64));
        assert!(!is_archive_at_offset(&data, 0));
        assert!(!is_archive_at_offset(&data, 999));
    }

    #[test]
    fn detects_jpeg() {
        let data = b"\xff\xd8\xff\xe0rest";
        assert!(detect_non_archive_header(data));
    }

    #[test]
    fn detects_png() {
        let mut data = vec![0u8; 8];
        data[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
        assert!(detect_non_archive_header(&data));
    }

    #[test]
    fn detects_pdf() {
        let data = b"%PDF-1.4rest";
        assert!(detect_non_archive_header(data));
    }

    #[test]
    fn detects_elf() {
        // infer's ELF matcher requires >52 bytes
        let mut data = vec![0u8; 64];
        data[0..4].copy_from_slice(b"\x7fELF");
        data[1..4].copy_from_slice(b"ELF");
        assert!(detect_non_archive_header(&data));
    }

    #[test]
    fn detects_riff() {
        // infer detects WAV (RIFF/WAVE) not generic RIFF
        let mut data = vec![0u8; 16];
        data[0..4].copy_from_slice(b"RIFF");
        data[8..12].copy_from_slice(b"WAVE");
        assert!(detect_non_archive_header(&data));
    }

    #[test]
    fn detects_exe() {
        let data = b"MZ\x90\x00rest";
        assert!(detect_non_archive_header(data));
    }

    #[test]
    fn detects_mp4() {
        let mut data = vec![0u8; 16];
        data[0..3].copy_from_slice(b"\x00\x00\x00");
        data[4..8].copy_from_slice(b"ftyp");
        data[8..12].copy_from_slice(b"isom");
        assert!(detect_non_archive_header(&data));
    }

    #[test]
    fn non_archive_returns_false_for_empty() {
        assert!(!detect_non_archive_header(b""));
    }

    #[test]
    fn non_archive_returns_false_for_archive_header() {
        assert!(!detect_non_archive_header(b"PK\x03\x04"));
    }

    #[test]
    fn classify_direct_archive() {
        assert_eq!(
            classify_by_header(Some(ArchiveFormat::Zip), false, false),
            DetectionKind::DirectArchive
        );
    }

    #[test]
    fn classify_not_archive_with_header() {
        assert_eq!(
            classify_by_header(None, true, false),
            DetectionKind::NotArchive
        );
    }

    #[test]
    fn classify_not_archive_ext_but_non_archive_header() {
        assert_eq!(
            classify_by_header(None, true, true),
            DetectionKind::NotArchive
        );
    }

    #[test]
    fn classify_ambiguous_ext_only() {
        assert_eq!(
            classify_by_header(None, false, true),
            DetectionKind::Ambiguous
        );
    }

    #[test]
    fn classify_not_archive_nothing() {
        assert_eq!(
            classify_by_header(None, false, false),
            DetectionKind::NotArchive
        );
    }

    #[test]
    fn probe_file_header_reads_zip() {
        let dir = std::env::temp_dir().join(format!("smartzip-detect-zip-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("zip.dat");
        std::fs::write(&path, b"PK\x03\x04some data here").unwrap();
        let result = probe_file_header(&path);
        assert_eq!(result, Some((ArchiveFormat::Zip, 0)));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn probe_file_header_returns_none_for_unknown() {
        let dir = std::env::temp_dir().join(format!("smartzip-detect-unk-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("unknown.dat");
        std::fs::write(&path, b"hello world this is not an archive").unwrap();
        assert!(probe_file_header(&path).is_none());
        let _ = std::fs::remove_dir_all(dir);
    }
}
