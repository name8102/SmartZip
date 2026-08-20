# Implementation Plan — Robust Split-Volume Resolution

## Phase 1 — Replace generic handwritten file magic

- Add `infer` as a workspace dependency and wire it into `smartzip-engine`.
- Replace the handwritten ordinary-file magic table in `detect.rs` with `infer`-based classification.
- Keep SmartZip archive-specific detection/structure probing separate from generic file-type inference.
- Preserve embedded-archive scanning as an independent path; an ordinary carrier type must not suppress later-offset embedded findings.
- Add regression tests for existing ZIP/RAR/7z/archive detection plus JPEG/PNG/PDF/MP4/etc ordinary-file exclusion.

## Phase 2 — Add archive-layer static volume probes

Create `smartzip-archive` volume-probe modules for RAR, ZIP, and 7z.

### RAR

- Parse multivolume flags.
- Parse internal volume number where available.
- Surface continuation/last-volume evidence that can be obtained cheaply.
- Cover RAR3/4 and RAR5 fixtures where practical.

### ZIP

- Parse EOCD disk metadata.
- Parse ZIP64 locator/EOCD when present.
- Determine strong standalone closure when disk metadata and central-directory bounds allow it.

### 7z

- Parse and validate signature/start header.
- Validate StartHeader CRC.
- Surface `expected_logical_size = 32 + NextHeaderOffset + NextHeaderSize` when valid.
- Distinguish strong standalone closure from "logical extent exceeds this physical file".

No backend `test` invocation belongs in these probes.

## Phase 3 — Directory index and Unicode sequence hypotheses

- Add a task-scoped `DirectoryVolumeIndex` cache keyed by directory.
- Enumerate all regular files in the seed directory only when sibling volume discovery is required.
- Preserve original paths while building normalized analysis strings.
- Reuse `unicode-normalization` NFKC support already present in the engine.
- Add a mature numeral parser for Unicode/Chinese ordinal forms rather than custom lookup tables.
- Generate filename hypotheses by varying exactly one ordinal token at a time.
- Keep `filename_ordinal` separate from archive-internal `logical_index`.
- Treat filename gaps as warnings only.

Unit-test at minimum:

- ASCII decimal numbering;
- full-width/circled compatible forms;
- Chinese compound numerals supported by the chosen library;
- multiple numeric tokens where each produces a separate one-dimensional hypothesis;
- two-member sequences;
- arbitrary starting ordinal;
- filenames with no usable ordinal token.

## Phase 4 — Alias recovery and alternate slots

- Add a deliberately small set of common duplicate/copy suffix aliases.
- Do not globally normalize them away before primary hypothesis generation.
- Allow alias views to add/fill candidates only inside a hypothesis that has independent sequence/structure support.
- Model each logical/filename slot with 0..N candidates.
- Add tests for:
  - `01, 02, 03_1, 04`;
  - `01, 02, 03, 03_1, 04`;
  - `(1)/(2)/(3)` as the actual sequence, proving alias logic does not erase the primary ordinal dimension.

## Phase 5 — Candidate elimination and sampled fingerprints

For slots with more than one candidate:

1. use archive-internal metadata/volume number;
2. reject definite truncation or impossible logical-size constraints;
3. compare size;
4. use bounded sampled BLAKE3 fingerprints for likely duplicate copies.

- Use fixed bounded samples rather than full-volume hashing.
- Keep exact sample positions/block size as an internal constant or small helper configuration, not a product setting.
- Fold fingerprint-identical alternates as duplicates.
- Return `GroupingAmbiguous` when materially different candidates remain plausible.

## Phase 6 — Resolve interval and outcomes

Implement the shared resolver producing:

- `Single`;
- `Resolved`;
- `ResolvedWithWarnings`;
- `Incomplete`;
- `GroupingAmbiguous`.

Rules:

- use strong archive-start evidence to clip unrelated prefix files;
- use expected logical extent/last-volume/footer evidence to clip unrelated suffix files;
- never arbitrary-select an interior subset;
- filename gap alone never yields `Incomplete`;
- definite static missing-volume evidence yields `Incomplete`;
- unique membership/order with uncertain completeness proceeds as resolved;
- unresolved membership/order yields `GroupingAmbiguous`.

Add focused resolver tests with synthetic directory layouts before wiring into the main workflow.

## Phase 7 — Canonical volume materialization

- Add a RAII staging object for resolved volume sets.
- Generate backend-compatible canonical names for each supported volume family.
- Prefer reflink/clone/CoW materialization on the source filesystem.
- Fall back to regular copy when clone is unavailable.
- Never rename or move originals.
- Validate that 7zz/unrar follow the canonical subsequent-volume names in integration fixtures.

## Phase 8 — Replace current workflow shortcut

- Remove the main-loop `is_first_volume()` early skip in `extract_workflow.rs`.
- Remove/retire `is_first_volume`, `rar_part_volume_index`, and `numeric_volume_index` from `nested.rs` after migration.
- Insert shared volume resolution after cheap local classification but before backend list/extract.
- Embedded findings at nonzero offsets must bypass cross-file sibling resolution.
- Preserve ordinary standalone archive and known ordinary-file fast paths.

## Phase 9 — Root coalescing and nested reuse

- Coalesce explicit input paths that resolve to the same `VolumeSet` so one logical archive is processed once.
- Make `list` and `extract` call the same resolution API.
- Route nested extracted candidates through the same resolver.
- Reuse `DirectoryVolumeIndex` for multiple nested candidates from the same directory.

## Phase 10 — Error/history/events integration

Introduce structured error/event semantics as needed for at least:

- missing/incomplete volumes;
- grouping ambiguity;
- canonical materialization failure.

Do not rely on backend stderr text alone for errors already established by the resolver.

Where existing history fields permit it, record useful volume diagnostics without changing unrelated history semantics. Keep any schema expansion narrowly scoped and separately justified if required.

## Integration Matrix

Cover at least:

- normal `.partNN.rar`;
- old-style RAR volumes where supported by fixtures;
- `.7z.001/.002/...`;
- ZIP split/spanned;
- disguised extensions such as `.jpg`;
- Unicode ordinals;
- two-member sets;
- user selects a middle member;
- multiple explicit roots from one set;
- filename gap with a valid set;
- definite missing member;
- unique set with static completeness unknown;
- unrelated numbered files before/after the real interval;
- alternate/incomplete copy replaced by a valid alternate;
- duplicate alternate folded by bounded fingerprint;
- two distinct plausible alternates -> `GroupingAmbiguous`;
- ordinary JPEG/PNG/PDF sequence does not trigger cross-file volume resolution;
- ordinary carrier with an embedded archive still reaches embedded scanning;
- generic arbitrary byte-split input remains unsupported.

## Verification

- Run crate-level unit tests while each phase lands.
- Run SmartZip engine/archive integration tests after workflow wiring.
- Run full workspace `cargo test` before completion.
- Keep detection static/cheap: no backend full `test` calls and no unconditional full-file hashing in the resolver path.
