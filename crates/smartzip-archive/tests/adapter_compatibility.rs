#![cfg(unix)]
use smartzip_archive::{
    ArchiveAdapter, ArchiveExecutor, BackendRouter, ExtractArchiveRequest, ListRequest,
};
use smartzip_config::{AdapterFamily, BackendConfig, BackendInstallation};
use smartzip_core::{
    ArchiveFormat, ArchiveOperation, ArchiveRequirements, EncodingMode, TaskExecutionContext,
};
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
esac
"#).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    (root, path)
}

#[tokio::test]
async fn configured_seven_zip_routes_rar_only_to_installations_with_rar_support() {
    let (legacy_root, legacy) = fake_backend();
    let (_headers_root, headers_only) = fake_backend();
    let (_modern_root, modern) = fake_backend();
    std::fs::write(&legacy, "#!/bin/sh\nprintf '7-Zip 17.05\\nFormats:\\n 0 C   F         7z       7z\\n 0 CK           zip      zip\\n\\nCodecs:\\n 0 ED Rar5\\n'\n").unwrap();
    std::fs::write(&modern, "#!/bin/sh\ncase \"$1\" in\ni) printf '7-Zip 26.04\\nFormats:\\n C...F... 7z 7z\\n ...F.... Rar rar r00\\n ...F.... Rar5 rar r00\\n CK...... zip zip\\n\\nCodecs:\\n D 40305 Rar5\\n';;\nl) printf 'Path = archive.rar\\nType = Rar5\\n\\nPath = payload.txt\\nSize = 1\\n';;\nesac\n").unwrap();
    std::fs::write(&headers_only, "#!/bin/sh\nprintf '7-Zip 26.04\\nFormats:\\n ...F.... Rar5 rar r00\\n CK...... zip zip\\n\\nCodecs:\\n ED 40108 Deflate\\n'\n").unwrap();
    let router = BackendRouter::from_config(&BackendConfig {
        auto_discover: false,
        installations: [
            ("legacy", legacy, 100),
            ("headers-only", headers_only, 50),
            ("modern", modern, 10),
        ]
        .into_iter()
        .map(|(id, executable, priority)| BackendInstallation {
            id: id.into(),
            family: AdapterFamily::SevenZipCli,
            executable,
            declared_version: None,
            enabled: true,
            priority,
        })
        .collect(),
    })
    .unwrap();
    let plan = router.plan(
        ArchiveOperation::Extract,
        Some(&ArchiveFormat::Rar),
        ArchiveRequirements::default(),
    );
    assert_eq!(
        plan.candidates
            .iter()
            .map(|candidate| candidate.adapter_id.as_str())
            .collect::<Vec<_>>(),
        ["modern"]
    );
    let diagnostics = router.diagnostics();
    let legacy_containers = diagnostics[0]["capabilities"]["read_containers"]
        .as_array()
        .unwrap();
    assert!(!legacy_containers.contains(&serde_json::json!("Rar")));
    assert!(legacy_containers.contains(&serde_json::json!("Zip")));
    assert!(!diagnostics[1]["capabilities"]["read_containers"]
        .as_array()
        .unwrap()
        .contains(&serde_json::json!("Rar")));
    let listing = router
        .list(ListRequest {
            archive: legacy_root.path().join("archive.rar"),
            format: Some(ArchiveFormat::Rar),
            password: Some(String::new()),
            encoding: EncodingMode::Auto,
        })
        .await
        .unwrap();
    assert_eq!(listing.format, Some(ArchiveFormat::Rar));
    assert_eq!(listing.entries[0].path, PathBuf::from("payload.txt"));
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
