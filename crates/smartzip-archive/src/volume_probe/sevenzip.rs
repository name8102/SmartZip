use super::{VolumeProbeResult, VolumeStructure};
use smartzip_core::ArchiveFormat;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

const SEVENZIP_SIG: &[u8] = b"\x37\x7a\xbc\xaf\x27\x1c";
const SEVENZIP_SIG_LEN: usize = 6;
const START_HEADER_SIZE: usize = 32;

pub fn probe_7z(path: &Path) -> Option<VolumeProbeResult> {
    let mut file = File::open(path).ok()?;
    let file_len = file.metadata().ok()?.len();
    if file_len < START_HEADER_SIZE as u64 {
        return None;
    }
    let mut header = [0u8; 32];
    file.seek(SeekFrom::Start(0)).ok()?;
    file.read_exact(&mut header).ok()?;

    if header[..SEVENZIP_SIG_LEN] != *SEVENZIP_SIG {
        // Not a 7z start. Could be raw continuation chunk without header – then not applicable.
        // But resolver still needs filename ordering to discover continuation chunks once start anchored.
        return None;
    }
    // Verify version
    let _major = header[6];
    let _minor = header[7];
    // StartHeaderCRC at 8..12 little endian
    let stored_crc = u32::from_le_bytes([header[8], header[9], header[10], header[11]]);
    let computed_crc = crc32fast::hash(&header[12..32]);
    if stored_crc != computed_crc {
        // Invalid CRC -> PossiblyMultiVolume? But spec says validate.
        // Return NotApplicable? However corrupted 7z shouldn't be considered standalone.
        return Some(VolumeProbeResult::PossiblyMultiVolume(VolumeStructure {
            format: ArchiveFormat::SevenZip,
            logical_volume_index: Some(0),
            expected_volume_count: None,
            expected_logical_size: None,
            is_last_volume: None,
        }));
    }
    let next_header_offset = u64::from_le_bytes([
        header[12], header[13], header[14], header[15], header[16], header[17], header[18], header[19],
    ]);
    let next_header_size = u64::from_le_bytes([
        header[20], header[21], header[22], header[23], header[24], header[25], header[26], header[27],
    ]);
    let _next_header_crc = u32::from_le_bytes([header[28], header[29], header[30], header[31]]);

    let expected_logical_size = 32u64.saturating_add(next_header_offset).saturating_add(next_header_size);

    // If logical extent closes inside current physical file with valid structure, treat as standalone.
    // If extent points beyond file, that proves current file is not complete standalone 7z.
    // 7z split archives: first volume contains start header (32 bytes) + packed streams. Later volumes are raw.
    // So we need to check if expected_logical_size <= file_len => likely standalone (but could still be split? For split, first file size may be larger than logical? Actually split size is arbitrary, logical extent is inside first file only for non-split? For split .7z.001, the logical size may span multiple files. How to distinguish? For 7z split, NextHeaderOffset+NextHeaderSize often exceeds first file's size, indicating continuation. If it does, then it's multivolume.
    if expected_logical_size == 0 {
        return Some(VolumeProbeResult::PossiblyMultiVolume(VolumeStructure {
            format: ArchiveFormat::SevenZip,
            logical_volume_index: Some(0),
            expected_volume_count: None,
            expected_logical_size: None,
            is_last_volume: None,
        }));
    }
    if expected_logical_size <= file_len {
        // Validate that NextHeader is within file and plausible (we could try to read it, but cheap).
        // If valid, treat as standalone.
        return Some(VolumeProbeResult::Standalone(ArchiveFormat::SevenZip));
    } else {
        // Logical extent exceeds this physical file → proves this file is not complete standalone, but membership still needs resolution.
        // This is strong evidence for multivolume start.
        return Some(VolumeProbeResult::MultiVolume(VolumeStructure {
            format: ArchiveFormat::SevenZip,
            logical_volume_index: Some(0),
            expected_volume_count: None,
            expected_logical_size: Some(expected_logical_size),
            is_last_volume: Some(false),
        }));
    }
}
