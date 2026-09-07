# Selective regression restoration

## History and chosen sources

The first review used `4830f73...291bc3f`; the user's concrete R4189 sample and
request to recover prior behavior required tracing every reachable branch.

| Behavior | Existing implementation and regression |
| --- | --- |
| Direct password extraction | `748c873` introduced it on the old main; `4325d43` retained it under ArchiveExecutor. Restore the behavior of `attempt_candidate`, `is_password_failure`, and `attempt_with_password` from `4325d43:crates/smartzip-engine/src/lib.rs`. |
| Lost during clean integration | `748c873 → 4325d43` is outside current main ancestry. The July 29 clean integration chose the feat/history base; direct extraction was not ported. |
| Remaining SevenZip direct path | `28a0a25` still consulted `should_test_before_extract`; `f717003` removed the policy seam and `170eb57` replaced the gate with literal true. `555c4b3` later deleted dead direct branches. The recent `fa38539` merge was not the original loss. |
| Simple output names | `748c873` and `4325d43` use archive stem and candidate relative path. Current ancestry introduced `-embedded-N-HEX` in `42a2625`; `8751f43` only moved it into nested.rs. |
| Nested volume deduplication | `fcec934` restricted preparation/deduplication to RootInput. Restore all non-carved candidates and remember member paths before successful extraction can recycle those files. |
| Encrypted ZIP encoding assessment | `555c4b3` excluded encrypted ZIPs from early assessment while removing the later assessment path. Restore assessment using raw ZIP metadata before attempts. |

Restoration is selective adaptation to current interfaces, not whole-file checkout:
each password directly extracts into its own OutputMaterializer staging directory;
password failures retry after rollback. Current routing, cancellation, output budgets,
path/link validation, history and evidence-based password statistics remain active.
Extraction results carry encryption evidence from the existing backend listing so
successful direct extraction can update password reuse without a full test pass.

## Explicit behavior added this round

No reachable historical scanner implements “continue after a finding, stop at an
empty window”; old scan_path_mmap accumulated a Vec and scanned once. Likewise,
multiple payloads with only numeric suffixes were not found in reachable history.
These follow the user's current explicit requirements and are not claimed as
verbatim restorations:

- Root signature windows are 64 MiB; parsers see the complete archive and scanning
  resumes at its checked end. An empty search window stops the chain. Nested scan
  caps, size/ratio and business-container efficiency gates do not skip explicit roots.
- One root payload uses the mother stem; multiple payloads use mother-1, mother-2.
- RAR boundaries walk checked blocks, ZIP ends must reference the candidate's local
  headers, and unknown sizes stay unknown instead of truncating to the scan window.
- Fix binwalk confidence mapping to its actual 0/128/250 constants. Catch malformed
  third-party parser panics so later valid payloads remain discoverable.

The concrete R4189.jpg is a 725,270,389-byte JPEG carrier with a RAR5 at offset
904,331 and archive size 724,366,058. The old 64 MiB input truncation left binwalk's
RAR result low-confidence, so default filtering returned not_found. Recognition
is checked with an isolated temporary database; no plaintext extraction or user
password database access is required.

## Other retained repairs

- Preserve ambiguous password errors and bounded retries without incorrect penalties.
- Accept harmless TAR root directory entries while retaining traversal/link rejection.
- Interpret diagnostic phrases, not password/CRC words appearing in filenames.
- Keep resolved volumes out of embedded rescanning; use canonical backend input.
- Remove own compiler warnings and repair the invalid ZIP-as-JPG fixture generator.

## Validation

- `cargo test --workspace --exclude smartzip-gui --locked`: 432 passed.
- `cargo check --workspace --all-targets --locked`: passed; only the two known GUI dependency future-incompatibility warnings.
- Release build: passed without project warnings; real CLI acceptance: 23 groups passed.
- `cargo fmt --all --check`, routing guards, and `git diff --check`: passed.
- Real R4189 detect: status detected, Rar, encrypted=true, needs_password=true;
  embedded offset 904331, size 724366058 (source unchanged, isolated temporary DB). Real CLI acceptance
includes zero full-test calls during password trials, staging rollback, simple names,
multiple payloads crossing 64 MiB, nested volume consumption, path/link attacks,
output budgets, cancellation, history, and source preservation.

## Limits

The scanner may load the full carrier after the initial signature window hits;
scan memory budgeting remains separate work. The source sample is encrypted:
recognition and metadata verification do not claim plaintext extraction success.
GUI dependencies still report future incompatibilities in block 0.1.6 and
proc-macro-error2 2.0.1 via GPUI 0.2.2; no lint suppression or vendoring was added.
No commit, push, or remote publication is part of this repair.
