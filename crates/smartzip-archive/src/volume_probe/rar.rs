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
        return Some(probe_rar5(&header[..n], path, &mut file));
    }
    if n >= 7 && header[..7] == RAR4_MAGIC[..7] {
        return Some(probe_rar4(&header[..n], path, &mut file));
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
    let header_size = usize::try_from(read_vint(data, &mut pos)?).ok()?;
    let header_end = pos.checked_add(header_size)?;
    let data = data.get(..header_end)?;
    let header_type = read_vint(data, &mut pos)?;
    if header_type != 1 {
        return None;
    }
    let hdr_flags = read_vint(data, &mut pos)?;
    let has_extra = (hdr_flags & 0x01) != 0;
    let extra_size = if has_extra {
        usize::try_from(read_vint(data, &mut pos)?).ok()?
    } else {
        0
    };
    if hdr_flags & 0x02 != 0 {
        let _data_size = read_vint(data, &mut pos)?;
    }
    // Optional extra records follow the main-header body. Never read flags
    // or the volume number from that area, or beyond the declared header.
    let body_end = header_end.checked_sub(extra_size)?;
    let data = data.get(..body_end)?;
    let arc_flags = read_vint(data, &mut pos)?;
    let is_volume = (arc_flags & 0x01) != 0;
    let has_vol_number = (arc_flags & 0x02) != 0;
    let vol_number = if has_vol_number {
        Some(u32::try_from(read_vint(data, &mut pos)?).ok()?)
    } else if is_volume {
        // First volume has no explicit number field; it is logical 0
        Some(0)
    } else {
        None
    };
    Some((is_volume, vol_number))
}

fn read_bytes<'a>(data: &'a [u8], pos: &mut usize, n: usize) -> Option<&'a [u8]> {
    let end = pos.checked_add(n)?;
    let v = data.get(*pos..end)?;
    *pos = end;
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
        if shift == 63 && b > 1 {
            return None;
        }
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
    // header is already truncated to actually read bytes (&header[..n])
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn vint(mut number: u64) -> Vec<u8> {
        let mut bytes = Vec::new();
        loop {
            let byte = (number & 0x7f) as u8;
            number >>= 7;
            bytes.push(byte | if number != 0 { 0x80 } else { 0 });
            if number == 0 {
                return bytes;
            }
        }
    }
    fn header(body: &[u8]) -> Vec<u8> {
        let mut bytes = vec![0; 4];
        bytes.extend(vint(body.len() as u64));
        bytes.extend(body);
        bytes
    }

    #[test]
    fn rar5_bounds_header_extra_vint_and_volume_number() {
        assert_eq!(
            parse_rar5_main_flags(&header(&[1, 0, 0])),
            Some((false, None))
        );
        assert_eq!(
            parse_rar5_main_flags(&header(&[1, 0, 1])),
            Some((true, Some(0)))
        );
        assert_eq!(
            parse_rar5_main_flags(&header(&[1, 1, 2, 3, 7, 0, 0])),
            Some((true, Some(7)))
        );
        assert_eq!(
            parse_rar5_main_flags(&header(&[1, 2, 9, 3, 7])),
            Some((true, Some(7)))
        );
        let mut body = vec![1, 0, 3];
        body.extend(vint(u32::MAX as u64));
        assert_eq!(
            parse_rar5_main_flags(&header(&body)),
            Some((true, Some(u32::MAX)))
        );
        let mut malformed = Vec::new();
        let mut extra = vec![1, 1];
        extra.extend(vint(u64::MAX));
        extra.push(1);
        malformed.push(header(&extra));
        let mut volume = vec![1, 0, 3];
        volume.extend(vint(u32::MAX as u64 + 1));
        malformed.push(header(&volume));
        let mut oversized = vec![0; 4];
        oversized.extend(vint(u64::MAX));
        oversized.extend([1, 0, 1]);
        malformed.push(oversized);
        let mut invalid_vint = vec![0; 4];
        invalid_vint.extend([0xff; 9]);
        invalid_vint.push(2);
        invalid_vint.extend([1, 0, 1]);
        malformed.push(invalid_vint);
        malformed.push(header(&[1, 1, 2, 1]));
        malformed.push(vec![0, 0, 0, 0, 2, 1, 0, 1]); // Flags beyond the declared header.
        for data in malformed {
            assert_eq!(parse_rar5_main_flags(&data), None, "{data:?}");
            let mut file = tempfile::NamedTempFile::new().unwrap();
            file.write_all(RAR5_MAGIC).unwrap();
            file.write_all(&data).unwrap();
            assert!(matches!(
                probe_rar(file.path()),
                Some(VolumeProbeResult::PossiblyMultiVolume(_))
            ));
        }
        let valid = header(&[1, 0, 3, 7]);
        for end in 0..valid.len() {
            assert_eq!(parse_rar5_main_flags(&valid[..end]), None);
        }
    }
}
