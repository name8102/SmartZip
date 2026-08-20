# Design — Robust Split-Volume Resolution

## Architecture Boundary

### `smartzip-archive`: format facts only

Add a lightweight structural probe layer, for example:

```rust
pub enum VolumeProbeResult {
    NotApplicable,
    Standalone(ArchiveFormat),
    MultiVolume(VolumeStructure),
    PossiblyMultiVolume(VolumeStructure),
}

pub struct VolumeStructure {
    pub format: ArchiveFormat,
    pub logical_volume_index: Option<u32>,
    pub expected_volume_count: Option<u32>,
    pub expected_logical_size: Option<u64>,
    pub is_last_volume: Option<bool>,
}
```

The archive crate must not know about sibling directory enumeration, filename clustering, duplicate suffixes, task roots, or staging policy.

Suggested modules:

```text
smartzip-archive/src/volume_probe.rs
smartzip-archive/src/volume_probe/rar.rs
smartzip-archive/src/volume_probe/zip.rs
smartzip-archive/src/volume_probe/sevenzip.rs
```

### `smartzip-engine`: filesystem resolution and workflow

Suggested modules:

```text
smartzip-engine/src/volumes.rs
smartzip-engine/src/volumes/directory.rs
smartzip-engine/src/volumes/sequence.rs
smartzip-engine/src/volumes/alias.rs
smartzip-engine/src/volumes/fingerprint.rs
smartzip-engine/src/volumes/materialize.rs
```

Core result model:

```rust
pub enum VolumeResolution {
    Single,
    Resolved(VolumeSet),
    ResolvedWithWarnings {
        set: VolumeSet,
        warnings: Vec<VolumeWarning>,
    },
    Incomplete(VolumeProblem),
    GroupingAmbiguous {
        hypotheses: Vec<VolumeSetHypothesis>,
    },
}

pub struct VolumeSet {
    pub format: ArchiveFormat,
    pub entrypoint: PathBuf,
    pub members: Vec<VolumeMember>,
    pub expected_volume_count: Option<u32>,
    pub expected_logical_size: Option<u64>,
}

pub struct VolumeMember {
    pub path: PathBuf,
    pub filename_ordinal: Option<u64>,
    pub logical_index: Option<u32>,
}
```

Do not add a `complete: bool`; static resolution and backend verification are separate concerns.

## Resolution Pipeline

```text
candidate
  |
  +-- embedded finding at nonzero offset
  |      -> process as embedded archive; no sibling volume discovery
  |
  +-- ordinary/root or extracted physical file
         |
         v
     infer + archive structural probe
         |
         +-- known ordinary non-archive -> no VolumeResolver
         +-- structurally standalone archive -> no sibling scan
         +-- definite/possible multivolume -> directory hypothesis path
         +-- raw/unknown + usable ordinal hypothesis -> directory hypothesis path
         +-- raw/unknown + no sequence evidence -> normal unknown/non-archive path
```

When directory resolution is entered:

```text
DirectoryVolumeIndex
  -> primary single-token sequence hypotheses
  -> bounded alias views
  -> structural anchors
  -> logical slots with 0..N candidates
  -> candidate elimination / duplicate folding
  -> strong-evidence prefix/suffix clipping
  -> one of Resolved / ResolvedWithWarnings / Incomplete / GroupingAmbiguous
```

## Directory Index

Enumerate all regular files in the seed's directory, non-recursively, only when sibling discovery is required.

Cache at task/batch scope:

```rust
struct DirectoryVolumeIndex {
    directory: PathBuf,
    files: Vec<DirectoryFile>,
    // cached normalization/tokenization metadata
}
```

Do not repeatedly `read_dir` for multiple candidates in the same directory.

## Common-File Classification

Replace the handwritten ordinary-file magic table with `infer`.

Rules:

- archive formats still go to SmartZip archive-specific structure probes;
- known ordinary non-archive types are strong negative evidence for cross-file volume discovery;
- embedded scanning is independent and may still discover an archive later inside the file;
- do not add a generic "weak/strong magic" scoring system unless a concrete failure requires it.

## Sequence Hypotheses

### Unicode handling

- Preserve the original path exactly.
- Build a separate analysis string using NFKC.
- Use mature numeral parsing libraries; no custom Unicode/Chinese numeral map.
- Accept only ordinal/integer interpretations appropriate for a sequence; do not interpret fractions as volume numbers.

### One varying token

For every ordinal token in the normalized seed name, build a hypothesis where exactly that token is the varying dimension and every other normalized component is fixed.

Example:

```text
资源2026_第①卷.jpg
资源2026_第②卷.jpg
资源2026_第④卷.jpg
```

may yield:

```text
资源2026_第{#}卷.jpg -> 1, 2, 4
```

If multiple one-dimensional hypotheses survive and archive structure cannot disambiguate them, return `GroupingAmbiguous` instead of introducing multidimensional search.

`filename_ordinal` records ordering evidence only. It does not need to equal the archive's internal `logical_index`; e.g. filenames ②/③ may correspond to internal volume numbers 0/1.

