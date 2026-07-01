//! Stable hashes for archive paths used as history join keys.
//!
//! We store hashes rather than raw paths to keep history rows small and to
//! avoid leaking user directory layout into the schema more than necessary.
//! Paths are canonicalized when the file still exists (the extraction path);
//! when it doesn't (deleted after extract, or a not-yet-materialized carve
//! path), we fall back to hashing the raw byte representation so the value
//! stays stable within a single task.

use sha2::{Digest, Sha256};
use std::path::Path;

/// Hex-encoded SHA-256 of a canonicalized path (or the raw path bytes on
/// canonicalize failure).
pub fn path_hash(path: &Path) -> String {
    let canonical = std::fs::canonicalize(path).ok();
    let bytes: Vec<u8> = match &canonical {
        Some(p) => path_to_bytes(p.as_path()),
        None => path_to_bytes(path),
    };
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    hex_encode(&hasher.finalize())
}

#[cfg(unix)]
fn path_to_bytes(path: &Path) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    path.as_os_str().as_bytes().to_vec()
}

#[cfg(not(unix))]
fn path_to_bytes(path: &Path) -> Vec<u8> {
    // Lossy on Windows for non-UTF-16 paths, but such paths cannot be
    // materialized to the extract queue in the first place, so the
    // approximation is acceptable for a hash key.
    path.to_string_lossy().into_owned().into_bytes()
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn same_path_hashes_identically() {
        let path = PathBuf::from("/tmp/does-not-exist/archive.zip");
        assert_eq!(path_hash(&path), path_hash(&path));
    }

    #[test]
    fn distinct_paths_hash_differently() {
        let a = PathBuf::from("/tmp/a.zip");
        let b = PathBuf::from("/tmp/b.zip");
        assert_ne!(path_hash(&a), path_hash(&b));
    }

    #[test]
    fn hash_is_lowercase_hex_of_fixed_length() {
        let hash = path_hash(&PathBuf::from("/tmp/does-not-exist/archive.zip"));
        assert_eq!(hash.len(), 64);
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    }
}
