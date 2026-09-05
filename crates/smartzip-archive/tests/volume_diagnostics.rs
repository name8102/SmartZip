use smartzip_archive::diagnostic::{inspect, DiagnosticControl};
use smartzip_archive::integrity::{EvidenceKind, EvidenceStrength};
use smartzip_archive::volumes::VolumeSet;
use std::fs;
use std::path::{Path, PathBuf};

fn vint(mut value: u64) -> Vec<u8> {
    let mut bytes = Vec::new();
    loop {
        let byte = (value & 127) as u8;
        value >>= 7;
        bytes.push(byte | if value == 0 { 0 } else { 128 });
        if value == 0 {
            return bytes;
        }
    }
}
fn rar_block(body: &[u8]) -> Vec<u8> {
    let mut data = vint(body.len() as u64);
    data.extend(body);
    let mut result = crc32fast::hash(&data).to_le_bytes().to_vec();
    result.extend(data);
    result
}
fn rar_set(dir: &Path) -> Vec<PathBuf> {
    let parts = [vec![0x41; 100], vec![0x42; 110], vec![0x43; 70]];
    let entire: Vec<_> = parts.iter().flatten().copied().collect();
    let mut paths = Vec::new();
    for (index, part) in parts.iter().enumerate() {
        let mut bytes = b"Rar!\x1a\x07\x01\x00".to_vec();
        let mut main = vec![1, 0, if index == 0 { 1 } else { 3 }];
        if index > 0 {
            main.extend(vint(index as u64));
        }
        bytes.extend(rar_block(&main));
        let last = index == parts.len() - 1;
        let mut file = vec![
            2,
            2 | if index > 0 { 8 } else { 0 } | if last { 0 } else { 16 },
        ];
        file.extend(vint(part.len() as u64));
        file.push(4); // file CRC present
        file.extend(vint(entire.len() as u64));
        file.push(0); // attributes
        let crc = if last {
            crc32fast::hash(&entire)
        } else {
            crc32fast::hash(part)
        };
        file.extend(crc.to_le_bytes());
        file.extend([0, 1, 5]); // stored, Unix, filename length
        file.extend(b"a.bin");
        bytes.extend(rar_block(&file));
        bytes.extend(part);
        bytes.extend(rar_block(&[5, 0, if last { 0 } else { 1 }]));
        let path = dir.join(format!("set.part{:02}.rar", index + 1));
        fs::write(&path, bytes).unwrap();
        paths.push(path);
    }
    paths
}
fn flip(path: &Path, offset: usize) {
    let mut data = fs::read(path).unwrap();
    data[offset] ^= 0x55;
    fs::write(path, data).unwrap();
}
fn confirmed(report: &smartzip_archive::diagnostic::DiagnosticReport) -> Vec<PathBuf> {
    let mut paths: Vec<_> = report
        .evidence
        .iter()
        .filter(|e| e.strength == EvidenceStrength::Confirmed)
        .flat_map(|e| e.ranges.iter().map(|r| r.volume.clone()))
        .collect();
    paths.sort();
    paths.dedup();
    paths
}

#[test]
fn rar5_checks_independent_segments_and_retains_missing_and_damage_separately() {
    let dir = tempfile::tempdir().unwrap();
    let paths = rar_set(dir.path());
    let set = VolumeSet::collect(&paths[1]).unwrap();
    let clean = inspect(&set, &[], &DiagnosticControl::default(), 1);
    assert!(clean.evidence.is_empty(), "{clean:?}");
    assert!(clean.stop_reasons.is_empty(), "{clean:?}");
    flip(&paths[0], 75);
    flip(&paths[1], 80);
    let result = inspect(
        &VolumeSet::collect(&paths[2]).unwrap(),
        &[],
        &DiagnosticControl::default(),
        2,
    );
    assert_eq!(confirmed(&result), paths[..2]);
    assert!(result
        .evidence
        .iter()
        .all(|e| e.kind == EvidenceKind::PackedChecksum));
    fs::remove_file(&paths[0]).unwrap();
    let result = inspect(
        &VolumeSet::collect(&paths[1]).unwrap(),
        &[],
        &DiagnosticControl::default(),
        3,
    );
    assert_eq!(confirmed(&result), vec![paths[1].clone()]);
    assert!(VolumeSet::collect(&paths[1])
        .unwrap()
        .missing
        .contains(&paths[0]));
}

