//! Configuration and caller-owned runtime dependency construction.
use super::*;

pub(super) fn config_selection(
    cli: &Cli,
) -> Result<(Option<PathBuf>, PathBuf, Vec<String>), Box<dyn std::error::Error>> {
    if cli.no_config && !matches!(cli.command, Command::Config(_)) {
        return Ok((None, PathBuf::new(), Vec::new()));
    }
    let environment = std::env::var_os("SMARTZIP_CONFIG").map(PathBuf::from);
    if let Some(path) = cli
        .config
        .as_ref()
        .or(environment.as_ref())
        .filter(|_| !cli.no_config)
    {
        return Ok((Some(path.clone()), path.clone(), Vec::new()));
    }
    let paths = PlatformPaths::try_new()?;
    let legacy = PlatformPaths::legacy()?;
    let default = paths.config_path();
    let (selected, warnings) = smartzip_config::selected_config(
        None,
        cli.no_config,
        None,
        &default,
        &legacy.config_path(),
    )?;
    Ok((selected, default, warnings))
}

pub(super) fn config_command(
    command: &ConfigCmd,
    selected: Option<&Path>,
    default: &Path,
    diagnostics: &[String],
) -> Result<(), Box<dyn std::error::Error>> {
    let path = selected.unwrap_or(default);
    match command {
        ConfigCmd::Path => println!(
            "{}",
            serde_json::json!({"selected": selected, "default": default, "diagnostics": diagnostics, "managed": std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink() || m.permissions().readonly())})
        ),
        ConfigCmd::Init { full } => {
            smartzip_config::init_config(path, *full)?;
            println!("created {}", path.display());
        }
        ConfigCmd::Set { key, value } => smartzip_config::edit_config(path, key, Some(value))?,
        ConfigCmd::Unset { key } => smartzip_config::edit_config(path, key, None)?,
        ConfigCmd::Migrate { apply, .. } => {
            println!("{}", smartzip_config::migrate_config(path, *apply)?)
        }
        _ => {
            let defaults = matches!(command, ConfigCmd::Show { defaults: true, .. });
            let mut resolved =
                smartzip_config::ResolvedConfig::load(if defaults { None } else { selected })?;
            if !defaults {
                resolved.diagnostics.extend_from_slice(diagnostics);
            }
            match command {
                ConfigCmd::Show { sources: true, .. } => println!(
                    "{}",
                    serde_json::to_string_pretty(
                        &serde_json::json!({"values": resolved.values, "origins": resolved.origins, "inactive": resolved.explanation(), "diagnostics": resolved.diagnostics})
                    )?
                ),
                ConfigCmd::Show { .. } => println!("{}", toml::to_string_pretty(&resolved.values)?),
                ConfigCmd::Get { key, sources } => {
                    let value = toml::Value::try_from(&resolved.values)?;
                    let value = smartzip_config::value_at(&value, key)
                        .ok_or_else(|| format!("unknown or unset configuration key: {key}"))?;
                    if *sources {
                        println!(
                            "{}",
                            serde_json::json!({"key": key, "value": value, "source": resolved.origins.get(key), "inactive": resolved.explanation().get(key)})
                        );
                    } else {
                        println!("{value}");
                    }
                }
                ConfigCmd::Check => println!(
                    "configuration valid (schema/defaults {}/{})",
                    resolved.values.schema_version, resolved.values.defaults_version
                ),
                _ => unreachable!(),
            }
        }
    }
    Ok(())
}

