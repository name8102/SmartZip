# Modularize feat engine: thin facade, extract capabilities

## Parent

`07-29-feat-routing-integration`

## Why

Feat engine **behavior matches design** (history, header-first root detection, business-container skip, file-aware inspect/list, interactive prompters, nested enqueue). The problem is **structure**: `crates/smartzip-engine/src/lib.rs` is ~4828 lines with a ~1443-line `extract_recursive_with_listener_interactive` and large free-function tails (password, encoding, nested, volumes, policy).

That weight:

- fights ADR-001 (thin engine + caller injection) in practice
- makes the later `ArchiveBackend` → `ArchiveExecutor` port high-risk
- blocks clean DB/CLI evolution at stable hook points

**Do not** replace feat behavior with the thinner wip engine. **Do** split feat code so `SmartZipEngine` only schedules.

## Locked decisions

1. **Behavior freeze** for this task: same public workflow outcomes, events, history timing, and tests as feat on `origin/feat/db-history-persistence` (current clean branch base).
2. **Unidirectional deps**: modules must not depend on `SmartZipEngine`; facade depends on modules.
3. **No backend trait rename** here — still `ArchiveBackend` / existing progress helpers until `07-29-port-routing-engine`.
4. **No DB/CLI product redesign** here — preserve `TaskHistoryRecorder` injection and call-site meaning so later redesigns plug in without another god-file edit.
5. **Reference for “thin extract path” shape only**: `c64f77b` engine may inspire module boundaries; it is **not** a behavior source.

## Target shape

```
smartzip-engine
├── lib.rs                 # SmartZipEngine facade + re-exports (aim ≪ current size)
├── types.rs               # requests/results/candidates (pure data)
├── interactive.rs         # prompter traits + choice/context types
├── events.rs              # TaskEventListener, EventSink
├── workflow.rs            # extract_recursive* orchestration (schedule only)
├── access.rs              # root resolve, prepare archive, password access loop
├── password_order.rs      # load/order/remember password candidates
├── encoding_flow.rs       # encoding mode resolve, zip assessment, mojibake heuristics
├── nested.rs              # discover nested, carve, recycle, volume/first-volume helpers
├── policy.rs              # business container, scan policy, min-size gates
├── backend_util.rs        # backend_call, panic map, progress callback glue
├── history.rs             # existing
├── layout.rs / materialize.rs / detect.rs / embedded*.rs / …
└── (tests moved out of lib.rs where practical)
```

### Facade responsibilities only

```text
detect | inspect | list | extract
  → EventSink / optional history.start_*
  → access (root + password + encoding)
  → backend probe/list/test/extract
  → layout / materialize
  → nested enqueue / recycle
  → history.record_* / finish
  → ExtractWorkflowResult
```

## Suggested move order (small PR-sized steps)

1. **Leaf pure modules** (low risk): `interactive`, `events`, `types`, `policy`, volume helpers → `nested` (partial), `password_order`.
2. **Encoding + access**: `encoding_flow`, then `access` (`resolve_root_candidate`, `prepare_resolved_archive`, `access_archive_with_password`).
3. **Workflow**: peel `extract_recursive*` wrappers + main loop into `workflow` calling the above; keep public method names on `SmartZipEngine` as thin delegates if needed for API stability.
4. **Tests**: relocate `mod tests` from `lib.rs` to `tests/` or `src/*_tests` without weakening coverage.
5. **Stop condition for this task**: structure done + green tests; **not** routing port.

Exact file names may vary; acceptance is boundaries + size, not a mandatory directory tree.

## Documented history hook points (do not scramble)

Keep these semantics even if call sites move files:

| Phase | Expectation |
| --- | --- |
| Task start | `TaskHistoryRecorder::start_*` when recorder present |
| Per-file | `record_file_extraction` / known-file upserts where output + password + encoding context is complete (feat v3 file-grain) |
| Events | Engine-side sink first; replay/record consistent with current feat |
| Finish | `finish` with task outcome; DB failures stay best-effort warnings |

## Constraints / non-goals

- No capability-aware routing, no `ArchiveExecutor`, no sevenzz report port.
- No schema or CLI command redesign.
- No “simplify” that drops interactive paths, file-aware list/inspect, or history.
- No hybrid dual modules (old free fns + new modules both live as permanent duplicates).

## Depends on

None (first structural task on the clean line). May run **in parallel** with `07-29-port-routing-core` (core is additive and does not touch engine).

## Blocks

- **`07-29-port-routing-engine`** — retarget generics only after workflow/access boundaries exist.
- Indirectly lowers risk for later DB/CLI work (stable hooks).

## Acceptance

- [ ] `SmartZipEngine` public entrypoints remain; behavior covered by existing engine tests
- [ ] `lib.rs` no longer owns password ordering, encoding heuristics, nested discovery, and volume parsing as large inline free-fn blocks (moved to modules)
- [ ] Main extract path reads as orchestration over modules, not a 1k+ line monolith
- [ ] Modules do not import/call `SmartZipEngine`
- [ ] `TaskHistoryRecorder` still injected; history integration tests pass
- [ ] `cargo test -p smartzip-engine` green (including `history_integration` / `smartzip_integration` as applicable)
- [ ] No `ArchiveExecutor` / routing profile work mixed into this task’s commits unless purely incidental re-exports

## Success metric (soft)

Aim for `lib.rs` on the order of **hundreds of lines** of facade/wiring rather than multi-thousand-line ownership of every policy. Exact LOC is guidance; review for “scheduler vs capability” clarity.