#[test]
fn rar5_final_crc_is_not_a_local_packed_crc_and_truncation_is_local() {
    let dir = tempfile::tempdir().unwrap();
    let paths = rar_set(dir.path());
    flip(&paths[2], 60);
    let result = inspect(
        &VolumeSet::collect(&paths[2]).unwrap(),
        &[],
        &DiagnosticControl::default(),
        1,
    );
    assert!(
        confirmed(&result).is_empty(),
        "final CRC covers the whole file: {result:?}"
    );
    let data = fs::read(&paths[2]).unwrap();
    fs::write(&paths[2], &data[..data.len() - 25]).unwrap();
    let result = inspect(
        &VolumeSet::collect(&paths[2]).unwrap(),
        &[],
        &DiagnosticControl::default(),
        2,
    );
    assert_eq!(confirmed(&result), vec![paths[2].clone()]);
    assert!(result
        .evidence
        .iter()
        .any(|e| e.kind == EvidenceKind::StructuralTruncation));
}

#[test]
fn rar5_bad_header_stops_that_chain_but_checks_other_volumes_and_missing_tail() {
    let dir = tempfile::tempdir().unwrap();
    let paths = rar_set(dir.path());
    flip(&paths[0], 12);
    flip(&paths[1], 80);
    fs::remove_file(&paths[2]).unwrap();
    let result = inspect(
        &VolumeSet::collect(&paths[1]).unwrap(),
        &[],
        &DiagnosticControl::default(),
        1,
    );
    assert_eq!(confirmed(&result), paths[..2]);
    assert!(result.missing.contains(&paths[2]));
    assert!(result
        .evidence
        .iter()
        .any(|e| e.kind == EvidenceKind::HeaderChecksum));
}

fn u16v(bytes: &mut Vec<u8>, v: u16) {
    bytes.extend(v.to_le_bytes());
}
fn u32v(bytes: &mut Vec<u8>, v: u32) {
    bytes.extend(v.to_le_bytes());
}
fn local(name: &[u8], data: &[u8]) -> Vec<u8> {
    let mut b = b"PK\x03\x04".to_vec();
    for v in [20, 0, 0, 0, 0] {
        u16v(&mut b, v);
    }
    u32v(&mut b, crc32fast::hash(data));
    u32v(&mut b, data.len() as u32);
    u32v(&mut b, data.len() as u32);
    u16v(&mut b, name.len() as u16);
    u16v(&mut b, 0);
    b.extend(name);
    b.extend(data);
    b
}
fn zip_set(dir: &Path) -> Vec<PathBuf> {
    let data = [vec![0x41; 80], vec![0x42; 80]];
    let mut bytes = local(b"a.bin", &data[0]);
    let second = bytes.len();
    bytes.extend(local(b"b.bin", &data[1]));
    let cd = bytes.len();
    let cuts = [0, 64, 130, cd];
    for (index, name) in [b"a.bin", b"b.bin"].iter().enumerate() {
        bytes.extend(b"PK\x01\x02");
        for v in [20, 20, 0, 0, 0, 0] {
            u16v(&mut bytes, v);
        }
        u32v(&mut bytes, crc32fast::hash(&data[index]));
        u32v(&mut bytes, 80);
        u32v(&mut bytes, 80);
        for v in [5, 0, 0] {
            u16v(&mut bytes, v);
        }
        let pos = if index == 0 { 0 } else { second };
        let disk = cuts.iter().rposition(|cut| *cut <= pos).unwrap();
        u16v(&mut bytes, disk as u16);
        u16v(&mut bytes, 0);
        u32v(&mut bytes, 0);
        u32v(&mut bytes, (pos - cuts[disk]) as u32);
        bytes.extend(*name);
    }
    let cd_size = bytes.len() - cd;
    bytes.extend(b"PK\x05\x06");
    for v in [3, 3, 2, 2] {
        u16v(&mut bytes, v);
    }
    u32v(&mut bytes, cd_size as u32);
    u32v(&mut bytes, 0);
    u16v(&mut bytes, 0);
    let mut paths = Vec::new();
    for i in 0..4 {
        let path = dir.join(if i == 3 {
            "set.zip".into()
        } else {
            format!("set.z{:02}", i + 1)
        });
        fs::write(
            &path,
            &bytes[cuts[i]..if i == 3 { bytes.len() } else { cuts[i + 1] }],
        )
        .unwrap();
        paths.push(path);
    }
    paths
}

