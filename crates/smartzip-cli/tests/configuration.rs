use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
};

struct Workspace(tempfile::TempDir);
impl Workspace {
    fn new(config: &str) -> Self {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("config.toml"), config).unwrap();
        Self(root)
    }
    fn path(&self, p: &str) -> PathBuf {
        self.0.path().join(p)
    }
    fn run(&self, args: &[&str], code: i32) -> Output {
        let output = Command::new(env!("CARGO_BIN_EXE_smartzip"))
            .current_dir(self.0.path())
            .env_remove("SMARTZIP_CONFIG")
            .env("XDG_DATA_HOME", self.path("data"))
            .env("XDG_CONFIG_HOME", self.path("config"))
            .env("XDG_CACHE_HOME", self.path("cache"))
            .arg("--config")
            .arg(self.path("config.toml"))
            .args(args)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(code),
            "{args:?}: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }
    fn json(&self, args: &[&str], code: i32) -> Value {
        serde_json::from_slice(&self.run(args, code).stdout).unwrap()
    }
    fn fixture(&self, name: &str) -> PathBuf {
        let source = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures")
            .join(name);
        let target = self.path(name);
        fs::copy(source, &target).unwrap();
        target
    }
}

#[cfg(unix)]
#[test]
fn disabled_backend_is_neither_probed_nor_started() {
    use std::os::unix::fs::PermissionsExt;
    let w = Workspace::new("schema_version=1\n[state]\nmode='off'\n[extraction.recursion]\nenabled=false\n[backends]\nauto_discover=false\n[[backends.installations]]\nid='disabled'\nfamily='seven-zip-cli'\nexecutable='./backend'\ndeclared_version='24.09'\nenabled=false\n");
    fs::write(
        w.path("backend"),
        "#!/bin/sh\ntouch backend-started\nexit 1\n",
    )
    .unwrap();
    fs::set_permissions(w.path("backend"), fs::Permissions::from_mode(0o755)).unwrap();
    let archive = w.fixture("enc_utf8.zip");
    let report = w.json(
        &[
            "extract",
            archive.to_str().unwrap(),
            "--output",
            "out",
            "--json",
            "--non-interactive",
        ],
        1,
    );
    assert_eq!(report["failed_count"], 1);
    assert!(report.to_string().contains("archive backend unavailable"));
    assert!(!w.path("backend-started").exists());
}

#[test]
fn explain_and_config_commands_have_no_archive_backend_or_state_side_effects() {
    let w = Workspace::new("schema_version=1\n[extraction.recursion]\nenabled=false\nmax_depth=7\n[state]\ndatabase='state/db.sqlite'\n[backends]\nauto_discover=false\n[[backends.installations]]\nid='never-run'\nfamily='seven-zip-cli'\nexecutable='./nonexistent-executable'\n");
    let report = w.json(&["extract", "nonexistent-archive.zip", "--explain"], 0);
    assert_eq!(
        report["configuration"]["values"]["extraction"]["recursion"]["enabled"],
        false
    );
    assert_eq!(
        report["configuration"]["values"]["extraction"]["recursion"]["max_depth"],
        7
    );
    let overridden = w.json(
        &[
            "extract",
            "missing.zip",
            "--explain",
            "--recursion-limit",
            "2",
            "--stateless",
        ],
        0,
    );
    assert_eq!(
        overridden["configuration"]["values"]["extraction"]["recursion"]["max_depth"],
        2
    );
    assert_eq!(
        overridden["configuration"]["origins"]["extraction.recursion.max_depth"],
        "command_line"
    );
    assert_eq!(
        overridden["configuration"]["values"]["state"]["mode"],
        "off"
    );
    w.run(&["config", "check"], 0);
    w.run(&["config", "show", "--effective", "--sources"], 0);
    w.run(
        &[
            "extract",
            "missing.zip",
            "--no-recursive",
            "--set",
            "extraction.recursion.enabled=true",
            "--explain",
        ],
        1,
    );
    for path in ["state", "data", "config", "cache"] {
        assert!(!w.path(path).exists(), "created {path}");
    }
}

