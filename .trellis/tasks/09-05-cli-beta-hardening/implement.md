# Implementation and validation

1. Preserve original work (35c240d), merge into latest main, adapt conflicting APIs and verify.
2. Transactional commit and injected filesystem failures.
3. Bounded scanning and extraction budgets.
4. Cancellation, password/skip semantics and shared outcomes.
5. CLI usability, release documentation and mandatory real-backend CI.
6. Focused tests plus final workspace/CLI checks and compact evidence.

Initial working tree baseline: cargo test --workspace --locked succeeded.
Trellis runtime/scripts/specs are absent; task artifacts and CONTEXT.md are the available repo contracts.