#[test]
fn zip_candidates_include_crc_metadata_and_do_not_intersect_two_faults() {
    let dir = tempfile::tempdir().unwrap();
    let paths = zip_set(dir.path());
    let set = VolumeSet::collect(&paths[1]).unwrap();
    let clean = inspect(&set, &[], &DiagnosticControl::default(), 1);
    assert!(clean.evidence.is_empty(), "{clean:?}");
    assert!(clean.stop_reasons.is_empty(), "{clean:?}");
    flip(&paths[1], 10);
    flip(&paths[2], 30);
    let report = inspect(
        &VolumeSet::collect(&paths[2]).unwrap(),
        &[],
        &DiagnosticControl::default(),
        2,
    );
    assert!(confirmed(&report).is_empty());
    assert_eq!(report.evidence.len(), 2, "{report:?}");
    for evidence in &report.evidence {
        assert!(evidence
            .reference_ranges
            .iter()
            .any(|r| r.volume == paths[3]));
    }
    let groups: Vec<Vec<_>> = report
        .evidence
        .iter()
        .map(|e| e.ranges.iter().map(|r| r.volume.clone()).collect())
        .collect();
    assert!(groups[0].contains(&paths[1]));
    assert!(!groups[0].contains(&paths[2]));
    assert!(groups[1].contains(&paths[2]));
}

#[test]
fn zip_missing_disk_still_allows_independent_existing_entry_check() {
    let dir = tempfile::tempdir().unwrap();
    let paths = zip_set(dir.path());
    fs::remove_file(&paths[0]).unwrap();
    flip(&paths[2], 30);
    let result = inspect(
        &VolumeSet::collect(&paths[1]).unwrap(),
        &[],
        &DiagnosticControl::default(),
        1,
    );
    assert!(result.missing.contains(&paths[0]));
    assert!(confirmed(&result).is_empty());
    assert!(result
        .evidence
        .iter()
        .any(|e| e.affected_entries == ["b.bin"]));
}

#[test]
fn expired_diagnostic_budget_keeps_no_fabricated_checks() {
    let dir = tempfile::tempdir().unwrap();
    let paths = rar_set(dir.path());
    let mut control = DiagnosticControl::default();
    control.deadline = Some(std::time::Instant::now());
    let result = inspect(&VolumeSet::collect(&paths[0]).unwrap(), &[], &control, 1);
    assert!(result.evidence.is_empty());
    assert!(result.checked_scopes.is_empty());
    assert!(result.stop_reasons.iter().any(|s| s.contains("timeout")));
}

