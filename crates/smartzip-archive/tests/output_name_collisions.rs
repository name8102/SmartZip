use smartzip_archive::{ArchiveAdapter, ExtractArchiveRequest, SevenZipBackend, UnrarBackend};
use smartzip_core::{ArchiveFormat, EncodingMode, SmartZipError};
use std::io::{Cursor, Write};

fn zip(names: &[&str], legacy: bool) -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for name in names.iter().copied().chain(legacy.then_some("qq")) {
        writer
            .start_file(name, zip::write::SimpleFileOptions::default())
            .unwrap();
        writer.write_all(name.as_bytes()).unwrap();
    }
    let mut bytes = writer.finish().unwrap().into_inner();
    if legacy {
        // A separate legacy member exercises the decoded ZIP extraction path;
        // the names under test retain their UTF-8 flags and spelling.
        for magic in [b"PK\x03\x04", b"PK\x01\x02"] {
            let header = bytes
                .windows(4)
                .enumerate()
                .filter(|(_, b)| *b == magic)
                .map(|(i, _)| i)
                .collect::<Vec<_>>();
            for position in header {
                let name = position + if magic == b"PK\x03\x04" { 30 } else { 46 };
                if bytes.get(name..name + 2) == Some(b"qq") {
                    bytes[name..name + 2].copy_from_slice(b"\xb2\xe2");
                }
            }
        }
    }
    bytes
}

fn vint(mut value: u64) -> Vec<u8> {
    let mut result = Vec::new();
    loop {
        let byte = (value & 127) as u8;
        value >>= 7;
        result.push(byte | if value != 0 { 128 } else { 0 });
        if value == 0 {
            return result;
        }
    }
}
fn rar5(names: &[&str]) -> Vec<u8> {
    fn block(archive: &mut Vec<u8>, body: &[u8]) {
        let mut header = vint(body.len() as u64);
        header.extend(body);
        archive.extend(crc32fast::hash(&header).to_le_bytes());
        archive.extend(header);
    }
    let mut archive = b"Rar!\x1a\x07\x01\x00".to_vec();
    block(&mut archive, &[1, 0, 0]);
    for name in names {
        let payload = name.as_bytes();
        let mut body = vec![2, 2]; // File block with a data area.
        body.extend(vint(payload.len() as u64));
        body.push(4); // Unpacked CRC32 present.
        body.extend(vint(payload.len() as u64));
        body.extend(vint(0o100644));
        body.extend(crc32fast::hash(payload).to_le_bytes());
        body.extend([0, 1]); // Stored, Unix host.
        body.extend(vint(name.len() as u64));
        body.extend(name.as_bytes());
        block(&mut archive, &body);
        archive.extend(payload);
    }
    block(&mut archive, &[5, 0, 0]);
    archive
}

async fn check(
    backend: &dyn ArchiveAdapter,
    format: ArchiveFormat,
    bytes: Vec<u8>,
    names: &[&str],
    legacy: bool,
) {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join(format!("source.{}", format.as_str()));
    std::fs::write(&source, &bytes).unwrap();
    let output = root.path().join("out");
    std::fs::create_dir(&output).unwrap();
    std::fs::write(output.join("old.txt"), "old output").unwrap();
    let volume = tempfile::tempdir_in(&output).unwrap();
    std::fs::write(volume.path().join(names[0]), []).unwrap();
    let aliases = volume.path().join(names[1]).exists();
    volume.close().unwrap();
    let result = backend
        .extract(ExtractArchiveRequest {
            archive: source.clone(),
            format: Some(format),
            output_dir: output.clone(),
            password: None,
            encoding: if legacy {
                EncodingMode::Override("gbk".into())
            } else {
                EncodingMode::Auto
            },
        })
        .await;
    assert_eq!(std::fs::read(&source).unwrap(), bytes);
    assert_eq!(
        std::fs::read_to_string(output.join("old.txt")).unwrap(),
        "old output"
    );
    if aliases {
        assert!(
            matches!(result, Err(SmartZipError::UnsafeArchivePath { .. })),
            "{}: {result:?}",
            backend.id()
        );
        assert_eq!(
            std::fs::read_dir(&output).unwrap().count(),
            1,
            "probe or member output left behind"
        );
    } else {
        result.unwrap();
        for name in names {
            assert_eq!(std::fs::read(output.join(name)).unwrap(), name.as_bytes());
        }
        assert!(!std::fs::read_dir(&output).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".smartzip-name-check-")
        }));
    }
}

#[tokio::test]
async fn real_seven_zip_rejects_equivalent_members_before_writing_plain_or_decoded_zip() {
    let mut tested = 0;
    for command in ["7zz", "7z"] {
        let Ok(path) = which::which(command) else {
            continue;
        };
        let backend = SevenZipBackend::new(path);
        for names in [["Readme.txt", "README.txt"], ["é.txt", "e\u{301}.txt"]] {
            for legacy in [false, true] {
                check(
                    &backend,
                    ArchiveFormat::Zip,
                    zip(&names, legacy),
                    &names,
                    legacy,
                )
                .await;
            }
        }
        tested += 1;
    }
    if tested == 0 {
        eprintln!("requires real 7-Zip");
    }
}

#[tokio::test]
async fn real_unrar_rejects_equivalent_rar5_members_before_writing() {
    let Ok(path) = which::which("unrar") else {
        eprintln!("requires real Unrar");
        return;
    };
    let backend = UnrarBackend::new(path);
    for names in [["Readme.txt", "README.txt"], ["é.txt", "e\u{301}.txt"]] {
        check(&backend, ArchiveFormat::Rar, rar5(&names), &names, false).await;
    }
}

#[test]
fn synthetic_rar5_main_header_remains_a_standalone_volume_probe() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("probe.rar");
    std::fs::write(&path, rar5(&["hello.txt"])).unwrap();
    assert_eq!(
        smartzip_archive::volume_probe::probe_volume_structure(&path),
        smartzip_archive::volume_probe::VolumeProbeResult::Standalone(ArchiveFormat::Rar)
    );
}