#[test]
fn stateless_extract_respects_recursion_and_cleanup_without_creating_a_database() {
    let w = Workspace::new("schema_version=1\n[extraction.recursion]\nenabled=false\n[extraction.embedded]\nroot='off'\nnested='off'\n[extraction.encoding]\nmode='backend'\n[extraction.output]\nlayout='raw'\n[extraction.cleanup]\nnested_archives='keep'\n[state]\ndatabase='state/db.sqlite'\nmode='off'\n");
    let archive = w.fixture("nested_zip_in_zip.zip");
    let before = fs::read(&archive).unwrap();
    let report = w.json(
        &[
            "extract",
            archive.to_str().unwrap(),
            "--output",
            "out",
            "--json",
            "--non-interactive",
        ],
        0,
    );
    assert_eq!(report["processed_count"], 1);
    assert_eq!(report["enqueued_count"], 0);
    assert!(report["events"]
        .as_array()
        .unwrap()
        .iter()
        .any(|e| e["kind"]["Decision"]["stage"] == "nested_discovery"
            && e["kind"]["Decision"]["action"] == "skip"));
    assert_eq!(fs::read(&archive).unwrap(), before);
    for path in ["state", "data", "config", "cache"] {
        assert!(!w.path(path).exists(), "created {path}");
    }
}

#[test]
fn history_and_known_files_are_independent_and_read_only_really_stays_read_only() {
    let w = Workspace::new("schema_version=1\n[state]\ndatabase='db.sqlite'\nhistory=false\nknown_files='read-write'\n[extraction.recursion]\nenabled=false\n");
    let archive = w.fixture("enc_utf8.zip");
    w.run(
        &[
            "extract",
            archive.to_str().unwrap(),
            "--output",
            "first",
            "--json",
            "--non-interactive",
        ],
        0,
    );
    let db = smartzip_db::SmartZipDb::open_read_only(w.path("db.sqlite")).unwrap();
    let count = |table: &str| {
        db.connection()
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| {
                r.get::<_, i64>(0)
            })
            .unwrap()
    };
    assert_eq!(count("tasks"), 0);
    assert_eq!(count("known_files"), 1);
    drop(db);
    let before = fs::read(w.path("db.sqlite")).unwrap();
    w.run(
        &[
            "extract",
            archive.to_str().unwrap(),
            "--output",
            "second",
            "--json",
            "--non-interactive",
            "--set",
            "state.mode='read-only'",
        ],
        0,
    );
    assert_eq!(fs::read(w.path("db.sqlite")).unwrap(), before);
    w.run(
        &[
            "extract",
            archive.to_str().unwrap(),
            "--output",
            "third",
            "--json",
            "--non-interactive",
            "--set",
            "state.history=true",
            "--set",
            "state.known_files='off'",
        ],
        0,
    );
    let db = smartzip_db::SmartZipDb::open_read_only(w.path("db.sqlite")).unwrap();
    assert_eq!(
        db.connection()
            .query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        db.connection()
            .query_row("SELECT COUNT(*) FROM known_files", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[test]
fn config_and_environment_select_one_file_and_never_trust_the_working_directory() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("config.toml"), "invalid untrusted config").unwrap();
    let env_config = root.path().join("selected.toml");
    fs::write(
        &env_config,
        "schema_version=1\n[extraction.recursion]\nmax_depth=6\n",
    )
    .unwrap();
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_smartzip"))
            .current_dir(root.path())
            .env("SMARTZIP_CONFIG", &env_config)
            .env("XDG_CONFIG_HOME", root.path().join("xdg"))
            .args(args)
            .output()
            .unwrap()
    };
    let selected = run(&["config", "get", "extraction.recursion.max_depth"]);
    assert!(selected.status.success());
    assert_eq!(String::from_utf8(selected.stdout).unwrap().trim(), "6");
    let defaults = run(&[
        "--no-config",
        "config",
        "get",
        "extraction.recursion.max_depth",
    ]);
    assert!(defaults.status.success());
    assert_eq!(String::from_utf8(defaults.stdout).unwrap().trim(), "3");
    let explicit = run(&["--config", "config.toml", "config", "check"]);
    assert!(!explicit.status.success());
}
