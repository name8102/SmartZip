# Ponytail-style repository simplification

## Background

A whole-repository Ponytail-style review of the current `main` branch found that SmartZip is generally modular for good reasons, but several areas have accumulated unnecessary surface area or duplicate ownership. The goal of this task is not generic refactoring and not bug fixing. It is to reduce concepts, code paths, public APIs, dead configuration, and duplicate staging/resolution logic while preserving current behavior, safety boundaries, compatibility requirements that are actually in use, and non-trivial tests.

The review prioritized deletion and reuse over new abstractions. In particular, this task must not remove input validation, archive safety checks, strict cleanup semantics, cancellation handling, corruption/data-loss protections, or meaningful regression tests merely to reduce LOC.

## Goal

Reduce the amount of code and conceptual machinery required to understand and maintain SmartZip, with highest priority on:

1. shrinking capability-aware backend routing to the capabilities the product actually consumes;
2. completing ADR-009 so extraction has one staging owner;
3. making volume resolution/coalescing/materialization a single shared path used by list and extract.

Secondary cleanup should remove public/config/UI/API surface that has no current consumer.

## Non-goals

- Do not change supported archive formats or backend fallback semantics merely for simplification.
- Do not weaken archive path safety, extraction limits, cleanup verification, cancellation, or failure isolation.
- Do not merge crates solely to reduce crate count. Existing `scanner`, `passwords`, `platform`, `db`, `encoding`, and similar boundaries should remain unless a concrete consumer analysis proves otherwise.
- Do not turn this into a correctness, security, or performance review. Fix such issues only when they are a direct consequence of removing unnecessary complexity.
- Do not implement compression, the pending integrity-check command, clipboard integration, or future GUI features as part of this task.
- Do not introduce a new generic framework to replace an old generic framework.

## Required work

### P0 — Simplify backend routing capability model

Current routing includes a general capability rule language (`CapabilityId`, `CapabilityRule`, support states, requirement classes, family/version/installation profile composition, facts, negative capability keys, etc.). Production routing currently has only concrete SevenZip and Unrar adapter families; configured NativeZip is deprecated/ignored and `Custom(String)` cannot instantiate an adapter. Normal requests primarily need operation/container support plus password and charset-override requirements.

Refactor routing toward the smallest model that still preserves current real behavior:

- keep `ArchiveExecutor` as the engine test seam;
- keep `ArchiveAdapter` identity and multi-adapter routing;
- keep stable priority ordering, explicit configured installations, auto-discovery, forced-adapter diagnostics, route diagnostics, retryable fallback, and task-local rejection of proven incompatibilities;
- keep support checks that are currently exercised by real production paths;
- remove or collapse capability/profile machinery that exists only for hypothetical adapter/capability combinations;
- remove config fields/types that become unreachable after the simplification;
- migrate tests to assert behavior rather than the removed generic policy language.

Do not remove codec/filter-aware routing if a production caller is found to populate and depend on those facts. In that case, document the consumer and retain the minimal representation required by it.

### P0 — Complete ADR-009: one extraction staging owner

Current extraction nests router-owned `.smartzip-attempt-*` directories under/alongside an `OutputMaterializer` staging lifecycle and then moves the selected attempt tree into the materializer staging directory. This splits cleanup and ownership across `smartzip-archive` and `smartzip-engine`.

Implement ADR-009:

- `OutputMaterializer` (or the renamed equivalent) owns extraction staging;
- each adapter attempt still receives an isolated directory;
- failed attempts are deleted and absence is verified before fallback;
- a successful adapter tree becomes the selected staging tree and is then passed to layout planning/collision/commit;
- once an adapter is selected, failures in layout/collision/commit must not trigger fallback to another adapter;
- cancellation semantics and backend process kill/wait contracts remain intact;
- remove router-side `move_attempt_output`/duplicate temp ownership once no longer needed.

The implementation may adjust executor/adapter request signatures if that produces a cleaner ownership model, but avoid a broad API redesign unrelated to staging.

### P0 — Unify volume resolution integration

`VolumeResolver` is already the intended shared resolution layer, but current callers duplicate integration logic:

- extract pre-resolves roots with a temporary resolver for dedup, then resolves them again in the main loop;
- `coalesce_roots()` exists but is not used by production callers;
- list may build a synthetic candidate, resolve once to decide whether it is a volume continuation, then construct another resolver and resolve again;
- `Resolved` and `ResolvedWithWarnings` materialization paths contain repeated code.

Create one shared integration path that:

