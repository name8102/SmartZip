# Implementation and validation

1. Preserve original work (35c240d), merge into latest main, adapt conflicting APIs and verify.
2. Transactional commit and injected filesystem failures.
3. Bounded scanning and extraction budgets.
4. Cancellation, password/skip semantics and shared outcomes.
5. CLI usability, release documentation and mandatory real-backend CI.
6. Focused tests plus final workspace/CLI checks and compact evidence.

Initial working tree baseline: cargo test --workspace --locked succeeded.
Trellis runtime/scripts/specs are absent; task artifacts and CONTEXT.md are the available repo contracts.

## Final local verification (2026-09-05)

- Preserved feature work as 35c240d; merged origin/main 2a50722 with that work as fa38539 on main. Fixed migrated schema assertions and generated the ignored embedded test fixture at runtime.
- Rust 1.97.1: `cargo +stable test --workspace --exclude smartzip-gui --locked` passed (421 tests, no failures/ignored cases). GUI is outside the beta delivery gate.
- `cargo +stable build --release -p smartzip-cli --locked` and `cargo +stable clippy -p smartzip-cli --all-targets --locked` passed. Clippy still emits existing style/large-function warnings plus portable statvfs casts; no zero-warning claim.
- `cargo fmt --all --check`, `git diff --check`, routing guard, actionlint 1.7.7 and Python syntax checks passed.
- Real 7-Zip 26.03 on Linux: release CLI and separately unpacked tar.gz both passed all 13 acceptance groups in scripts/verify_cli_beta.py, including terminal echo restoration on Ctrl+C, whitespace password, local encoding skip, history/JSON/exit agreement, missing/corrupt volumes, growing external output cancellation, budgets, traversal/links and original-file hashes. SHA-256 verified.
- Commit failure injections cover restoring old output after rename failure and preserving concurrent output plus a recoverable old backup. Deep scanning includes a 16 GiB sparse-file bounded-input check.
- Fixed fixture generation to avoid embedding developer absolute directories; regenerated two small encrypted samples. Real TAR listing uses empty link fields, which are not links.
- Runtime shutdown now returns ExitCode instead of process::exit: bounded prompt workers must finish and restore terminal echo even if their async caller was cancelled.
- Local binary links liblzma/libbz2/system C libraries. The hosted Linux and macOS jobs install their declared backend/runtime dependencies and rerun unpacked-binary acceptance.

Remote CI is pending at this checkpoint. No beta tag or public release has been created. The pipeline publishes only a matching beta tag after both target jobs succeed. Dynamic budgets are polling checkpoints, not strict OS quotas; crash/power-loss recovery, GUI/compression and parallel password recovery remain out of scope.

First hosted run 33954631865 found missing fontconfig development files on Ubuntu before tests; the CLI scanner dependency graph requires them even though the optimized executable does not link fontconfig. Added fontconfig/freetype development packages and rerun the gate.

macOS first-run tests exposed fixture expectations using /var while VolumeSet reports canonical /private/var. Volume diagnostic and test-workflow fixtures now create temporary trees under the canonical parent; data/evidence assertions stay unchanged.
