//! Command-specific parsed requests. Field order and help are public CLI contracts.
use super::{
    ConfidenceArg, DiagnoseArg, EmbeddedModeArg, LayoutPolicyArg, SingleRootNameArg,
    DEFAULT_RECURSION_LIMIT,
};
use std::path::PathBuf;

#[derive(Debug, clap::Args)]
pub(super) struct DetectCommand {
    pub(super) path: PathBuf,

    #[arg(long)]
    pub(super) deep: bool,

    #[arg(long)]
    pub(super) json: bool,

    /// Nested scan byte limit (0 = unlimited); explicit root archives are parsed in full.
    #[arg(long)]
    pub(super) max_scan_bytes: Option<u64>,

    #[arg(long, value_enum, default_value_t = ConfidenceArg::Medium)]
    pub(super) min_confidence: ConfidenceArg,
}

#[derive(Debug, clap::Args)]
pub(super) struct ListCommand {
    pub(super) path: PathBuf,

    /// Password to try first. May be repeated.
    #[arg(short = 'p', long)]
    pub(super) password: Vec<String>,

    /// Skip empty password attempt.
    #[arg(long)]
    pub(super) no_empty: bool,

    /// Encoding for entry names: "auto", "UTF-8", "GB18030", "GBK", "Big5", "Shift_JIS", "EUC-JP", "EUC-KR".
    #[arg(long, default_value = "auto")]
    pub(super) encoding: String,

    /// Print several candidate encodings, then prompt once for one to use.
    #[arg(long)]
    pub(super) pick_encoding: bool,

    #[arg(long)]
    pub(super) json: bool,

    #[arg(long)]
    pub(super) deep: bool,

    /// Nested scan byte limit (0 = unlimited); explicit root archives are parsed in full.
    #[arg(long)]
    pub(super) max_scan_bytes: Option<u64>,

    #[arg(long, value_enum, default_value_t = ConfidenceArg::Medium)]
    pub(super) min_confidence: ConfidenceArg,
}

#[derive(Debug, clap::Args)]
pub(super) struct TestCommand {
    #[arg(required = true)]
    pub(super) paths: Vec<PathBuf>,

    /// Additional read-only diagnosis after a failed test.
    #[arg(long, value_enum, default_value_t = DiagnoseArg::Auto)]
    pub(super) diagnose: DiagnoseArg,

    /// Time budget in seconds for additional diagnosis only.
    #[arg(long, value_parser = clap::value_parser!(u64).range(1..))]
    pub(super) diagnostic_timeout: Option<u64>,

    /// Do not save this test in task history.
    #[arg(long)]
    pub(super) no_history: bool,

    /// Password to try first. May be repeated.
    #[arg(short = 'p', long)]
    pub(super) password: Vec<String>,

    /// Read password from clipboard (platform-dependent placeholder).
    #[arg(long)]
    pub(super) use_clipboard: bool,

    /// Skip empty password attempt.
    #[arg(long)]
    pub(super) no_empty: bool,

    /// Encoding for entry names: "auto", "UTF-8", "GB18030", "GBK", "Big5", "Shift_JIS", "EUC-JP", "EUC-KR".
    #[arg(long, default_value = "auto")]
    pub(super) encoding: String,

    #[arg(long)]
    pub(super) json: bool,

    #[arg(long)]
    pub(super) deep: bool,

    /// Nested scan byte limit (0 = unlimited); explicit root archives are parsed in full.
    #[arg(long)]
    pub(super) max_scan_bytes: Option<u64>,

    #[arg(long, value_enum, default_value_t = ConfidenceArg::Medium)]
    pub(super) min_confidence: ConfidenceArg,
}

#[derive(Debug, clap::Args)]
pub(super) struct ExtractCommand {
    pub(super) paths: Vec<PathBuf>,

    /// Output directory. Defaults to first archive's parent directory.
    #[arg(short, long)]
    pub(super) output: Option<PathBuf>,

    /// Maximum nested archive depth.
    #[arg(long, default_value_t = DEFAULT_RECURSION_LIMIT)]
    pub(super) recursion_limit: u8,

    /// Password to try first. May be repeated.
    #[arg(short = 'p', long)]
    pub(super) password: Vec<String>,

    /// Skip empty password attempt.
    #[arg(long)]
    pub(super) no_empty: bool,

    /// Use deep scan for nested archives.
    #[arg(long)]
    pub(super) deep: bool,

    /// Nested scan byte limit (0 = unlimited); explicit root archives are parsed in full.
    #[arg(long)]
    pub(super) max_scan_bytes: Option<u64>,

    /// Encoding for entry names: "auto", "UTF-8", "GB18030", "GBK", "Big5", "Shift_JIS", "EUC-JP", "EUC-KR".
    #[arg(long, default_value = "auto")]
    pub(super) encoding: String,

    #[arg(long)]
    pub(super) json: bool,

    /// Output layout policy: "conservative", "smart", "raw", "flat-single".
    #[arg(long, default_value = "conservative", value_enum)]
    pub(super) layout: LayoutPolicyArg,

    /// Single root name policy: "auto", "archive", "inner", "preserve-both".
    #[arg(long, default_value = "auto", value_enum)]
    pub(super) single_root_name: SingleRootNameArg,

    /// Show planned output without extracting.
    #[arg(long)]
    pub(super) dry_run: bool,

    /// Embedded scan mode: "auto", "ask", "largest", "aggressive", "all", "ignore".
    #[arg(long, default_value = "auto")]
    pub(super) embedded: EmbeddedModeArg,

    /// Minimum ratio for a finding to be considered dominant (0.0-1.0).
    #[arg(long, default_value_t = 0.70)]
    pub(super) dominant_min_ratio: f32,

    /// Auto-confirm large file scans (>10GB).
    #[arg(long)]
    pub(super) confirm_large_scan: bool,

    /// Do not record this extraction in the task history tables.
    /// Password statistics are still updated.
    #[arg(long)]
    pub(super) no_history: bool,

    /// Re-extract even if this file was already extracted recently,
    /// bypassing the known_files dedup window.
    #[arg(long)]
    pub(super) force: bool,
}
