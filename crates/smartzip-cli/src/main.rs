mod render;
use render::*;
mod commands;
use commands::*;
mod bootstrap;
use bootstrap::*;
mod command_requests;
use async_trait::async_trait;
use clap::{CommandFactory, FromArgMatches, Parser, Subcommand, ValueEnum};
use command_requests::{DetectCommand, ExtractCommand, ListCommand, TestCommand};
use smartzip_archive::BackendRouter;
use smartzip_core::{EncodingMode, TaskEvent};
use smartzip_db::{password::PasswordRepository, SmartZipDb};
use smartzip_engine::{
    EmbeddedSelectionChoice, EncodingConfirmationChoice, ExtractWorkflowRequest,
    FileAwareDetectResult, InspectRequest, InteractiveEmbeddedPrompter,
    InteractiveEncodingPrompter, InteractiveOutputPrompter, InteractivePasswordPrompter,
    ListArchiveRequest, OutputCollisionStrategy, SmartZipEngine,
};
use smartzip_passwords::{PasswordCandidateRequest, PasswordService};
use smartzip_platform::PlatformPaths;
use smartzip_scanner::{Confidence, ScanMode, ScannerConfig};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

const DEFAULT_RECURSION_LIMIT: u8 = smartzip_config::DEFAULT_RECURSION_DEPTH;

#[derive(Debug, Parser)]
#[command(name = "smartzip", version)]
#[command(about = "SmartZip cross-platform archive helper")]
struct Cli {
    /// Path to database file. Defaults to the platform data directory if not set.
    #[arg(long)]
    db: Option<PathBuf>,

    /// Path to the TOML routing configuration.
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    /// Ignore external TOML (does not disable state).
    #[arg(long, global = true, conflicts_with = "config")]
    no_config: bool,
    /// Do not read or write persistent runtime state.
    #[arg(long, global = true)]
    stateless: bool,
    #[arg(long = "set", global = true)]
    runtime_set: Vec<String>,
    #[arg(long, global = true)]
    no_recursive: bool,
    #[arg(long, global = true)]
    explain: bool,

    /// Force one configured/discovered backend adapter by ID.
    #[arg(long)]
    backend: Option<String>,

    /// Print router warnings and route diagnostics.
    #[arg(long)]
    verbose_routing: bool,

    #[command(flatten)]
    safety: SafetyOptions,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Clone, clap::Args)]
struct SafetyOptions {
    /// Maximum stored password candidates (manual passwords are tried first).
    #[arg(long, global = true, default_value_t = smartzip_config::DEFAULT_PASSWORD_LIMIT)]
    password_limit: usize,
    #[arg(skip)]
    defaults: smartzip_config::ExtractionLimits,
    #[arg(skip)]
    policy: Option<Arc<smartzip_engine::CompiledRunPolicy>>,

    /// Maximum total output entries, including directories and nested outputs (0 = unlimited).
    #[arg(long, global = true)]
    max_files: Option<u64>,
    /// Maximum cumulative output bytes (0 = unlimited).
    #[arg(long, global = true)]
    max_output_bytes: Option<u64>,
    /// Minimum free disk space (0 disables capacity queries).
    #[arg(long, global = true)]
    min_free_bytes: Option<u64>,
    #[arg(long, global = true)]
    max_nested_candidates: Option<usize>,
    /// Disable all prompts, even when stdin is a terminal.
    #[arg(long, global = true)]
    non_interactive: bool,
    /// Existing output policy. Ask becomes skip without an interactive terminal.
    #[arg(long, global = true, value_enum, default_value_t = ConflictArg::Ask)]
    on_conflict: ConflictArg,
    /// Suspicious names policy. Ask becomes skip without an interactive terminal.
    #[arg(long, global = true, value_enum, default_value_t = SuspiciousEncodingArg::Ask)]
    suspicious_encoding: SuspiciousEncodingArg,
}
impl SafetyOptions {
    fn limits(&self) -> smartzip_engine::budget::ExtractionLimits {
        smartzip_engine::budget::ExtractionLimits {
            max_files: self.max_files.unwrap_or(self.defaults.max_files),
            max_output_bytes: self
                .max_output_bytes
                .unwrap_or(self.defaults.max_output_bytes),
            min_free_bytes: self.min_free_bytes.unwrap_or(self.defaults.min_free_bytes),
            max_nested_candidates: self
                .max_nested_candidates
                .unwrap_or(self.defaults.max_nested_candidates),
        }
    }
}
#[derive(Debug, Clone, Copy, ValueEnum)]
enum ConflictArg {
    Ask,
    Skip,
    Overwrite,
    Rename,
}
#[derive(Debug, Clone, Copy, ValueEnum)]
enum SuspiciousEncodingArg {
    Ask,
    Skip,
    Accept,
}

