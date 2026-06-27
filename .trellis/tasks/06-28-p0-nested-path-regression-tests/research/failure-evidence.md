# P0-1 嵌套路径冲突 — 失败证据

## Method

4 TDD regression tests added under `crates/smartzip-engine/tests/smartzip_integration.rs`,
marked `#[ignore = "TDD: blocked by P0-2 nested output path model fix"]`.

Each test asserts the **target (post-fix) behavior**.  All 4 fail with current
implementation, confirming the path collision bug.

## Test Fixtures

| Fixture | Size | Structure |
|---------|------|-----------|
| `real_tar.tar.gz` | 160 B | gzip → `real_tar.tar` → `leaf_rt.txt` |
| `matching.tar.gz` | 157 B | gzip → `matching.tar` → `leaf_m.txt` |
| `zip_containing_real_tar_gz.zip` | 336 B | zip → `real_tar.tar.gz` → gzip→tar→leaf |
| `zip_inner_zip.zip` | 236 B | zip → `zip_inner_zip.zip` → `zip_inner_leaf.txt` |

All fixtures are small (< 400 B each).

## Trigger Condition

The collision only fires when the extracted inner file name is **Equivalent**
(normalized similarity ≥ 0.85) to the archive stem.  With such naming,
`decide_single_file` returns `CommitSingleFileAsInnerName`, which commits the
single file as a **file path** rather than wrapping it in a directory.  The
nested candidate then uses that file path as a directory prefix, and
`create_dir_all` fails with `File exists (os error 17)`.

Example trace (`real_tar.tar.gz`):

```
archive_stem("real_tar.tar.gz") = "real_tar"
7z gzip extraction → single file "real_tar.tar"
name_similarity("real_tar.tar", "real_tar") ≈ 1.0 → Equivalent
  → CommitSingleFileAsInnerName, target = output_root/"real_tar.tar"
  → output_dir = output_root/"real_tar.tar" (a FILE)
  → candidate.relative_path updated to "real_tar.tar"
discover_nested_candidates(output_root/"real_tar.tar", prefix="real_tar.tar")
  → root.is_file() → detected as tar archive
  → relative_path = "real_tar.tar/real_tar"  ←  PATH COLLISION
  → output_dir_for_candidate = base/"real_tar.tar"/"real_tar"
  → create_dir_all(base/"real_tar.tar") FAILS: File exists (os error 17)
```

The fixture `tar_gz_leaf.tar.gz` (where extracted `tar_leaf.tar` name partial
vs `tar_gz_leaf`) does NOT trigger the bug — similarity is Partial → takes the
`CommitWholeTempAsArchiveDir` branch → directory output → no collision.

## Observed Failure: `real_tar.tar.gz`

```
cargo test -p smartzip-engine --test smartzip_integration test_TDD_tar_gz_name_equivalent_to_inner_tar_collision -- --include-ignored

→ FAILED at crates/smartzip-engine/tests/smartzip_integration.rs:419
  expected >=2 processed (gzip + tar), got [".../real_tar.tar.gz"]
```

Only the root gzip archive shows up in `processed`.  The nested tar candidate
is silently dropped (not in `processed`, not in `skipped` — a `Failed` event
is emitted).  `leaf_rt.txt` is never produced.

## Observed Failure: `matching.tar.gz`

Same root cause, same failure signature.  Confirms the bug is about naming
equivalence, not a specific name string.

## Observed Failure: `zip_containing_real_tar_gz.zip`

Same signature at depth 2 (gzip→tar step inside the zip extraction chain).
Confirms the bug propagates through nested extraction levels.

## Observed Failure: `zip_inner_zip.zip`

`zip_inner_zip.zip` extracts a single file also named `zip_inner_zip.zip`.
Because that file name is equivalent to the outer archive stem, layout commits
it as a single file path.  The subsequent nested inner-zip candidate then uses
that file path as a directory prefix, hitting the same archive-file-as-directory
class of conflict before `zip_inner_leaf.txt` can be produced.

## Current Tests That Still Pass

- Existing `nested_multi_level.zip` coverage remains in the integration suite
  through list/extract scanner cases and the recursion-limit regression.
- `test_engine_preserves_nested_archive_paths` (zip→inner.zip→hello.txt —
  zip normally produces directory output when the outer archive is not laid out
  as a single-file output path)
- All fixture existence checks pass (including new fixtures)
