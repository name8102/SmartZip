use async_trait::async_trait;
use clap::{Parser, Subcommand, ValueEnum};
use smartzip_archive::{ArchiveExecutor, BackendRouter};
use smartzip_config::SmartZipConfig;
use smartzip_core::{EncodingMode, TaskEvent, TaskEventSink, TaskId};
use smartzip_db::{password::PasswordRepository, SmartZipDb};
use smartzip_engine::name_score;
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

const DEFAULT_RECURSION_LIMIT: u8 = 3;

#[derive(Debug, Parser)]
#[command(name = "smartzip", version)]
#[command(about = "SmartZip cross-platform archive helper")]
struct Cli {
    /// Path to database file. Defaults to the platform data directory if not set.
    #[arg(long)]
    db: Option<PathBuf>,

    /// Path to the TOML routing configuration.
    #[arg(long)]
    config: Option<PathBuf>,

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
    #[arg(long, global = true, default_value_t = 128)]
    password_limit: usize,
    #[arg(skip)]
    defaults: smartzip_config::ExtractionLimits,

    /// Maximum total output entries, including directories and nested outputs.
    #[arg(long, global = true)]
    max_files: Option<u64>,
    #[arg(long, global = true)]
    max_output_bytes: Option<u64>,
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
    /// Diagnose backend availability, versions, capabilities and database location.
    Doctor {
        #[arg(long)]
        json: bool,
    },
    /// Inspect archive format, encoding, embedded findings, and password requirements.
    #[command(visible_alias = "d")]
    Detect {
        path: PathBuf,

        #[arg(long)]
        deep: bool,

        #[arg(long)]
        json: bool,

        #[arg(long)]
        max_scan_bytes: Option<u64>,

        #[arg(long, value_enum, default_value_t = ConfidenceArg::Medium)]
        min_confidence: ConfidenceArg,
    },

    /// List archive entries using shared password and encoding resolution.
    #[command(visible_alias = "l")]
    List {
        path: PathBuf,

        /// Password to try first. May be repeated.
        #[arg(short = 'p', long)]
        password: Vec<String>,

        /// Skip empty password attempt.
        #[arg(long)]
        no_empty: bool,

        /// Encoding for entry names: "auto", "UTF-8", "GB18030", "GBK", "Big5", "Shift_JIS", "EUC-JP", "EUC-KR".
        #[arg(long, default_value = "auto")]
        encoding: String,

        /// Print several candidate encodings, then prompt once for one to use.
        #[arg(long)]
        pick_encoding: bool,

        #[arg(long)]
        json: bool,

        #[arg(long)]
        deep: bool,

        #[arg(long)]
        max_scan_bytes: Option<u64>,

        #[arg(long, value_enum, default_value_t = ConfidenceArg::Medium)]
        min_confidence: ConfidenceArg,
    },

    /// Test archive groups and diagnose damaged volumes (exit: 0 all intact, 1 none, 2 mixed, 130 cancelled; argument errors also use 2).
    #[command(visible_alias = "t")]
    Test {
        #[arg(required = true)]
        paths: Vec<PathBuf>,

        /// Additional read-only diagnosis after a failed test.
        #[arg(long, value_enum, default_value_t = DiagnoseArg::Auto)]
        diagnose: DiagnoseArg,

        /// Time budget in seconds for additional diagnosis only.
        #[arg(long, value_parser = clap::value_parser!(u64).range(1..))]
        diagnostic_timeout: Option<u64>,

        /// Do not save this test in task history.
        #[arg(long)]
        no_history: bool,

        /// Password to try first. May be repeated.
        #[arg(short = 'p', long)]
        password: Vec<String>,

        /// Read password from clipboard (platform-dependent placeholder).
        #[arg(long)]
        use_clipboard: bool,

        /// Skip empty password attempt.
        #[arg(long)]
        no_empty: bool,

        /// Encoding for entry names: "auto", "UTF-8", "GB18030", "GBK", "Big5", "Shift_JIS", "EUC-JP", "EUC-KR".
        #[arg(long, default_value = "auto")]
        encoding: String,

        #[arg(long)]
        json: bool,

        #[arg(long)]
        deep: bool,

        #[arg(long)]
        max_scan_bytes: Option<u64>,

        #[arg(long, value_enum, default_value_t = ConfidenceArg::Medium)]
        min_confidence: ConfidenceArg,
    },

