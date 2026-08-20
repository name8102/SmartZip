use super::{VolumeProbeResult, VolumeStructure};
use smartzip_core::ArchiveFormat;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

const RAR4_MAGIC: &[u8] = b"Rar!\x1a\x07\x00";
const RAR5_MAGIC: &[u8] = b"Rar!\x1a\x07\x01\x00";

/// Probe RAR volume structure.
/// - Uses flags in main header where cheaply readable.
/// - Covers RAR5 and RAR3/4 sufficiently for resolver needs.
pub fn probe_rar(path: &Path) -> Option<VolumeProbeResult> {
    let mut file = File::open(path).ok()?;
    let mut header = [0u8; 64];
    let n = file.read(&mut header).ok()?;
    if n < 8 {
        return None;
    }
    if n >= 8 && header[..8] == RAR5_MAGIC[..8] {
        return Some(probe_rar5(&header, path, &mut file));
    }
    if n >= 7 && header[..7] == RAR4_MAGIC[..7] {
        return Some(probe_rar4(&header, path, &mut file));
    }
    None
}

fn probe_rar5(header: &[u8], _path: &Path, _file: &mut File) -> VolumeProbeResult {
    // RAR5 after 8-byte signature: sequence of headers each starting with CRC32.
    // Main header (type 1) general flags 0x0001 = extra area present, 0x0002 = data area present.
    // Archive flags (volume 0x0001, volume number present 0x0002) are inside data area, not header flags.
    // Correct parsing must skip extra area and read archive flags from data.
    if header.len() <= 8 {
        return VolumeProbeResult::PossiblyMultiVolume(VolumeStructure {
            format: ArchiveFormat::Rar,
            logical_volume_index: None,
            expected_volume_count: None,
            expected_logical_size: None,
            is_last_volume: None,
        });
    }
    match parse_rar5_main_flags(&header[8..]) {
        Some((is_volume, volume_number)) => {
            if is_volume {
                // Volume flag alone does not imply not-last; last volume also has it. End-of-archive header determines last.
                VolumeProbeResult::MultiVolume(VolumeStructure {
                    format: ArchiveFormat::Rar,
                    logical_volume_index: volume_number,
                    expected_volume_count: None,
                    expected_logical_size: None,
                    is_last_volume: None,
                })
            } else {
                VolumeProbeResult::Standalone(ArchiveFormat::Rar)
            }
        }
        None => VolumeProbeResult::PossiblyMultiVolume(VolumeStructure {
            format: ArchiveFormat::Rar,
            logical_volume_index: None,
            expected_volume_count: None,
            expected_logical_size: None,
            is_last_volume: None,
        }),
    }
}

fn parse_rar5_main_flags(data: &[u8]) -> Option<(bool, Option<u32>)> {
    let mut pos = 0usize;
    let _crc = read_bytes(data, &mut pos, 4)?;
    let _header_size = read_vint(data, &mut pos)?;
    let header_type = read_vint(data, &mut pos)?;
    if header_type != 1 {
        return None;
    }
    let hdr_flags = read_vint(data, &mut pos)?;
    let has_extra = (hdr_flags & 0x01) != 0;
    let extra_size = if has_extra { read_vint(data, &mut pos)? } else { 0 };
    // For RAR5 main header, Archive flags follow immediately after Extra area size, not in Data area.
    // Layout: Header flags, [Extra area size], Archive flags, [Volume number], [Extra area]
    // Archive flags 0x0001 = Volume, 0x0002 = Volume number present.
    if data.len() < pos + 1 {
        return None;
    }
    let arc_flags = read_vint(data, &mut pos)?;
    let is_volume = (arc_flags & 0x01) != 0;
    let has_vol_number = (arc_flags & 0x02) != 0;
    let vol_number = if has_vol_number {
        Some(read_vint(data, &mut pos)? as u32)
    } else {
        None
    };
    // Validate extra area fits (if present, it follows after archive flags/volume number)
    if has_extra && data.len() < pos + extra_size as usize {
        return None;
    }
    Some((is_volume, vol_number))
}

fn read_bytes<'a>(data: &'a [u8], pos: &mut usize, n: usize) -> Option<&'a [u8]> {
    if *pos + n > data.len() {
        return None;
    }
    let v = &data[*pos..*pos + n];
    *pos += n;
    Some(v)
}

fn read_vint(data: &[u8], pos: &mut usize) -> Option<u64> {
    let mut result = 0u64;
    let mut shift = 0;
    loop {
        if *pos >= data.len() {
            return None;
        }
        let b = data[*pos];
        *pos += 1;
        result |= ((b & 0x7F) as u64) << shift;
        if (b & 0x80) == 0 {
            break;
        }
        shift += 7;
        if shift >= 64 {
            return None;
        }
    }
    Some(result)
}

fn probe_rar4(header: &[u8], _path: &Path, file: &mut File) -> VolumeProbeResult {
    // RAR3/4: after 7-byte magic, main header: [HEAD_CRC 2][HEAD_TYPE 1][HEAD_FLAGS 2][HEAD_SIZE 2]...
    // HEAD_TYPE 0x73 = MAIN_HEAD, flags 0x0001 = volume, 0x0002 = comment, 0x0100 = solid, etc.
    // Also at offset 10-11 flags, 12-13 size, 14-15 high pos? etc.
    // We'll read flags directly.
    if header.len() < 13 {
        return VolumeProbeResult::PossiblyMultiVolume(VolumeStructure {
            format: ArchiveFormat::Rar,
            logical_volume_index: None,
            expected_volume_count: None,
            expected_logical_size: None,
            is_last_volume: None,
        });
    }
    let flags = u16::from_le_bytes([header[10], header[11]]);
    let is_volume = (flags & 0x0001) != 0;
    if is_volume {
        // Try to read volume number? In old RAR, volume number stored in main header reserve?
        // For old-style .r00 volumes, numbering is implicit via extension.
        // We'll treat logical index as None and let filename sequence drive it.
        // Check if last volume: old RAR last volume may have flag 0x0002? Not exactly.
        // Use is_last_volume = None for now.
        VolumeProbeResult::MultiVolume(VolumeStructure {
            format: ArchiveFormat::Rar,
            logical_volume_index: None,
            expected_volume_count: None,
            expected_logical_size: None,
            is_last_volume: None,
        })
    } else {
        // Check if file is old-style volume like .r00? Need to see if file extension is numeric?
        // If file ends with .rar and no volume flag, it's standalone.
        // But we still need to handle old-style volumes where main header still has volume flag?
        // Assume standalone if no volume flag.
        // Additionally, if file size is small and we cannot see end header, keep as standalone.
        // To be safe, treat as standalone; resolver will fallback to filename hypotheses if needed.
        // We could also verify by seeking to see if file ends with end header.
        let _ = file.seek(SeekFrom::End(-10)).is_ok();
        VolumeProbeResult::Standalone(ArchiveFormat::Rar)
    }
}
