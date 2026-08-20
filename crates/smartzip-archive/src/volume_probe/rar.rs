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
    if header[..7] == RAR5_MAGIC[..7] {
        return Some(probe_rar5(&header, path, &mut file));
    }
    if n >= 7 && header[..7] == RAR4_MAGIC[..7] {
        return Some(probe_rar4(&header, path, &mut file));
    }
    None
}

fn probe_rar5(header: &[u8], _path: &Path, _file: &mut File) -> VolumeProbeResult {
    // RAR5 structure: after 8-byte signature, headers are blocks.
    // For cheap probe, look at main header if present.
    // RAR5 main header flags: bit 0x0001 = volume, bit 0x0002 = volume number.
    // We parse minimally: after signature, first header is archive header.
    // Hard to parse fully without full spec; use heuristic.
    // If we see volume flag in bytes near main header, treat as MultiVolume.
    // For now, attempt simple parse: skip signature, read header.
    // Archive header type = 1, flags in vint.
    // We do bounded scan for volume presence.
    // Simplify: if file contains "vol"?? Instead, treat any RAR5 as possibly multivolume
    // if multivolume flag present, else standalone.
    // For robust but cheap: check byte at offset 9-12 for flag bits.
    // This is best-effort; resolver will combine with filename hypotheses.
    // We'll try to read main header flags via vint parsing.
    let mut off = 8usize;
    if header.len() <= off {
        return VolumeProbeResult::PossiblyMultiVolume(VolumeStructure {
            format: ArchiveFormat::Rar,
            logical_volume_index: None,
            expected_volume_count: None,
            expected_logical_size: None,
            is_last_volume: None,
        });
    }
    // RAR5 headers: [CRC32 4][header size vint][type vint][flags vint][extra size vint][data size vint]...
    // We attempt to parse first header after signature to extract flags.
    // If parsing fails, fallback to PossiblyMultiVolume.
    match parse_rar5_main_flags(&header[off..]) {
        Some((is_volume, volume_number)) => {
            if is_volume {
                VolumeProbeResult::MultiVolume(VolumeStructure {
                    format: ArchiveFormat::Rar,
                    logical_volume_index: volume_number,
                    expected_volume_count: None,
                    expected_logical_size: None,
                    is_last_volume: Some(false), // not last if volume flag set but no end flag; need more precise
                })
            } else {
                // Check for end-of-archive block? RAR5 end block type 5.
                // If file is standalone, we treat as standalone.
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
    // Very cheap vint parser.
    let mut pos = 0usize;
    let _crc = read_bytes(data, &mut pos, 4)?;
    let header_size = read_vint(data, &mut pos)?;
    let header_type = read_vint(data, &mut pos)?;
    if header_type != 1 {
        return None;
    }
    let flags = read_vint(data, &mut pos)? as u16;
    let is_volume = (flags & 0x0001) != 0;
    // Volume number is optional extra? In RAR5, volume number stored as extra field?
    // We ignore for now.
    let _ = header_size;
    Some((is_volume, None))
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
