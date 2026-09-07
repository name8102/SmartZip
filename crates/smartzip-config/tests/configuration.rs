use smartzip_config::*;
use std::fs;

#[test]
fn precedence_preserves_false_empty_arrays_and_file_relative_paths() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("config.toml");
    fs::write(&path, "schema_version=1\n[extraction.recursion]\nenabled=false\n[passwords]\nsources=[]\n[state]\ndatabase='data.db'\n[[backends.installations]]\nid='local'\nfamily='seven-zip-cli'\nexecutable='./bin/7z'\n").unwrap();
    let mut resolved = ResolvedConfig::load(Some(&path)).unwrap();
    assert!(!resolved.values.extraction.recursion.enabled);
    assert_eq!(resolved.values.extraction.recursion.max_depth, 3);
    assert!(resolved.values.passwords.sources.is_empty());
    assert_eq!(
        resolved.values.state.database,
        Some(root.path().join("data.db"))
    );
    assert_eq!(
        resolved.values.backends.installations[0].executable,
        root.path().join("./bin/7z")
    );
    assert_eq!(
        resolved.origins["extraction.recursion.max_depth"],
        "defaults-v1"
    );
    resolved
        .apply(&[("extraction.recursion.enabled".into(), true.into())])
        .unwrap();
    assert!(resolved.values.extraction.recursion.enabled);
    assert_eq!(
        resolved.origins["extraction.recursion.enabled"],
        "command_line"
    );
    assert!(resolved
        .apply(&[
            ("state.history".into(), true.into()),
            ("state.history".into(), false.into())
        ])
        .is_err());
    assert!(resolved.values.state.history);
}

#[test]
fn invalid_files_fail_without_falling_back_or_being_rewritten() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("config.toml");
    for text in [
        "schema_version=99",
        "schema_version=1\ndefaults_version=99",
        "schema_version=1\n[extraction.recursion]\nrecusive=false",
        "schema_version=1\n[extraction.embedded]\nroot='mystery'",
        "schema_version=1\n[passwords]\nsources=['manual','manual']",
        "schema_version=1\n[passwords]\nsources=['clipboard']",
        "schema_version=1\n[logging]\nfile=true",
        "schema_version=1\n[extraction.output]\ndestination='directory'",
        "schema_version=1\n[extraction.embedded]\ndominant_min_ratio=nan",
        "[extraction]\nmax_filez=20",
        "schema_version=1\n[extraction.recursion]\nmax_depth=256",
    ] {
        fs::write(&path, text).unwrap();
        assert!(
            ResolvedConfig::load(Some(&path)).is_err(),
            "accepted {text}"
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), text);
    }
    assert!(ResolvedConfig::load(Some(&root.path().join("missing.toml"))).is_err());
}

#[test]
fn selection_uses_one_file_and_default_absence_has_no_side_effects() {
    let root = tempfile::tempdir().unwrap();
    let default = root.path().join("default.toml");
    let legacy = root.path().join("legacy.toml");
    assert_eq!(
        selected_config(None, false, None, &default, &legacy)
            .unwrap()
            .0,
        None
    );
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
    fs::write(&legacy, "").unwrap();
    assert_eq!(
        selected_config(None, false, None, &default, &legacy)
            .unwrap()
            .0,
        Some(legacy.clone())
    );
    fs::write(&default, "").unwrap();
    let (selection, diagnostics) = selected_config(None, false, None, &default, &legacy).unwrap();
    assert_eq!(selection, Some(default.clone()));
    assert_eq!(diagnostics.len(), 1);
    let explicit = root.path().join("explicit.toml");
    let env = root.path().join("env.toml");
    assert_eq!(
        selected_config(Some(&explicit), false, Some(&env), &default, &legacy)
            .unwrap()
            .0,
        Some(explicit.clone())
    );
    assert_eq!(
        selected_config(None, false, Some(&env), &default, &legacy)
            .unwrap()
            .0,
        Some(env.clone())
    );
    assert_eq!(
        selected_config(None, true, Some(&env), &default, &legacy)
            .unwrap()
            .0,
        None
    );
}

#[test]
fn editing_preserves_comments_and_rejects_conflicts_invalid_values_and_links() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("config.toml");
    init_config(&path, false).unwrap();
    assert!(init_config(&path, true).is_err());
    let original = "# user heading\nschema_version=1\n[extraction.recursion]\nenabled=true # keep this comment\n";
    fs::write(&path, original).unwrap();
    edit_config(&path, "extraction.recursion.enabled", Some("false")).unwrap();
    let modified = fs::read_to_string(&path).unwrap();
    assert!(modified.contains("# user heading") && modified.contains("# keep this comment"));
    assert!(
        !ResolvedConfig::load(Some(&path))
            .unwrap()
            .values
            .extraction
            .recursion
            .enabled
    );
    assert!(edit_config(&path, "extraction.recursion.enabled", Some("'invalid'")).is_err());
    assert_eq!(fs::read_to_string(&path).unwrap(), modified);
    fs::write(path.with_extension("toml.lock"), "another writer").unwrap();
    assert!(edit_config(&path, "extraction.recursion.enabled", Some("true")).is_err());
    assert_eq!(fs::read_to_string(&path).unwrap(), modified);
    fs::remove_file(path.with_extension("toml.lock")).unwrap();
    edit_config(&path, "extraction.recursion.enabled", None).unwrap();
    assert!(
        ResolvedConfig::load(Some(&path))
            .unwrap()
            .values
            .extraction
            .recursion
            .enabled
    );
    #[cfg(unix)]
    {
        let link = root.path().join("linked.toml");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert!(ResolvedConfig::load(Some(&link)).is_ok());
        assert!(edit_config(&link, "state.history", Some("false")).is_err());
        assert!(fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink());
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o444)).unwrap();
        assert!(edit_config(&path, "state.history", Some("false")).is_err());
    }
}

#[test]
fn legacy_migration_is_explicit_and_keeps_a_backup() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("config.toml");
    let original =
        "# budgets\n[extraction]\nmax_files=12 # user limit\n[backends]\nauto_discover=false\n";
    fs::write(&path, original).unwrap();
    assert_eq!(
        ResolvedConfig::load(Some(&path))
            .unwrap()
            .values
            .limits
            .max_files,
        12
    );
    let preview = migrate_config(&path, false).unwrap();
    assert!(preview.contains("# user limit"));
    assert_eq!(fs::read_to_string(&path).unwrap(), original);
    migrate_config(&path, true).unwrap();
    assert_eq!(
        fs::read_to_string(path.with_extension("toml.v0.bak")).unwrap(),
        original
    );
    assert_eq!(
        ResolvedConfig::load(Some(&path))
            .unwrap()
            .values
            .limits
            .max_files,
        12
    );
    assert!(
        !ResolvedConfig::load(Some(&path))
            .unwrap()
            .values
            .backends
            .auto_discover
    );
}
