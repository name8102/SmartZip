# Implementation Plan — Ponytail-style repository simplification

## Phase 0 — Establish a behavior/consumer baseline

Before deleting abstractions, build an explicit consumer map for every item being simplified.

- Record production callers for routing profiles/capability facts, config fields, engine extraction entry points, volume helpers, and CLI/GUI surfaces.
- Separate production consumers from test-only consumers and future-task comments.
- Run the relevant baseline tests before each major deletion so failures can be attributed to the simplification.
- Do not treat a symbol as dead based only on repository search when indirect trait/dynamic dispatch is involved; confirm the real construction/call path.

Deliverable: a short implementation note or commit message summary documenting any candidate from the PRD that is retained because a real consumer was found.

## Phase 1 — Collapse routing to the real backend decision model

Work from `smartzip-core::routing`, `smartzip-config`, and `smartzip-archive::router` outward.

1. Enumerate every production-created routing requirement/fact.
2. Identify the minimum adapter metadata needed for current SevenZip/Unrar selection:
   - adapter identity;
   - supported operations;
   - supported containers;
   - password handling where it changes eligibility;
   - charset override where it changes eligibility;
   - priority;
   - explicit/auto-discovered installation metadata needed by the CLI;
   - task-local negative observations needed for safe fallback.
3. Replace the generic profile composition/policy language with concrete fields or the smallest equivalent representation.
4. Remove family/version/installation capability composition if no production behavior depends on it.
5. Remove `NativeZip`/`Custom` configuration variants if they still cannot create usable adapters.
6. Preserve route diagnostics and deterministic candidate ordering.
7. Update routing tests to cover actual adapter eligibility/fallback behavior rather than generic capability-language semantics.
8. Update `scripts/check_routing_guards.sh` if its assertions refer to architecture that no longer exists.

Do not merge `ArchiveExecutor` into the router: it is a real engine testing seam with multiple fake implementations.

## Phase 2 — Complete ADR-009 staging ownership

Refactor extraction so staging ownership has one source of truth.

1. Define the adapter-attempt staging lifecycle in `OutputMaterializer` or its replacement.
2. Give each adapter attempt an isolated directory owned by that lifecycle.
3. Make router/executor choose and invoke adapters against caller-provided attempt paths rather than creating its own second temp hierarchy.
4. On retryable adapter failure:
   - wait for backend cancellation/process cleanup semantics to complete;
   - delete the attempt directory;
   - verify it is gone;
   - only then try the next adapter.
5. On successful adapter selection:
   - retain the selected tree as the staging tree;
   - perform layout planning, collision resolution, and commit from that tree;
   - never fallback to another backend after selection if commit/layout fails.
6. Remove `move_attempt_output` and any router temp helpers that become unnecessary.
7. Keep strict cleanup/error propagation tests, including cancellation.

This phase should reduce ownership, not merely relocate the same two-layer design.

## Phase 3 — Create one canonical volume preparation path

Refactor `VolumeResolver` integration before changing the resolver algorithm itself.

1. Introduce a concrete preparation result/helper used by both list and extract. It should carry at minimum:
   - logical resolution (`Single` or resolved set);
   - canonical input/entrypoint when materialization is required;
   - detected format;
   - warnings;
   - RAII keepalive for staged volume materialization where needed.
2. Fold `Resolved` and `ResolvedWithWarnings` into one materialization code path with warnings as data.
3. Replace extract's temporary pre-queue resolver + main-loop re-resolution with one root coalescing/preparation flow.
4. Replace list's synthetic-candidate preliminary resolution + second resolution with the same shared helper.
5. Ensure nested candidates reuse the same semantics and directory cache where the workflow lifetime permits it.
6. Either:
   - make `coalesce_roots()` the production entry point and complete its semantics; or
   - replace it with the new preparation API and delete it plus its test-only implementation.
7. Preserve `Incomplete`, `GroupingAmbiguous`, raw continuation handling, and non-zero embedded-offset isolation.

Do not redesign filename/structure heuristics in this phase unless required to remove duplicate integration logic.

## Phase 4 — Delete dead config model

After routing changes settle, audit `SmartZipConfig` field by field.

