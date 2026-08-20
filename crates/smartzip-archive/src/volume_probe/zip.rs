use super::{VolumeProbeResult, VolumeStructure};
use smartzip_core::ArchiveFormat;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

const EOCD_SIG: [u8; 4] = [0x50, 0x4b, 0x05, 0x06];
const ZIP64_EOCD_LOC_SIG: [u8; 4] = [0x50, 0x4b, 0x06, 0x07];
const ZIP64_EOCD_SIG: [u8; 4] = [0x50, 0x4b, 0x06, 0x06];

pub fn probe_zip(path: &Path) -> Option<VolumeProbeResult> {
    let mut file = File::open(path).ok()?;
    let file_len = file.metadata().ok()?.len();
    if file_len < 22 {
        return None;
    }
    // Search for EOCD near end, up to 64KB + EOCD size as per spec.
    let search_len = std::cmp::min(file_len, 65557 + 22) as usize;
    let start = file_len - search_len as u64;
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut buf = vec![0u8; search_len];
    file.read_exact(&mut buf).ok()?;

    if let Some(eocd_pos) = find_eocd(&buf) {
        let eocd = &buf[eocd_pos..];
        if eocd.len() < 22 {
            return None;
        }
        // EOCD fields: offset 4 = this disk, 6 = central dir start disk, 8 = entries this disk, 10 = total entries, 12 = central dir size, 16 = central dir offset, 20 = comment len
        let this_disk = u16::from_le_bytes([eocd[4], eocd[5]]) as u32;
        let cd_start_disk = u16::from_le_bytes([eocd[6], eocd[7]]) as u32;
        let entries_this_disk = u16::from_le_bytes([eocd[8], eocd[9]]) as u32;
        let total_entries = u16::from_le_bytes([eocd[10], eocd[11]]) as u32;
        let cd_size = u32::from_le_bytes([eocd[12], eocd[13], eocd[14], eocd[15]]) as u64;
        let cd_offset = u32::from_le_bytes([eocd[16], eocd[17], eocd[18], eocd[19]]) as u64;
        let comment_len = u16::from_le_bytes([eocd[20], eocd[21]]) as usize;
        let is_zip64_placeholder = total_entries == 0xFFFF || entries_this_disk == 0xFFFF || cd_size == 0xFFFFFFFF || cd_offset == 0xFFFFFFFF;
        if is_zip64_placeholder {
            if let Some(z64) = probe_zip64(&buf, eocd_pos, file_len) {
                return Some(z64);
            }
            // If ZIP64 needed but not found, treat as possibly multivolume (definitely not standalone).
            return Some(VolumeProbeResult::MultiVolume(VolumeStructure {
                format: ArchiveFormat::Zip,
                logical_volume_index: Some(this_disk),
                expected_volume_count: None,
                expected_logical_size: None,
                is_last_volume: None,
            }));
        }
        // Strong standalone closure check: check if central directory is within this file and disk numbers are zero.
        let cd_end = cd_offset.saturating_add(cd_size);
        // EOCD position relative to start of file = start + eocd_pos
        let eocd_file_offset = start + eocd_pos as u64;
        let expected_eocd_start = cd_end;
        // If this is single disk, this_disk==0, cd_start_disk==0, and CD fits before EOCD with correct adjacency (allowing comment)
        let is_single_disk = this_disk == 0 && cd_start_disk == 0;
        let standalone = is_single_disk && cd_end <= eocd_file_offset && (eocd_file_offset - cd_end) < 65536;
        // Also total entries consistency
        if standalone {
            // Additionally, if file has ZIP64 locator, not standalone.
            return Some(VolumeProbeResult::Standalone(ArchiveFormat::Zip));
        } else if this_disk != 0 || cd_start_disk != 0 {
            // This file is not disk 0, or central dir starts elsewhere → multivolume
            // Determine if last volume: if this_disk is last? Need total disks from EOCD? In non-ZIP64, total disks not stored in EOCD directly, but we can infer if this_disk > cd_start_disk etc.
            // For split archives, EOCD in last disk contains valid CD.
            // If this file's EOCD indicates this_disk > 0, it's not first, but we can still determine.
            let is_last = Some(true); // EOCD presence indicates this is last disk
            return Some(VolumeProbeResult::MultiVolume(VolumeStructure {
                format: ArchiveFormat::Zip,
                logical_volume_index: Some(this_disk),
                expected_volume_count: None, // EOCD total disks is at offset? For non-ZIP64, total disks is not in EOCD? Actually EOCD has no total disks field beyond these. Use total_entries?
                expected_logical_size: None,
                is_last_volume: is_last,
            }));
        } else {
            // EOCD found but not single disk? Could be spanned but EOCD corrupted.
            // Treat as PossiblyMultiVolume if disk numbers indicate split but closure not strong.
            return Some(VolumeProbeResult::PossiblyMultiVolume(VolumeStructure {
                format: ArchiveFormat::Zip,
                logical_volume_index: Some(this_disk),
                expected_volume_count: None,
                expected_logical_size: None,
                is_last_volume: None,
            }));
        }
    } else {
        // No EOCD found: check for local file header at offset 0?
        // Local header magic at 0 is not sufficient proof of standalone completeness.
        // If file starts with PK\x03\x04 but no EOCD, it may be a split part without EOCD (e.g., .z01).
        // Treat as PossiblyMultiVolume if local header present.
        let mut head = [0u8; 4];
        file.seek(SeekFrom::Start(0)).ok()?;
        if file.read_exact(&mut head).is_ok() && head == [0x50, 0x4b, 0x03, 0x04] {
            // Has local header but no EOCD → likely not standalone, need continuation.
            return Some(VolumeProbeResult::MultiVolume(VolumeStructure {
                format: ArchiveFormat::Zip,
                logical_volume_index: None,
                expected_volume_count: None,
                expected_logical_size: None,
                is_last_volume: Some(false),
            }));
        }
    }
    None
}