fn sevenz_stream_set(dir: &Path, pack_crcs: bool) -> Vec<PathBuf> {
    // CRC-valid BCJ2 folder metadata with four packed inputs. This fixture
    // tests dependency mapping only; its payload is not a decoded BCJ2 file.
    let payload: Vec<u8> = (0..64).collect();
    let mut header = vec![1, 4, 6, 0, 4, 9, 16, 16, 16, 16];
    if pack_crcs {
        header.extend([10, 1]);
        for part in payload.chunks(16) {
            header.extend(crc32fast::hash(part).to_le_bytes());
        }
    }
    header.extend([0, 7, 11, 1, 0]);
    header.extend([1, 0x14, 3, 3, 1, 0x1b, 4, 1, 0, 1, 2, 3]);
    header.extend([12, 64, 0, 0, 5, 1, 17, 13, 0]);
    for ch in "x.bin\0".encode_utf16() {
        header.extend(ch.to_le_bytes());
    }
    header.extend([0, 0]);
    let mut start = 64u64.to_le_bytes().to_vec();
    start.extend((header.len() as u64).to_le_bytes());
    start.extend(crc32fast::hash(&header).to_le_bytes());
    let mut bytes = b"7z\xbc\xaf\x27\x1c\x00\x04".to_vec();
    bytes.extend(crc32fast::hash(&start).to_le_bytes());
    bytes.extend(start);
    bytes.extend(payload);
    bytes.extend(header);
    let cuts = [0, 48, 64, 80, 96];
    let mut paths = Vec::new();
    for i in 0..5 {
        let path = dir.join(format!("set.7z.{:03}", i + 1));
        fs::write(
            &path,
            &bytes[cuts[i]..if i == 4 { bytes.len() } else { cuts[i + 1] }],
        )
        .unwrap();
        paths.push(path);
    }
    paths
}

#[test]
fn sevenz_multistream_folder_keeps_every_packed_input() {
    let dir = tempfile::tempdir().unwrap();
    let paths = sevenz_stream_set(dir.path(), false);
    let result = inspect(
        &VolumeSet::collect(&paths[2]).unwrap(),
        &["x.bin".into()],
        &DiagnosticControl::default(),
        1,
    );
    assert!(result.stop_reasons.is_empty(), "{result:?}");
    assert!(confirmed(&result).is_empty());
    let evidence = result
        .evidence
        .iter()
        .find(|e| e.kind == EvidenceKind::EntryChecksum)
        .unwrap();
    let ranges: Vec<_> = evidence.ranges.iter().map(|r| r.volume.clone()).collect();
    assert_eq!(ranges, paths[..4]);
}

#[test]
fn sevenz_verified_packed_crc_confirms_only_the_modified_volume() {
    let dir = tempfile::tempdir().unwrap();
    let paths = sevenz_stream_set(dir.path(), true);
    let clean = inspect(
        &VolumeSet::collect(&paths[0]).unwrap(),
        &[],
        &DiagnosticControl::default(),
        1,
    );
    assert!(clean.evidence.is_empty(), "{clean:?}");
    assert!(clean.stop_reasons.is_empty(), "{clean:?}");
    flip(&paths[1], 5);
    flip(&paths[3], 10);
    let result = inspect(
        &VolumeSet::collect(&paths[2]).unwrap(),
        &[],
        &DiagnosticControl::default(),
        2,
    );
    assert_eq!(confirmed(&result), vec![paths[1].clone(), paths[3].clone()]);
}

#[test]
fn shortened_early_sevenz_member_does_not_blame_a_healthy_tail() {
    let dir = tempfile::tempdir().unwrap();
    let paths = sevenz_stream_set(dir.path(), true);
    let bytes = fs::read(&paths[1]).unwrap();
    fs::write(&paths[1], &bytes[..bytes.len() - 3]).unwrap();
    let result = inspect(
        &VolumeSet::collect(&paths[2]).unwrap(),
        &[],
        &DiagnosticControl::default(),
        1,
    );
    assert!(confirmed(&result).is_empty());
    assert!(result
        .evidence
        .iter()
        .any(|e| e.ranges.iter().any(|r| r.volume == paths[1])));
}