- Keep backend configuration that is actually read by `build_backend`/router.
- Remove top-level compression/scanner/delete/log/gui/layout settings that still have no production consumer.
- If the config wrapper becomes only a backend wrapper, decide whether `SmartZipConfig { backends }` still adds value or whether loading `BackendConfig` directly is simpler.
- Update serialization tests to cover the remaining schema only.
- Do not preserve compatibility for config keys that have never been part of a released/consumed configuration contract unless release evidence proves otherwise.

If backward compatibility is uncertain, mark that specific deletion as uncertain and retain it until release/config usage is confirmed.

## Phase 5 — Remove placeholder product surfaces

### CLI

- Delete `Command::Test` if it still always exits unimplemented.
- Delete `Command::Compress` if it still always exits unimplemented.
- Delete ignored `--use-clipboard` flags until clipboard integration exists.
- Keep backend/internal `test` operations that are consumed by extraction/password flows or tests.

### GUI

- Remove inactive tabs/views for Passwords, Rules, Logs, and Settings while they have no navigation/actions/models.
- Keep the functional Tasks/drop-detect surface.
- Avoid adding click handling merely to justify keeping placeholder views.

## Phase 6 — Remove deprecated/future-only residue

Audit and delete confirmed dead compatibility scaffolding:

- `ArchiveAccessOutcome.used_password`;
- `ArchiveAccessOutcome.password_prompt_cancelled`;
- retired `is_first_volume` export and its filename-only helpers;
- old core request/task types superseded by the active engine/archive request models;
- any routing/config types left unreachable after Phases 1 and 4.

For `is_first_volume`, migrate tests to assert resolver behavior before deleting the helper.

## Phase 7 — Shrink SmartZipEngine forwarding surface

With the earlier APIs stable:

1. List production callers of each extraction method.
2. Introduce one small hooks/context value only if it eliminates multiple public forwarding functions without adding more concepts.
3. Keep one canonical extraction entry point.
4. Move test convenience construction into test helpers.
5. Avoid source churn if the forwarding methods still have distinct real callers and the replacement would not reduce conceptual load.

This phase is optional if the consumer map shows little net benefit.

## Phase 8 — Remove config writer dependency if unused

If no production code writes config after Phase 4:

- delete `SmartZipConfig::save()` or equivalent unused write API;
- remove `atomic-write-file` from the config crate/workspace dependencies;
- keep the round-trip/load tests relevant to the remaining read path.

If a real config writer is found, keep atomic persistence; do not replace it with direct `std::fs::write` merely to remove a dependency.

## Verification

Run focused tests after each phase, then full validation:

```bash
cargo test -p smartzip-core
cargo test -p smartzip-config
cargo test -p smartzip-archive
cargo test -p smartzip-engine
cargo test -p smartzip-cli
cargo test --workspace
```

Also run the routing guard script if it still exists and remains meaningful:

```bash
scripts/check_routing_guards.sh
```

Where external archive tools are required, run the existing integration suite with the same prerequisites documented by the repository.

## Review checklist

Before completion, verify:

- every removed abstraction had its production callers checked;
- no safety/trust-boundary check disappeared as incidental cleanup;
- staging has exactly one owner;
- route fallback still cleans failed attempts before retry;
- list/extract use the same volume preparation path;
- no placeholder CLI option/subcommand silently remains;
- retained config fields have named production consumers;
- no new generic framework was introduced to replace deleted generic machinery;
- net concept count and code volume are materially lower.

## Implementation notes

- Retained `ArchiveExecutor`, `ArchiveAdapter` identity, internal `test`/`compress` operations, deterministic routing diagnostics, retry taxonomy, and task-local unsupported-codec observations because production workflows or regression tests still consume them.
- Replaced the unused generic capability/profile rule language with concrete adapter operation/container/password/charset capabilities; configuration now carries only backend routing settings.
- `OutputMaterializer` remains the extraction staging owner: the router reuses its caller-provided directory and verifies failed-attempt cleanup instead of creating `.smartzip-attempt-*` siblings.
- Added one `VolumeResolver::prepare` path shared by list and extract, with warnings and materialization keepalive in one result; removed the filename-only volume helper and unused root coalescing API.
