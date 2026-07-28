# Rewrite CLI backend wiring for routing + keep feat commands

## Parent

`07-29-feat-routing-integration`

## Depends on

`07-29-port-routing-engine`

## Goal

One CLI `main.rs` design:

- Feat: file-aware detect/list, history subcommands, encoding preview, `no_history` / `force`, JSON outputs
- Routing: `--config`, extract `--backend`, `--verbose-routing`, `BackendRouter::from_config`, route event printing

## Single-path rules

- **One** extract options plumbing (struct **or** clear locals)—not both legacy positional and new struct half-migrated
- **One** backend construction helper used by detect/list/extract/encoding-preview
- No remaining production `BackendRouter::locate()` unless explicitly wrapped by the same helper for tests/dev and documented

## Reference

- Command surface / history: feat `main.rs`
- Routing flags / `print_route_events` / config load: `c64f77b` `main.rs`

Rewrite by reading both and implementing once; do not merge conflict markers.

## Acceptance

- [ ] `cargo check -p smartzip-cli` green
- [ ] Help shows history + routing flags
- [ ] Single backend construction path
