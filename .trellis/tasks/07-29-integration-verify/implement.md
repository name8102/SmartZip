# Verification and landing plan

## Verification

- `cargo check --workspace --all-targets` passed.
- `cargo test -p smartzip-core -p smartzip-archive -p smartzip-db -p smartzip-engine -p smartzip-cli` passed.
- Production guard searches are empty for `ArchiveBackend`, `extract_with_progress`, and `fn capabilities(&self) -> BackendCapabilities`.
- Engine facade is 221 lines; workflow scheduling is separated from recursive implementation and capability modules.
- DB/history tests remain on the feat fine-grained model and pass.

## Landing plan

Land the clean line commits in order: engine modularize (`8751f43`, `28a0a25`), routing core (`cf97d42`), archive/config (`f717003`, `f3f7929`), engine/CLI (`170eb57`). Do not merge `integration/feat-plus-routing` or reintroduce the local-main DB redo. Future `db-*` and `cli-surface-*` redesigns remain separate follow-up work.
