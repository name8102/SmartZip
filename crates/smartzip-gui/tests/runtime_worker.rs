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

#[test]
#[ignore = "requires installed 7z; run with --include-ignored for backend acceptance"]
fn real_backend_source_recycling_removes_the_successful_volume_set() {
    for all in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let payload: Vec<u8> = (0..40_000u32)
            .flat_map(|n| n.wrapping_mul(2654435761).to_le_bytes())
            .collect();
        std::fs::write(temp.path().join("payload.bin"), &payload).unwrap();
        let created = std::process::Command::new("7z")
            .current_dir(temp.path())
            .args(["a", "-t7z", "-mx=0", "-v64k", "bundle.7z", "payload.bin"])
            .output()
            .unwrap();
        assert!(created.status.success());
        let mut volumes: Vec<_> = std::fs::read_dir(temp.path())
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|p| {
                p.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("bundle.7z.")
            })
            .collect();
        volumes.sort();
        let handle = spawn_job(JobRequest {
            operation: TaskOperation::Extract,
            paths: if all {
                volumes.clone()
            } else {
                vec![volumes[0].clone()]
            },
            settings: TaskSettings {
                output: Some(temp.path().join("output")),
                recursive: Some(false),
                delete_source: true,
                ..Default::default()
            },
            resolved: Some(if all {
                config_with_database(&temp.path().join("execution.db"))
            } else {
                config()
            }),
        })
        .unwrap();
        let (outcome, events) = finish(&handle);
        let snapshots = handle.roots.snapshot();
        let roots: Vec<_> = snapshots.iter().filter(|n| n.parent_id.is_none()).collect();
        assert_eq!(
            roots.len(),
            1,
            "all physical volumes must share one control row"
        );
        let group = roots[0].volumes.as_ref().unwrap();
        assert_eq!(group.members.len(), volumes.len());
        assert_eq!(group.selected.len(), 1);
        assert!(group.selected[0].iter().all(|path| volumes.contains(path)));
        assert_eq!(outcome.status, "completed", "{:?}", outcome.detail);
        assert!(
            volumes.iter().all(|p| !p.exists()),
            "{:?}",
            outcome.warnings
        );
        let output = events
            .iter()
            .find_map(|event| match &event.kind {
                smartzip_core::TaskEventKind::OutputCreated { path } => {
                    Some(path.join("payload.bin"))
                }
                _ => None,
            })
            .unwrap();
        assert_eq!(std::fs::read(output).unwrap(), payload);
    }
}

