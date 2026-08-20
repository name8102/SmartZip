use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// Bounded sampled BLAKE3 fingerprint.
/// Hash size plus fixed-size samples around head/25%/50%/75%/tail.
/// Exact sample block size is an internal constant, not a product setting. Do not read full volume solely for duplicate detection.
const SAMPLE_BLOCK: usize = 8192;
const SAMPLE_HEAD: u64 = 0;

pub fn sampled_fingerprint(path: &Path) -> std::io::Result<blake3::Hash> {
    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    let mut hasher = blake3::Hasher::new();
    hasher.update(&len.to_le_bytes());

    if len == 0 {
        return Ok(hasher.finalize());
    }

    // If file < SAMPLE_BLOCK*5, hash entire? But requirement says do not hash complete large volumes.
    // For small files (< 128KB originally sample_hash uses 64KB head/tail, but here we use bounded samples anyway.
    // If file is small (< 64KB), reading entire is bounded anyway, but we follow spec: bounded samples only.
    // We'll read at most 5*SAMPLE_BLOCK bytes.
    let positions: [u64; 5] = [
        0,
        len / 4,
        len / 2,
        len * 3 / 4,
        len.saturating_sub(SAMPLE_BLOCK as u64),
    ];
    let mut buf = vec![0u8; SAMPLE_BLOCK];
    for pos in positions {
        let pos = pos.min(len.saturating_sub(1));
        file.seek(SeekFrom::Start(pos))?;
        let to_read = std::cmp::min(SAMPLE_BLOCK as u64, len - pos) as usize;
        buf.truncate(to_read);
        let mut read = 0usize;
        while read < to_read {
            let n = file.read(&mut buf[read..to_read])?;
            if n == 0 {
                break;
            }
            read += n;
        }
        hasher.update(&buf[..read]);
        // Reset buf size for next iteration
        buf.resize(SAMPLE_BLOCK, 0);
    }
    Ok(hasher.finalize())
}

/// Compare two files for duplicate via sampled fingerprint plus size.
pub fn are_sampled_duplicates(a: &Path, b: &Path) -> std::io::Result<bool> {
    let ma = std::fs::metadata(a)?;
    let mb = std::fs::metadata(b)?;
    if ma.len() != mb.len() {
        return Ok(false);
    }
    let ha = sampled_fingerprint(a)?;
    let hb = sampled_fingerprint(b)?;
    Ok(ha == hb)
}
