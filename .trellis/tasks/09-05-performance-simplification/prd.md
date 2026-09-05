# Behavior-preserving performance and simplification

User supplied the performance audit attachment and explicitly chose: "先完成保持行为不变的性能优化，其余只评估".
Baseline main: 291bc3f (production code verified at a21c3eb).

## Implement

- Split disk-space checks and full-tree accounting; one controlled blocking scan at a time, no catch-up tick storm, cost-aware polling and exactly one fresh final verification returning Usage.
- Stream password imports, batch writes/disables using prepared statements and transactions; keep duplicate/pin/reactivation and CLI count semantics. Match the ranking index to current ordering; verify migration and query plans.
- Remove directory-index deep copies and redundant encoding intermediate containers; reuse task-scoped ZIP metadata where compatible with current preparation lifecycle.
- Keep output transactions, cancellation cleanup, dangerous-path checks, password classification, explicit integrity tests and all final commit safety checks.
- Compare bounded representative workloads using raw baseline/final samples; state limitations without generalizing microbenchmarks.

## Evaluate only

Default test-before-extract and detect integrity behavior; broad process/context interface consolidation; CLI module split; binwalk scan-only/upstream work and zip feature pruning; fs4/chrono/criterion/proptest/crossterm choices. No default behavior changes, new crates or external upstream contribution in this task.

## Validation

Focused semantics tests, complete CLI/library suite and 13 real-backend acceptance groups. SQL query plan and fixed-size batch/ranking benchmark. Bounded budget checks must not block task polling or overlap; cancellation must await scans and backend cleanup before staging deletion. Keep compact evidence in research/ and intermediate files in .work/.
