# Robust Split-Volume Resolution

## Goal

Replace the current filename-only split-volume handling with a shared resolver that can safely identify and materialize common RAR/ZIP/7z volume sets even when filenames/extensions are disguised, Unicode-heavy, incomplete, duplicated, or otherwise nonstandard.

The resolver must distinguish three questions instead of collapsing them into one filename heuristic:

1. Is this physical file a standalone ordinary/archive file, a supported multivolume archive, or an unresolved possible continuation?
2. Which same-directory physical files uniquely belong to the same logical archive, and in what order?
3. Does cheap static evidence definitively prove a missing/incomplete set, or is completeness merely unknown and therefore safe to defer to the normal backend flow?

## Problem

The current extraction loop calls `is_first_volume()` before real header/structure detection and skips candidates solely from filename patterns. This breaks the target workload:

- archive extensions may be deliberately disguised;
- the user may select any physical member rather than the first volume;
- later 7z split chunks may be raw continuation bytes without archive magic;
- filename numbering may contain gaps or start from arbitrary values;
- numbering may use Unicode forms such as full-width, circled, Roman, or Chinese numerals;
- duplicate/incomplete downloads may produce alternate candidates such as `03` and `03_1`;
- multiple explicit roots may point into the same logical volume set.

Filename syntax is useful for proposing ordering hypotheses, but cannot be the authority for archive type, completeness, or logical volume index.

## Scope

### Supported archive-volume families

- RAR multivolume, including modern `partNN` naming and old-style RAR volume layouts when structure supports them.
- ZIP split/spanned archives using EOCD/ZIP64 disk metadata.
- 7z/7-Zip-produced split archive streams where the logical archive start can be identified and the remaining members are raw continuation chunks.

### Discovery and resolution

- Use only the seed file's directory, non-recursively, when sibling discovery is needed.
- A physical member selected directly by the user may be used as the seed; selection of the first volume is not required.
- Coalesce multiple explicit inputs that resolve to the same logical `VolumeSet`.
- Reuse one shared resolution layer for list/extract instead of backend-specific or command-specific heuristics.
- Cache directory enumeration/indexing within a task so multiple candidates in the same directory do not repeatedly call `read_dir`.

### Filename hypothesis generation

- Filename evidence proposes sequence/order only.
- Normalize analysis strings with Unicode normalization; preserve the original `PathBuf` unchanged.
- Use mature libraries for numeral parsing rather than custom Unicode/Chinese numeral tables.
- Support Unicode ordinal forms where available through normalization/library parsing, including common full-width/circled forms and Chinese compound numerals.
- Generate each sequence hypothesis by varying exactly one ordinal token; do not perform multidimensional/cartesian filename-pattern search.
- A filename gap is only a warning/hint, never proof of a missing volume.
- Filename ordinal values are not the same thing as archive-internal logical volume indices.

### Alternate candidates

- Support a narrow set of common duplicate/copy suffix aliases as secondary candidate views.
- Alias processing must not globally strip suffixes before primary sequence hypotheses are formed.
- Alias views may strengthen an existing hypothesis or fill a slot implied by it; they must not independently prove a volume set.
- Multiple candidates for one slot are eliminated using format-specific static evidence and deterministic size constraints first.
- Potential duplicate files may be compared using size plus a bounded sampled BLAKE3 fingerprint; do not hash complete large volumes.
- If multiple distinct candidates remain plausible, return grouping ambiguity instead of guessing.

### Static structural probing

Add cheap archive-specific structure probes without invoking full backend `test` operations:

- RAR: multivolume flag, internal volume number when available, continuation/last-volume structure.
- ZIP: EOCD/ZIP64 disk metadata and physical-file closure where cheaply verifiable.
- 7z: signature/start-header validation and logical archive extent from `NextHeaderOffset + NextHeaderSize` where available.

Strong standalone structure should bypass sibling discovery entirely.

### Resolution semantics

Use outcomes equivalent to:

