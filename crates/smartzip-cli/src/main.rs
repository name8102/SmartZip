use async_trait::async_trait;
use clap::{Parser, Subcommand, ValueEnum};
use smartzip_archive::{ArchiveBackend, BackendRouter, NativeZipBackend};
use smartzip_core::EncodingMode;
use smartzip_db::{password::PasswordRepository, SmartZipDb};
use smartzip_engine::name_score;
use smartzip_engine::{
    DetectRequest, EmbeddedSelectionChoice, EncodingConfirmationChoice, ExtractWorkflowRequest,
    InteractiveEmbeddedPrompter, InteractiveEncodingPrompter, InteractiveOutputPrompter,
    InteractivePasswordPrompter, OutputCollisionStrategy, SmartZipEngine,
};
use smartzip_passwords::{PasswordCandidateRequest, PasswordService};
use smartzip_platform::PlatformPaths;
use smartzip_scanner::{Confidence, ScanMode, ScannerConfig};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

const DEFAULT_RECURSION_LIMIT: u8 = 3;

#[derive(Debug, Parser)]
#[command(name = "smartzip")]
#[command(about = "SmartZip cross-platform archive helper")]
struct Cli {
    /// Path to database file. Defaults to the platform data directory if not set.
    #[arg(long)]
    db: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
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
    /// Detect embedded archives or disguised archive data.
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

    /// Extract archives, optionally with nested scanning.
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

        /// Read password from clipboard (platform-dependent placeholder).
        #[arg(long)]
        use_clipboard: bool,

        /// Skip empty password attempt.
        #[arg(long)]
        no_empty: bool,

        /// Use deep scan for nested archives.
        #[arg(long)]
        deep: bool,

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
    EncodingPreview {
        path: PathBuf,

        /// Password to use when the archive requires one.
        #[arg(short = 'p', long)]
        password: Option<String>,

        #[arg(long)]
        json: bool,
    },

    /// Placeholder for future compression implementation.
    Compress { paths: Vec<PathBuf> },

    /// Manage password database.
    #[command(subcommand)]
    Password(PasswordCmd),

