# Verify clean routing port and plan landing

## Parent

`07-29-feat-routing-integration`

## Depends on

- `07-29-engine-modularize`
- `07-29-port-routing-core`
- `07-29-port-routing-archive`
- `07-29-port-routing-engine`
- `07-29-port-routing-cli` (wiring scope; see that task — not full CLI product redesign)

## Goal

Prove the **clean** branch has one stack (modular feat engine + capability-aware routing), then land it without reintroducing hybrid or main DB redo.

## Checks

1. `cargo check --workspace --all-targets`
2. `cargo test -p smartzip-core smartzip-archive smartzip-db smartzip-engine` (and CLI if practical)
3. Grep guards (must be empty in production sources):
   - `ArchiveBackend`
   - `fn capabilities(&self) -> BackendCapabilities` as routing authority
   - conflict markers
4. Engine structure smoke: facade/modules present; no return to a single god-file owning password/encoding/nested policy (spot-check `smartzip-engine` layout)
5. Confirm DB modules are feat fine-grained only (until a **separate** DB redesign task lands)
6. Optional: port GUI prototype from `c64f77b` only if still wanted (separate note/task)
7. Landing plan: replace local main line or PR; **never** merge main DB redo back
8. Explicit non-blockers for *this* verify: future `db-*` / `cli-surface-*` redesign tasks — track them outside or after this gate

## Abandoned refs

- Do not resume `integration/feat-plus-routing` hybrid work
- Keep `wip/routing-gui-worktree` as reference until clean branch is verified
- Do not cancel the routing child chain because engine was modularized first — modularize is a prerequisite, not a replacement

## Acceptance

- [ ] Workspace green
- [ ] Grep guards pass
- [ ] Engine modularize + routing port both reflected on the branch
- [ ] Landing plan written
