//! Fast content sampling hash used to deduplicate and recognize files.
//!
//! Hashing a multi-gigabyte archive end to end just to answer "have I seen
//! this before?" is unacceptable on the extract hot path. Instead we sample
//! the head and tail and fold the file size into the identity: two files are
//! treated as the same known file when both their [`sample_hash`] and size
//! match. Collisions are possible in theory but astronomically unlikely for
//! real archives, and the cost of a false match is only a skipped re-extract
//! (guarded by a time window and `--force`), never data loss.
//!
//! - `< SMALL_FILE_THRESHOLD` (128 KiB): the whole file is hashed, so the
//!   result is a true content hash.
//! - Otherwise: `BLAKE3(first 64 KiB ‖ last 64 KiB)`. The size returned
//!   alongside is what actually disambiguates same-prefix/same-suffix files.
//!
//! [`sample_hash_segment`] applies the same head/tail sampling to a byte
//! range carved out of a host file (an embedded archive). When the segment
//! length is unknown the file cannot be identified reliably, so it returns
//! `None` and the caller skips dedup for that carve.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// Files smaller than this are hashed in full rather than sampled.
const SMALL_FILE_THRESHOLD: u64 = 128 * 1024;
/// Bytes sampled from each of the head and tail for large files.
const SAMPLE_LEN: u64 = 64 * 1024;

/// Compute the sampling hash of a file on disk.
///
/// Returns `(hex_hash, size)` or `None` if the path can't be opened or read.
/// The size is part of the identity — callers must compare both.
pub fn sample_hash(path: &Path) -> Option<(String, u64)> {
    let mut file = File::open(path).ok()?;
    let size = file.metadata().ok()?.len();
    let mut hasher = blake3::Hasher::new();

    if size <= SMALL_FILE_THRESHOLD {
        let mut buf = Vec::with_capacity(size as usize);
        file.read_to_end(&mut buf).ok()?;
        hasher.update(&buf);
    } else {
        let mut head = vec![0u8; SAMPLE_LEN as usize];
        file.read_exact(&mut head).ok()?;
        hasher.update(&head);

        let mut tail = vec![0u8; SAMPLE_LEN as usize];
        file.seek(SeekFrom::End(-(SAMPLE_LEN as i64))).ok()?;
        file.read_exact(&mut tail).ok()?;
        hasher.update(&tail);
    }

    Some((hasher.finalize().to_hex().to_string(), size))
}

/// Compute the sampling hash of the `[offset, offset + size)` range of a host
/// file — used to identify an embedded/carved archive without materializing
/// it first.
///
/// Returns `None` when `size` is unknown (`None`): a carve of unknown length
/// can't be deduplicated safely, so it never enters `known_files`. Otherwise
/// returns `(hex_hash, size)` mirroring [`sample_hash`]'s head/tail policy
/// applied within the segment.
pub fn sample_hash_segment(path: &Path, offset: u64, size: Option<u64>) -> Option<(String, u64)> {
    let size = size?;
    if size == 0 {
        return None;
    }
    let mut file = File::open(path).ok()?;
    let mut hasher = blake3::Hasher::new();

    if size <= SMALL_FILE_THRESHOLD {
        file.seek(SeekFrom::Start(offset)).ok()?;
        let mut buf = vec![0u8; size as usize];
        file.read_exact(&mut buf).ok()?;
        hasher.update(&buf);
    } else {
        file.seek(SeekFrom::Start(offset)).ok()?;
        let mut head = vec![0u8; SAMPLE_LEN as usize];
        file.read_exact(&mut head).ok()?;
        hasher.update(&head);

        file.seek(SeekFrom::Start(offset + size - SAMPLE_LEN))
            .ok()?;
        let mut tail = vec![0u8; SAMPLE_LEN as usize];
        file.read_exact(&mut tail).ok()?;
        hasher.update(&tail);
    }

    Some((hasher.finalize().to_hex().to_string(), size))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn write_temp(bytes: &[u8]) -> NamedTempFile {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(bytes).unwrap();
        f.flush().unwrap();
        f
    }

    #[test]
    fn small_file_hashes_full_content() {
        let a = write_temp(b"hello world");
        let b = write_temp(b"hello world");
        let (ha, sa) = sample_hash(a.path()).unwrap();
        let (hb, sb) = sample_hash(b.path()).unwrap();
        assert_eq!(ha, hb);
        assert_eq!(sa, 11);
        assert_eq!(sb, 11);
    }

    #[test]
    fn small_files_differing_content_differ() {
        let a = write_temp(b"hello world");
        let b = write_temp(b"hello WORLD");
        assert_ne!(
            sample_hash(a.path()).unwrap().0,
            sample_hash(b.path()).unwrap().0
        );
    }

    #[test]
    fn large_file_samples_head_and_tail() {
        // 256 KiB: crosses the sampling threshold.
        let mut data = vec![0u8; 256 * 1024];
        for (i, byte) in data.iter_mut().enumerate() {
            *byte = (i % 251) as u8;
        }
        let f = write_temp(&data);
        let (h, size) = sample_hash(f.path()).unwrap();
        assert_eq!(size, 256 * 1024);
        assert_eq!(h.len(), 64);

        // A change strictly in the un-sampled middle keeps the hash identical
        // but the caller still separates them by size if it differs — here
        // size is unchanged, so the sampling hash collides by design.
        let mut middle_changed = data.clone();
        middle_changed[128 * 1024] ^= 0xFF;
        let g = write_temp(&middle_changed);
        assert_eq!(sample_hash(g.path()).unwrap().0, h);

        // A change in the head is caught.
        let mut head_changed = data.clone();
        head_changed[0] ^= 0xFF;
        let hc = write_temp(&head_changed);
        assert_ne!(sample_hash(hc.path()).unwrap().0, h);
    }

    #[test]
    fn segment_unknown_size_returns_none() {
        let f = write_temp(b"abcdefgh");
        assert!(sample_hash_segment(f.path(), 0, None).is_none());
    }

    #[test]
    fn segment_hashes_requested_range() {
        // Two host files whose [2, 6) window is identical hash the same even
        // though the surrounding bytes differ.
        let a = write_temp(b"XXhelloYY");
        let b = write_temp(b"ZZhelloWW");
        let (ha, sa) = sample_hash_segment(a.path(), 2, Some(5)).unwrap();
        let (hb, _) = sample_hash_segment(b.path(), 2, Some(5)).unwrap();
        assert_eq!(ha, hb);
        assert_eq!(sa, 5);
    }
}