- `Single`: no supported cross-file volume resolution required.
- `Resolved`: membership/order uniquely determined.
- `ResolvedWithWarnings`: uniquely determined, but filename gaps or other nonblocking warnings exist.
- `Incomplete`: static archive-format evidence definitively proves missing/incomplete data; fail the current logical archive.
- `GroupingAmbiguous`: multiple membership/order hypotheses remain plausible; fail rather than guess.

`Resolved` must not claim that the archive contents have been fully tested or verified complete.

### Membership selection

- Strong structural evidence may clip unrelated files before the logical start or after a determinable logical end from a larger filename sequence.
- Do not perform arbitrary subset search inside the interval.
- If static evidence cannot prove completeness but membership/order are unique, continue to the normal backend flow.

### Common-file detection

- Replace the handwritten common non-archive magic table in `smartzip-engine/src/detect.rs` with the mature `infer` crate.
- An `infer` hit for a known ordinary non-archive format prevents cross-file volume discovery.
- Embedded-archive scanning remains independent; a JPEG/PDF/etc carrier may still contain an embedded archive at a later offset.

### Canonical backend view

Once a `VolumeSet` is resolved, create a temporary canonical view with backend-compatible filenames, without renaming the originals.

- Prefer CoW/reflink/clone semantics on the source filesystem.
- Regular copy is an accepted fallback.
- Stage files on the same filesystem as the source when practical to preserve CoW behavior.
- 7zz/unrar receive the canonical entrypoint and discover subsequent canonical members normally.

## Explicit Non-Goals

- Generic arbitrary byte splits such as Unix `split` output for unrelated TAR/bin files.
- Full backend `test` as part of volume detection.
- Recursive sibling discovery outside the seed directory.
- Arbitrary filename fuzzy matching or machine-learning/confidence scoring.
- Cross-file continuation discovery for an embedded archive carved from inside another carrier file.
- Filesystem watching or `WaitingForVolumes`; after the user supplies missing files, the task is rerun/retried.

## Required Integration Changes

- Remove the early `is_first_volume()` skip from `extract_workflow`.
- Remove/retire filename-only volume helpers from `nested.rs` instead of extending them.
- Put archive-format structure knowledge in `smartzip-archive`.
- Put directory indexing, filename hypotheses, alternate candidates, resolution, and canonical materialization in `smartzip-engine`.
- Ensure nested candidates use the same resolver without running an expensive fresh directory scan per candidate.

## Acceptance Criteria

- Selecting any member of a supported RAR/ZIP/7z volume set resolves to one logical archive when the set is uniquely identifiable.
- Disguised extensions do not prevent resolution when filename ordering plus content structure is sufficient.
- Common Unicode numbering forms work without custom hardcoded numeral tables.
- Two-member sets can resolve when structural evidence is strong even though filename sequence evidence is weak.
- A single visible member can be reported incomplete when the format itself definitively says additional volumes are required.
- Filename gaps alone never cause `Incomplete`.
- Definite missing-volume structure causes terminal failure for that logical archive while other batch roots may continue.
- Unknown completeness with unique membership/order proceeds to backend processing.
- Multiple plausible groupings or alternate candidates fail as `GroupingAmbiguous`.
- Prefix/suffix clipping is supported; arbitrary interior subset selection is not.
- Alternate candidates such as `03`/`03_1` can be resolved when one is statically invalid or when they are sampled-fingerprint duplicates.
- `infer` replaces the current handwritten ordinary-file magic table without disabling embedded-archive scanning.
- Canonical staging uses CoW/reflink where available and regular copy fallback otherwise.
- Multiple explicit roots belonging to one `VolumeSet` are coalesced.
- List and extract share the same resolution semantics.
- Unit/integration tests cover standard names, disguised names, Unicode ordinals, gaps, alternate copies, truncated alternates, duplicate alternates, selecting a middle member, two-member sets, definite missing members, uncertain completeness, grouping ambiguity, and root coalescing.
- Full workspace tests pass.
