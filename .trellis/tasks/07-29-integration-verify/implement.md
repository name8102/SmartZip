# Verification and landing plan

## Verification

- `cargo check --workspace --all-targets` passed.
- `cargo test -p smartzip-core -p smartzip-archive -p smartzip-db -p smartzip-engine -p smartzip-cli` passed.
- `scripts/check_routing_guards.sh` is the persisted production guard; it is clean for `ArchiveBackend`, `extract_with_progress`, `BackendCapabilities`, router side-channel events, `BackendRouter::locate`, and the compatibility constructor.
- Route observations now enter `TaskEventKind::Route`; extraction staging has one owner and facts-aware extraction is part of the executor seam.
- `--verbose-routing` renders route planning, attempts, cleanup, selection, and exhaustion for engine-backed CLI operations.
- Engine facade is 221 lines; workflow scheduling is separated from recursive implementation and capability modules.
- DB/history tests remain on the feat fine-grained model and pass.

## Landing plan

Land the clean line commits in order: engine modularize (`8751f43`, `28a0a25`), routing core (`cf97d42`), archive/config (`f717003`, `f3f7929`), engine/CLI (`170eb57`). Do not merge `integration/feat-plus-routing` or reintroduce the local-main DB redo. Future `db-*` and `cli-surface-*` redesigns remain separate follow-up work.
