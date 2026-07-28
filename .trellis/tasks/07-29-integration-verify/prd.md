# Verify clean routing port and plan landing

## Parent

`07-29-feat-routing-integration`

## Depends on

- `07-29-port-routing-core`
- `07-29-port-routing-archive`
- `07-29-port-routing-engine`
- `07-29-port-routing-cli`

## Goal

Prove the **clean** branch has one stack, then land it without reintroducing hybrid or main DB redo.

## Checks

1. `cargo check --workspace --all-targets`
2. `cargo test -p smartzip-core smartzip-archive smartzip-db smartzip-engine` (and CLI if practical)
3. Grep guards (must be empty in production sources):
   - `ArchiveBackend`
   - `fn capabilities(&self) -> BackendCapabilities`
   - conflict markers
4. Confirm DB modules are feat fine-grained only
5. Optional: port GUI prototype from `c64f77b` only if still wanted (separate note)
6. Landing plan: replace local main line or PR; **never** merge main DB redo back

## Abandoned refs

- Do not resume `integration/feat-plus-routing` hybrid work
- Keep `wip/routing-gui-worktree` as reference until clean branch is verified

## Acceptance

- [ ] Workspace green
- [ ] Grep guards pass
- [ ] Landing plan written