pub(super) fn apply_cli_overrides(
    cli: &Cli,
    matches: &clap::ArgMatches,
    resolved: &mut smartzip_config::ResolvedConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    let sub = matches.subcommand().map(|(_, m)| m).unwrap_or(matches);
    let explicit = |id: &str| -> Option<String> {
        [sub, matches]
            .into_iter()
            .find(|m| {
                m.try_contains_id(id).unwrap_or(false)
                    && m.value_source(id) == Some(clap::parser::ValueSource::CommandLine)
            })
            .and_then(|m| m.get_raw(id))
            .and_then(|mut values| values.next().map(|v| v.to_string_lossy().into_owned()))
    };
    let mut patches: Vec<(String, toml::Value)> = Vec::new();
    for (id, key) in [
        ("password_limit", "passwords.database_limit"),
        ("recursion_limit", "extraction.recursion.max_depth"),
        ("max_files", "limits.max_files"),
        ("max_output_bytes", "limits.max_output_bytes"),
        ("min_free_bytes", "limits.min_free_bytes"),
        ("max_nested_candidates", "limits.max_nested_candidates"),
    ] {
        if let Some(value) = explicit(id) {
            patches.push((key.into(), toml::Value::Integer(value.parse()?)));
        }
    }
    for (id, key) in [
        ("layout", "extraction.output.layout"),
        ("single_root_name", "extraction.output.single_root_name"),
        ("encoding", "extraction.encoding.mode"),
        ("on_conflict", "extraction.output.on_conflict"),
        ("suspicious_encoding", "extraction.encoding.on_suspicious"),
    ] {
        if let Some(value) = explicit(id) {
            patches.push((key.into(), value.into()));
        }
    }
    if let Some(value) = explicit("dominant_min_ratio") {
        patches.push((
            "extraction.embedded.dominant_min_ratio".into(),
            toml::Value::Float(value.parse()?),
        ));
    }
    for (id, key, value) in [
        ("stateless", "state.mode", toml::Value::String("off".into())),
        ("non_interactive", "interaction.mode", "never".into()),
        ("no_recursive", "extraction.recursion.enabled", false.into()),
        ("no_history", "state.history", false.into()),
    ] {
        if explicit(id).is_some() {
            patches.push((key.into(), value));
        }
    }
    if explicit("verbose_routing").is_some() {
        patches.push(("logging.level".into(), "debug".into()));
    }
    if let Some(value) = explicit("embedded") {
        let root = match value.as_str() {
            "ignore" => "off",
            "aggressive" => "all",
            other => other,
        };
        patches.push(("extraction.embedded.root".into(), root.into()));
        patches.push((
            "extraction.embedded.nested".into(),
            if value == "ignore" {
                "off".into()
            } else {
                value.into()
            },
        ));
    }
    if explicit("no_empty").is_some() {
        let sources: Vec<_> = resolved
            .values
            .passwords
            .sources
            .iter()
            .filter(|s| **s != smartzip_config::PasswordSource::Empty)
            .collect();
        patches.push(("passwords.sources".into(), toml::Value::try_from(sources)?));
    }
    if let Some(value) = explicit("output") {
        patches.push(("extraction.output.destination".into(), "directory".into()));
        patches.push((
            "extraction.output.directory".into(),
            std::path::absolute(value)?
                .to_string_lossy()
                .into_owned()
                .into(),
        ));
    }
    if let Some(path) = &cli.db {
        patches.push((
            "state.database".into(),
            std::path::absolute(path)?
                .to_string_lossy()
                .into_owned()
                .into(),
        ));
    }
    for assignment in &cli.runtime_set {
        let (key, value) = assignment
            .split_once('=')
            .ok_or("--set requires KEY=TOML_VALUE")?;
        let parsed: toml::Value = format!("value = {value}").parse()?;
        if parsed.as_table().is_none_or(|t| t.len() != 1) {
            return Err("--set requires one TOML literal".into());
        }
        patches.push((key.trim().into(), parsed["value"].clone()));
    }
    if explicit("use_clipboard").is_some() {
        return Err("unsupported_option: clipboard source is not implemented".into());
    }
    resolved.apply(&patches)?;
    if resolved.values.passwords.mode == smartzip_config::PasswordMode::Off
        && explicit("password").is_some()
    {
        return Err("explicit password conflicts with passwords.mode=off".into());
    }
    if let Some(adapter) = &cli.backend {
        if resolved
            .values
            .backends
            .installations
            .iter()
            .any(|b| &b.id == adapter && !b.enabled)
        {
            return Err(format!("forced backend {adapter} is disabled").into());
        }
    }
    Ok(())
}

