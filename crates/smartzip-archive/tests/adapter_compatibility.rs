#![cfg(unix)]
use smartzip_archive::{
    ArchiveAdapter, CompressArchiveRequest, ExtractArchiveRequest, ListRequest,
};
use smartzip_core::{EncodingMode, TaskExecutionContext};
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};

fn fake_backend() -> (tempfile::TempDir, PathBuf) {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("backend");
    std::fs::write(&path, r#"#!/bin/sh
printf '%s\n' "$*" >> "$0.log"
case "$1" in
l) printf 'Path = archive.7z\nType = 7z\n\nPath = file.txt\nSize = 1\nPacked Size = 1\nAttributes = A\n\n';;
lb) printf 'file.txt\n';;
lt) printf 'Name: file.txt\nType: File\nSize: 1\n';;
x) printf '50%% 1 - file.txt\rFiles: 1\n';;
a) printf 'Everything is Ok\n';;
esac
"#).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    (root, path)
}

async fn check_compatibility(backend: &dyn ArchiveAdapter, root: &std::path::Path) {
    let list = ListRequest {
        archive: root.join("archive.7z"),
        format: None,
        password: Some("test-value".into()),
        encoding: EncodingMode::Auto,
    };
    let a = backend.list(list.clone()).await.unwrap();
    let log = root.join("backend.log");
    let first = std::fs::read(&log).unwrap();
    std::fs::write(&log, "").unwrap();
    let b = backend
        .list_with_context(list, Arc::new(TaskExecutionContext::detached()))
        .await
        .unwrap();
    assert_eq!(a, b);
    assert_eq!(std::fs::read(&log).unwrap(), first);
    std::fs::write(&log, "").unwrap();
    let request = ExtractArchiveRequest {
        archive: root.join("archive.7z"),
        format: None,
        output_dir: root.join("out"),
        password: Some("test-value".into()),
        encoding: EncodingMode::Auto,
    };
    let a = backend.extract(request.clone()).await.unwrap();
    let first = std::fs::read(&log).unwrap();
    std::fs::write(&log, "").unwrap();
    let b = backend
        .extract_with_context(request, Arc::new(TaskExecutionContext::detached()))
        .await
        .unwrap();
    assert_eq!(a, b);
    assert_eq!(std::fs::read(&log).unwrap(), first);
}

#[tokio::test]
async fn seven_zip_legacy_entries_preserve_commands_results_and_observer_delivery() {
    let (root, executable) = fake_backend();
    let delivered = Arc::new(AtomicUsize::new(0));
    let observed = delivered.clone();
    let backend =
        smartzip_archive::sevenzz::SevenZipBackend::new(executable).with_observer(move |_| {
            observed.fetch_add(1, Ordering::SeqCst);
        });
    check_compatibility(&backend, root.path()).await;
    assert_eq!(delivered.load(Ordering::SeqCst), 2);
    let request = CompressArchiveRequest {
        inputs: vec![root.path().join("file.txt")],
        output: root.path().join("new.7z"),
        format: smartzip_core::ArchiveFormat::SevenZip,
        level: Default::default(),
        password: None,
    };
    std::fs::write(root.path().join("backend.log"), "").unwrap();
    let a = backend.compress(request.clone()).await.unwrap();
    let first = std::fs::read(root.path().join("backend.log")).unwrap();
    std::fs::write(root.path().join("backend.log"), "").unwrap();
    let b = backend
        .compress_with_context(request, Arc::new(TaskExecutionContext::detached()))
        .await
        .unwrap();
    assert_eq!(a, b);
    assert_eq!(
        std::fs::read(root.path().join("backend.log")).unwrap(),
        first
    );
}

#[tokio::test]
async fn unrar_legacy_entries_preserve_commands_and_results() {
    let (root, executable) = fake_backend();
    let backend = smartzip_archive::unrar::UnrarBackend::new(executable);
    check_compatibility(&backend, root.path()).await;
}

#[tokio::test]
async fn probe_preserves_legacy_and_context_diagnostic_differences() {
    use std::os::unix::fs::PermissionsExt;
    let (root, executable) = fake_backend();
    std::fs::write(&executable, "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$0.log\"\nprintf 'ERROR: Wrong password\\n' >&2\nexit 2\n").unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = root.path().join("secret.7z");
    let seven = smartzip_archive::sevenzz::SevenZipBackend::new(executable.clone());
    assert!(seven.probe(&path).await.unwrap().supported);
    let log = root.path().join("backend.log");
    assert!(std::fs::read_to_string(&log)
        .unwrap()
        .contains("-bd -bb1 -sccUTF-8"));
    std::fs::write(&log, "").unwrap();
    assert!(
        seven
            .probe_with_context(&path, Arc::new(TaskExecutionContext::detached()))
            .await
            .unwrap()
            .supported
    );
    assert!(!std::fs::read_to_string(&log).unwrap().contains("-bd"));
    let unrar = smartzip_archive::unrar::UnrarBackend::new(executable);
    assert!(unrar.probe(&path).await.unwrap().supported);
    assert!(
        !unrar
            .probe_with_context(&path, Arc::new(TaskExecutionContext::detached()))
            .await
            .unwrap()
            .supported
    );
}
