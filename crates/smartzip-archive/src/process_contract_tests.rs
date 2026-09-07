//! Fault cases captured against the original runners before centralization.
use crate::{ArchiveAdapter, ListRequest, SevenZipBackend, TestRequest};
use smartzip_core::{EncodingMode, SmartZipError, TaskExecutionContext};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
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
fn list() -> ListRequest {
    ListRequest {
        archive: "fixture.7z".into(),
        format: None,
        password: None,
        encoding: EncodingMode::Auto,
    }
}
fn test() -> TestRequest {
    TestRequest {
        archive: "fixture.7z".into(),
        format: None,
        password: None,
        encoding: EncodingMode::Auto,
    }
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

#[tokio::test]
async fn process_output_limits_preserve_three_distinct_contracts() {
    let (_root, path) = executable("dd if=/dev/zero bs=1048576 count=17 2>/dev/null &\n(dd if=/dev/zero bs=1048576 count=17 2>/dev/null) >&2\nwait");
    let backend = SevenZipBackend::new(path.clone());
    let ordinary = tokio::time::timeout(Duration::from_secs(15), backend.list(list()))
        .await
        .unwrap();
    assert!(matches!(ordinary, Err(SmartZipError::ResourceLimit { .. })));
    let streaming = tokio::time::timeout(Duration::from_secs(15), backend.test_with_report(test()))
        .await
        .unwrap()
        .unwrap();
    assert!(streaming.value.is_some());
    assert_eq!(streaming.stdout.len(), crate::process::MAX_OUTPUT);
    assert_eq!(streaming.stderr.len(), crate::process::MAX_OUTPUT);
    let (diagnostic, truncated) = tokio::time::timeout(
        Duration::from_secs(15),
        crate::test_output::run(&path, "fault-test", &[], &CancellationToken::new()),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(truncated);
    assert_eq!(diagnostic.status, Some(0));
    assert_eq!(diagnostic.stdout.len(), crate::process::MAX_OUTPUT);
    assert_eq!(diagnostic.stderr.len(), crate::process::MAX_OUTPUT);
}

#[tokio::test]
async fn process_start_errors_preserve_backend_identity_and_io_path() {
    let root = tempfile::tempdir().unwrap();
    let missing = root.path().join("missing");
    let error = SevenZipBackend::new(missing.clone())
        .with_id("selected")
        .list(list())
        .await
        .unwrap_err();
    assert!(
        matches!(error, SmartZipError::BackendUnavailable { backend } if backend == "selected")
    );
    let error = crate::test_output::run(&missing, "selected", &[], &CancellationToken::new())
        .await
        .unwrap_err();
    assert!(
        matches!(error, SmartZipError::BackendUnavailable { backend } if backend == "selected")
    );
    let error = SevenZipBackend::new(root.path().to_path_buf())
        .list(list())
        .await
        .unwrap_err();
    assert!(matches!(error, SmartZipError::Io { path: Some(path), .. } if path == root.path()));
}

#[tokio::test]
async fn process_cancellation_before_start_and_after_pipe_eof_remains_bounded() {
    let (_root, path) = executable("exec 1>&- 2>&-\nprintf ready > \"$0.ready\"\nsleep 30");
    let token = CancellationToken::new();
    token.cancel();
    assert!(matches!(
        crate::test_output::run(&path, "fault-test", &[], &token).await,
        Err(SmartZipError::Cancelled)
    ));
    assert!(!path.with_extension("ready").exists());
    for mode in 0..3 {
        let marker = path.with_extension("ready");
        let _ = std::fs::remove_file(&marker);
        let token = CancellationToken::new();
        let child_token = token.clone();
        let executable = path.clone();
        let pending = tokio::spawn(async move {
            let backend = SevenZipBackend::new(executable.clone());
            match mode {
                0 => backend
                    .list_with_context(
                        list(),
                        Arc::new(TaskExecutionContext::detached().with_cancellation(child_token)),
                    )
                    .await
                    .map(|_| ()),
                1 => backend
                    .test_with_report_and_token(test(), &child_token)
                    .await
                    .map(|_| ()),
                _ => crate::test_output::run(&executable, "fault-test", &[], &child_token)
                    .await
                    .map(|_| ()),
            }
        });
        ready(&marker).await;
        token.cancel();
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(5), pending)
                .await
                .unwrap()
                .unwrap(),
            Err(SmartZipError::Cancelled)
        ));
    }
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
                    break
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
                    break
                }
                _ => tokio::time::sleep(Duration::from_millis(5)).await,
            }
        }
    })
    .await
    .expect("cancel failed to kill descendants after the parent exited");
}
