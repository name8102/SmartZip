#[allow(dead_code)]
#[path = "../src/runtime.rs"]
mod runtime;

use runtime::*;
use std::{
    path::Path,
    time::{Duration, Instant},
};

fn spawn_job(request: JobRequest) -> Result<JobHandle, String> {
    spawn_job_at(request, 0)
}

fn config() -> smartzip_config::ResolvedConfig {
    let mut config = smartzip_config::ResolvedConfig::load(None).unwrap();
    config
        .apply(&[
            ("limits.min_free_bytes".into(), toml::Value::Integer(0)),
            ("state.mode".into(), toml::Value::String("off".into())),
            (
                "passwords.mode".into(),
                toml::Value::String("manual".into()),
            ),
        ])
        .unwrap();
    config
}
fn config_with_database(path: &Path) -> smartzip_config::ResolvedConfig {
    let mut config = config();
    config
        .apply(&[
            (
                "state.mode".into(),
                toml::Value::String("read-write".into()),
            ),
            (
                "state.database".into(),
                toml::Value::String(path.to_string_lossy().into_owned()),
            ),
            ("passwords.save_success".into(), toml::Value::Boolean(true)),
            (
                "passwords.record_statistics".into(),
                toml::Value::Boolean(true),
            ),
        ])
        .unwrap();
    config
}
fn fixture(directory: &Path, encrypted: bool) -> std::path::PathBuf {
    std::fs::write(directory.join("hello.txt"), b"GUI runtime acceptance\n").unwrap();
    let archive = directory.join("fixture.zip");
    let seven = ["7zz", "7z"]
        .into_iter()
        .find(|name| {
            std::process::Command::new(name)
                .arg("i")
                .output()
                .is_ok_and(|output| output.status.success())
        })
        .expect("7zz or 7z required for backend acceptance");
    let mut command = std::process::Command::new(seven);
    command.current_dir(directory).args(["a", "-tzip"]);
    if encrypted {
        command.arg("-pRuntimeFixtureOnly");
    }
    let output = command
        .arg(&archive)
        .arg("hello.txt")
        .output()
        .expect("7zz or 7z required for backend acceptance");
    assert!(output.status.success(), "fixture creation failed");
    archive
}
fn finish(handle: &JobHandle) -> (JobOutcome, Vec<smartzip_core::TaskEvent>) {
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut events = vec![];
    loop {
        for message in handle.drain() {
            match message {
                JobMessage::Event(event) => events.push(event),
                JobMessage::Finished(outcome) => return (outcome, events),
                JobMessage::Failed(error) => panic!("worker failed: {error}"),
                JobMessage::Prompt(_) => panic!("unexpected interaction"),
            }
        }
        assert!(Instant::now() < deadline, "worker timeout");
        std::thread::sleep(Duration::from_millis(5));
    }
}
#[test]
#[ignore = "requires installed 7zz or 7z; run with --include-ignored for backend acceptance"]
fn real_backend_detect_list_test_extract_preserve_source() {
    let temp = tempfile::tempdir().unwrap();
    let archive = fixture(temp.path(), false);
    for operation in [
        TaskOperation::Detect,
        TaskOperation::List,
        TaskOperation::Test,
        TaskOperation::Extract,
    ] {
        let handle = spawn_job(JobRequest {
            operation,
            paths: vec![archive.clone()],
            settings: TaskSettings {
                output: Some(temp.path().join("output")),
                recursive: Some(false),
                ..Default::default()
            },
            resolved: Some(config()),
        })
        .unwrap();
        let (outcome, events) = finish(&handle);
        assert_eq!(outcome.status, "completed", "{}", outcome.detail);
        assert!(
            events
                .iter()
                .any(|event| matches!(event.kind, smartzip_core::TaskEventKind::Route(_))),
            "missing real backend event"
        );
        if operation == TaskOperation::List {
            assert_eq!(outcome.detail["entries"][0]["path"], "hello.txt");
        }
        assert!(archive.exists());
    }
    assert_eq!(
        std::fs::read(temp.path().join("output/fixture/hello.txt")).unwrap(),
        b"GUI runtime acceptance\n"
    );
}
#[test]
#[ignore = "requires installed 7zz or 7z; run with --include-ignored for backend acceptance"]
fn real_backend_password_wait_cancels_without_output_or_source_deletion() {
    let temp = tempfile::tempdir().unwrap();
    let archive = fixture(temp.path(), true);
    let handle = spawn_job(JobRequest {
        operation: TaskOperation::Extract,
        paths: vec![archive.clone()],
        settings: TaskSettings {
            output: Some(temp.path().join("output")),
            delete_source: true,
            ..Default::default()
        },
        resolved: Some(config()),
    })
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    let pending;
    'wait: loop {
        for message in handle.drain() {
            match message {
                JobMessage::Prompt(InteractionRequest::Password { respond, .. }) => {
                    pending = respond;
                    break 'wait;
                }
                JobMessage::Finished(outcome) => {
                    panic!("finished without password prompt: {}", outcome.status)
                }
                JobMessage::Failed(error) => panic!("{error}"),
                _ => {}
            }
        }
        assert!(Instant::now() < deadline, "password wait timeout");
        std::thread::sleep(Duration::from_millis(5));
    }
    handle.cancel();
    let (outcome, _) = finish(&handle);
    assert_eq!(outcome.status, "cancelled");
    assert!(pending.is_closed());
    assert!(archive.exists());
    assert!(!temp.path().join("output/fixture/hello.txt").exists());
}

