use serde_json::Value;
use smartzip_db::{
    file_extractions::{FileExtractionRepository, NewFileExtraction},
    task::{NewTask, TaskRepository},
    SmartZipDb,
};
use std::process::{Command, Output, Stdio};

struct Cli {
    root: tempfile::TempDir,
}

impl Cli {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("config.toml"),
            "[backends]\nauto_discover = false\n",
        )
        .unwrap();
        Self { root }
    }

    fn run(&self, args: &[&str], code: i32) -> Output {
        let output = Command::new(env!("CARGO_BIN_EXE_smartzip"))
            .arg("--db")
            .arg(self.root.path().join("history.db"))
            .arg("--config")
            .arg(self.root.path().join("config.toml"))
            .args(args)
            .current_dir(self.root.path())
            .env("XDG_DATA_HOME", self.root.path().join("data"))
            .env("XDG_CONFIG_HOME", self.root.path().join("config"))
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(code),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    fn json(&self, args: &[&str]) -> Value {
        serde_json::from_slice(&self.run(args, 0).stdout).unwrap()
    }
}

#[test]
fn password_cleanup_preview_preserves_data_and_apply_preserves_pins() {
    let cli = Cli::new();
    cli.run(&["pw", "add", "keep", "--pin"], 0);
    std::fs::write(cli.root.path().join("passwords.txt"), "old\nold\nspare\n").unwrap();
    cli.run(&["password", "import", "passwords.txt"], 0);
    let before = cli.json(&["pw", "list", "--json"]);
    assert_eq!(before.as_array().unwrap().len(), 3);
    assert_eq!(before[0]["value"], "keep");
    cli.run(
        &["pw", "cleanup", "--max-passwords", "1", "--stale-days", "0"],
        0,
    );
    assert_eq!(cli.json(&["pw", "list", "--json"]), before);
    cli.run(
        &[
            "pw",
            "cleanup",
            "--max-passwords",
            "1",
            "--stale-days",
            "0",
            "--apply",
        ],
        0,
    );
    let after = cli.json(&["pw", "list", "--json"]);
    assert_eq!(after.as_array().unwrap().len(), 1);
    assert_eq!(after[0]["value"], "keep");
    cli.run(&["pw", "export", "--path", "export.txt"], 0);
    assert_eq!(
        std::fs::read_to_string(cli.root.path().join("export.txt")).unwrap(),
        "keep\n"
    );
    // Reimport re-enables disabled candidates; failed imports leave the database unchanged.
    cli.run(&["pw", "import", "passwords.txt"], 0);
    let restored = cli.json(&["pw", "list", "--json"]);
    assert_eq!(restored.as_array().unwrap().len(), 3);
    cli.run(&["pw", "import", "missing.txt"], 1);
    assert_eq!(cli.json(&["pw", "list", "--json"]), restored);
    cli.run(&["pw", "remove", &after[0]["id"].to_string()], 0);
    assert!(cli
        .json(&["pw", "list", "--json"])
        .as_array()
        .unwrap()
        .iter()
        .all(|p| p["value"] != "keep"));
}

#[test]
fn history_dispatch_and_combined_filters_return_only_matching_actions() {
    let cli = Cli::new();
    let db = SmartZipDb::open(cli.root.path().join("history.db")).unwrap();
    TaskRepository::new(db.connection())
        .insert(NewTask {
            id: "task-a",
            kind: "extract",
            output_path: None,
            started_at: "2026-01-01 00:00:00",
        })
        .unwrap();
    for (path, status, reason) in [
        ("a.zip", "skipped", Some("duplicate")),
        ("b.zip", "skipped", Some("collision")),
        ("c.zip", "failed", Some("duplicate")),
    ] {
        FileExtractionRepository::new(db.connection())
            .insert(NewFileExtraction {
                task_id: "task-a",
                input_path: path,
                sample_hash: None,
                file_size: None,
                offset: None,
                output_path: None,
                has_password: false,
                password_id: None,
                status,
                reason,
                encoding: None,
                encoding_corrected: false,
                damaged_volumes_json: None,
                test_report_json: None,
                created_at: "2026-01-01 00:00:00",
            })
            .unwrap();
    }
    drop(db);
    assert_eq!(
        cli.run(&["history"], 0).stdout,
        cli.run(&["history", "tasks"], 0).stdout
    );
    assert!(String::from_utf8(cli.run(&["hist"], 0).stdout)
        .unwrap()
        .contains("task-a"));
    assert_eq!(cli.json(&["history", "tasks", "--json"])[0]["id"], "task-a");
    for (filters, expected) in [
        (vec![], vec!["a.zip", "b.zip", "c.zip"]),
        (vec!["--status", "skipped"], vec!["a.zip", "b.zip"]),
        (vec!["--reason", "duplicate"], vec!["a.zip", "c.zip"]),
        (
            vec!["--status", "skipped", "--reason", "duplicate"],
            vec!["a.zip"],
        ),
    ] {
        let mut args = vec!["history", "files", "--json"];
        args.extend(filters);
        let rows = cli.json(&args);
        let mut paths: Vec<_> = rows
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["input_path"].as_str().unwrap())
            .collect();
        paths.sort();
        assert_eq!(paths, expected);
    }
    let show = cli.json(&["history", "show", "task-a", "--json"]);
    assert_eq!(show["task"]["id"], "task-a");
    assert_eq!(show["files"].as_array().unwrap().len(), 3);
    let missing = cli.run(&["history", "show", "absent", "--json"], 1);
    assert!(String::from_utf8(missing.stderr)
        .unwrap()
        .contains("no task with id absent"));
}
