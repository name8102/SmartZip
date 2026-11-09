use smartzip_config::*;
use std::fs;

#[test]
fn precedence_preserves_false_empty_arrays_and_file_relative_paths() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("config.toml");
    fs::write(&path, "[extraction.recursion]\nenabled=false\n[passwords]\nsources=[]\n[state]\ndatabase='data.db'\n[[backends.installations]]\nid='local'\nfamily='seven-zip-library'\npath='./lib/7z.so'\n").unwrap();
    let mut resolved = ResolvedConfig::load(Some(&path)).unwrap();
    assert!(!resolved.values.extraction.recursion.enabled);
    assert_eq!(resolved.values.extraction.recursion.max_depth, None);
    assert!(resolved.values.passwords.sources.is_empty());
    assert_eq!(
        resolved.values.state.database,
        Some(root.path().join("data.db"))
    );
    assert_eq!(
        resolved.values.backends.installations[0].path,
        root.path().join("./lib/7z.so")
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
fn backend_library_bare_relative_path_is_resolved_from_config_directory() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("config.toml");
    fs::write(
        &path,
        "[[backends.installations]]\nid='local'\nfamily='seven-zip-library'\npath='7z.so'\n",
    )
    .unwrap();
    let resolved = ResolvedConfig::load(Some(&path)).unwrap();
    assert_eq!(
        resolved.values.backends.installations[0].path,
        root.path().join("7z.so")
    );
    assert_eq!(
        resolved.values.backends.installations[0].family,
        AdapterFamily::SevenZipLibrary
    );
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
        "[extraction]\nmax_files=20",
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
    assert_eq!(selected_config(None, false, None, &default).unwrap(), None);
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
    fs::write(&default, "").unwrap();
    assert_eq!(
        selected_config(None, false, None, &default).unwrap(),
        Some(default.clone())
    );
    let explicit = root.path().join("explicit.toml");
    let env = root.path().join("env.toml");
    assert_eq!(
        selected_config(Some(&explicit), false, Some(&env), &default).unwrap(),
        Some(explicit.clone())
    );
    assert_eq!(
        selected_config(None, false, Some(&env), &default).unwrap(),
        Some(env.clone())
    );
    assert_eq!(
        selected_config(None, true, Some(&env), &default).unwrap(),
        None
    );
}

#[test]
fn editing_preserves_comments_and_rejects_conflicts_invalid_values_and_links() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("config.toml");
    init_config(&path, false).unwrap();
    assert!(init_config(&path, true).is_err());
    let original = "# user heading\n[extraction.recursion]\nenabled=true # keep this comment\n";
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
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path.with_extension("toml.lock"))
        .unwrap();
    lock.try_lock().unwrap();
    assert!(edit_config(&path, "extraction.recursion.enabled", Some("true")).is_err());
    assert_eq!(fs::read_to_string(&path).unwrap(), modified);
    drop(lock);
    // A leftover lock file has no ownership after the writer exits.
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
fn batch_edit_validates_combined_output_and_preserves_unrelated_comments() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("config.toml");
    let original = "# user heading\nschema_version=1\n[state]\nhistory=false # keep history comment\n[extraction.recursion]\nenabled=true # keep recursion comment\n";
    fs::write(&path, original).unwrap();
    let patches = vec![
        (
            "extraction.output.destination".into(),
            Some("'directory'".into()),
        ),
        (
            "extraction.output.directory".into(),
            Some("'output'".into()),
        ),
        ("extraction.recursion.enabled".into(), Some("false".into())),
    ];
    edit_config_fields(&path, &patches).unwrap();
    let resolved = ResolvedConfig::load(Some(&path)).unwrap();
    assert_eq!(
        resolved.values.extraction.output.destination,
        Destination::Directory
    );
    assert_eq!(
        resolved.values.extraction.output.directory,
        Some(root.path().join("output"))
    );
    assert!(!resolved.values.extraction.recursion.enabled);
    let edited = fs::read_to_string(&path).unwrap();
    assert!(edited.starts_with(
        "# user heading\nschema_version=1\n[state]\nhistory=false # keep history comment\n"
    ));
    assert!(edited.contains("# keep recursion comment"));
    edit_config_fields(
        &path,
        &[
            ("extraction.output.directory".into(), None),
            ("extraction.output.destination".into(), None),
        ],
    )
    .unwrap();
    let resolved = ResolvedConfig::load(Some(&path)).unwrap();
    assert_eq!(
        resolved.values.extraction.output.destination,
        Destination::FirstInputParent
    );
    assert_eq!(resolved.values.extraction.output.directory, None);
}

#[test]
fn failed_batch_edits_preserve_entire_file_and_missing_file_stays_absent() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("config.toml");
    let valid = ("state.history".into(), Some("false".into()));
    assert!(edit_config_fields(&path, &[valid.clone()]).is_err());
    assert!(!path.exists());
    init_config(&path, false).unwrap();
    let original = fs::read(&path).unwrap();
    for invalid_patch in [
        (
            "extraction.output.destination".into(),
            Some("'directory'".into()),
        ),
        ("state.mode".into(), Some("'invalid'".into())),
        ("limits.max_files".into(), Some("[".into())),
        ("state.unknown".into(), Some("false".into())),
        ("schema_version".into(), Some("2".into())),
        valid.clone(),
    ] {
        assert!(edit_config_fields(&path, &[valid.clone(), invalid_patch]).is_err());
        assert_eq!(fs::read(&path).unwrap(), original);
    }
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path.with_extension("toml.lock"))
        .unwrap();
    lock.try_lock().unwrap();
    assert!(edit_config_fields(&path, &[valid]).is_err());
    assert_eq!(fs::read(&path).unwrap(), original);
}

#[test]
fn recursion_depth_is_optional_and_accepts_large_explicit_values() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("config.toml");
    init_config(&path, false).unwrap();
    assert_eq!(
        ResolvedConfig::load(Some(&path))
            .unwrap()
            .values
            .extraction
            .recursion
            .max_depth,
        None
    );
    edit_config(&path, "extraction.recursion.max_depth", Some("512")).unwrap();
    assert_eq!(
        ResolvedConfig::load(Some(&path))
            .unwrap()
            .values
            .extraction
            .recursion
            .max_depth,
        Some(512)
    );
    edit_config(&path, "extraction.recursion.max_depth", None).unwrap();
    assert_eq!(
        ResolvedConfig::load(Some(&path))
            .unwrap()
            .values
            .extraction
            .recursion
            .max_depth,
        None
    );
}
