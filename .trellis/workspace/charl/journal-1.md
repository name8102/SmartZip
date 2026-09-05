# Journal - charl (Part 1)

> AI development session journal
> Started: 2026-06-27

---



## Session 1: Add nested path collision regressions

**Date**: 2026-06-30
**Task**: Add nested path collision regressions
**Package**: smartzip-core
**Branch**: `main`

### Summary

Added minimal ignored TDD regressions and fixtures for tar.gz, zip-to-tar.gz, and single-file inner ZIP path collisions, with captured failure evidence.

### Main Changes

(Add details)

### Git Commits

| Hash | Message |
|------|---------|
| `c5c3eb8` | (see git log) |

### Testing

- [OK] (Add test results)

### Status

[OK] **Completed**

### Next Steps

- None - task complete


## Session 2: Merge main and harden CLI beta delivery

**Date**: 2026-09-05
**Branch**: main
**Commits**: 35c240d, fa38539, 555c4b3, 15000ee, a21c3eb

Preserved integrity/diagnostic work, merged current main, implemented recoverable output commits, bounded scans and extraction budgets, task cancellation and outcome semantics, unattended CLI policies and doctor. Added locked Linux/macOS build, package/checksum and mandatory real-backend acceptance.

Local Rust 1.97.1: 421 tests passed, CLI release and unpacked archive passed 13 acceptance groups; fmt, routing, actionlint and clippy completed (warnings remain). Fixed a real hidden-prompt echo-restoration race, unsafe legacy fixture paths, Ubuntu development dependencies and macOS canonical fixture-path expectations.

Both native CI jobs passed tests/build/unpacked acceptance/upload at a21c3eb: https://github.com/name8102/SmartZip/actions/runs/33954747834 . No beta tag or public release created. Task archived under tasks/archive/2026-09/09-05-cli-beta-hardening. Source preservation, polling-budget overshoot, recoverable backups and beta limitations documented in docs/cli-beta.md.

**Status**: Completed. GUI, compression, password pools and crash resume remain deferred.


## Session 3: Optimize password operations and extraction accounting

**Date**: 2026-09-05
**Branch**: main
**Work commit**: 12df2ff

Implemented atomic streamed password imports and batched disables, schema v5 ranking index, controlled off-thread budget scans returning final Usage, borrowed volume indices/groups with ordinal-token reuse, and single-pass unencrypted ZIP encoding preparation. Preserved test-before-extract, route/error contracts and output commit checks. Cross-stage ZIP classification/caching and dependencies were evaluated only.

Rust stable 1.97.1: 426 tests and 13 real-backend release acceptance groups passed; release, fmt, routing guards and clippy completed (existing warnings remain). On /tmp tmpfs, seven-sample data medians improved from 174.74 to 7.19 ms (import), 42.20 to 2.37 ms (rank), and 128.65 to 11.99 ms (cleanup). Paired 3000-file extraction measurements did not establish a speedup; both sample groups and worse combined median are retained. Initial nightly results are explicitly exploratory, excluded from comparisons.

Task and detailed evaluation archived at tasks/archive/2026-09/09-05-performance-simplification. No new runtime dependency, default workflow change or current macOS verification. Commits are local; no push or release performed in this task.

**Status**: Completed within the user's behavior-preserving scope; overall extraction performance remains unproven.