#[test]
#[ignore = "requires installed 7zz or 7z; run with --include-ignored for backend acceptance"]
fn real_backend_success_recycles_only_explicit_source() {
    let temp = tempfile::tempdir().unwrap();
    let archive = fixture(temp.path(), false);
    let untouched = temp.path().join("unrelated.zip");
    std::fs::copy(&archive, &untouched).unwrap();
    let handle = spawn_job(JobRequest {
        operation: TaskOperation::Extract,
        paths: vec![archive.clone()],
        settings: TaskSettings {
            output: Some(temp.path().join("output")),
            recursive: Some(false),
            delete_source: true,
            ..Default::default()
        },
        resolved: Some(config()),
    })
    .unwrap();
    let (outcome, _) = finish(&handle);
    assert_eq!(outcome.status, "completed");
    assert!(outcome.warnings.is_empty(), "{:?}", outcome.warnings);
    assert!(!archive.exists());
    assert!(untouched.exists());
    assert_eq!(
        std::fs::read(temp.path().join("output/fixture/hello.txt")).unwrap(),
        b"GUI runtime acceptance\n"
    );
}

#[test]
#[ignore = "requires installed 7zz or 7z; run with --include-ignored for backend acceptance"]
fn changed_source_is_kept_after_successful_password_response() {
    let temp = tempfile::tempdir().unwrap();
    let archive = fixture(temp.path(), true);
    let handle = spawn_job(JobRequest {
        operation: TaskOperation::Extract,
        paths: vec![archive.clone()],
        settings: TaskSettings {
            output: Some(temp.path().join("output")),
            recursive: Some(false),
            delete_source: true,
            ..Default::default()
        },
        resolved: Some(config()),
    })
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    'wait: loop {
        for message in handle.drain() {
            match message {
                JobMessage::Prompt(InteractionRequest::Password { respond, .. }) => {
                    let contents = std::fs::read(&archive).unwrap();
                    let replacement = temp.path().join("replacement.zip");
                    std::fs::write(&replacement, contents).unwrap();
                    std::fs::rename(replacement, &archive).unwrap();
                    assert!(respond.send(Some("RuntimeFixtureOnly".into())).is_ok());
                    break 'wait;
                }
                JobMessage::Finished(outcome) => {
                    panic!("finished without password prompt: {}", outcome.status)
                }
                JobMessage::Failed(error) => panic!("{error}"),
                _ => {}
            }
        }
        assert!(Instant::now() < deadline, "password wait timeout");
        std::thread::sleep(Duration::from_millis(5));
    }
    let (outcome, events) = finish(&handle);
    assert_eq!(outcome.status, "completed");
    assert!(archive.exists());
    assert!(outcome
        .warnings
        .iter()
        .any(|warning| warning.contains("原包已保留")));
    assert!(!serde_json::to_string(&events)
        .unwrap()
        .contains("RuntimeFixtureOnly"));
    assert!(!outcome.detail.to_string().contains("RuntimeFixtureOnly"));
}

#[test]
#[ignore = "requires installed 7zz or 7z; run with --include-ignored for backend acceptance"]
fn real_backend_temporary_password_is_not_persisted() {
    let temp = tempfile::tempdir().unwrap();
    let archive = fixture(temp.path(), true);
    let database = temp.path().join("temporary-passwords.db");
    let handle = spawn_job(JobRequest {
        operation: TaskOperation::Extract,
        paths: vec![archive],
        settings: TaskSettings {
            output: Some(temp.path().join("output")),
            passwords: vec!["RuntimeFixtureOnly".into()],
            temporary_passwords: true,
            ..Default::default()
        },
        resolved: Some(config_with_database(&database)),
    })
    .unwrap();
    let (outcome, events) = finish(&handle);
    assert_eq!(outcome.status, "completed", "{}", outcome.detail);
    assert!(!serde_json::to_string(&events)
        .unwrap()
        .contains("RuntimeFixtureOnly"));
    assert!(!outcome.detail.to_string().contains("RuntimeFixtureOnly"));
    let db = smartzip_db::SmartZipDb::open_read_only(&database).unwrap();
    assert!(
        smartzip_db::password::PasswordRepository::new(db.connection())
            .ranked_candidates(100)
            .unwrap()
            .is_empty()
    );
}