pub(super) fn task_passwords<'a>(
    db: Option<&'a SmartZipDb>,
    safety: &SafetyOptions,
) -> PasswordService<'a> {
    let c = safety.policy.as_ref().unwrap().values();
    PasswordService::configured(
        db.map(|db| PasswordRepository::new(db.connection())),
        c.passwords.clone(),
        c.state.mode,
    )
}

pub(super) fn task_stores<'a>(
    db: Option<&'a SmartZipDb>,
    safety: &SafetyOptions,
) -> (
    Option<smartzip_engine::history::DbTaskHistoryRecorder<'a>>,
    Option<smartzip_engine::history::DbKnownFileStore<'a>>,
) {
    use smartzip_config::StateMode;
    let c = safety.policy.as_ref().unwrap().values();
    let history = db.filter(|_| c.state.mode != StateMode::Off).map(|db| {
        smartzip_engine::history::DbTaskHistoryRecorder::new(db.connection())
            .with_writes(c.state.mode == StateMode::ReadWrite && c.state.history)
    });
    let known = db
        .filter(|_| c.state.mode != StateMode::Off && c.state.known_files != StateMode::Off)
        .map(|db| smartzip_engine::history::DbKnownFileStore {
            connection: db.connection(),
            writable: c.state.mode == StateMode::ReadWrite
                && c.state.known_files == StateMode::ReadWrite,
            password_hint: c.extraction.reuse.password_hint
                && c.passwords.mode == smartzip_config::PasswordMode::Auto
                && c.passwords
                    .sources
                    .contains(&smartzip_config::PasswordSource::Known),
            encoding_hint: c.extraction.reuse.encoding_hint && c.extraction.encoding.mode == "auto",
        });
    (history, known)
}

pub(super) fn run_stores<'a>(
    history: &'a Option<smartzip_engine::history::DbTaskHistoryRecorder<'_>>,
    known: &'a Option<smartzip_engine::history::DbKnownFileStore<'_>>,
) -> smartzip_engine::history::RunStores<'a> {
    smartzip_engine::history::RunStores {
        history: history
            .as_ref()
            .map(|s| s as &dyn smartzip_engine::history::TaskHistoryRecorder),
        known_files: known
            .as_ref()
            .map(|s| s as &dyn smartzip_engine::history::KnownFileStore),
    }
}

pub(super) fn build_backend(
    config: &smartzip_config::BackendConfig,
    forced_adapter: Option<&str>,
    verbose_routing: bool,
) -> Result<BackendRouter, Box<dyn std::error::Error>> {
    let mut backend = BackendRouter::from_config(config)?;
    if let Some(adapter) = forced_adapter {
        backend = backend.with_forced_adapter(adapter);
    }
    if verbose_routing {
        for warning in backend.warnings() {
            eprintln!("routing warning: {warning}");
        }
        eprintln!("routing adapters: {}", backend.adapter_ids().join(", "));
    }
    Ok(backend)
}

pub(super) fn open_task_db(
    path: Option<PathBuf>,
    policy: &smartzip_engine::CompiledRunPolicy,
) -> Result<Option<SmartZipDb>, Box<dyn std::error::Error>> {
    if policy.needs_database() {
        Ok(Some(open_state_db(path, policy.values().state.mode)?))
    } else {
        Ok(None)
    }
}

pub(super) fn open_state_db(
    path: Option<PathBuf>,
    mode: smartzip_config::StateMode,
) -> Result<SmartZipDb, Box<dyn std::error::Error>> {
    let path = if let Some(path) = path {
        path
    } else {
        let (path, diagnostic) =
            PlatformPaths::try_new()?.select_database(&PlatformPaths::legacy()?)?;
        if let Some(message) = diagnostic {
            eprintln!("{message}");
        }
        path
    };
    let db = if mode == smartzip_config::StateMode::ReadOnly {
        SmartZipDb::open_read_only(&path)?
    } else {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        SmartZipDb::open(&path)?
    };

    Ok(db)
}
