#!/usr/bin/env bash
# Wrapper around rust-docs-mcp MCP server
# Provides CLI-like access to the MCP server's tools
set -euo pipefail

RUST_DOCS="${RUST_DOCS_MCP_BIN:-rust-docs-mcp}"
CACHE_DIR="${RUST_DOCS_MCP_CACHE_DIR:-}"

# Find the binary
if ! command -v "$RUST_DOCS" &>/dev/null; then
  if [ -f "$HOME/.cargo/bin/rust-docs-mcp" ]; then
    RUST_DOCS="$HOME/.cargo/bin/rust-docs-mcp"
  else
    echo "ERROR: rust-docs-mcp not found in PATH" >&2
    echo "Install with: cargo install rust-docs-mcp" >&2
    exit 1
  fi
fi

CMD="${1:-help}"
shift 2>/dev/null || true

case "$CMD" in
  doctor)
    "$RUST_DOCS" doctor
    ;;
  cache)
    "$RUST_DOCS" doctor --json >/dev/null 2>&1 || true
    exec python3 "$(dirname "$0")/mcp_client.py" cache "$@"
    ;;
  list-cached)
    exec python3 "$(dirname "$0")/mcp_client.py" list-cached "$@"
    ;;
  remove)
    exec python3 "$(dirname "$0")/mcp_client.py" remove "$@"
    ;;
  list-items)
    exec python3 "$(dirname "$0")/mcp_client.py" list-items "$@"
    ;;
  search)
    exec python3 "$(dirname "$0")/mcp_client.py" search "$@"
    ;;
  search-preview)
    exec python3 "$(dirname "$0")/mcp_client.py" search-preview "$@"
    ;;
  search-fuzzy)
    exec python3 "$(dirname "$0")/mcp_client.py" search-fuzzy "$@"
    ;;
  get-details)
    exec python3 "$(dirname "$0")/mcp_client.py" get-details "$@"
    ;;
  get-docs)
    exec python3 "$(dirname "$0")/mcp_client.py" get-docs "$@"
    ;;
  get-source)
    exec python3 "$(dirname "$0")/mcp_client.py" get-source "$@"
    ;;
  deps)
    exec python3 "$(dirname "$0")/mcp_client.py" deps "$@"
    ;;
  structure)
    exec python3 "$(dirname "$0")/mcp_client.py" structure "$@"
    ;;
  help|--help|-h)
    echo "rust-docs-mcp skill wrapper"
    echo ""
    echo "Usage: $(basename "$0") <command> [args]"
    echo ""
    echo "Cache Management:"
    echo "  cache <name> [--version V] [--source cratesio|github|local] [--github-url URL] [--tag T] [--branch B] [--path P]"
    echo "  list-cached                              List all cached crates"
    echo "  remove <name> [--version V]              Remove a cached crate"
    echo ""
    echo "Documentation:"
    echo "  list-items <crate> [--version V] [--kind K] [--query Q]"
    echo "  search <crate> <query>                   Full search (may hit token limits)"
    echo "  search-preview <crate> <query>            Lightweight search preview"
    echo "  search-fuzzy <query>                     Fuzzy search across cached crates"
    echo "  get-details <crate> <item-path>          Get item details"
    echo "  get-docs <crate> <item-path>             Get item documentation"
    echo "  get-source <crate> <item-path> [--context N]  Get item source code"
    echo ""
    echo "Analysis:"
    echo "  deps <crate> [--version V] [--depth direct|transitive]"
    echo "  structure <crate> [--version V]"
    echo ""
    echo "Utility:"
    echo "  doctor                                   Run diagnostics"
    echo "  help                                     Show this help"
    exit 0
    ;;
  *)
    echo "Unknown command: $CMD" >&2
    echo "Run '$0 help' for usage" >&2
    exit 1
    ;;
esac
