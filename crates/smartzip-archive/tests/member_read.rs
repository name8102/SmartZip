use smartzip_archive::{AdapterRegistration, BackendRouter, MemberReadRequest, SevenZipBackend};
use smartzip_core::EncodingMode;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

fn seven_zip() -> Option<PathBuf> {
    which::which("7zz").ok().or_else(|| which::which("7z").ok())
}

fn make_archive(dir: &TempDir, password: Option<&str>) -> Option<(PathBuf, BackendRouter)> {
    let seven_zip = seven_zip()?;
    let text = dir.path().join("text.txt");
    let binary = dir.path().join("data.bin");
    std::fs::write(&text, b"member text\n").unwrap();
    std::fs::write(&binary, (0u8..=255).collect::<Vec<_>>()).unwrap();
    let archive = dir.path().join("sample.zip");
    let mut command = Command::new(&seven_zip);
    command.args(["a", "-tzip", "-y"]);
    if let Some(password) = password {
        command.arg(format!("-p{password}"));
    }
    let status = command
        .arg(&archive)
        .arg(&text)
        .arg(&binary)
        .status()
        .unwrap();
    assert!(status.success(), "7zz failed creating test archive");
    let backend = SevenZipBackend::new(seven_zip).with_id("test-seven");
    Some((
        archive,
        BackendRouter::from_adapters(vec![AdapterRegistration::from_adapter(backend, 10)]),
    ))
}

fn request(
    archive: &Path,
    member: &str,
    password: Option<&str>,
    max_bytes: usize,
) -> MemberReadRequest {
    MemberReadRequest {
        archive: archive.to_path_buf(),
        member: PathBuf::from(member),
        password: password.map(str::to_owned),
        encoding: EncodingMode::Auto,
        max_bytes,
    }
}

#[tokio::test]
async fn reads_single_text_and_binary_member() {
    let dir = tempfile::tempdir().unwrap();
    let Some((archive, router)) = make_archive(&dir, None) else {
        eprintln!("skipping member read acceptance: 7zz/7z not installed");
        return;
    };
    let token = CancellationToken::new();
    let text = router
        .read_member(
            "test-seven",
            request(&archive, "text.txt", None, 8 * 1024 * 1024),
            token.clone(),
        )
        .await
        .unwrap();
    assert_eq!(text, b"member text\n");
    let binary = router
        .read_member(
            "test-seven",
            request(&archive, "data.bin", None, 8 * 1024 * 1024),
            token,
        )
        .await
        .unwrap();
    assert_eq!(binary, (0u8..=255).collect::<Vec<_>>());
}

#[tokio::test]
async fn password_success_and_failure_are_distinguished() {
    let dir = tempfile::tempdir().unwrap();
    let Some((archive, router)) = make_archive(&dir, Some("secret")) else {
        eprintln!("skipping password member read acceptance: 7zz/7z not installed");
        return;
    };
    let good = router
        .read_member(
            "test-seven",
            request(&archive, "text.txt", Some("secret"), 8 * 1024 * 1024),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(good, b"member text\n");
    assert!(router
        .read_member(
            "test-seven",
            request(&archive, "text.txt", Some("wrong"), 8 * 1024 * 1024),
            CancellationToken::new()
        )
        .await
        .is_err());
}

#[tokio::test]
async fn declared_size_limit_and_cancel_are_enforced() {
    let dir = tempfile::tempdir().unwrap();
    let Some((archive, router)) = make_archive(&dir, None) else {
        eprintln!("skipping limit/cancel acceptance: 7zz/7z not installed");
        return;
    };
    let limited = router
        .read_member(
            "test-seven",
            request(&archive, "data.bin", None, 8),
            CancellationToken::new(),
        )
        .await;
    assert!(matches!(
        limited,
        Err(smartzip_core::SmartZipError::ResourceLimit { .. })
    ));

    let token = CancellationToken::new();
    token.cancel();
    let cancelled = router
        .read_member(
            "test-seven",
            request(&archive, "text.txt", None, 8 * 1024 * 1024),
            token,
        )
        .await;
    assert!(matches!(
        cancelled,
        Err(smartzip_core::SmartZipError::Cancelled)
    ));
}

#[tokio::test]
async fn unsafe_member_path_is_rejected_before_backend() {
    let dir = tempfile::tempdir().unwrap();
    let Some((archive, router)) = make_archive(&dir, None) else {
        eprintln!("skipping unsafe member acceptance: 7zz/7z not installed");
        return;
    };
    let result = router
        .read_member(
            "test-seven",
            request(&archive, "../text.txt", None, 8 * 1024 * 1024),
            CancellationToken::new(),
        )
        .await;
    assert!(matches!(
        result,
        Err(smartzip_core::SmartZipError::UnsafeArchivePath { .. })
    ));
}
