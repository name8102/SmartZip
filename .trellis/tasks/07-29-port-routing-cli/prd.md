# CLI backend wiring for routing (keep feat commands for now)

## Parent

`07-29-feat-routing-integration`

## Depends on

`07-29-port-routing-engine`

## Scope discipline (important)

CLI **product** redesign (command surface, history UX, output shape, GUI split) is **out of scope** for this task. Those ideas get **separate** trellis tasks later.

This task is **wiring only**: one backend construction path on top of the **current** feat command set so the clean branch compiles and extract/detect/list/history still run through `ArchiveExecutor` / `from_config`.

If a future CLI redesign lands first, shrink this task further to “thin adapter over the new CLI” — do not delete the need for a single backend helper.

## Goal

One CLI backend path:

- **Keep (until redesign task)**: feat file-aware detect/list, history subcommands, encoding preview, `no_history` / `force`, JSON outputs as they exist on feat
- **Add**: `--config`, extract `--backend` (or equivalent), `--verbose-routing` as needed, `BackendRouter::from_config`, optional route event printing

## Single-path rules

- **One** extract options plumbing (struct **or** clear locals)—not both legacy positional and new struct half-migrated
- **One** backend construction helper used by detect/list/extract/encoding-preview
- No remaining production `BackendRouter::locate()` unless explicitly wrapped by the same helper for tests/dev and documented

## Reference

- Command surface / history: feat `main.rs` (behavior freeze for *this* ticket)
- Routing flags / `print_route_events` / config load: `c64f77b` `main.rs`

Implement once from both readings; do not merge conflict markers; do not invent a third command taxonomy here.

## Acceptance

- [ ] `cargo check -p smartzip-cli` green
- [ ] Single backend construction path (no locate+from_config dual production path)
- [ ] Existing feat commands still reachable (help lists them) **or** explicitly deferred only if a superseding CLI redesign task already replaced them
- [ ] Routing flags needed for executor construction are present
- [ ] No drive-by CLI product redesign in the same commits