## Alias / Alternate Candidate Recovery

Alias handling is secondary hypothesis support, not global normalization.

Do not strip suffixes before primary sequence generation because names such as:

```text
photo (1).jpg
photo (2).jpg
photo (3).jpg
```

may legitimately use `(1)/(2)/(3)` as the sequence itself.

A bounded set of common duplicate/copy suffix rules may produce alternate views such as:

```text
03_1 -> 03
foo_02 (1) -> foo_02
foo_02 - copy -> foo_02
foo_02 副本 -> foo_02
```

but an alias view cannot independently prove a volume set. It may:

- add an alternate candidate to an existing/implied slot;
- fill a gap in a primary hypothesis when the surrounding hypothesis and archive structure provide the anchor.

Suggested model:

```rust
struct VolumeSlot {
    filename_ordinal: u64,
    candidates: Vec<VolumeCandidate>,
}

enum CandidateOrigin {
    PrimarySequence,
    CopyAlias,
    InternalVolumeMetadata,
}
```

## Alternate Candidate Elimination

For a slot with multiple candidates:

1. Apply archive-format internal metadata and definite truncation/continuation facts.
2. Apply deterministic physical-size/logical-extent constraints when the format supplies them.
3. Compare exact file size.
4. If duplicate copies remain possible, compute a bounded sampled BLAKE3 fingerprint.

A practical fingerprint can hash size plus fixed-size samples around head/25%/50%/75%/tail. The exact sample block size is an implementation tuning parameter. Do not read the full volume solely for duplicate detection.

Outcomes:

- one candidate remains -> use it;
- multiple candidates have the same bounded fingerprint -> fold as duplicate copies;
- multiple materially different candidates remain plausible -> `GroupingAmbiguous`.

## Interval Clipping

A filename hypothesis can be larger than the real archive set:

```text
01 02 03 04 05
   [02 03 04]
```

Strong archive-start evidence may clip a prefix; expected logical extent, last-volume metadata, or format footer/bounds may clip a suffix.

Do not allow arbitrary interior subset search. The resolver must not solve a knapsack/combinatorial membership problem by selecting arbitrary members merely because sizes happen to sum correctly.

Filename gaps remain warnings only.

## Format-Specific Static Evidence

### RAR

Use RAR3/4/5 volume flags and internal volume numbers where available. Continuation and end-of-archive metadata can provide strong membership and last-volume evidence. RAR is expected to be the strongest content-first case.

### ZIP

Use EOCD and ZIP64 disk metadata:

- current disk;
- disk where central directory starts;
- total disk count where available;
- central-directory physical closure for strong standalone evidence.

A local-file-header magic at offset zero is not sufficient proof of standalone completeness.

### 7z

Use the 7z signature/start header and validate the start-header CRC. Derive logical archive extent from:

```text
32 + NextHeaderOffset + NextHeaderSize
```

when valid and available.

Later split chunks may be raw bytes and have no 7z header. Therefore the resolver needs filename ordering to discover them once a logical start is anchored.

If the logical extent closes inside the current physical file with valid structure, treat it as standalone and skip directory discovery. If the extent points beyond the file, that proves only that the current file is not a complete standalone 7z; membership/completeness still need resolution.

## Failure Semantics

### `Incomplete`

Use only when static format evidence definitively proves required data/volumes are missing or truncated beyond recovery from available candidates.

Do not infer `Incomplete` from a filename gap.

### `GroupingAmbiguous`

Use when membership/order cannot be uniquely determined after all cheap static evidence and bounded duplicate checks.

Never guess among distinct plausible alternates.

### Completeness unknown

If membership/order are unique but static detection cannot prove the set complete, return `Resolved`/`ResolvedWithWarnings` and let the ordinary backend operation discover runtime corruption/missing-data errors.

## Canonical Materialization

Backends may require conventional names even when the real files are disguised.

Create a temporary canonical staging directory and materialize the resolved set under backend-compatible filenames, for example:

```text
资源①.jpg -> payload.7z.001
资源②.jpg -> payload.7z.002
资源③.jpg -> payload.7z.003
```

Policy:

1. place staging on the source filesystem when practical;
2. attempt reflink/clone/CoW copy;
3. fall back to regular copy;
4. never rename/move the originals;
5. delete the staging directory with task-scoped RAII cleanup.

No symlink/hardlink strategy is required for this task.

## Workflow Integration

Delete the current filename-only early skip:

```rust
if !is_first_volume(&candidate.path) {
    ...
    continue;
}
```

and remove the old `is_first_volume`/ASCII-only volume helpers from `nested.rs` once all call sites are migrated.

The resolver should run before backend extraction/listing but after enough local classification exists to avoid scanning sibling directories for obvious standalone archives and ordinary files.

Multiple explicit roots that resolve to the same canonical `VolumeSet` should be coalesced before duplicate backend work.

Nested extracted candidates must reuse the same resolution API and directory index cache.