    /// Extract archives, optionally with nested scanning.
    #[command(visible_alias = "x")]
    Extract {
        paths: Vec<PathBuf>,

        /// Output directory. Defaults to first archive's parent directory.
        #[arg(short, long)]
        output: Option<PathBuf>,

        /// Maximum nested archive depth.
        #[arg(long, default_value_t = DEFAULT_RECURSION_LIMIT)]
        recursion_limit: u8,

        /// Password to try first. May be repeated.
        #[arg(short = 'p', long)]
        password: Vec<String>,

        /// Skip empty password attempt.
        #[arg(long)]
        no_empty: bool,

        /// Use deep scan for nested archives.
        #[arg(long)]
        deep: bool,

        #[arg(long)]
        max_scan_bytes: Option<u64>,

        /// Encoding for entry names: "auto", "UTF-8", "GB18030", "GBK", "Big5", "Shift_JIS", "EUC-JP", "EUC-KR".
        #[arg(long, default_value = "auto")]
        encoding: String,

        #[arg(long)]
        json: bool,

        /// Output layout policy: "conservative", "smart", "raw", "flat-single".
        #[arg(long, default_value = "conservative", value_enum)]
        layout: LayoutPolicyArg,

        /// Single root name policy: "auto", "archive", "inner", "preserve-both".
        #[arg(long, default_value = "auto", value_enum)]
        single_root_name: SingleRootNameArg,

        /// Show planned output without extracting.
        #[arg(long)]
        dry_run: bool,

        /// Embedded scan mode: "auto", "ask", "largest", "aggressive", "all", "ignore".
        #[arg(long, default_value = "auto")]
        embedded: EmbeddedModeArg,

        /// Minimum ratio for a finding to be considered dominant (0.0-1.0).
        #[arg(long, default_value_t = 0.70)]
        dominant_min_ratio: f32,

        /// Auto-confirm large file scans (>10GB).
        #[arg(long)]
        confirm_large_scan: bool,

        /// Do not record this extraction in the task history tables.
        /// Password statistics are still updated.
        #[arg(long)]
        no_history: bool,

        /// Re-extract even if this file was already extracted recently,
        /// bypassing the known_files dedup window.
        #[arg(long)]
        force: bool,
    },

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
    let cli = Cli::parse();
    let json = matches!(
        &cli.command,
        Command::Doctor { json: true }
            | Command::Detect { json: true, .. }
            | Command::List { json: true, .. }
            | Command::Test { json: true, .. }
            | Command::Extract { json: true, .. }
            | Command::EncodingPreview { json: true, .. }
    );
    if let Err(error) = run(cli).await {
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

async fn run(mut cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(path) = &cli.config {
        cli.safety.defaults = SmartZipConfig::load(path)?.extraction;
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
        cli.config.as_deref(),
        cli.backend.as_deref(),
        verbose_routing,
    )?;

    let result = match cli.command {
        Command::Doctor { json } => {
            let db_path = cli.db.unwrap_or_else(|| PlatformPaths::new().db_path());
            let adapters = backend.diagnostics();
            let healthy = adapters
                .iter()
                .any(|a| a["family"] == "7z" && a["version"].is_string());
            let result = serde_json::json!({"schema_version": 1, "version": env!("CARGO_PKG_VERSION"), "database": db_path,
                "backends": adapters, "warnings": backend.warnings(), "status": if healthy { "completed" } else { "failed" },
                "exit_code": if healthy { 0 } else { 1 }, "extraction_limits": cli.safety.limits(), "scan_default_bytes": smartzip_scanner::DEFAULT_SCAN_BYTES,
                "scan_hard_limit_bytes": smartzip_scanner::MAX_SCAN_BYTES});
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
        Command::Detect {
            path,
            deep,
            json,
            max_scan_bytes,
            min_confidence,
        } => {
            let db = open_db(cli.db)?;
            detect(
                &backend,
                &db,
                path,
                deep,
                json,
                max_scan_bytes,
                min_confidence,
                verbose_routing,
                &cli.safety,
                cancellation.clone(),
            )
            .await
        }
        Command::List {
            path,
            password: manual_passwords,
            no_empty,
            encoding,
            pick_encoding,
            json,
            deep,
            max_scan_bytes,
            min_confidence,
        } => {
            let db = open_db(cli.db)?;
            list_archive(
                &backend,
                &db,
                path,
                manual_passwords,
                no_empty,
                &encoding,
                pick_encoding,
                json,
                deep,
                max_scan_bytes,
                min_confidence,
                verbose_routing,
                &cli.safety,
                cancellation.clone(),
            )
            .await
        }
        Command::Test {
            paths,
            password,
            use_clipboard: _use_clipboard,
            no_empty,
            encoding,
            json,
            deep,
            max_scan_bytes,
            min_confidence,
            diagnose,
            diagnostic_timeout,
            no_history,
        } => {
            let db = open_db(cli.db)?;
            test_archives(
                &backend,
                &db,
                smartzip_engine::TestWorkflowRequest {
                    paths,
                    encoding: parse_encoding_mode(&encoding),
                    scanner: ScannerConfig {
                        min_confidence: min_confidence.into(),
                        ..scanner_config(deep, max_scan_bytes)
                    },
                    password_candidates: PasswordCandidateRequest {
                        manual: password,
                        clipboard: None,
                        include_empty: !no_empty,
                        limit: cli.safety.password_limit,
                    },
                    diagnose: match diagnose {
                        DiagnoseArg::Auto => smartzip_engine::DiagnoseMode::Auto,
                        DiagnoseArg::Off => smartzip_engine::DiagnoseMode::Off,
                    },
                    diagnostic_timeout: diagnostic_timeout.map(std::time::Duration::from_secs),
                    control: smartzip_archive::diagnostic::DiagnosticControl::with_cancellation(
                        cancellation.clone(),
                    ),
                },
                json,
                no_history,
                verbose_routing,
                &cli.safety,
                cancellation.clone(),
            )
            .await
        }
        Command::Extract {
            paths,
            output,
            recursion_limit,
            password: manual_passwords,
            no_empty,
            deep,
            max_scan_bytes,
            encoding,
            json,
            layout,
            single_root_name,
            dry_run,
            embedded,
            dominant_min_ratio,
            confirm_large_scan,
            force,
            no_history,
        } => {
            let db = open_db(cli.db)?;
            extract(
                &backend,
                &db,
                paths,
                output,
                recursion_limit,
                manual_passwords,
                no_empty,
                deep,
                max_scan_bytes,
                &encoding,
                json,
                layout.into(),
                single_root_name.into(),
                dry_run,
                embedded,
                dominant_min_ratio,
                confirm_large_scan,
                force,
                no_history,
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
            preview_encodings(
                &backend,
                path,
                password,
                json,
                verbose_routing,
                cancellation.clone(),
            )
            .await
        }
        Command::Password(cmd) => {
            let db = open_db(cli.db)?;
            password(&db, cmd)
        }
        Command::History { command } => {
            let db = open_db(cli.db)?;
            history(
                &db,
                command.unwrap_or(HistoryCmd::Tasks {
                    json: false,
                    limit: 20,
                }),
            )
        }
    };
    signal.abort();
    result
}

fn build_backend(
    config_path: Option<&Path>,
    forced_adapter: Option<&str>,
    verbose_routing: bool,
) -> Result<BackendRouter, Box<dyn std::error::Error>> {
    let config = match config_path {
        Some(path) => SmartZipConfig::load(path)?,
        None => SmartZipConfig::default(),
    };
    let mut backend = BackendRouter::from_config(&config.backends)?;
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

fn open_db(path: Option<PathBuf>) -> Result<SmartZipDb, Box<dyn std::error::Error>> {
    let db = match path {
        Some(path) => SmartZipDb::open(&path).map_err(|e| {
            eprintln!(
                "warning: failed to open database at {}: {}",
                path.display(),
                e
            );
            e
        })?,
        None => {
            let paths = PlatformPaths::new();
            paths.ensure_dirs()?;
            let db_path = paths.db_path();
            SmartZipDb::open(&db_path).map_err(|e| {
                eprintln!(
                    "warning: failed to open database at {}: {}",
                    db_path.display(),
                    e
                );
                e
            })?
        }
    };

    match db.db_path() {
        Some(p) => eprintln!("Database: {}", p.display()),
        None => {
            eprintln!("warning: using in-memory database — passwords will NOT be saved to disk")
        }
    }

    Ok(db)
}

fn scanner_config(deep: bool, max_scan_bytes: Option<u64>) -> ScannerConfig {
    let mut config = ScannerConfig {
        mode: if deep { ScanMode::Deep } else { ScanMode::Fast },
        ..ScannerConfig::default()
    };
    config.max_scan_bytes = match max_scan_bytes {
        Some(0) => None,
        Some(value) => Some(value),
        None if deep => None,
        None => config.max_scan_bytes,
    };
    config
}

async fn detect(
    backend: &BackendRouter,
    db: &SmartZipDb,
    path: PathBuf,
    deep: bool,
    json: bool,
    max_scan_bytes: Option<u64>,
    min_confidence: ConfidenceArg,
    verbose_routing: bool,
    _safety: &SafetyOptions,
    cancellation: tokio_util::sync::CancellationToken,
) -> Result<(), Box<dyn std::error::Error>> {
    let config = ScannerConfig {
        min_confidence: min_confidence.into(),
        ..scanner_config(deep, max_scan_bytes)
    };
    let service = PasswordService::new(PasswordRepository::new(db.connection()));
    let engine = SmartZipEngine::with_scanner_config(config.clone())
        .with_cancellation_token(cancellation.clone());
    let recorder = smartzip_engine::history::DbTaskHistoryRecorder::new(db.connection());
    let result = engine
        .inspect_file_with_listener(
            backend,
            &service,
            InspectRequest {
                path,
                scanner: config,
            },
            routing_listener(json, verbose_routing),
            Some(&recorder),
        )
        .await?;

    print_detect_result(&result, json)?;
    command_exit(if result.status == "unreadable" { 1 } else { 0 })
}

async fn test_archives(
    backend: &BackendRouter,
    db: &SmartZipDb,
    request: smartzip_engine::TestWorkflowRequest,
    json: bool,
    no_history: bool,
    verbose_routing: bool,
    safety: &SafetyOptions,
    cancellation: tokio_util::sync::CancellationToken,
) -> Result<(), Box<dyn std::error::Error>> {
    let service = PasswordService::new(PasswordRepository::new(db.connection()));
    let engine = SmartZipEngine::with_scanner_config(request.scanner.clone());
    let recorder = smartzip_engine::history::DbTaskHistoryRecorder::new(db.connection());
    let prompter = StdinPrompter {
        lock: StdinLock::configured(cancellation.clone(), safety, json),
    };
    let listener = (!json).then(|| {
        Arc::new(move |event: &TaskEvent| match &event.kind {
            smartzip_core::TaskEventKind::TestPhase { path, phase, .. } => {
                eprintln!("{}: {}", safe_text(&path.to_string_lossy()), phase)
            }
            smartzip_core::TaskEventKind::Warning { message } => {
                eprintln!("warning: {}", safe_text(message))
            }
            smartzip_core::TaskEventKind::Route(route) if verbose_routing => {
                render_route_event(route, false)
            }
            _ => {}
        }) as smartzip_engine::TaskEventListener
    });
    let result = engine
        .test_archives(
            backend,
            &service,
            request,
            if prompter.lock.interactive {
                Some(&prompter)
            } else {
                None
            },
            listener,
            if no_history { None } else { Some(&recorder) },
        )
        .await;
    let result = result?;
    if json {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        for report in &result.files {
            print_test_report(report);
        }
        println!("task-id: {}", result.task_id);
    }
    command_exit(result.exit_code)
}

fn safe_text(value: &str) -> String {
    value
        .chars()
        .flat_map(|c| {
            if c.is_control() {
                c.escape_default().collect::<Vec<_>>()
            } else {
                vec![c]
            }
        })
        .collect()
}

fn print_test_report(report: &smartzip_archive::integrity::TestArchiveReport) {
    use smartzip_archive::integrity::{Integrity, SuspectRelation};
    println!(
        "{}: {} (coverage={}, localization={}, password={})",
        safe_text(&report.entrypoint.to_string_lossy()),
        enum_text(&report.integrity),
        enum_text(&report.coverage),
        enum_text(&report.localization),
        enum_text(&report.password_status)
    );
    for volume in &report.confirmed_volumes {
        println!(
            "  Confirmed damaged: {}",
            safe_text(&volume.path.to_string_lossy())
        );
        for evidence in report
            .evidence
            .iter()
            .filter(|e| volume.evidence_ids.contains(&e.id))
        {
            println!(
                "    {} [{}]",
                safe_text(&evidence.summary),
                safe_text(&evidence.source)
            );
        }
    }
    for (index, group) in report.suspect_groups.iter().enumerate() {
        println!(
            "  Suspected group {} ({}): {}",
            index + 1,
            match group.relation {
                SuspectRelation::OneOrMore => "one or more may be damaged",
                SuspectRelation::Possible => "possible; exact range unknown",
            },
            group
                .members
                .iter()
                .map(|p| safe_text(&p.to_string_lossy()))
                .collect::<Vec<_>>()
                .join(", ")
        );
        for evidence in report
            .evidence
            .iter()
            .filter(|e| group.evidence_ids.contains(&e.id))
        {
            println!(
                "    {} [{}]",
                safe_text(&evidence.summary),
                safe_text(&evidence.source)
            );
        }
    }
    for (label, paths) in [
        ("Missing", &report.missing_volumes),
        ("Unreadable", &report.unreadable_volumes),
    ] {
        for path in paths {
            println!("  {label}: {}", safe_text(&path.to_string_lossy()));
        }
    }
    if !report.unchecked_volumes.is_empty() {
        println!(
            "  Full-volume health unchecked: {} volume(s)",
            report.unchecked_volumes.len()
        );
    }
    for reason in &report.stop_reasons {
        println!("  Note: {}", safe_text(reason));
    }
    if report.integrity != Integrity::Intact {
        println!("  Next: restore missing/unreadable volumes, replace confirmed damaged volumes, then test again; suspected members need further checking.");
    }
}

fn enum_text(value: &impl std::fmt::Debug) -> String {
    format!("{value:?}")
        .chars()
        .enumerate()
        .flat_map(|(index, c)| {
            if c.is_uppercase() && index > 0 {
                vec!['_', c.to_ascii_lowercase()]
            } else {
                vec![c.to_ascii_lowercase()]
            }
        })
        .collect()
}

async fn list_archive(
    backend: &BackendRouter,
    db: &SmartZipDb,
    path: PathBuf,
    manual_passwords: Vec<String>,
    no_empty: bool,
    encoding: &str,
    pick_encoding: bool,
    json: bool,
    deep: bool,
    max_scan_bytes: Option<u64>,
    min_confidence: ConfidenceArg,
    verbose_routing: bool,
    safety: &SafetyOptions,
    cancellation: tokio_util::sync::CancellationToken,
) -> Result<(), Box<dyn std::error::Error>> {
    let config = ScannerConfig {
        min_confidence: min_confidence.into(),
        ..scanner_config(deep, max_scan_bytes)
    };
    let service = PasswordService::new(PasswordRepository::new(db.connection()));
    let engine = SmartZipEngine::with_scanner_config(config.clone())
        .with_cancellation_token(cancellation.clone());
    let recorder = smartzip_engine::history::DbTaskHistoryRecorder::new(db.connection());
    let stdin_lock = StdinLock::configured(cancellation.clone(), safety, json);
    let password_prompter = StdinPrompter {
        lock: stdin_lock.clone(),
    };
    let encoding_mode =
        select_list_encoding(path.clone(), encoding, pick_encoding, stdin_lock.clone()).await?;
    let result = engine
        .list_archive_with_listener_interactive(
            backend,
            &service,
            ListArchiveRequest {
                path,
                scanner: config,
                encoding_mode,
                password_candidates: PasswordCandidateRequest {
                    manual: manual_passwords,
                    clipboard: None,
                    include_empty: !no_empty,
                    limit: safety.password_limit,
                },
            },
            if stdin_lock.interactive {
                Some(&password_prompter)
            } else {
                None
            },
            Some(&StdinEncodingPrompter { lock: stdin_lock }),
            routing_listener(json, verbose_routing),
            Some(&recorder),
        )
        .await?;

    if json {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        println!(
            "{} [{}] enc={} password={} task-id={}",
            result.path.display(),
            result
                .detected_format
                .as_ref()
                .map(|fmt| fmt.as_str())
                .unwrap_or("unknown"),
            result.encoding,
            if result.used_password { "yes" } else { "no" },
            result.task_id,
        );
        for entry in &result.entries {
            let suffix = if entry.is_dir {
                "/"
            } else {
                Default::default()
            };
            println!("{}{}", entry.path.display(), suffix);
        }
    }
    Ok(())
}

fn print_detect_result(
    result: &FileAwareDetectResult,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if json {
        println!("{}", serde_json::to_string_pretty(result)?);
        return Ok(());
    }

    println!(
        "{} [{}] status={} embedded={} encrypted={} task-id={}",
        result.path.display(),
        result
            .detected_format
            .as_ref()
            .map(|fmt| fmt.as_str())
            .unwrap_or("unknown"),
        result.status,
        result.embedded_count,
        match result.encrypted {
            Some(true) => "yes",
            Some(false) => "no",
            None => "unknown",
        },
        result.task_id,
    );
    if let Some(encoding) = &result.encoding {
        if let Some(confidence) = result.encoding_confidence {
            println!("encoding: {encoding} ({:.0}%)", confidence * 100.0);
        } else {
            println!("encoding: {encoding}");
        }
    }
    if let Some(reason) = &result.reason {
        println!("reason: {reason}");
    }
    if result.needs_password {
        println!("password: required to continue");
    }
    if result.known_password {
        println!("known password: available");
    }
    if let Some(known_encoding) = &result.known_encoding {
        println!("known encoding: {known_encoding}");
    }
    if !result.embedded_findings.is_empty() {
        println!("embedded findings:");
        for finding in &result.embedded_findings {
            println!(
                "  - {} @ 0x{:X} size={} confidence={:?} {}",
                finding.format.as_str(),
                finding.offset,
                finding
                    .size
                    .map(|size| size.to_string())
                    .unwrap_or_else(|| "unknown".into()),
                finding.confidence,
                finding.description,
            );
        }
    }
    Ok(())
}

fn parse_encoding_mode(encoding: &str) -> EncodingMode {
    if encoding == "auto" {
        EncodingMode::Auto
    } else {
        EncodingMode::Override(encoding.to_string())
    }
}

async fn select_list_encoding(
    path: PathBuf,
    encoding: &str,
    pick_encoding: bool,
    control: StdinLock,
) -> Result<EncodingMode, Box<dyn std::error::Error>> {
    if !pick_encoding || !control.interactive {
        return Ok(parse_encoding_mode(encoding));
    }
    let choice = tokio::task::spawn_blocking(move || {
        use std::io::Write;
        let _guard = control.gate.lock().unwrap();
        let candidates = encoding_preview_candidates();
        eprintln!("\n  Candidate encodings for {}:", path.display());
        for (idx, candidate) in candidates.iter().enumerate() {
            eprintln!("  [{}] {}", idx + 1, candidate);
        }
        eprint!("  Pick encoding number (Enter for auto): ");
        let _ = std::io::stderr().flush();
        terminal_line(&control, false)
    })
    .await?;
    let Some(choice) = choice else {
        return Err(smartzip_core::SmartZipError::Cancelled.into());
    };
    let trimmed = choice.trim();
    if trimmed.is_empty() {
        return Ok(EncodingMode::Auto);
    }
    let idx: usize = trimmed.parse()?;
    let selected = encoding_preview_candidates()
        .get(idx.checked_sub(1).ok_or("invalid encoding choice")?)
        .copied()
        .ok_or_else(|| format!("invalid encoding choice: {trimmed}"))?;
    Ok(parse_encoding_mode(selected))
}

async fn extract(
    backend: &BackendRouter,
    db: &SmartZipDb,
    paths: Vec<PathBuf>,
    output: Option<PathBuf>,
    recursion_limit: u8,
    manual_passwords: Vec<String>,
    no_empty: bool,
    deep: bool,
    max_scan_bytes: Option<u64>,
    encoding: &str,
    json: bool,
    layout_policy: smartzip_engine::layout::OutputLayoutPolicy,
    single_root_name_policy: smartzip_engine::layout::SingleRootNamePolicy,
    dry_run: bool,
    embedded: EmbeddedModeArg,
    dominant_min_ratio: f32,
    confirm_large_scan: bool,
    force: bool,
    no_history: bool,
    verbose_routing: bool,
    safety: &SafetyOptions,
    cancellation: tokio_util::sync::CancellationToken,
) -> Result<(), Box<dyn std::error::Error>> {
    if paths.is_empty() {
        return Err("no paths provided".into());
    }

    if dry_run {
        let output_dir = output.unwrap_or_else(|| default_output_dir(paths.first().unwrap()));
        let archive_stem = name_score::archive_display_stem(paths.first().unwrap());
        println!("Archive: {}", paths.first().unwrap().display());
        println!("Archive stem: {archive_stem}");
        println!("Layout policy: {layout_policy:?}");
        println!("Note: --dry-run shows initial candidate path. Final layout depends on extracted content.");
        println!(
            "Planned output: {}",
            output_dir.join(&archive_stem).display()
        );
        return Ok(());
    }

    let encoding_mode = if encoding == "auto" {
        EncodingMode::Auto
    } else {
        EncodingMode::Override(encoding.to_string())
    };

    let output_dir = output.unwrap_or_else(|| default_output_dir(paths.first().unwrap()));

    let service = PasswordService::new(PasswordRepository::new(db.connection()));

    let stdin_lock = StdinLock::configured(cancellation.clone(), safety, json);
    let password_prompter = StdinPrompter {
        lock: stdin_lock.clone(),
    };
    let engine = SmartZipEngine::default().with_cancellation_token(cancellation.clone());
    let event_listener = routing_listener(json, verbose_routing);

    // History recorder shares the same connection as the password service.
    // Both borrow `&Connection` immutably, which SQLite allows. Suppressed
    // with `--no-history`; password success/failure writes happen regardless.
    let recorder = (!no_history)
        .then(|| smartzip_engine::history::DbTaskHistoryRecorder::new(db.connection()));
    let recorder_ref = recorder
        .as_ref()
        .map(|r| r as &dyn smartzip_engine::history::TaskHistoryRecorder);

    let result = engine
        .extract_recursive_with_listener_interactive(
            backend,
            &service,
            ExtractWorkflowRequest {
                inputs: paths,
                output_dir,
                recursion_limit,
                scanner: scanner_config(deep, max_scan_bytes),
                encoding_mode,
                password_candidates: PasswordCandidateRequest {
                    manual: manual_passwords,
                    clipboard: None,
                    include_empty: !no_empty,
                    limit: safety.password_limit,
                },
                layout_policy,
                single_root_name_policy,
                embedded_scan_mode: embedded.into(),
                dominant_min_ratio,
                confirm_large_scan,
                force,
                limits: safety.limits(),
            },
            if stdin_lock.interactive {
                Some(&password_prompter)
            } else {
                None
            },
            Some(&StdinOutputPrompter {
                lock: stdin_lock.clone(),
            }),
            Some(&StdinEmbeddedPrompter {
                lock: stdin_lock.clone(),
            }),
            Some(&StdinEncodingPrompter { lock: stdin_lock }),
            event_listener,
            recorder_ref,
        )
        .await?;

    let exit_code = result.status.exit_code() as i32;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&build_extract_json_output(&result, exit_code))?
        );
    } else {
        let processed_count = result.processed.len();
        let skipped_count = result.skipped.len();
        if processed_count > 0 {
            println!("processed {} archive(s)", processed_count);
        }
        if skipped_count > 0 {
            println!("skipped {} candidate(s)", skipped_count);
            for skipped in &result.skipped {
                println!("  - {} (depth {})", skipped.path.display(), skipped.depth);
            }
        }
        if recorder_ref.is_some() {
            println!("task-id: {}", result.task_id);
        }
    }

    command_exit(exit_code)
}

struct SilentSink;
impl TaskEventSink for SilentSink {
    fn push(&self, _: TaskEvent) {}
}

struct RoutingPrintSink;

impl TaskEventSink for RoutingPrintSink {
    fn push(&self, event: TaskEvent) {
        if let smartzip_core::TaskEventKind::Route(route) = &event.kind {
            render_route_event(route, true);
        }
    }
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

fn render_extract_event(event: &smartzip_core::TaskEvent, verbose_routing: bool) {
    match &event.kind {
        smartzip_core::TaskEventKind::Progress(progress) => match progress.percent {
            Some(percent) => println!("  {percent:>3.0}%  {}", progress.message),
            None => println!("  {}", progress.message),
        },
        smartzip_core::TaskEventKind::EncodingDetected(detection) => {
            let encoding = match &detection.selected {
                smartzip_core::EncodingMode::Auto => "auto",
                smartzip_core::EncodingMode::Override(s) => s.as_str(),
            };
            println!(
                "  encoding: {encoding} (confidence: {:.0}%)",
                detection.confidence * 100.0
            );
        }
        smartzip_core::TaskEventKind::EmbeddedArchiveSelectionRequired {
            path,
            findings_count,
        } => {
            println!(
                "  embedded selection required: {} ({} finding(s))",
                path.display(),
                findings_count
            );
        }
        smartzip_core::TaskEventKind::LargeEmbeddedScanConfirmationRequired {
            path,
            file_size,
            threshold,
        } => {
            eprintln!(
                "  large embedded scan skipped without confirmation: {} ({} bytes > {} bytes)",
                path.display(),
                file_size,
                threshold
            );
        }
        smartzip_core::TaskEventKind::BusinessContainerSkipped { path, kind } => {
            println!("  skipped business container {kind}: {}", path.display());
        }
        smartzip_core::TaskEventKind::OutputCreated { path } => {
            println!("  -> {}", path.display());
        }
        smartzip_core::TaskEventKind::Route(route) if verbose_routing => {
            render_route_event(route, false);
        }
        smartzip_core::TaskEventKind::Failed { error } => eprintln!("  FAILED: {error}"),
        smartzip_core::TaskEventKind::Warning { message } => {
            eprintln!("  warning: {message}")
        }
        _ => {}
    }
}

fn render_route_event(route: &smartzip_core::RouteEvent, stderr: bool) {
    macro_rules! output {
        ($($args:tt)*) => {
            if stderr {
                eprintln!($($args)*);
            } else {
                println!($($args)*);
            }
        };
    }

    match route {
        smartzip_core::RouteEvent::RoutePlanned { plan } => {
            output!("  route: {:?}", plan.operation);
            for candidate in &plan.candidates {
                output!("    candidate: {}", candidate.adapter_id);
                for note in &candidate.notes {
                    output!("      note: {note}");
                }
            }
            for rejected in &plan.rejected {
                output!(
                    "    rejected: {} ({})",
                    rejected.adapter_id,
                    rejected.reasons.join("; ")
                );
            }
        }
        smartzip_core::RouteEvent::BackendAttemptStarted { adapter_id } => {
            output!("  route: trying {adapter_id}")
        }
        smartzip_core::RouteEvent::BackendAttemptFailed { adapter_id, class } => {
            output!("  route: {adapter_id} failed ({class})")
        }
        smartzip_core::RouteEvent::BackendAttemptCleaned { adapter_id } => {
            output!("  route: cleaned {adapter_id} output")
        }
        smartzip_core::RouteEvent::BackendSelected { adapter_id } => {
            output!("  route: selected {adapter_id}")
        }
        smartzip_core::RouteEvent::RouteExhausted { attempted } => {
            output!("  route: exhausted [{}]", attempted.join(", "))
        }
    }
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
    path: PathBuf,
    password: Option<String>,
    json: bool,
    verbose_routing: bool,
    cancellation: tokio_util::sync::CancellationToken,
) -> Result<(), Box<dyn std::error::Error>> {
    let candidates = encoding_preview_candidates();
    let mut previews = Vec::new();

    for encoding in candidates {
        let mode = match *encoding {
            "auto" => EncodingMode::Auto,
            other => EncodingMode::Override(other.to_string()),
        };
        let request = smartzip_archive::ListRequest {
            archive: path.clone(),
            format: smartzip_engine::format_from_extension(&path),
            password: password.clone(),
            encoding: mode,
        };
        if cancellation.is_cancelled() {
            return Err(smartzip_core::SmartZipError::Cancelled.into());
        }
        let listing = if verbose_routing {
            let context = backend.begin_task_with_cancellation(
                TaskId::new(),
                std::sync::Arc::new(RoutingPrintSink),
                cancellation.clone(),
            );
            backend.list_with_context(request, context).await
        } else {
            let context = backend.begin_task_with_cancellation(
                TaskId::new(),
                std::sync::Arc::new(SilentSink),
                cancellation.clone(),
            );
            backend.list_with_context(request, context).await
        };
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

fn build_extract_json_output(
    result: &smartzip_engine::ExtractWorkflowResult,
    exit_code: i32,
) -> serde_json::Value {
    serde_json::json!({
        "task_id": result.task_id,
        "status": result.status,
        "failed_count": result.failed_count,
        "processed_count": result.processed.len(),
        "skipped_count": result.skipped.len(),
        "enqueued_count": result.enqueued.len(),
        "processed": result.processed,
        "skipped": result.skipped,
        "enqueued": result.enqueued,
        "events": result.events,
        "exit_code": exit_code,
    })
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
            let content = std::fs::read_to_string(&path)?;
            let mut count = 0u32;
            for line in content.lines() {
                let trimmed = line.trim();
                if !trimmed.is_empty() {
                    service.add_password(trimmed, &source, false)?;
                    count += 1;
                }
            }
            println!("imported {count} password(s) from {}", path.display());
        }
        PasswordCmd::Export { path } => {
            let repo = PasswordRepository::new(db.connection());
            let passwords = repo.ranked_candidates(usize::MAX)?;
            let out_path = if let Some(path) = path {
                path
            } else {
                let paths = PlatformPaths::new();
                paths.ensure_dirs()?;
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
                let already_disabled: Vec<i64> = to_disable.clone();
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
                for id in &to_disable {
                    repo.disable(*id)?;
                }
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

fn print_history_test_report(json: Option<&str>) {
    if let Some(json) = json {
        match serde_json::from_str::<smartzip_archive::integrity::TestArchiveReport>(json) {
            Ok(report) => print_test_report(&report),
            Err(_) => eprintln!("  test report has an unsupported or invalid schema"),
        }
    }
}

fn default_output_dir(first_path: &Path) -> PathBuf {
    first_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf()
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
    use super::{build_extract_json_output, encoding_preview_candidates, Cli, Command};
    use clap::Parser;
    use serde_json::json;
    use smartzip_core::TaskId;
    use smartzip_engine::ExtractWorkflowResult;

    #[test]
    fn history_without_subcommand_defaults_at_dispatch_layer() {
        let cli = Cli::try_parse_from(["smartzip", "history"]).unwrap();
        assert!(matches!(cli.command, Command::History { command: None }));
    }

    #[test]
    fn test_alias_accepts_groups_and_validates_the_diagnostic_budget() {
        for name in ["test", "t"] {
            let cli =
                Cli::try_parse_from(["smartzip", name, "a.part2.rar", "b.zip", "--json"]).unwrap();
            assert!(matches!(cli.command, Command::Test {
                paths, diagnose: super::DiagnoseArg::Auto, diagnostic_timeout: None, json: true, ..
            } if paths.len() == 2));
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
            Command::Test {
                diagnose: super::DiagnoseArg::Off,
                diagnostic_timeout: Some(5),
                no_history: true,
                ..
            }
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
    fn extract_json_output_includes_exit_code_and_counts() {
        let result = ExtractWorkflowResult {
            status: smartzip_engine::history::TaskCompletionStatus::Completed,
            failed_count: 0,
            task_id: TaskId::new(),
            processed: Vec::new(),
            skipped: Vec::new(),
            enqueued: Vec::new(),
            events: Vec::new(),
        };

        let output = build_extract_json_output(&result, 1);
        assert_eq!(output["processed_count"], json!(0));
        assert_eq!(output["skipped_count"], json!(0));
        assert_eq!(output["exit_code"], json!(1));
    }

    #[test]
    fn encoding_preview_candidates_cover_expected_defaults() {
        assert_eq!(encoding_preview_candidates()[0], "auto");
        assert!(encoding_preview_candidates().contains(&"UTF-8"));
        assert!(encoding_preview_candidates().contains(&"GB18030"));
        assert!(encoding_preview_candidates().contains(&"Shift_JIS"));
    }
}
