//! Public CLI contracts captured before orchestration refactoring.
use std::{path::Path, process::Command};

#[test]
fn public_help_and_streams_remain_stable() {
    for (command, snapshot) in [
        ("", include_str!("snapshots/root-help.txt")),
        ("extract", include_str!("snapshots/extract-help.txt")),
        ("list", include_str!("snapshots/list-help.txt")),
        ("detect", include_str!("snapshots/detect-help.txt")),
        ("test", include_str!("snapshots/test-help.txt")),
    ] {
        let mut process = Command::new(env!("CARGO_BIN_EXE_smartzip"));
        if !command.is_empty() {
            process.arg(command);
        }
        let output = process.arg("--help").env("NO_COLOR", "1").output().unwrap();
        assert!(output.status.success(), "{command}");
        assert!(output.stderr.is_empty(), "{command}");
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            snapshot,
            "{command}"
        );
    }
}

#[test]
fn argument_errors_remain_on_stderr_without_creating_state() {
    let root = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_smartzip"))
        .args(["extract", "archive.zip", "--recursion-limit", "256"])
        .current_dir(root.path())
        .env("XDG_DATA_HOME", root.path().join("data"))
        .env("XDG_CONFIG_HOME", root.path().join("config"))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("invalid value"));
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
}

#[test]
fn extract_keeps_progress_encoding_password_output_and_completion_order() {
    let root = tempfile::tempdir().unwrap();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/enc_utf8.zip");
    let archive = root.path().join("input.zip");
    std::fs::copy(fixture, &archive).unwrap();
    let before = std::fs::read(&archive).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_smartzip"))
        .args(["--no-config", "extract"])
        .arg(&archive)
        .args([
            "--stateless",
            "--no-recursive",
            "--non-interactive",
            "--json",
            "--output",
        ])
        .arg(root.path().join("out"))
        .current_dir(root.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let events: Vec<_> = result["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            e["kind"].as_str().map(str::to_owned).unwrap_or_else(|| {
                e["kind"]
                    .as_object()
                    .unwrap()
                    .keys()
                    .next()
                    .unwrap()
                    .clone()
            })
        })
        .filter(|kind| !matches!(kind.as_str(), "Route" | "Decision" | "Progress"))
        .collect();
    assert_eq!(
        events,
        [
            "Started",
            "EncodingDetected",
            "PasswordTried",
            "OutputCreated",
            "Finished"
        ]
    );
    assert_eq!(result["status"], "completed");
    assert_eq!(result["exit_code"], 0);
    assert_eq!(result["processed_count"], 1);
    assert_eq!(std::fs::read(&archive).unwrap(), before);
    assert!(!root.path().join("out/.smartzip").exists());
}