#[derive(Debug)]
struct CommandExit(i32);
impl std::fmt::Display for CommandExit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "command exited with {}", self.0)
    }
}
impl std::error::Error for CommandExit {}
fn command_exit(code: i32) -> Result<(), Box<dyn std::error::Error>> {
    if code == 0 {
        Ok(())
    } else {
        Err(Box::new(CommandExit(code)))
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum EmbeddedModeArg {
    Auto,
    Ask,
    Largest,
    Aggressive,
    All,
    Ignore,
}

impl From<EmbeddedModeArg> for smartzip_core::EmbeddedScanMode {
    fn from(value: EmbeddedModeArg) -> Self {
        match value {
            EmbeddedModeArg::Auto => Self::Auto,
            EmbeddedModeArg::Ask => Self::Ask,
            EmbeddedModeArg::Largest => Self::Largest,
            EmbeddedModeArg::Aggressive => Self::Aggressive,
            EmbeddedModeArg::All => Self::All,
            EmbeddedModeArg::Ignore => Self::Ignore,
        }
    }
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Inspect or edit configuration without accessing archives, databases or backends.
    #[command(subcommand)]
    Config(ConfigCmd),

    /// Diagnose backend availability, versions, capabilities and database location.
    Doctor {
        #[arg(long)]
        json: bool,
    },
    /// Inspect archive format, encoding, embedded findings, and password requirements.
    #[command(visible_alias = "d")]
    Detect(DetectCommand),

    /// List archive entries using shared password and encoding resolution.
    #[command(visible_alias = "l")]
    List(ListCommand),

    /// Test archive groups and diagnose damaged volumes (exit: 0 all intact, 1 none, 2 mixed, 130 cancelled; argument errors also use 2).
    #[command(visible_alias = "t")]
    Test(TestCommand),

    /// Extract archives, optionally with nested scanning.
    #[command(visible_alias = "x")]
    Extract(ExtractCommand),

    /// Preview archive entry names under several encodings.
    #[command(name = "enc", alias = "encoding-preview")]
    EncodingPreview {
        path: PathBuf,

        /// Password to use when the archive requires one.
        #[arg(short = 'p', long)]
        password: Option<String>,

        #[arg(long)]
        json: bool,
    },

    /// Manage password database.
    #[command(subcommand, visible_alias = "pw")]
    Password(PasswordCmd),

    /// Inspect recorded task history. Defaults to recent tasks.
    #[command(visible_alias = "hist")]
    History {
        #[command(subcommand)]
        command: Option<HistoryCmd>,
    },
}

impl Command {
    fn json_output(&self) -> bool {
        match self {
            Self::Doctor { json } | Self::EncodingPreview { json, .. } => *json,
            Self::Detect(request) => request.json,
            Self::List(request) => request.json,
            Self::Test(request) => request.json,
            Self::Extract(request) => request.json,
            _ => false,
        }
    }
}

#[derive(Debug, Subcommand)]
enum ConfigCmd {
    Path,
    Init {
        #[arg(long)]
        full: bool,
    },
    Show {
        #[arg(long)]
        defaults: bool,
        #[arg(long)]
        effective: bool,
        #[arg(long)]
        sources: bool,
    },
    Get {
        key: String,
        #[arg(long)]
        sources: bool,
    },
    Set {
        key: String,
        value: String,
    },
    Unset {
        key: String,
    },
    Check,
    Migrate {
        #[arg(long, conflicts_with = "dry_run")]
        apply: bool,
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Debug, Subcommand)]
enum PasswordCmd {
    /// List passwords with statistics.
    List {
        #[arg(long)]
        json: bool,
        /// Only show top N passwords.
        #[arg(long, default_value = "50")]
        limit: usize,
    },

    /// Add a password to the database.
    Add {
        password: String,
        /// Source label for this password (e.g. "manual", "import").
        #[arg(long, default_value = "manual")]
        source: String,
        /// Pin this password so it always ranks at top.
        #[arg(long)]
        pin: bool,
    },

    /// Remove a password by id.
    Remove { id: i64 },

    /// Import passwords from a text file (one per line).
    Import {
        path: PathBuf,
        #[arg(long, default_value = "import")]
        source: String,
    },

    /// Export passwords to a text file.
    Export {
        #[arg(long)]
        path: Option<PathBuf>,
    },

    /// Remove low-value passwords (long-failed, unused, over limit).
    Cleanup {
        /// Keep at most this many passwords.
        #[arg(long, default_value = "500")]
        max_passwords: usize,
        /// Disable passwords that have not been used successfully in N days.
        #[arg(long)]
        stale_days: Option<u64>,
        /// Also apply cleanup, not just preview.
        #[arg(long)]
        apply: bool,
    },
}

#[derive(Debug, Subcommand)]
enum HistoryCmd {
    /// List recent tasks (operations), newest first.
    Tasks {
        #[arg(long)]
        json: bool,
        /// Show at most this many tasks.
        #[arg(long, default_value = "20")]
        limit: usize,
    },

    /// List recent per-file extraction actions, newest first.
    Files {
        #[arg(long)]
        json: bool,
        /// Show at most this many rows.
        #[arg(long, default_value = "50")]
        limit: usize,
        /// Filter by status (e.g. extracted / skipped / failed).
        #[arg(long)]
        status: Option<String>,
        /// Filter by reason (e.g. duplicate / wrong_password / password_required).
        #[arg(long)]
        reason: Option<String>,
    },

    /// Show a single task: its event timeline plus every file action logged.
    Show {
        task_id: String,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum ConfidenceArg {
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum DiagnoseArg {
    Auto,
    Off,
}

impl From<ConfidenceArg> for Confidence {
    fn from(value: ConfidenceArg) -> Self {
        match value {
            ConfidenceArg::Low => Self::Low,
            ConfidenceArg::Medium => Self::Medium,
            ConfidenceArg::High => Self::High,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum LayoutPolicyArg {
    Conservative,
    Smart,
    Raw,
    FlatSingle,
}

impl From<LayoutPolicyArg> for smartzip_engine::layout::OutputLayoutPolicy {
    fn from(value: LayoutPolicyArg) -> Self {
        match value {
            LayoutPolicyArg::Conservative => Self::Conservative,
            LayoutPolicyArg::Smart => Self::Smart,
            LayoutPolicyArg::Raw => Self::Raw,
            LayoutPolicyArg::FlatSingle => Self::FlatSingle,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum SingleRootNameArg {
    Auto,
    Archive,
    Inner,
    PreserveBoth,
}

impl From<SingleRootNameArg> for smartzip_engine::layout::SingleRootNamePolicy {
    fn from(value: SingleRootNameArg) -> Self {
        match value {
            SingleRootNameArg::Auto => Self::Auto,
            SingleRootNameArg::Archive => Self::PreferArchiveName,
            SingleRootNameArg::Inner => Self::PreferInnerName,
            SingleRootNameArg::PreserveBoth => Self::PreserveBoth,
        }
    }
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let matches = Cli::command().get_matches();
    let cli = Cli::from_arg_matches(&matches).expect("clap validated arguments");
    let json = cli.command.json_output();
    if let Err(error) = run(cli, &matches).await {
        if let Some(exit) = error.downcast_ref::<CommandExit>() {
            return std::process::ExitCode::from(exit.0 as u8);
        }
        let cancelled = matches!(
            error.downcast_ref::<smartzip_core::SmartZipError>(),
            Some(smartzip_core::SmartZipError::Cancelled)
        );
        let code = if cancelled { 130 } else { 1 };
        if json {
            println!(
                "{}",
                serde_json::json!({"schema_version": 1, "status": if cancelled { "cancelled" } else { "failed" }, "exit_code": code, "error": error.to_string()})
            );
        } else {
            eprintln!("error: {error}");
        }
        return std::process::ExitCode::from(code as u8);
    }
    std::process::ExitCode::SUCCESS
}

async fn run(mut cli: Cli, matches: &clap::ArgMatches) -> Result<(), Box<dyn std::error::Error>> {
    let (selected, default_path, diagnostics) = config_selection(&cli)?;
    if let Command::Config(command) = &cli.command {
        return config_command(command, selected.as_deref(), &default_path, &diagnostics);
    }
    let mut resolved = smartzip_config::ResolvedConfig::load(selected.as_deref())?;
    resolved.diagnostics.extend(diagnostics);
    apply_cli_overrides(&cli, matches, &mut resolved)?;
    let policy = smartzip_engine::CompiledRunPolicy::compile(resolved)?;
    let c = policy.values();
    match &mut cli.command {
        Command::List(request) => request.encoding = c.extraction.encoding.mode.clone(),
        Command::Test(request) => request.encoding = c.extraction.encoding.mode.clone(),
        Command::Extract(request) => request.encoding = c.extraction.encoding.mode.clone(),
        _ => {}
    }
    cli.safety.defaults = c.limits.clone();
    cli.safety.password_limit = c.passwords.database_limit;
    cli.safety.non_interactive = c.interaction.mode == smartzip_config::InteractionMode::Never;
    cli.safety.on_conflict = match c.extraction.output.on_conflict {
        smartzip_config::Conflict::Ask => ConflictArg::Ask,
        smartzip_config::Conflict::Skip => ConflictArg::Skip,
        smartzip_config::Conflict::Rename => ConflictArg::Rename,
        smartzip_config::Conflict::Overwrite => ConflictArg::Overwrite,
    };
    cli.safety.suspicious_encoding = match c.extraction.encoding.on_suspicious {
        smartzip_config::SuspiciousEncoding::Ask => SuspiciousEncodingArg::Ask,
        smartzip_config::SuspiciousEncoding::Skip => SuspiciousEncodingArg::Skip,
        smartzip_config::SuspiciousEncoding::Accept => SuspiciousEncodingArg::Accept,
    };
    if cli.explain || matches!(&cli.command, Command::Extract(request) if request.dry_run) {
        println!(
            "{}",
            serde_json::to_string_pretty(
                &serde_json::json!({"schema_version": 1, "configuration": policy.resolved(), "inactive": policy.resolved().explanation(), "stages": policy.stage_plan(), "dynamic_content": "unknown_until_run", "root_archives": "keep", "transaction": "staged"})
            )?
        );
        return Ok(());
    }
    if c.interaction.mode == smartzip_config::InteractionMode::Always {
        use std::io::IsTerminal;
        if !std::io::stdin().is_terminal() {
            return Err("interaction.mode=always requires an interactive terminal".into());
        }
    }
    cli.db = cli.db.or_else(|| c.state.database.clone());
    cli.safety.policy = Some(Arc::new(policy));
    let policy = cli.safety.policy.as_ref().unwrap();
    if !matches!(
        policy.values().logging.level,
        smartzip_config::LogLevel::Off | smartzip_config::LogLevel::Error
    ) {
        for diagnostic in &policy.resolved().diagnostics {
            eprintln!("configuration: {diagnostic}");
        }
    }
    // Management commands don't require discovering or starting archive backends.
    if let Command::Password(command) = &cli.command {
        if !policy.reads_state()
            || !policy.writes_state()
                && !matches!(
                    command,
                    PasswordCmd::List { .. }
                        | PasswordCmd::Export { .. }
                        | PasswordCmd::Cleanup { apply: false, .. }
                )
        {
            return Err("password command forbidden by state.mode".into());
        }
    }
    match cli.command {
        Command::Password(command) => {
            let db = open_state_db(cli.db, policy.values().state.mode)?;
            return password(&db, command);
        }
        Command::History { command } => {
            if !policy.reads_state() {
                return Err("history unavailable with state.mode=off".into());
            }
            let db = open_state_db(cli.db, policy.values().state.mode)?;
            return history(
                &db,
                command.unwrap_or(HistoryCmd::Tasks {
                    json: false,
                    limit: 20,
                }),
            );
        }
        _ => {}
    }
    let verbose_routing = cli.verbose_routing;
    let cancellation = tokio_util::sync::CancellationToken::new();
    let signal_token = cancellation.clone();
    let signal = tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            signal_token.cancel();
        }
    });
    let backend = build_backend(
        &policy.values().backends,
        cli.backend.as_deref(),
        verbose_routing,
    )?;

    let result = match cli.command {
        Command::Doctor { json } => {
            let db_path = if let Some(path) = cli.db {
                path
            } else {
                PlatformPaths::try_new()?.db_path()
            };
            let adapters = backend.diagnostics();
            let healthy = adapters
                .iter()
                .any(|a| a["family"] == "7z" && a["version"].is_string());
            let result = serde_json::json!({"schema_version": 1, "version": env!("CARGO_PKG_VERSION"), "database": db_path,
                "backends": adapters, "warnings": backend.warnings(), "status": if healthy { "completed" } else { "failed" },
                "exit_code": if healthy { 0 } else { 1 }, "extraction_limits": cli.safety.limits(), "scan_default_bytes": smartzip_scanner::DEFAULT_SCAN_BYTES,
                "scan_hard_limit_bytes": null, "root_scan_strategy": "bounded_metadata_scan_to_eof"});
            if json {
                println!("{}", serde_json::to_string_pretty(&result)?);
            } else {
                println!(
                    "SmartZip {}\nDatabase: {}",
                    env!("CARGO_PKG_VERSION"),
                    db_path.display()
                );
                for adapter in &adapters {
                    println!(
                        "{}: {} ({}) {}",
                        adapter["id"], adapter["version"], adapter["family"], adapter["executable"]
                    );
                }
                if !healthy {
                    eprintln!("7-Zip backend missing or unusable; install 7z or 7zz");
                }
            }
            command_exit(if healthy { 0 } else { 1 })
        }
        Command::Detect(request) => {
            let db = open_task_db(cli.db, policy)?;
            detect(
                &backend,
                db.as_ref(),
                request,
                verbose_routing,
                &cli.safety,
                cancellation.clone(),
            )
            .await
        }
        Command::List(request) => {
            let db = open_task_db(cli.db, policy)?;
            list_archive(
                &backend,
                db.as_ref(),
                request,
                verbose_routing,
                &cli.safety,
                cancellation.clone(),
            )
            .await
        }
        Command::Test(request) => {
            let db = open_task_db(cli.db, policy)?;
            test_archives(
                &backend,
                db.as_ref(),
                request,
                verbose_routing,
                &cli.safety,
                cancellation.clone(),
            )
            .await
        }
        Command::Extract(request) => {
            let db = open_task_db(cli.db, policy)?;
            extract(
                &backend,
                db.as_ref(),
                request,
                verbose_routing,
                &cli.safety,
                cancellation.clone(),
            )
            .await
        }
        Command::EncodingPreview {
            path,
            password,
            json,
        } => {
            let db = open_task_db(cli.db, policy)?;
            preview_encodings(
                &backend,
                db.as_ref(),
                &cli.safety,
                path,
                password,
                json,
                verbose_routing,
                cancellation.clone(),
            )
            .await
        }
        Command::Config(_) | Command::Password(_) | Command::History { .. } => {
            unreachable!("handled before creating backends")
        }
    };
    signal.abort();
    result
}

fn task_listener(
    json: bool,
    verbose: bool,
    safety: &SafetyOptions,
) -> Option<smartzip_engine::TaskEventListener> {
    use smartzip_config::LogLevel;
    let level = safety
        .policy
        .as_ref()
        .map(|p| p.values().logging.level)
        .unwrap_or(LogLevel::Info);
    if level == LogLevel::Off {
        return None;
    }
    let listener = routing_listener(json, verbose || level == LogLevel::Debug)?;
    Some(Arc::new(move |event| {
        let required = match event.kind {
            smartzip_core::TaskEventKind::Failed { .. } => 1,
            smartzip_core::TaskEventKind::Warning { .. } => 2,
            _ => 3,
        };
        let allowed = match level {
            LogLevel::Off => 0,
            LogLevel::Error => 1,
            LogLevel::Warn => 2,
            LogLevel::Info | LogLevel::Debug => 3,
        };
        if required <= allowed {
            listener(event);
        }
    }))
}

fn routing_listener(
    json: bool,
    verbose_routing: bool,
) -> Option<smartzip_engine::TaskEventListener> {
    (!json).then(|| {
        std::sync::Arc::new(move |event: &smartzip_core::TaskEvent| {
            render_extract_event(event, verbose_routing)
        }) as smartzip_engine::TaskEventListener
    })
}

#[derive(Debug)]
struct EncodingPreviewEntry {
    encoding: String,
    ok: bool,
    names: Vec<String>,
    error: Option<String>,
}

async fn preview_encodings(
    backend: &BackendRouter,
    db: Option<&SmartZipDb>,
    safety: &SafetyOptions,
    path: PathBuf,
    password: Option<String>,
    json: bool,
    verbose_routing: bool,
    cancellation: tokio_util::sync::CancellationToken,
) -> Result<(), Box<dyn std::error::Error>> {
    struct PreviewPrompter {
        delegate: StdinPrompter,
        last: Mutex<Option<String>>,
    }
    #[async_trait]
    impl InteractivePasswordPrompter for PreviewPrompter {
        async fn prompt(&self, path: &Path) -> Option<String> {
            let value = self.delegate.prompt(path).await;
            *self.last.lock().unwrap() = value.clone();
            value
        }
    }
    let candidates = encoding_preview_candidates();
    let mut previews = Vec::new();
    let service = task_passwords(db, safety);
    let engine = SmartZipEngine::default()
        .with_cancellation_token(cancellation.clone())
        .with_run_policy(safety.policy.as_ref().unwrap().as_ref().clone());
    let (_, mut known_store) = task_stores(db, safety);
    if let Some(store) = &mut known_store {
        store.writable = false;
    }
    let history_store = None;
    let recorder = run_stores(&history_store, &known_store);
    let control = StdinLock::configured(cancellation.clone(), safety, json);
    let prompt = PreviewPrompter {
        delegate: StdinPrompter {
            lock: control.clone(),
        },
        last: Mutex::new(None),
    };
    let mut manual: Vec<_> = password.into_iter().collect();

    for encoding in candidates {
        let mode = match *encoding {
            "auto" => EncodingMode::Auto,
            other => EncodingMode::Override(other.to_string()),
        };
        if cancellation.is_cancelled() {
            return Err(smartzip_core::SmartZipError::Cancelled.into());
        }
        let listing = engine
            .list_archive_with_listener_interactive(
                backend,
                &service,
                ListArchiveRequest {
                    path: path.clone(),
                    scanner: ScannerConfig::default(),
                    encoding_mode: mode,
                    password_candidates: PasswordCandidateRequest {
                        manual: manual.clone(),
                        include_empty: true,
                        limit: safety.password_limit,
                        ..Default::default()
                    },
                },
                control
                    .interactive
                    .then_some(&prompt as &dyn InteractivePasswordPrompter),
                None,
                task_listener(json, verbose_routing, safety),
                known_store
                    .as_ref()
                    .map(|_| &recorder as &dyn smartzip_engine::history::TaskHistoryRecorder),
            )
            .await;
        if listing.is_ok() {
            if let Some(value) = prompt.last.lock().unwrap().take() {
                manual.insert(0, value);
            }
        }
        match listing {
            Ok(listing) => previews.push(EncodingPreviewEntry {
                encoding: encoding.to_string(),
                ok: true,
                names: listing
                    .entries
                    .into_iter()
                    .map(|entry| entry.path.display().to_string())
                    .collect(),
                error: None,
            }),
            Err(error) => previews.push(EncodingPreviewEntry {
                encoding: encoding.to_string(),
                ok: false,
                names: Vec::new(),
                error: Some(error.to_string()),
            }),
        }
    }

    if cancellation.is_cancelled() {
        return Err(smartzip_core::SmartZipError::Cancelled.into());
    }
    let failed = previews.iter().all(|preview| !preview.ok);
    if json {
        let output: Vec<serde_json::Value> = previews
            .into_iter()
            .map(|preview| {
                serde_json::json!({
                    "encoding": preview.encoding,
                    "ok": preview.ok,
                    "names": preview.names,
                    "error": preview.error,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        for preview in previews {
            println!("[{}]", preview.encoding);
            if preview.ok {
                if preview.names.is_empty() {
                    println!("  (no entries)");
                } else {
                    for name in preview.names.iter().take(20) {
                        println!("  {name}");
                    }
                    if preview.names.len() > 20 {
                        println!("  ... {} more", preview.names.len() - 20);
                    }
                }
            } else if let Some(error) = preview.error {
                println!("  ERROR: {error}");
            }
        }
    }

    command_exit(if failed { 1 } else { 0 })
}

fn encoding_preview_candidates() -> &'static [&'static str] {
    &[
        "auto",
        "UTF-8",
        "GB18030",
        "GBK",
        "Big5",
        "Shift_JIS",
        "EUC-JP",
        "EUC-KR",
    ]
}

fn password(db: &SmartZipDb, cmd: PasswordCmd) -> Result<(), Box<dyn std::error::Error>> {
    let repo = PasswordRepository::new(db.connection());
    let service = PasswordService::new(repo);

    match cmd {
        PasswordCmd::List { json, limit } => {
            let repo = PasswordRepository::new(db.connection());
            let passwords = repo.ranked_candidates(limit)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&passwords)?);
            } else if passwords.is_empty() {
                println!("No passwords in database.");
            } else {
                println!(
                    "{:>6} {:>2} {:>4} {:>4} {:20} {:20} {:30}",
                    "id", "P", "ok", "fail", "last_ok", "last_fail", "value"
                );
                for p in &passwords {
                    let value = if p.value.len() > 30 {
                        format!("{}...", p.value.chars().take(27).collect::<String>())
                    } else {
                        p.value.clone()
                    };
                    println!(
                        "{:>6} {:>2} {:>4} {:>4} {:20} {:20} {}",
                        p.id,
                        if p.pinned { "*" } else { "" },
                        p.success_count,
                        p.failure_count,
                        p.last_success_at.as_deref().unwrap_or("-"),
                        p.last_failure_at.as_deref().unwrap_or("-"),
                        value
                    );
                }
            }
        }
        PasswordCmd::Add {
            password,
            source,
            pin,
        } => {
            let id = service.add_password(&password, &source, pin)?;
            println!("added password id={id}");
        }
        PasswordCmd::Remove { id } => {
            let repo = PasswordRepository::new(db.connection());
            repo.delete(id)?;
            println!("removed password id={id}");
        }
        PasswordCmd::Import { path, source } => {
            let reader = std::io::BufReader::new(std::fs::File::open(&path)?);
            let count = PasswordRepository::new(db.connection()).import_lines(reader, &source)?;
            println!("imported {count} password(s) from {}", path.display());
        }
        PasswordCmd::Export { path } => {
            let repo = PasswordRepository::new(db.connection());
            let passwords = repo.ranked_candidates(usize::MAX)?;
            let out_path = if let Some(path) = path {
                path
            } else {
                let paths = PlatformPaths::try_new()?;
                std::fs::create_dir_all(&paths.data_dir)?;
                paths.password_export_path()
            };
            let lines: Vec<String> = passwords.iter().map(|p| p.value.clone()).collect();
            std::fs::write(&out_path, lines.join("\n") + "\n")?;
            println!(
                "exported {} password(s) to {}",
                lines.len(),
                out_path.display()
            );
        }
        PasswordCmd::Cleanup {
            max_passwords,
            stale_days,
            apply,
        } => {
            let repo = PasswordRepository::new(db.connection());
            let all = repo.ranked_candidates(usize::MAX)?;
            let mut to_disable = Vec::new();

            for (idx, p) in all.iter().enumerate() {
                if idx >= max_passwords && !p.pinned {
                    to_disable.push(p.id);
                }
            }

            if let Some(days) = stale_days {
                let cutoff = chrono::Utc::now() - chrono::Duration::days(days as i64);
                let cutoff_str = cutoff.format("%Y-%m-%d %H:%M:%S").to_string();
                let already_disabled: std::collections::HashSet<i64> =
                    to_disable.iter().copied().collect();
                for p in all
                    .iter()
                    .filter(|p| !p.pinned && !already_disabled.contains(&p.id))
                {
                    let stale = match &p.last_success_at {
                        Some(ts) => ts < &cutoff_str,
                        None => true,
                    };
                    if stale {
                        to_disable.push(p.id);
                    }
                }
            }

            if apply {
                repo.disable_many(&to_disable)?;
                println!("cleanup applied: {} disabled", to_disable.len());
            } else {
                println!(
                    "cleanup preview: {} would be disabled. Use --apply to execute.",
                    to_disable.len()
                );
            }
        }
    }

    Ok(())
}

fn history(db: &SmartZipDb, cmd: HistoryCmd) -> Result<(), Box<dyn std::error::Error>> {
    use smartzip_db::file_extractions::FileExtractionRepository;
    use smartzip_db::task::TaskRepository;
    use smartzip_db::task_event::TaskEventRepository;

    match cmd {
        HistoryCmd::Tasks { limit, json } => {
            let tasks = TaskRepository::new(db.connection()).recent(limit)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&tasks)?);
            } else if tasks.is_empty() {
                println!("No task history recorded.");
            } else {
                for t in &tasks {
                    println!(
                        "{}  {:<8} {:<9} {}  {}",
                        t.id,
                        t.kind,
                        t.status,
                        t.started_at,
                        t.output_path.as_deref().unwrap_or("-"),
                    );
                }
            }
        }
        HistoryCmd::Files {
            limit,
            json,
            status,
            reason,
        } => {
            let repo = FileExtractionRepository::new(db.connection());
            let rows = match (status.as_deref(), reason.as_deref()) {
                (Some(s), Some(r)) => repo.list_by_status_and_reason(s, r, limit)?,
                (Some(s), None) => repo.list_by_status(s, limit)?,
                (None, Some(r)) => repo.list_by_reason(r, limit)?,
                (None, None) => repo.recent(limit)?,
            };
            if json {
                println!("{}", serde_json::to_string_pretty(&rows)?);
            } else if rows.is_empty() {
                println!("No file extraction history recorded.");
            } else {
                for r in &rows {
                    let offset = r.offset.map(|o| format!("@0x{o:X}")).unwrap_or_default();
                    let detail = match (&r.status, &r.reason) {
                        (s, Some(reason)) => format!("{s} ({reason})"),
                        (s, None) => s.clone(),
                    };
                    println!(
                        "{:<10} {} {}  enc={}  damaged={}  -> {}",
                        detail,
                        r.input_path,
                        offset,
                        r.encoding.as_deref().unwrap_or("-"),
                        r.damaged_volumes_json.as_deref().unwrap_or("-"),
                        r.output_path.as_deref().unwrap_or("-"),
                    );
                    print_history_test_report(r.test_report_json.as_deref());
                }
            }
        }
        HistoryCmd::Show { task_id, json } => {
            let task = TaskRepository::new(db.connection()).find_by_id(&task_id)?;
            let Some(task) = task else {
                return Err(format!("no task with id {task_id}").into());
            };
            let events = TaskEventRepository::new(db.connection()).list_by_task(&task_id)?;
            let files = FileExtractionRepository::new(db.connection()).list_by_task(&task_id)?;
            if json {
                let output = serde_json::json!({
                    "task": task,
                    "events": events,
                    "files": files,
                });
                println!("{}", serde_json::to_string_pretty(&output)?);
            } else {
                println!("Task {}", task.id);
                println!("  kind:      {}", task.kind);
                println!("  status:    {}", task.status);
                if let Some(output) = &task.output_path {
                    println!("  output:    {output}");
                }
                println!("  started:   {}", task.started_at);
                if let Some(finished) = &task.finished_at {
                    println!("  finished:  {finished}");
                }
                println!("  files:");
                for f in &files {
                    let detail = match &f.reason {
                        Some(reason) => format!("{} ({reason})", f.status),
                        None => f.status.clone(),
                    };
                    println!(
                        "    {:<10} {} -> {}",
                        detail,
                        f.input_path,
                        f.output_path.as_deref().unwrap_or("-"),
                    );
                    print_history_test_report(f.test_report_json.as_deref());
                }
                println!("  events:");
                for event in &events {
                    println!(
                        "    {}  [{}] {}: {}",
                        event.created_at, event.level, event.event_type, event.message,
                    );
                }
            }
        }
    }

    Ok(())
}

// ── Interactive password prompt via stdin ────────────────────────────────

/// Serializes access to stdin so only one interactive prompt reads at a time.
/// Prevents interleaved display when output-collision and password prompts
/// are both active concurrently (e.g. engine continues processing while an
/// output-collision prompt is pending, then a password prompt is triggered).
#[derive(Clone)]
struct StdinLock {
    gate: Arc<Mutex<()>>,
    cancellation: tokio_util::sync::CancellationToken,
    interactive: bool,
    conflict: ConflictArg,
    encoding: SuspiciousEncodingArg,
}
impl StdinLock {
    fn configured(
        cancellation: tokio_util::sync::CancellationToken,
        safety: &SafetyOptions,
        json: bool,
    ) -> Self {
        use std::io::IsTerminal;
        Self {
            gate: Arc::new(Mutex::new(())),
            cancellation,
            interactive: !json && !safety.non_interactive && std::io::stdin().is_terminal(),
            conflict: safety.on_conflict,
            encoding: safety.suspicious_encoding,
        }
    }

    fn for_policy(&self, policy: &smartzip_engine::CompiledRunPolicy, json: bool) -> Self {
        use std::io::IsTerminal;
        let c = policy.values();
        Self {
            gate: self.gate.clone(),
            cancellation: self.cancellation.clone(),
            interactive: !json
                && c.interaction.mode != smartzip_config::InteractionMode::Never
                && std::io::stdin().is_terminal(),
            conflict: match c.extraction.output.on_conflict {
                smartzip_config::Conflict::Ask => ConflictArg::Ask,
                smartzip_config::Conflict::Skip => ConflictArg::Skip,
                smartzip_config::Conflict::Rename => ConflictArg::Rename,
                smartzip_config::Conflict::Overwrite => ConflictArg::Overwrite,
            },
            encoding: match c.extraction.encoding.on_suspicious {
                smartzip_config::SuspiciousEncoding::Ask => SuspiciousEncodingArg::Ask,
                smartzip_config::SuspiciousEncoding::Skip => SuspiciousEncodingArg::Skip,
                smartzip_config::SuspiciousEncoding::Accept => SuspiciousEncodingArg::Accept,
            },
        }
    }
}

/// A blocking prompt with bounded waits. Ctrl+C wakes the token; no stdin
/// worker remains blocked when the async workflow is cancelled.
fn terminal_line(control: &StdinLock, hidden: bool) -> Option<String> {
    if !control.interactive || control.cancellation.is_cancelled() {
        return None;
    }
    #[cfg(unix)]
    {
        struct EchoGuard(Option<libc::termios>);
        impl Drop for EchoGuard {
            fn drop(&mut self) {
                if let Some(old) = &self.0 {
                    // SAFETY: stdin descriptor and saved termios remain valid.
                    unsafe {
                        libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, old);
                    }
                }
            }
        }
        let mut guard = EchoGuard(None);
        if hidden {
            let mut old = std::mem::MaybeUninit::<libc::termios>::uninit();
            // SAFETY: tcgetattr writes into allocated storage on success.
            if unsafe { libc::tcgetattr(libc::STDIN_FILENO, old.as_mut_ptr()) } != 0 {
                return None;
            }
            let old = unsafe { old.assume_init() };
            let mut quiet = old;
            quiet.c_lflag &= !libc::ECHO;
            if unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &quiet) } != 0 {
                return None;
            }
            guard.0 = Some(old);
        }
        let mut bytes = Vec::new();
        loop {
            if control.cancellation.is_cancelled() {
                return None;
            }
            let mut descriptor = libc::pollfd {
                fd: libc::STDIN_FILENO,
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: poll sees one live pollfd; read writes at most one byte.
            let ready = unsafe { libc::poll(&mut descriptor, 1, 50) };
            if ready < 0 {
                if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return None;
            }
            if ready == 0 {
                continue;
            }
            let mut byte = 0u8;
            let count = unsafe { libc::read(libc::STDIN_FILENO, (&mut byte as *mut u8).cast(), 1) };
            if count <= 0 {
                return None;
            }
            if byte == b'\n' {
                if bytes.last() == Some(&b'\r') {
                    bytes.pop();
                }
                if hidden {
                    eprintln!();
                }
                return String::from_utf8(bytes).ok();
            }
            if bytes.len() >= 64 * 1024 {
                return None;
            }
            bytes.push(byte);
        }
    }
    #[cfg(not(unix))]
    {
        let _ = hidden;
        None
    }
}

struct StdinPrompter {
    lock: StdinLock,
}

#[async_trait]
impl InteractivePasswordPrompter for StdinPrompter {
    async fn prompt(&self, archive_path: &Path) -> Option<String> {
        let control = self.lock.clone();
        let path = archive_path.to_path_buf();
        tokio::task::spawn_blocking(move || {
            let _guard = control.gate.lock().unwrap();
            prompt_password_stdin(&path, &control)
        })
        .await
        .unwrap_or(None)
    }
}

struct StdinOutputPrompter {
    lock: StdinLock,
}

#[async_trait]
impl InteractiveOutputPrompter for StdinOutputPrompter {
    async fn prompt(&self, archive_path: PathBuf, output_path: PathBuf) -> OutputCollisionStrategy {
        let control = self.lock.clone();
        tokio::task::spawn_blocking(move || {
            let _guard = control.gate.lock().unwrap();
            prompt_output_collision_stdin(&archive_path, &output_path, &control)
        })
        .await
        .unwrap_or(OutputCollisionStrategy::Skip)
    }
}

struct StdinEmbeddedPrompter {
    lock: StdinLock,
}

#[async_trait]
impl InteractiveEmbeddedPrompter for StdinEmbeddedPrompter {
    async fn prompt(
        &self,
        archive_path: &Path,
        decision: &smartzip_core::DetectionDecision,
    ) -> EmbeddedSelectionChoice {
        let control = self.lock.clone();
        let path = archive_path.to_path_buf();
        let decision = decision.clone();
        tokio::task::spawn_blocking(move || {
            let _guard = control.gate.lock().unwrap();
            prompt_embedded_stdin(&path, &decision, &control)
        })
        .await
        .unwrap_or(EmbeddedSelectionChoice::Skip)
    }
}

struct StdinEncodingPrompter {
    lock: StdinLock,
}

#[async_trait]
impl InteractiveEncodingPrompter for StdinEncodingPrompter {
    async fn prompt(
        &self,
        archive_path: &Path,
        context: &smartzip_engine::EncodingConfirmationContext,
    ) -> EncodingConfirmationChoice {
        let control = self.lock.clone();
        let path = archive_path.to_path_buf();
        let context = context.clone();
        tokio::task::spawn_blocking(move || {
            let _guard = control.gate.lock().unwrap();
            prompt_encoding_stdin(&path, &context, &control)
        })
        .await
        .unwrap_or(EncodingConfirmationChoice::SkipArchive)
    }
}

fn prompt_password_stdin(path: &Path, control: &StdinLock) -> Option<String> {
    use std::io::{self, Write};

    if !control.interactive || control.cancellation.is_cancelled() {
        return None;
    }

    eprint!(
        "\n  No matching password for \"{}\".\n  Enter password (or press Enter to skip): ",
        path.display()
    );
    let _ = io::stderr().flush();

    let pw = terminal_line(control, true)?;

    if pw.is_empty() {
        eprintln!("  (skipped)");
        None
    } else {
        Some(pw)
    }
}

fn prompt_output_collision_stdin(
    archive_path: &Path,
    output_path: &Path,
    control: &StdinLock,
) -> OutputCollisionStrategy {
    use std::io::{self, Write};

    if control.cancellation.is_cancelled() {
        return OutputCollisionStrategy::Skip;
    }
    match control.conflict {
        ConflictArg::Overwrite => return OutputCollisionStrategy::Overwrite,
        ConflictArg::Rename => return OutputCollisionStrategy::Rename,
        ConflictArg::Skip => return OutputCollisionStrategy::Skip,
        ConflictArg::Ask => {}
    }
    if !control.interactive {
        return OutputCollisionStrategy::Skip;
    }

    loop {
        eprint!(
            "\n  Output already exists for \"{}\": {}\n  Choose [s]kip, [o]verwrite, [r]ename: ",
            archive_path.display(),
            output_path.display()
        );
        let _ = io::stderr().flush();

        let Some(choice) = terminal_line(control, false) else {
            return OutputCollisionStrategy::Skip;
        };

        match choice.trim().to_ascii_lowercase().as_str() {
            "s" | "skip" => {
                eprintln!("  (skipped)");
                return OutputCollisionStrategy::Skip;
            }
            "o" | "overwrite" => return OutputCollisionStrategy::Overwrite,
            "r" | "rename" => return OutputCollisionStrategy::Rename,
            _ => {
                eprintln!("  Please enter s, o, or r.");
            }
        }
    }
}

fn prompt_embedded_stdin(
    path: &Path,
    decision: &smartzip_core::DetectionDecision,
    control: &StdinLock,
) -> EmbeddedSelectionChoice {
    use std::io::{self, Write};

    if !control.interactive || control.cancellation.is_cancelled() {
        return EmbeddedSelectionChoice::Skip;
    }

    loop {
        eprintln!("\n  Embedded archive decision required: {}", path.display());
        eprintln!(
            "  {} finding(s), reason: {}",
            decision.findings_summary.len(),
            decision.reason
        );
        eprint!("  Choose [e]xtract, [s]kip, [a]lways extract remaining ask findings: ");
        let _ = io::stderr().flush();

        let Some(choice) = terminal_line(control, false) else {
            return EmbeddedSelectionChoice::Skip;
        };

        match choice.trim().to_ascii_lowercase().as_str() {
            "e" | "extract" => return EmbeddedSelectionChoice::Extract,
            "s" | "skip" => return EmbeddedSelectionChoice::Skip,
            "a" | "always" => return EmbeddedSelectionChoice::ExtractAll,
            _ => eprintln!("  Please enter e, s, or a."),
        }
    }
}

fn prompt_encoding_stdin(
    path: &Path,
    context: &smartzip_engine::EncodingConfirmationContext,
    control: &StdinLock,
) -> EncodingConfirmationChoice {
    use std::io::{self, Write};

    if control.cancellation.is_cancelled() {
        return EncodingConfirmationChoice::SkipArchive;
    }
    match control.encoding {
        SuspiciousEncodingArg::Accept => return EncodingConfirmationChoice::AcceptDetected,
        SuspiciousEncodingArg::Skip => return EncodingConfirmationChoice::SkipArchive,
        SuspiciousEncodingArg::Ask => {}
    }
    if !control.interactive {
        return EncodingConfirmationChoice::SkipArchive;
    }

    let detected = match &context.detected.selected {
        smartzip_core::EncodingMode::Auto => "auto".to_string(),
        smartzip_core::EncodingMode::Override(value) => value.clone(),
    };

    loop {
        eprintln!(
            "\n  ZIP filename encoding looks suspicious: {}",
            path.display()
        );
        eprintln!("  detected: {detected}");
        if !context.suspicious_reasons.is_empty() {
            eprintln!("  reasons: {}", context.suspicious_reasons.join(", "));
        }
        for preview in &context.preview_names {
            eprintln!("  preview: {preview}");
        }
        eprint!("  Choose [Enter] accept, [m]anual encoding, [s]kip archive: ");
        let _ = io::stderr().flush();

        let Some(choice) = terminal_line(control, false) else {
            return EncodingConfirmationChoice::SkipArchive;
        };
        match choice.trim() {
            "" => return EncodingConfirmationChoice::AcceptDetected,
            value if value.eq_ignore_ascii_case("s") || value.eq_ignore_ascii_case("skip") => {
                return EncodingConfirmationChoice::SkipArchive;
            }
            value if value.eq_ignore_ascii_case("m") || value.eq_ignore_ascii_case("manual") => {
                eprint!("  Enter encoding name: ");
                let _ = io::stderr().flush();
                let Some(encoding) = terminal_line(control, false) else {
                    return EncodingConfirmationChoice::SkipArchive;
                };
                let encoding = encoding.trim();
                if !encoding.is_empty() {
                    return EncodingConfirmationChoice::Override(encoding.to_string());
                }
            }
            other => return EncodingConfirmationChoice::Override(other.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        build_extract_json_output, Arc, Cli, Command, ConflictArg, StdinLock, SuspiciousEncodingArg,
    };
    use clap::Parser;
    use serde_json::json;
    use smartzip_core::TaskId;
    use smartzip_engine::ExtractWorkflowResult;

    #[test]
    fn recovered_prompt_policy_keeps_host_gate_and_uses_its_own_decisions() {
        let safety =
            Cli::try_parse_from(["smartzip", "--non-interactive", "extract", "archive.zip"])
                .unwrap()
                .safety;
        let lock =
            StdinLock::configured(tokio_util::sync::CancellationToken::new(), &safety, false);
        let mut resolved = smartzip_config::ResolvedConfig::load(None).unwrap();
        resolved.values.extraction.output.on_conflict = smartzip_config::Conflict::Overwrite;
        resolved.values.extraction.encoding.on_suspicious =
            smartzip_config::SuspiciousEncoding::Accept;
        let policy = smartzip_engine::CompiledRunPolicy::compile(resolved).unwrap();
        let recovered = lock.for_policy(&policy, true);
        assert!(Arc::ptr_eq(&recovered.gate, &lock.gate));
        assert!(
            !recovered.interactive,
            "JSON mode must not ask for terminal input"
        );
        assert!(matches!(recovered.conflict, ConflictArg::Overwrite));
        assert!(matches!(recovered.encoding, SuspiciousEncodingArg::Accept));
        lock.cancellation.cancel();
        assert!(recovered.cancellation.is_cancelled());
    }

    #[test]
    fn test_alias_accepts_groups_and_validates_the_diagnostic_budget() {
        for name in ["test", "t"] {
            let cli =
                Cli::try_parse_from(["smartzip", name, "a.part2.rar", "b.zip", "--json"]).unwrap();
            assert!(matches!(cli.command, Command::Test(super::TestCommand {
                paths, diagnose: super::DiagnoseArg::Auto, diagnostic_timeout: None, json: true, ..
            }) if paths.len() == 2));
            assert!(
                Cli::try_parse_from(["smartzip", name, "a.zip", "--diagnostic-timeout", "0"])
                    .is_err()
            );
        }
        let cli = Cli::try_parse_from([
            "smartzip",
            "t",
            "a.zip",
            "--diagnose",
            "off",
            "--diagnostic-timeout",
            "5",
            "--no-history",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Command::Test(super::TestCommand {
                diagnose: super::DiagnoseArg::Off,
                diagnostic_timeout: Some(5),
                no_history: true,
                ..
            })
        ));
    }

    #[test]
    fn exit_codes_use_errors_and_cancellation_not_benign_skips() {
        use smartzip_engine::history::TaskCompletionStatus as Status;
        for (success, errors, cancelled, code) in [
            (0, 0, false, 0),
            (2, 0, false, 0),
            (2, 1, false, 2),
            (0, 1, false, 1),
            (2, 1, true, 130),
        ] {
            assert_eq!(
                Status::from_counts(success, errors, cancelled).exit_code(),
                code
            );
        }
    }

    #[test]
    fn extract_json_preserves_partial_results_and_status() {
        let candidate = smartzip_engine::ExtractionCandidate {
            path: "good.zip".into(),
            relative_path: "good".into(),
            depth: 0,
            source: smartzip_engine::CandidateSource::RootInput,
            detected_format: Some(smartzip_core::ArchiveFormat::Zip),
            embedded_offset: None,
            embedded_size: None,
        };
        let failed = smartzip_engine::ExtractionCandidate {
            path: "bad.zip".into(),
            relative_path: "bad".into(),
            ..candidate.clone()
        };
        let result = ExtractWorkflowResult {
            status: smartzip_engine::history::TaskCompletionStatus::Partial,
            failed_count: 1,
            task_id: TaskId::new(),
            processed: vec![candidate],
            skipped: vec![failed],
            enqueued: Vec::new(),
            events: Vec::new(),
        };

        let output = build_extract_json_output(&result);
        assert_eq!(output["processed_count"], json!(1));
        assert_eq!(output["skipped_count"], json!(1));
        assert_eq!(output["processed"][0]["path"], "good.zip");
        assert_eq!(output["skipped"][0]["path"], "bad.zip");
        assert_eq!(output["failed_count"], 1);
        assert_eq!(output["status"], "partial");
        assert_eq!(output["exit_code"], 2);
        assert_eq!(output["task_id"], json!(result.task_id));
    }
}