    /// Inspect recorded task history. Defaults to recent tasks.
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
async fn main() {
    if let Err(error) = run().await {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    match cli.command {
        Command::Detect {
            path,
            deep,
            json,
            max_scan_bytes,
            min_confidence,
        } => detect(path, deep, json, max_scan_bytes, min_confidence),
        Command::Extract {
            paths,
            output,
            recursion_limit,
            password: manual_passwords,
            use_clipboard: _use_clipboard,
            no_empty,
            deep,
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
                &db,
                paths,
                output,
                recursion_limit,
                manual_passwords,
                no_empty,
                deep,
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
            )
            .await
        }
        Command::EncodingPreview {
            path,
            password,
            json,
        } => preview_encodings(path, password, json).await,
        Command::Compress { paths } => {
            println!(
                "compress is not implemented yet; received {} path(s)",
                paths.len()
            );
            Ok(())
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
    }
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
    ScannerConfig {
        mode: if deep { ScanMode::Deep } else { ScanMode::Fast },
        max_scan_bytes: max_scan_bytes.and_then(|value| (value != 0).then_some(value)),
        ..ScannerConfig::default()
    }
}

fn detect(
    path: PathBuf,
    deep: bool,
    json: bool,
    max_scan_bytes: Option<u64>,
    min_confidence: ConfidenceArg,
) -> Result<(), Box<dyn std::error::Error>> {
    let config = ScannerConfig {
        min_confidence: min_confidence.into(),
        ..scanner_config(deep, max_scan_bytes)
    };
    let engine = SmartZipEngine::with_scanner_config(config.clone());
    let detect_path = path.clone();
    let result = engine.detect(DetectRequest {
        path,
        scanner: config,
    })?;

    if json {
        let file_size = std::fs::metadata(&detect_path)
            .map(|m| m.len())
            .unwrap_or(0);
        let policy = smartzip_core::EmbeddedScanPolicy::default();
        let ext_is_archive = smartzip_engine::format_from_extension(&detect_path).is_some();
        let decision = smartzip_engine::embedded::select_embedded_action(
            file_size,
            &result.findings,
            &policy,
            ext_is_archive,
        );

        let output = serde_json::json!({
            "path": detect_path,
            "file_size": file_size,
            "classification": format!("{:?}", decision.kind).to_lowercase(),
            "action": format!("{:?}", decision.action).to_lowercase(),
            "archive_ratio": decision.archive_ratio,
            "selected_index": decision.selected_index,
            "reason": decision.reason,
            "findings": result.findings,
        });
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else if result.findings.is_empty() {
        println!("No embedded archives found.");
    } else {
        for finding in result.findings {
            println!(
                "{format} @ 0x{offset:X} size={size} confidence={confidence:?} {description}",
                format = finding.format.as_str(),
                offset = finding.offset,
                size = finding
                    .size
                    .map(|size| size.to_string())
                    .unwrap_or_else(|| "unknown".into()),
                confidence = finding.confidence,
                description = finding.description,
            );
        }
    }

    Ok(())
}

async fn extract(
    db: &SmartZipDb,
    paths: Vec<PathBuf>,
    output: Option<PathBuf>,
    recursion_limit: u8,
    manual_passwords: Vec<String>,
    no_empty: bool,
    deep: bool,
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

    let backend = BackendRouter::locate()?;
    let service = PasswordService::new(PasswordRepository::new(db.connection()));

    let stdin_lock = StdinLock::new();
    let engine = SmartZipEngine::default();
    let event_listener = (!json)
        .then(|| std::sync::Arc::new(render_extract_event) as smartzip_engine::TaskEventListener);

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
            &backend,
            &service,
            ExtractWorkflowRequest {
                inputs: paths,
                output_dir,
                recursion_limit,
                scanner: scanner_config(deep, None),
                encoding_mode,
                password_candidates: PasswordCandidateRequest {
                    manual: manual_passwords,
                    clipboard: None,
                    include_empty: !no_empty,
                    limit: 128,
                },
                layout_policy,
                single_root_name_policy,
                embedded_scan_mode: embedded.into(),
                dominant_min_ratio,
                confirm_large_scan,
                force,
            },
            Some(&StdinPrompter {
                lock: stdin_lock.clone(),
            }),
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

    let exit_code = extraction_exit_code(result.processed.len(), result.skipped.len());
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

    std::process::exit(exit_code);
}

fn render_extract_event(event: &smartzip_core::TaskEvent) {
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
        smartzip_core::TaskEventKind::Failed { error } => eprintln!("  FAILED: {error}"),
        smartzip_core::TaskEventKind::Warning { message } => {
            eprintln!("  warning: {message}")
        }
        _ => {}
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
    path: PathBuf,
    password: Option<String>,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let backend = BackendRouter::locate()?;
    let native_zip = NativeZipBackend::new();
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
        let listing = if smartzip_engine::format_from_extension(&path)
            == Some(smartzip_core::ArchiveFormat::Zip)
        {
            native_zip.list(request).await
        } else {
            backend.list(request).await
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

    Ok(())
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

fn extraction_exit_code(processed_count: usize, skipped_count: usize) -> i32 {
    if processed_count > 0 && skipped_count == 0 {
        0
    } else if processed_count > 0 && skipped_count > 0 {
        2
    } else {
        1
    }
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
                        format!("{}...", &p.value[..27])
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
struct StdinLock(Arc<Mutex<()>>);

impl StdinLock {
    fn new() -> Self {
        Self(Arc::new(Mutex::new(())))
    }
}

impl Clone for StdinLock {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

struct StdinPrompter {
    lock: StdinLock,
}

#[async_trait]
impl InteractivePasswordPrompter for StdinPrompter {
    async fn prompt(&self, archive_path: &Path) -> Option<String> {
        let lock = self.lock.0.clone();
        let path = archive_path.to_path_buf();
        tokio::task::spawn_blocking(move || {
            let _guard = lock.lock().unwrap();
            prompt_password_stdin(&path)
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
        let lock = self.lock.0.clone();
        tokio::task::spawn_blocking(move || {
            let _guard = lock.lock().unwrap();
            prompt_output_collision_stdin(&archive_path, &output_path)
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
        let lock = self.lock.0.clone();
        let path = archive_path.to_path_buf();
        let decision = decision.clone();
        tokio::task::spawn_blocking(move || {
            let _guard = lock.lock().unwrap();
            prompt_embedded_stdin(&path, &decision)
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
        let lock = self.lock.0.clone();
        let path = archive_path.to_path_buf();
        let context = context.clone();
        tokio::task::spawn_blocking(move || {
            let _guard = lock.lock().unwrap();
            prompt_encoding_stdin(&path, &context)
        })
        .await
        .unwrap_or(EncodingConfirmationChoice::AcceptDetected)
    }
}

fn prompt_password_stdin(path: &Path) -> Option<String> {
    use std::io::{self, IsTerminal, Write};

    if !io::stdin().is_terminal() {
        return None;
    }

    eprint!(
        "\n  No matching password for \"{}\".\n  Enter password (or press Enter to skip): ",
        path.display()
    );
    let _ = io::stderr().flush();

    let mut pw = String::new();
    io::stdin().read_line(&mut pw).ok()?;
    let pw = pw.trim().to_string();

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
) -> OutputCollisionStrategy {
    use std::io::{self, IsTerminal, Write};

    if !io::stdin().is_terminal() {
        return OutputCollisionStrategy::Skip;
    }

    loop {
        eprint!(
            "\n  Output already exists for \"{}\": {}\n  Choose [s]kip, [o]verwrite, [r]ename: ",
            archive_path.display(),
            output_path.display()
        );
        let _ = io::stderr().flush();

        let mut choice = String::new();
        if io::stdin().read_line(&mut choice).is_err() {
            return OutputCollisionStrategy::Skip;
        }

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
) -> EmbeddedSelectionChoice {
    use std::io::{self, IsTerminal, Write};

    if !io::stdin().is_terminal() {
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

        let mut choice = String::new();
        if io::stdin().read_line(&mut choice).is_err() {
            return EmbeddedSelectionChoice::Skip;
        }

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
) -> EncodingConfirmationChoice {
    use std::io::{self, IsTerminal, Write};

    if !io::stdin().is_terminal() {
        return EncodingConfirmationChoice::AcceptDetected;
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

        let mut choice = String::new();
        if io::stdin().read_line(&mut choice).is_err() {
            return EncodingConfirmationChoice::AcceptDetected;
        }
        match choice.trim() {
            "" => return EncodingConfirmationChoice::AcceptDetected,
            value if value.eq_ignore_ascii_case("s") || value.eq_ignore_ascii_case("skip") => {
                return EncodingConfirmationChoice::SkipArchive;
            }
            value if value.eq_ignore_ascii_case("m") || value.eq_ignore_ascii_case("manual") => {
                eprint!("  Enter encoding name: ");
                let _ = io::stderr().flush();
                let mut encoding = String::new();
                if io::stdin().read_line(&mut encoding).is_err() {
                    return EncodingConfirmationChoice::AcceptDetected;
                }
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
        build_extract_json_output, encoding_preview_candidates, extraction_exit_code, Cli, Command,
    };
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
    fn exit_code_is_success_when_all_candidates_process() {
        assert_eq!(extraction_exit_code(1, 0), 0);
    }

    #[test]
    fn exit_code_is_partial_when_some_candidates_are_skipped() {
        assert_eq!(extraction_exit_code(2, 1), 2);
    }

    #[test]
    fn exit_code_is_failure_when_nothing_processes() {
        assert_eq!(extraction_exit_code(0, 3), 1);
        assert_eq!(extraction_exit_code(0, 0), 1);
    }

    #[test]
    fn extract_json_output_includes_exit_code_and_counts() {
        let result = ExtractWorkflowResult {
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