#[test]
#[ignore = "requires installed 7z; run with --include-ignored for backend acceptance"]
fn root_controls_keep_siblings_running_while_password_prompt_is_parked() {
    let temp = tempfile::tempdir().unwrap();
    let mut inputs = vec![];
    for (index, name) in ["locked", "second", "third"].iter().enumerate() {
        let dir = temp.path().join(name);
        std::fs::create_dir(&dir).unwrap();
        let archive = fixture(&dir, index == 0);
        let renamed = dir.join(format!("{name}.zip"));
        std::fs::rename(archive, &renamed).unwrap();
        inputs.push(renamed);
    }
    let handle = spawn_job(JobRequest {
        operation: TaskOperation::Extract,
        paths: inputs.clone(),
        settings: TaskSettings {
            output: Some(temp.path().join("output")),
            ..Default::default()
        },
        resolved: Some(config_with_database(&temp.path().join("state.db"))),
    })
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut pending = None;
    let mut cancelled = false;
    loop {
        for message in handle.drain() {
            match message {
                JobMessage::Prompt(InteractionRequest::Password { respond, .. }) => {
                    pending = Some(respond)
                }
                JobMessage::Prompt(_) => panic!("unexpected prompt"),
                JobMessage::Failed(error) => panic!("{error}"),
                JobMessage::Finished(outcome) => {
                    assert!(cancelled);
                    assert_eq!(outcome.status, "cancelled");
                    assert!(pending.as_ref().unwrap().is_closed());
                    for input in &inputs {
                        assert!(input.exists());
                    }
                    let files = handle.roots.snapshot();
                    assert_eq!(files.iter().filter(|f| f.parent_id.is_none()).count(), 3);
                    assert_eq!(files[0].root_outcome.as_deref(), Some("cancelled"));
                    assert_eq!(files[1].root_outcome.as_deref(), Some("completed"));
                    assert_eq!(files[2].root_outcome.as_deref(), Some("completed"));
                    return;
                }
                _ => {}
            }
        }
        // Both siblings must finish before cancelling the unanswered first root.
        // This catches preparation slots being held indefinitely by parked roots.
        let files = handle.roots.snapshot();
        if !cancelled
            && pending.is_some()
            && files
                .iter()
                .filter(|f| f.root_outcome.as_deref() == Some("completed"))
                .count()
                == 2
        {
            let root = files.iter().find(|f| f.path == inputs[0]).unwrap();
            assert!(handle.roots.set_paused(&root.root_id, true));
            assert!(handle.roots.cancel(&root.root_id));
            cancelled = true;
        }
        assert!(Instant::now() < deadline, "root control timeout: {files:?}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
#[ignore = "requires installed 7z; run with --include-ignored for backend acceptance"]
fn explicit_recovery_keeps_identity_and_does_not_run_other_pending_tasks() {
    use smartzip_engine::state_store::{
        NodeSubmission, PersistedExtractPlan, StateStore, TaskSubmission,
    };
    let dir = tempfile::tempdir().unwrap();
    let archive = fixture(dir.path(), false);
    let database = dir.path().join("state.db");
    let cfg = config_with_database(&database);
    let first = smartzip_core::TaskId::new();
    let second = smartzip_core::TaskId::new();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let store = StateStore::start(&database).unwrap();
        for (id, name) in [(&first, "recovered"), (&second, "untouched")] {
            let node = smartzip_core::NodeId::new();
            let output = dir.path().join(name);
            let request = smartzip_engine::ExtractWorkflowRequest {
                inputs: vec![archive.clone()],
                output_dir: output.clone(),
                recursion_limit: 0,
                encoding_mode: Default::default(),
                scanner: Default::default(),
                password_candidates: Default::default(),
                layout_policy: smartzip_engine::layout::OutputLayoutPolicy::Conservative,
                single_root_name_policy: smartzip_engine::layout::SingleRootNamePolicy::Auto,
                embedded_scan_mode: smartzip_core::EmbeddedScanMode::Auto,
                dominant_min_ratio: 0.7,
                confirm_large_scan: false,
                force: true,
                limits: cfg.values.limits.clone(),
            };
            let plan = PersistedExtractPlan::new(cfg.values.clone(), request);
            store
                .submit(TaskSubmission {
                    task_id: id.clone(),
                    kind: "extract".into(),
                    output_path: Some(output),
                    started_at: smartzip_db::timestamp::now_utc_iso8601(),
                    inputs_json: serde_json::to_string(&[&archive]).unwrap(),
                    config_snapshot_json: serde_json::to_string(&plan).unwrap(),
                    priority: smartzip_db::task_execution::Priority::Normal,
                    queue_position: 0,
                    recoverable: true,
                    roots: vec![NodeSubmission {
                        node_id: node.clone(),
                        parent_id: None,
                        root_id: node,
                        input_path: archive.clone(),
                        input_ref_json: serde_json::to_string(
                            &smartzip_engine::ExtractionCandidate::root(archive.clone()),
                        )
                        .unwrap(),
                        config_revision: 0,
                        generation: 0,
                    }],
                })
                .await
                .unwrap();
        }
    });
    let new_job = spawn_job(JobRequest {
        operation: TaskOperation::Extract,
        paths: vec![archive.clone()],
        settings: TaskSettings {
            output: Some(dir.path().join("new")),
            recursive: Some(false),
            ..Default::default()
        },
        resolved: Some(cfg.clone()),
    })
    .unwrap();
    assert_eq!(finish(&new_job).0.status, "completed");
    drop(new_job);
    assert!(!dir.path().join("recovered").exists());
    assert!(!dir.path().join("untouched").exists());
    let recovered = spawn_job(JobRequest {
        operation: TaskOperation::Recover,
        paths: vec![archive.clone()],
        settings: TaskSettings {
            recovery_task_id: Some(first.to_string()),
            ..Default::default()
        },
        resolved: Some(cfg),
    })
    .unwrap();
    let (result, events) = finish(&recovered);
    assert_eq!(result.status, "completed");
    assert!(events.iter().all(|e| e.task_id == first));
    assert_eq!(
        std::fs::read(dir.path().join("recovered/fixture/hello.txt")).unwrap(),
        b"GUI runtime acceptance\n"
    );
    assert!(!dir.path().join("untouched").exists());
    assert!(archive.exists());
}

#[test]
#[ignore = "requires installed 7z; run with --include-ignored for backend acceptance"]
fn persisted_gui_draft_is_adopted_with_its_original_identity() {
    let dir = tempfile::tempdir().unwrap();
    let archive = fixture(dir.path(), false);
    let database = dir.path().join("state.db");
    let id = smartzip_core::TaskId::new();
    {
        let db = smartzip_db::SmartZipDb::open(&database).unwrap();
        db.connection()
            .execute(
                "INSERT INTO tasks(id,kind,status,started_at,recoverable,paused) \
                 VALUES (?1,'extract','queued_gui','draft',0,1)",
                [id.as_str()],
            )
            .unwrap();
    }
    let handle = spawn_job(JobRequest {
        operation: TaskOperation::Extract,
        paths: vec![archive.clone()],
        settings: TaskSettings {
            queued_task_id: Some(id.to_string()),
            output: Some(dir.path().join("output")),
            recursive: Some(false),
            ..Default::default()
        },
        resolved: Some(config_with_database(&database)),
    })
    .unwrap();
    let (result, events) = finish(&handle);
    assert_eq!(result.status, "completed");
    assert!(!events.is_empty());
    assert!(events.iter().all(|event| event.task_id == id));
    let db = smartzip_db::SmartZipDb::open(&database).unwrap();
    let (status, paused): (String, bool) = db
        .connection()
        .query_row(
            "SELECT status,paused FROM tasks WHERE id=?1",
            [id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(status, "completed");
    assert!(!paused);
    let count: i64 = db
        .connection()
        .query_row("SELECT count(*) FROM tasks", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 1);
    assert_eq!(
        std::fs::read(dir.path().join("output/fixture/hello.txt")).unwrap(),
        b"GUI runtime acceptance\n"
    );
    assert!(archive.exists());
}
