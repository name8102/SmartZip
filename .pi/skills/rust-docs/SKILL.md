---
name: rust-docs
description: >-
  Rust crate documentation, source code analysis, dependency trees, and module
  structure visualization. Use when the user needs to understand Rust crate APIs,
  search Rust documentation, inspect source code, or analyze dependencies.
---

# Rust Docs Skill

Provides access to Rust crate documentation, source code, dependency analysis, and module structure via the `rust-docs-mcp` tool.

## Setup

The `rust-docs-mcp` binary must be installed:

```bash
# Install if not already present
cargo install rust-docs-mcp
```

Verify the installation:

```bash
rust-docs-mcp doctor
```

Run `rust-docs-mcp install` to add to `~/.local/bin/` if needed.

## Environment Variables

- `RUST_DOCS_MCP_BIN` — Path to the `rust-docs-mcp` binary (default: auto-detected from PATH)
- `RUST_DOCS_MCP_CACHE_DIR` — Custom cache directory (default: `~/.rust-docs-mcp/cache`)
- `RUST_DOCS_MCP_TIMEOUT` — Timeout in seconds for MCP requests (default: 60)
- `GITHUB_TOKEN` — GitHub token for private repos and higher API rate limits

## Usage

### Wrapper Script

All commands go through the wrapper script at `scripts/rust-docs-mcp.sh`. Use it like:

```bash
.pi/skills/rust-docs/scripts/rust-docs-mcp.sh <command> [args...]
```

Or set the `RUST_DOCS_MCP_BIN` env var and run it directly.

### Basic Flow

1. **Cache a crate** (download its documentation locally)
2. **Browse items** in the crate
3. **Search** for specific APIs
4. **Get details** about items (signatures, docs, source)

### Cache Management

Cache a crate from crates.io:

```bash
.pi/skills/rust-docs/scripts/rust-docs-mcp.sh cache serde --version 1.0.215
```

Cache from GitHub:

```bash
.pi/skills/rust-docs/scripts/rust-docs-mcp.sh cache my-crate --source github --github-url https://github.com/user/repo --tag v1.0.0
```

Cache from a local path:

```bash
.pi/skills/rust-docs/scripts/rust-docs-mcp.sh cache my-crate --source local --path /path/to/crate
```

List cached crates:

```bash
.pi/skills/rust-docs/scripts/rust-docs-mcp.sh list-cached
```

Remove a cached crate:

```bash
.pi/skills/rust-docs/scripts/rust-docs-mcp.sh remove serde --version 1.0.215
```

### Documentation Queries

List items in a crate (with optional filtering by kind and query):

```bash
.pi/skills/rust-docs/scripts/rust-docs-mcp.sh list-items serde --version 1.0.215
.pi/skills/rust-docs/scripts/rust-docs-mcp.sh list-items serde --version 1.0.215 --kind trait
.pi/skills/rust-docs/scripts/rust-docs-mcp.sh list-items serde --version 1.0.215 --kind struct --query Deserialize
```

Full search within a crate:

```bash
.pi/skills/rust-docs/scripts/rust-docs-mcp.sh search serde "Serialize" --version 1.0.215
```

Preview search (lighter, returns only IDs/names/types):

```bash
.pi/skills/rust-docs/scripts/rust-docs-mcp.sh search-preview serde "Deserialize" --version 1.0.215
```

Fuzzy search across all cached crates:

```bash
.pi/skills/rust-docs/scripts/rust-docs-mcp.sh search-fuzzy "Deserializer"
```

### Item Inspection

Get detailed information about a specific item (signatures, fields, methods):

```bash
.pi/skills/rust-docs/scripts/rust-docs-mcp.sh get-details serde "serde::Serializer" --version 1.0.215
```

Get just the documentation string:

```bash
.pi/skills/rust-docs/scripts/rust-docs-mcp.sh get-docs serde "serde::Serializer" --version 1.0.215
```

View source code:

```bash
.pi/skills/rust-docs/scripts/rust-docs-mcp.sh get-source serde "serde::Serializer" --version 1.0.215 --context 5
```

### Dependency Analysis

Get dependency tree:

```bash
.pi/skills/rust-docs/scripts/rust-docs-mcp.sh deps serde --version 1.0.215
.pi/skills/rust-docs/scripts/rust-docs-mcp.sh deps serde --version 1.0.215 --depth transitive
.pi/skills/rust-docs/scripts/rust-docs-mcp.sh deps serde --version 1.0.215 --depth direct
```

### Module Structure

Generate hierarchical module tree:

```bash
.pi/skills/rust-docs/scripts/rust-docs-mcp.sh structure serde --version 1.0.215
```

## Examples

Cache and explore the `tokio` crate:

```bash
# Cache
.pi/skills/rust-docs/scripts/rust-docs-mcp.sh cache tokio --version 1.43.0

# Browse public API
.pi/skills/rust-docs/scripts/rust-docs-mcp.sh list-items tokio --version 1.43.0 --kind trait

# Search for specific functionality
.pi/skills/rust-docs/scripts/rust-docs-mcp.sh search-preview tokio "spawn" --version 1.43.0

# Get details on a specific item
.pi/skills/rust-docs/scripts/rust-docs-mcp.sh get-details tokio "tokio::spawn" --version 1.43.0

# View module structure
.pi/skills/rust-docs/scripts/rust-docs-mcp.sh structure tokio --version 1.43.0

# Check dependencies
.pi/skills/rust-docs/scripts/rust-docs-mcp.sh deps tokio --version 1.43.0 --depth direct
```

## Notes

- **First use**: Crate must be cached before browsing/searching. The `cache` command downloads the crate and generates rustdoc JSON.
- **Caching is persistent**: Cached crates are stored in `~/.rust-docs-mcp/cache/` and survive restarts.
- **Disk space**: Large crates (e.g., `tokio`) may use significant disk space.
- **Version pinning**: Always specify `--version` for crates.io sources.
- **GitHub rate limits**: Without `GITHUB_TOKEN`, GitHub API is limited to 60 requests/hour.
