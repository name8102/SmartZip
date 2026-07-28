# Port capability-aware routing onto feat history base (clean rewrite)

## Why rewrite, not merge

A partial three-way merge of `origin/feat/db-history-persistence` with the workspace routing snapshot produced a **hybrid** seam:

- `ArchiveAdapter` simultaneously carried routing `id()` and feat-era `capabilities()` / `extract_with_progress` / `should_test_before_extract`
- dual filename decode helpers in `native_zip`
- engine still called `extract_with_progress` on the executor while routing removed that from the production seam
- CLI still used `BackendRouter::locate()` beside config-aware router types

That is exactly the class of conflict / duplication / redundancy we refuse. Prefer a **clean rewrite**: keep feat behavior as the base, re-implement routing from the workspace design and code as the single source of truth for backend selection.

## Locked decisions

1. **Product base**: `origin/feat/db-history-persistence` (fine-grained DB + history + header-first detection + file-aware CLI).
2. **Discard**: local `main` simple DB redo (`history.rs` / `known_file.rs` style).
3. **Routing direction**: workspace capability-aware routing (`c64f77b` / `wip/routing-gui-worktree`) is the **spec and reference implementation**, not a patch to splice blindly.
4. **No hybrid traits**: one executor seam, one adapter seam, one capability model (profiles), one construction path for the router.

## Working branches

| Branch | Role |
| --- | --- |
| `integration/feat-plus-routing-clean` | **Active** clean rewrite line (starts at pure feat). |
| `wip/routing-gui-worktree` (`c64f77b`) | Immutable reference for routing + GUI + encoding work. |
| `integration/feat-plus-routing` (`5146265`) | **Abandoned hybrid checkpoint** — keep only as archaeology / diff source, do not continue. |
| local `main` | Device redo line; DB implementation not to be reintroduced. |

## Reference sources (read, then rewrite)

- Routing design / ADR text: `git show c64f77b:CONTEXT.md` and archived task `07-20-capability-aware-backend-routing`
- Code reference: `git show c64f77b:crates/smartzip-core/src/routing.rs` and archive/config/cli paths under that commit
- Feat behavior that must keep working: history recorder, file-grain DB, header-first root detection, business-container skip, file-aware detect/list/history CLI

## Target end-state (single stack)

```
CLI/GUI
  -> SmartZipEngine (feat history + extract workflow)
       -> ArchiveExecutor (BackendRouter only)
            -> ArchiveAdapter[] (native zip / 7z / unrar / …)
                 profiles from smartzip-config (family/version/installation)
```

- Capability claims live in **profiles**, not in `fn capabilities() -> BackendCapabilities` on adapters.
- Progress callbacks: either folded into extract requests / executor methods **once**, or kept only where the chosen design needs them — not both old and new.
- Router construction: `from_config` (+ optional forced adapter). Discovery supplements config; no parallel “old locate is the real path” forever.

## Child task sequence (strict order)

1. **`07-29-port-routing-core`** — add `smartzip-core` routing types + errors + `TaskEventKind::Route` from reference; no archive yet.
2. **`07-29-port-routing-archive`** — rewrite archive/config router & adapters on feat tree using reference; delete old capability dual paths.
3. **`07-29-port-routing-engine`** — point engine only at `ArchiveExecutor`; preserve history; remove `extract_with_progress` dual call sites in favor of one design.
4. **`07-29-port-routing-cli`** — feat CLI commands + routing flags/config in one coherent `main.rs` rewrite of the extract/backend wiring (not conflict resolution).
5. **`07-29-integration-verify`** — check/test + landing plan.

Optional later (not blocking routing port): GUI prototype binary from `c64f77b` if still desired.

## Explicit non-goals

- Continuing conflict resolution on `integration/feat-plus-routing`
- “Keeping both” BackendCapabilities and BackendCapabilityProfile
- Cherry-picking local main DB commits

## Success criteria

- [ ] Clean branch has **no** `ArchiveBackend` symbol
- [ ] Clean branch has **no** adapter `fn capabilities(&self) -> BackendCapabilities` as the routing authority
- [ ] Engine history + DB tests still express feat behavior
- [ ] Router selection is profile + requirements based (reference semantics)
- [ ] Workspace check/tests green on the clean branch
