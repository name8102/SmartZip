//! Fault cases captured against the original runners before centralization.
use smartzip_core::SmartZipError;
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

fn executable(body: &str) -> (tempfile::TempDir, PathBuf) {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("backend");
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    (root, path)
}
async fn ready(path: &Path) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !path.exists() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("child did not become ready");
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn diagnostic_cancellation_stops_descendants_and_reaps_parent() {
    let (_root, path) = executable(
        "sleep 30 &\nprintf '%s' \"$!\" > \"$0.child\"\nprintf '%s' \"$$\" > \"$0.parent\"\nwait",
    );
    let token = CancellationToken::new();
    let cancelled = token.clone();
    let program = path.clone();
    let pending = tokio::spawn(async move {
        crate::test_output::run(&program, "fault-test", &[], &cancelled).await
    });
    ready(&path.with_extension("parent")).await;
    let parent = std::fs::read_to_string(path.with_extension("parent")).unwrap();
    let child = std::fs::read_to_string(path.with_extension("child")).unwrap();
    token.cancel();
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(5), pending)
            .await
            .unwrap()
            .unwrap(),
        Err(SmartZipError::Cancelled)
    ));
    assert!(!Path::new(&format!("/proc/{parent}")).exists());
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            match std::fs::read_to_string(format!("/proc/{child}/stat")) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
                Ok(stat)
                    if stat
                        .rsplit_once(')')
                        .is_some_and(|(_, rest)| rest.trim_start().starts_with('Z')) =>
                {
                    break;
                }
                _ => tokio::time::sleep(Duration::from_millis(5)).await,
            }
        }
    })
    .await
    .expect("diagnostic descendant still executing after cancellation");
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn diagnostic_cancellation_keeps_group_identity_after_parent_exit() {
    struct CleanupGroup(i32);
    impl Drop for CleanupGroup {
        fn drop(&mut self) {
            unsafe {
                libc::kill(-self.0, libc::SIGKILL);
            }
        }
    }
    let (_root, path) = executable(
        "sleep 30 &\nprintf '%s' \"$!\" > \"$0.child\"\nprintf '%s' \"$$\" > \"$0.parent\"\nexit 0",
    );
    let token = CancellationToken::new();
    let cancelled = token.clone();
    let program = path.clone();
    let pending = tokio::spawn(async move {
        crate::test_output::run(&program, "fault-test", &[], &cancelled).await
    });
    ready(&path.with_extension("parent")).await;
    let parent: i32 = std::fs::read_to_string(path.with_extension("parent"))
        .unwrap()
        .parse()
        .unwrap();
    let _cleanup = CleanupGroup(parent);
    let child = std::fs::read_to_string(path.with_extension("child")).unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while Path::new(&format!("/proc/{parent}")).exists() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("parent did not exit");
    assert!(!pending.is_finished(), "descendant still owns the pipes");
    token.cancel();
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(5), pending)
            .await
            .unwrap()
            .unwrap(),
        Err(SmartZipError::Cancelled)
    ));
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            match std::fs::read_to_string(format!("/proc/{child}/stat")) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
                Ok(stat)
                    if stat
                        .rsplit_once(')')
                        .is_some_and(|(_, rest)| rest.trim_start().starts_with('Z')) =>
                {
                    break;
                }
                _ => tokio::time::sleep(Duration::from_millis(5)).await,
            }
        }
    })
    .await
    .expect("cancel failed to kill descendants after the parent exited");
}
