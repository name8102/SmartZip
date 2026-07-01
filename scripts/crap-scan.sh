#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

MODE="coverage"
TOP="20"
OUT_DIR="${OUT_DIR:-$ROOT_DIR/target/crap}"
LCOV_PATH="$OUT_DIR/smartzip.lcov"

usage() {
    cat <<'EOF'
Usage: scripts/crap-scan.sh [--quick] [--top N] [--out-dir DIR]

Runs cargo-crap with a workflow tuned for SmartZip investigation.

Defaults:
  - only analyzes smartzip-engine and smartzip-cli risk hotspots
  - collects LLVM coverage first
  - writes LCOV to target/crap/smartzip.lcov

Options:
  --quick         Skip coverage collection and rank by complexity only
  --top N         Show top N functions (default: 20)
  --out-dir DIR   Override output directory for LCOV/report artifacts
  -h, --help      Show this help

Examples:
  scripts/crap-scan.sh
  scripts/crap-scan.sh --quick --top 10
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --quick)
            MODE="quick"
            shift
            ;;
        --top)
            TOP="${2:?missing value for --top}"
            shift 2
            ;;
        --out-dir)
            OUT_DIR="${2:?missing value for --out-dir}"
            LCOV_PATH="$OUT_DIR/smartzip.lcov"
            shift 2
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            echo "unknown argument: $1" >&2
            usage >&2
            exit 2
            ;;
    esac
done

mkdir -p "$OUT_DIR"

common_crap_args=(
    --exclude 'tests/**'
    --top "$TOP"
    --format human
)

scan_target() {
    local label="$1"
    local path="$2"

    echo
    echo "== cargo-crap: $label =="

    if [[ "$MODE" == "quick" ]]; then
        cargo crap --path "$path" "${common_crap_args[@]}"
    else
        cargo crap --path "$path" "${common_crap_args[@]}" --lcov "$LCOV_PATH"
    fi
}

echo "== SmartZip CRAP scan =="
echo "mode: $MODE"
echo "top:  $TOP"

if [[ "$MODE" == "quick" ]]; then
    scan_target "smartzip-engine" "crates/smartzip-engine"
    scan_target "smartzip-cli" "crates/smartzip-cli"
    exit 0
fi

echo
echo "== Collecting coverage for workspace tests =="
echo "output: $LCOV_PATH"

XDG_DATA_HOME=/tmp \
XDG_CONFIG_HOME=/tmp \
XDG_CACHE_HOME=/tmp \
cargo llvm-cov \
    -p smartzip-engine \
    -p smartzip-cli \
    --tests \
    --ignore-run-fail \
    --lcov \
    --output-path "$LCOV_PATH"

scan_target "smartzip-engine" "crates/smartzip-engine"
scan_target "smartzip-cli" "crates/smartzip-cli"