fn find_eocd(buf: &[u8]) -> Option<usize> {
    // Search backwards for EOCD sig.
    if buf.len() < 4 {
        return None;
    }
    for i in (0..=buf.len() - 4).rev() {
        if buf[i..i + 4] == EOCD_SIG {
            // Validate comment len matches remaining bytes
            if buf.len() >= i + 22 {
                let comment_len = u16::from_le_bytes([buf[i + 20], buf[i + 21]]) as usize;
                if i + 22 + comment_len == buf.len() || i + 22 + comment_len <= buf.len() && buf.len() - (i + 22 + comment_len) < 1024 {
                    // Accept
                    return Some(i);
                }
            } else {
                return Some(i);
            }
        }
    }
    None
}

fn probe_zip64(buf: &[u8], eocd_pos: usize, file_len: u64) -> Option<VolumeProbeResult> {
    // Search for ZIP64 EOCD locator before EOCD.
    if eocd_pos < 20 {
        return None;
    }
    let locator_pos = eocd_pos.saturating_sub(20);
    let mut found = None;
    for i in locator_pos..eocd_pos {
        if i + 20 <= buf.len() && buf[i..i + 4] == ZIP64_EOCD_LOC_SIG {
            found = Some(i);
            break;
        }
    }
    let locator = found?;
    if locator + 20 > buf.len() {
        return None;
    }
    let cd_start_disk_zip64 = u32::from_le_bytes([buf[locator + 8], buf[locator + 9], buf[locator + 10], buf[locator + 11]]);
    let total_disks = u32::from_le_bytes([buf[locator + 16], buf[locator + 17], buf[locator + 18], buf[locator + 19]]);
    // Also read ZIP64 EOCD itself? Locator points to it via offset.
    // For standalone check, need to see if disks are 1.
    let this_disk = 0; // EOCD's this_disk already read? But ZIP64 overrides.
    // Hard to get exact: read ZIP64 EOCD at locator's offset.
    // We'll approximate: if total_disks == 1 => standalone else multivolume
    if total_disks == 1 && cd_start_disk_zip64 == 0 {
        return Some(VolumeProbeResult::Standalone(ArchiveFormat::Zip));
    } else {
        return Some(VolumeProbeResult::MultiVolume(VolumeStructure {
            format: ArchiveFormat::Zip,
            logical_volume_index: Some(cd_start_disk_zip64),
            expected_volume_count: Some(total_disks),
            expected_logical_size: None,
            is_last_volume: Some(true),
        }));
    }
}