- resolves a candidate/root once per workflow stage unless re-resolution is materially required;
- coalesces explicit roots that belong to the same logical volume set without reporting them as skipped failures;
- exposes warnings without duplicating the materialization branch;
- returns enough information for list and extract to use the canonical entrypoint and detected format;
- preserves `Incomplete` and `GroupingAmbiguous` behavior;
- keeps non-zero embedded offsets isolated from sibling discovery;
- either makes `coalesce_roots()` the production API or deletes/replaces it so there is only one canonical mechanism.

Prefer a concrete result type/helper over a second generic framework.

### P1 — Remove dead configuration surface

Audit `SmartZipConfig` against real consumers. At present CLI config loading uses backend routing configuration, while other top-level settings are not wired into CLI defaults or GUI behavior.

Remove configuration keys/types that have no current production consumer, including as applicable:

- compression defaults;
- scanner defaults duplicated by CLI/request construction;
- deletion policy flags;
- log level;
- GUI settings;
- layout settings duplicated by CLI/request construction.

If a field is retained, identify its current consumer in code. Avoid keeping a key solely because a future UI may use it.

### P1 — Remove unreachable GUI skeleton

The current GPUI shell contains tabs/views for Passwords, Rules, Logs, and Settings, but there is no tab click handler that changes `active_tab`; the non-Tasks views are therefore not part of the usable product, and several are only placeholders/hard-coded values.

Until those features have real models/actions:

- keep the working Tasks/drop-detect surface;
- remove unreachable placeholder views, state, and labels;
- do not add navigation infrastructure merely to make placeholders reachable.

### P1 — Remove placeholder CLI/API surface

Audit and remove public surface that currently advertises unavailable behavior:

- `smartzip test` CLI shell that discards its arguments and exits unimplemented;
- `smartzip compress` CLI shell that only exits unimplemented;
- `--use-clipboard` flags that are parsed but intentionally ignored and still build `PasswordCandidateRequest { clipboard: None, ... }`.

This does not mean deleting internal backend `test` support that extraction or tests currently consume. Remove only the non-functional user-facing shell.

### P1 — Remove future-only/deprecated API residue

Audit the following and delete them if the repository still has no production consumer:

- `ArchiveAccessOutcome.used_password` and `password_prompt_cancelled` fields kept under `#[allow(dead_code)]` for a future test-command task;
- old core request/task types superseded by the engine/archive request models;
- retired `is_first_volume()` and its filename-only helpers once remaining tests are migrated to `VolumeResolver`;
- `VolumeResolver::coalesce_roots()` if the new canonical root-resolution API replaces it.

Do not retain dead fields solely to avoid reshaping a struct in a future task.

### P2 — Reduce forwarding API variants

`SmartZipEngine` currently exposes multiple extraction methods that differ mostly by which optional hooks/listeners are supplied as `None`.

After the P0 work stabilizes, reduce this surface if it can be done without harming actual callers. Prefer one execution entry point plus a compact hooks/context value over multiple forwarding overloads. Test-only convenience should live in test helpers rather than permanently widening the public API.

### P2 — Remove config-only dependency if no writer exists

If `SmartZipConfig::save()` still has no production caller after the config cleanup, remove it and the `atomic-write-file` dependency. Do not replace atomic writing with a less safe ad-hoc write. If an actual settings/config writer exists by implementation time, retain crash-safe atomic persistence instead.

## Acceptance criteria

- Routing, extraction, list, password, encoding, embedded archive, history, and volume regression tests continue to pass for retained behavior.
- No safety validation, strict failed-attempt cleanup, cancellation, corruption handling, or data-loss protection is removed for LOC reduction.
- There is exactly one owner of extraction staging semantics.
- List and extract share one canonical volume-resolution integration path rather than independently reproducing the same state machine.
- No config field retained by `SmartZipConfig` is without a production consumer; any intentional exception is documented next to the field.
- No CLI flag/subcommand remains that is intentionally ignored or always exits as an unimplemented placeholder.
- No production-dead field is retained with `#[allow(dead_code)]` solely for a future feature.
- Retired filename-only volume detection is no longer exported once its test-only compatibility use is removed.
- The simplification does not add more conceptual layers than it removes.
- `cargo test` passes.
- `scripts/check_routing_guards.sh` is updated or removed if its guarded legacy symbols no longer match the final routing architecture.

## Expected impact

The review estimated roughly 1,300–2,000 lines of production Rust and 500–1,000 lines of obsolete/over-specific tests may be removable or collapsible, plus at least one direct dependency (`atomic-write-file`) if config writing remains unused. These are directional estimates, not targets. Do not delete useful code merely to hit them.

The primary success metric is fewer concepts and fewer duplicate ownership paths, not raw LOC.
