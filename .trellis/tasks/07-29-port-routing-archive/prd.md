# Rewrite archive routing stack on feat base

## Parent

`07-29-feat-routing-integration`

## Depends on

`07-29-port-routing-core`

## Goal

On the clean feat branch, **rewrite** `smartzip-archive` + backend config so there is one routing stack matching the workspace design.

## Single-model rules (no duals)

| Keep (routing) | Remove / do not keep as authority |
| --- | --- |
| `ArchiveExecutor` / `ArchiveAdapter` | `ArchiveBackend` |
| `BackendCapabilityProfile` + config composition | `fn capabilities() -> BackendCapabilities` as router input |
| `BackendRouter::from_config` / plan / fallback taxonomy | “locate then first match” as the real production path |
| One filename decode path in native zip | Two competing `decode_entry_name` implementations |

## Feat behaviors to re-express, not lose

- Header-first / cheap format sniff where feat relied on it for root detection (implement **once**, in the place the new design owns—adapter probe, facts, or engine—not two places disagreeing).
- Any feat `sevenzz` / safety fixes still required after reading feat diffs against reference.

## Progress

Reference routing extract path may differ from feat `extract_with_progress`. Choose **one**:

- Progress on extract request / executor API used by engine, **or**
- Adapter-only internal progress with engine using plain `extract`

Document the choice in `implement.md` when executing. Do not leave both required.

## Expected interim breakage

Engine and CLI will not compile until their port tasks. That is acceptable if this task leaves `smartzip-archive` + `smartzip-config` internally consistent (`cargo check -p smartzip-archive`).

## Acceptance

- [ ] No `ArchiveBackend` in archive crate
- [ ] No adapter `capabilities()` used for routing decisions
- [ ] Router implements `ArchiveExecutor`
- [ ] Adapters implement `ArchiveAdapter` with stable `id()`
- [ ] `cargo check -p smartzip-archive` green
