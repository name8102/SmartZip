//! Real worker/library acceptance. Run with SMARTZIP_7Z_LIBRARY and --include-ignored.
use smartzip_archive::*;
use smartzip_core::{
    ArchiveInput, EncodingMode, OutputBudget, SmartZipError, TaskExecutionContext,
};
use std::{
    io::Write,
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};
use tokio_util::sync::CancellationToken;
fn backend() -> SevenZipBackend {
    SevenZipBackend::locate(&SevenZipLocator::default())
        .expect("set SMARTZIP_7Z_LIBRARY")
        .with_id("native-test")
}
fn context() -> Arc<TaskExecutionContext> {
    Arc::new(TaskExecutionContext::detached())
}
async fn listing(backend: &SevenZipBackend, path: &std::path::Path) -> ArchiveListing {
    backend
        .list_with_context(
            ListRequest {
                archive: path.into(),
                format: None,
                password: None,
                encoding: EncodingMode::Auto,
            },
            context(),
        )
        .await
        .unwrap()
}
fn member(path: PathBuf, entry: EntryRef, extent: ReadExtent) -> MemberReadRequest {
    MemberReadRequest {
        archive: path,
        entry,
        password: None,
        encoding: EncodingMode::Auto,
        extent,
        input: None,
    }
}
fn write_zip(path: &std::path::Path, names: &[(&str, &[u8])]) {
    let mut writer = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
    for (name, bytes) in names {
        writer
            .start_file(*name, zip::write::SimpleFileOptions::default())
            .unwrap();
        writer.write_all(bytes).unwrap();
    }
    writer.finish().unwrap();
}
#[tokio::test]
#[ignore = "real native runtime"]
async fn indexed_read_prefix_selection_snapshot_and_embedded_source() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("input.zip");
    write_zip(
        &path,
        &[
            ("@a*\n.txt", b"first"),
            ("second.txt", b"second payload"),
            ("empty", b""),
        ],
    );
    let b = backend();
    let list = listing(&b, &path).await;
    assert_eq!(list.entries[0].path.to_str().unwrap(), "@a*\n.txt");
    let reference = list.entries[1].native.as_ref().unwrap().reference.clone();
    let prefix = b
        .read_member_result(
            member(
                path.clone(),
                reference.clone(),
                ReadExtent::Prefix { bytes: 6 },
            ),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(prefix.bytes, b"second");
    assert!(!prefix.complete);
    let full = b
        .read_member_result(
            member(
                path.clone(),
                reference.clone(),
                ReadExtent::Complete { max_bytes: 0 },
            ),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(full.bytes, b"second payload");
    assert!(full.complete);
    let output = root.path().join("selected");
    let extracted = b
        .extract_with_context(
            ExtractArchiveRequest {
                archive: path.clone(),
                selection: Some(vec![reference.clone()]),
                format: None,
                output_dir: output.clone(),
                password: None,
                encoding: EncodingMode::Auto,
            },
            context(),
        )
        .await
        .unwrap();
    assert_eq!(extracted.manifest.unwrap().files.len(), 1);
    assert_eq!(
        std::fs::read(output.join("second.txt")).unwrap(),
        b"second payload"
    );
    assert!(!output.join("empty").exists());
    let payload = std::fs::read(&path).unwrap();
    let carrier = root.path().join("carrier.bin");
    let mut bytes = vec![0x55; 12345];
    bytes.extend_from_slice(&payload);
    bytes.extend_from_slice(b"not archive data");
    std::fs::write(&carrier, bytes).unwrap();
    let input = ArchiveInput {
        path: carrier.clone(),
        offset: 12345,
        length: Some(payload.len() as u64),
        volumes: Default::default(),
    };
    let embedded = b
        .list_with_context(
            ListRequest {
                archive: carrier.clone(),
                format: None,
                password: None,
                encoding: EncodingMode::Auto,
            },
            Arc::new(TaskExecutionContext::detached().with_archive_input(input.clone())),
        )
        .await
        .unwrap();
    let mut request = member(
        carrier,
        embedded.entries[0]
            .native
            .as_ref()
            .unwrap()
            .reference
            .clone(),
        ReadExtent::Complete { max_bytes: 0 },
    );
    request.input = Some(input);
    assert_eq!(
        b.read_member_result(request, CancellationToken::new())
            .await
            .unwrap()
            .bytes,
        b"first"
    );
    std::fs::write(&path, b"changed source").unwrap();
    assert!(b
        .read_member_result(
            member(path, reference, ReadExtent::Complete { max_bytes: 0 }),
            CancellationToken::new()
        )
        .await
        .is_err());
}
#[tokio::test]
#[ignore = "real native runtime"]
async fn duplicate_names_are_independently_readable_and_safely_materialized() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("duplicate.zip");
    // ZipWriter intentionally rejects duplicate names. This checked fixture was made for ABI acceptance.
    std::fs::copy(
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../smartzip-sevenzip/tests/fixtures/plain.zip"
        ),
        &path,
    )
    .unwrap();
    let b = backend();
    let list = listing(&b, &path).await;
    let mut same = std::collections::BTreeMap::<_, Vec<_>>::new();
    for e in &list.entries {
        same.entry(e.path.clone()).or_default().push(e);
    }
    let duplicate = same
        .values()
        .find(|v| v.len() > 1)
        .expect("duplicate fixture");
    let mut contents = Vec::new();
    for e in duplicate {
        contents.push(
            b.read_member_result(
                member(
                    path.clone(),
                    e.native.as_ref().unwrap().reference.clone(),
                    ReadExtent::Complete { max_bytes: 0 },
                ),
                CancellationToken::new(),
            )
            .await
            .unwrap()
            .bytes,
        );
    }
    assert_ne!(contents[0], contents[1]);
    let output = root.path().join("output");
    let extracted = b
        .extract_with_context(
            ExtractArchiveRequest {
                archive: path,
                selection: None,
                format: None,
                output_dir: output.clone(),
                password: None,
                encoding: EncodingMode::Auto,
            },
            context(),
        )
        .await
        .unwrap();
    assert!(
        extracted.manifest.unwrap().files.len()
            >= list
                .entries
                .iter()
                .filter(|e| !e.is_dir && e.native.as_ref().unwrap().kind == EntryKind::File)
                .count()
    );
}
struct Budget(AtomicU64);
impl OutputBudget for Budget {
    fn reserve(&self, _: u64, bytes: u64) -> smartzip_core::Result<()> {
        let old = self.0.fetch_add(bytes, Ordering::SeqCst);
        if old + bytes > 3 {
            Err(SmartZipError::ResourceLimit {
                detail: "test budget".into(),
            })
        } else {
            Ok(())
        }
    }
}
#[tokio::test]
#[ignore = "real native runtime"]
async fn write_budget_precedes_disk_output_and_cancel_returns_without_writers() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("budget.zip");
    write_zip(&path, &[("data", b"0123456789")]);
    let b = backend();
    let out = root.path().join("output");
    let result = b
        .extract_with_context(
            ExtractArchiveRequest {
                archive: path.clone(),
                selection: None,
                format: None,
                output_dir: out.clone(),
                password: None,
                encoding: EncodingMode::Auto,
            },
            Arc::new(
                TaskExecutionContext::detached()
                    .with_output_budget(Arc::new(Budget(AtomicU64::new(0)))),
            ),
        )
        .await;
    assert!(matches!(result, Err(SmartZipError::ResourceLimit { .. })));
    assert_eq!(std::fs::metadata(out.join("data")).unwrap().len(), 0);
    let token = CancellationToken::new();
    token.cancel();
    let result = b
        .list_with_context(
            ListRequest {
                archive: path,
                format: None,
                password: None,
                encoding: EncodingMode::Auto,
            },
            Arc::new(TaskExecutionContext::detached().with_cancellation(token)),
        )
        .await;
    assert!(matches!(result, Err(SmartZipError::Cancelled)));
}

