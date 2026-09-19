use smartzip_archive::{ArchiveAdapter, ExtractArchiveRequest, ListRequest, SevenZipBackend};
use smartzip_core::{ArchiveFormat, EncodingMode};
use std::{
    io::{Cursor, Write},
    path::Path,
};

fn legacy_zip(path: &Path, name: &[u8], encrypted: bool) {
    let placeholder = "q".repeat(name.len());
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let mut options = zip::write::SimpleFileOptions::default();
    if encrypted {
        options = options.with_aes_encryption(zip::AesMode::Aes256, "fixture-password");
    }
    writer.start_file(&placeholder, options).unwrap();
    writer.write_all(b"verified fixture contents\n").unwrap();
    let mut bytes = writer.finish().unwrap().into_inner();
    let locations: Vec<_> = bytes
        .windows(name.len())
        .enumerate()
        .filter(|(_, window)| *window == placeholder.as_bytes())
        .map(|(i, _)| i)
        .collect();
    assert_eq!(locations.len(), 2);
    for offset in locations {
        bytes[offset..offset + name.len()].copy_from_slice(name);
    }
    std::fs::write(path, bytes).unwrap();
}

fn backend() -> Option<SevenZipBackend> {
    which::which("7zz")
        .or_else(|_| which::which("7z"))
        .ok()
        .map(SevenZipBackend::new)
}

#[tokio::test]
async fn legacy_zip_names_and_contents_match_for_plain_and_encrypted_entries() {
    let Some(backend) = backend() else {
        eprintln!("requires real 7-Zip");
        return;
    };
    for encrypted in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let archive = root.path().join("legacy.zip");
        legacy_zip(&archive, b"\xb2\xe2\xca\xd4\xce\xc4\xbc\xfe.txt", encrypted);
        let encoding = EncodingMode::Override("gbk".into());
        let password = encrypted.then(|| "fixture-password".into());
        let listing = backend
            .list(ListRequest {
                archive: archive.clone(),
                format: Some(ArchiveFormat::Zip),
                password: password.clone(),
                encoding: encoding.clone(),
            })
            .await
            .unwrap();
        assert_eq!(listing.entries[0].path, Path::new("测试文件.txt"));
        let router = smartzip_archive::BackendRouter::from_adapters(vec![
            smartzip_archive::AdapterRegistration::from_adapter(backend.clone(), 10),
        ]);
        let preview = router
            .read_member(
                backend.id(),
                smartzip_archive::MemberReadRequest {
                    archive: archive.clone(),
                    member: "测试文件.txt".into(),
                    password: password.clone(),
                    encoding: encoding.clone(),
                    max_bytes: 1024,
                },
                tokio_util::sync::CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(preview, b"verified fixture contents\n");
        let output_dir = root.path().join("output");
        backend
            .extract(ExtractArchiveRequest {
                archive,
                format: Some(ArchiveFormat::Zip),
                password,
                encoding,
                output_dir: output_dir.clone(),
            })
            .await
            .unwrap();
        assert_eq!(
            std::fs::read(output_dir.join("测试文件.txt")).unwrap(),
            b"verified fixture contents\n"
        );
    }
}

#[tokio::test]
async fn legacy_decoded_traversal_is_rejected_without_creating_output() {
    let Some(backend) = backend() else { return };
    let root = tempfile::tempdir().unwrap();
    let archive = root.path().join("unsafe.zip");
    legacy_zip(&archive, b"../\xb2\xe2\xca\xd4.txt", false);
    let output_dir = root.path().join("output");
    let result = backend
        .extract(ExtractArchiveRequest {
            archive,
            format: Some(ArchiveFormat::Zip),
            password: None,
            encoding: EncodingMode::Override("gbk".into()),
            output_dir,
        })
        .await;
    assert!(matches!(
        result,
        Err(smartzip_core::SmartZipError::UnsafeArchivePath { .. })
    ));
    assert!(!root.path().join("测试.txt").exists());
}