#[test]
fn zip64_split_descriptor_keeps_data_and_reference_volumes_as_candidates() {
    let dir = tempfile::tempdir().unwrap();
    let data = vec![0x61; 80];
    let crc = crc32fast::hash(&data);
    let mut bytes = b"PK\x03\x04".to_vec();
    for value in [45, 8, 0, 0, 0] {
        u16v(&mut bytes, value);
    }
    u32v(&mut bytes, 0);
    u32v(&mut bytes, u32::MAX);
    u32v(&mut bytes, u32::MAX);
    u16v(&mut bytes, 1);
    u16v(&mut bytes, 20);
    bytes.push(b'x');
    u16v(&mut bytes, 1);
    u16v(&mut bytes, 16);
    bytes.extend(80u64.to_le_bytes());
    bytes.extend(80u64.to_le_bytes());
    bytes.extend(&data);
    let final_start = bytes.len();
    bytes.extend(b"PK\x07\x08");
    u32v(&mut bytes, crc);
    bytes.extend(80u64.to_le_bytes());
    bytes.extend(80u64.to_le_bytes());
    let cd_start = bytes.len();
    bytes.extend(b"PK\x01\x02");
    for value in [45, 45, 8, 0, 0, 0] {
        u16v(&mut bytes, value);
    }
    u32v(&mut bytes, crc);
    u32v(&mut bytes, u32::MAX);
    u32v(&mut bytes, u32::MAX);
    for value in [1, 32, 0, u16::MAX, 0] {
        u16v(&mut bytes, value);
    }
    u32v(&mut bytes, 0);
    u32v(&mut bytes, u32::MAX);
    bytes.push(b'x');
    u16v(&mut bytes, 1);
    u16v(&mut bytes, 28);
    bytes.extend(80u64.to_le_bytes());
    bytes.extend(80u64.to_le_bytes());
    bytes.extend(0u64.to_le_bytes());
    u32v(&mut bytes, 0);
    let cd_size = bytes.len() - cd_start;
    let zip64_offset = bytes.len() - final_start;
    bytes.extend(b"PK\x06\x06");
    bytes.extend(44u64.to_le_bytes());
    u16v(&mut bytes, 45);
    u16v(&mut bytes, 45);
    u32v(&mut bytes, 2);
    u32v(&mut bytes, 2);
    bytes.extend(1u64.to_le_bytes());
    bytes.extend(1u64.to_le_bytes());
    bytes.extend((cd_size as u64).to_le_bytes());
    bytes.extend(((cd_start - final_start) as u64).to_le_bytes());
    bytes.extend(b"PK\x06\x07");
    u32v(&mut bytes, 2);
    bytes.extend((zip64_offset as u64).to_le_bytes());
    u32v(&mut bytes, 3);
    bytes.extend(b"PK\x05\x06");
    for _ in 0..4 {
        u16v(&mut bytes, u16::MAX);
    }
    u32v(&mut bytes, u32::MAX);
    u32v(&mut bytes, u32::MAX);
    u16v(&mut bytes, 0);
    let paths: Vec<_> = ["set.z01", "set.z02", "set.zip"]
        .iter()
        .map(|name| dir.path().join(name))
        .collect();
    let cuts = [0, 64, final_start, bytes.len()];
    for (index, path) in paths.iter().enumerate() {
        fs::write(path, &bytes[cuts[index]..cuts[index + 1]]).unwrap();
    }
    let run = || {
        inspect(
            &VolumeSet::collect(&paths[1]).unwrap(),
            &[],
            &DiagnosticControl::default(),
            1,
        )
    };
    let clean = run();
    assert!(clean.evidence.is_empty(), "{clean:?}");
    assert!(clean.stop_reasons.is_empty(), "{clean:?}");
    assert!(clean.checked_scopes.iter().any(|scope| scope.passed));
    flip(&paths[1], 4);
    let damage = run();
    assert_eq!(damage.evidence.len(), 1, "{damage:?}");
    assert!(confirmed(&damage).is_empty());
    assert!(damage.evidence[0]
        .ranges
        .iter()
        .any(|range| range.volume == paths[1]));
    assert!(damage.evidence[0]
        .reference_ranges
        .iter()
        .any(|range| range.volume == paths[2]));
    flip(&paths[1], 4);
    flip(&paths[2], 4);
    let descriptor_damage = run();
    assert_eq!(descriptor_damage.evidence.len(), 1, "{descriptor_damage:?}");
    assert!(confirmed(&descriptor_damage).is_empty());
    assert!(descriptor_damage.evidence[0].summary.contains("descriptor"));
}