#[tokio::test]
#[ignore = "real native runtime"]
async fn native_test_evidence_and_stream_output_names() {
    use smartzip_archive::integrity::{BackendDiagnosticDetail, TestFailure};
    let root = tempfile::tempdir().unwrap();
    let b = backend();
    let fixtures =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../smartzip-sevenzip/tests/fixtures");
    for (file, password, expected) in [
        ("headers.7z", None, Some(TestFailure::PasswordRequired)),
        (
            "headers.7z",
            Some("wrong"),
            Some(TestFailure::PasswordIndeterminate),
        ),
        (
            "encrypted.7z",
            Some("wrong"),
            Some(TestFailure::PasswordIndeterminate),
        ),
        ("corrupt.zip", None, Some(TestFailure::Corruption)),
        ("plain.zip", None, None),
    ] {
        let result = b
            .test_with_context(
                TestRequest {
                    archive: fixtures.join(file),
                    format: None,
                    password: password.map(str::to_owned),
                    encoding: EncodingMode::Auto,
                },
                context(),
            )
            .await
            .unwrap();
        assert_eq!(result.diagnostics.failure, expected, "{file}");
        assert!(matches!(
            result.diagnostics.detail,
            BackendDiagnosticDetail::Native { .. }
        ));
        assert_eq!(result.ok, expected.is_none());
    }
    // A valid encrypted member does not turn a different plaintext CRC failure
    // into a password challenge.
    let mixed = root.path().join("mixed.zip");
    let mut writer = zip::ZipWriter::new(std::fs::File::create(&mixed).unwrap());
    writer
        .start_file(
            "encrypted",
            zip::write::SimpleFileOptions::default()
                .with_aes_encryption(zip::AesMode::Aes256, "secret"),
        )
        .unwrap();
    writer.write_all(b"encrypted valid").unwrap();
    writer
        .start_file(
            "plain",
            zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored),
        )
        .unwrap();
    writer.write_all(b"plain-checksum").unwrap();
    writer.finish().unwrap();
    let mut bytes = std::fs::read(&mixed).unwrap();
    let offset = bytes
        .windows(14)
        .position(|w| w == b"plain-checksum")
        .unwrap();
    bytes[offset] ^= 1;
    std::fs::write(&mixed, bytes).unwrap();
    let result = b
        .test_with_context(
            TestRequest {
                archive: mixed,
                format: None,
                password: Some("secret".into()),
                encoding: EncodingMode::Auto,
            },
            context(),
        )
        .await
        .unwrap();
    assert_eq!(result.diagnostics.failure, Some(TestFailure::Corruption));
    for file in ["plain.gz", "plain.xz"] {
        let output = root.path().join(file);
        let result = b
            .extract_with_context(
                ExtractArchiveRequest {
                    archive: fixtures.join(file),
                    selection: None,
                    format: None,
                    output_dir: output.clone(),
                    password: None,
                    encoding: EncodingMode::Auto,
                },
                context(),
            )
            .await
            .unwrap();
        let manifest = result.manifest.unwrap();
        assert_eq!(manifest.files.len(), 1);
        assert_eq!(manifest.files[0].relative_path, PathBuf::from("plain"));
        assert!(output.join("plain").is_file());
    }
}
