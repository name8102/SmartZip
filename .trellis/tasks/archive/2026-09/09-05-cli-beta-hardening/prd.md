# CLI beta reliability

User authorized merging current work into main, then applying the release audit fixes.

## Acceptance

- Merge current test/diagnostic implementation with origin/main 2a50722, preserving main's ZIP reader, volume resolution and migration architecture.
- Transactional output commit: stage final shape, recoverable same-filesystem backup, no clobber of concurrent targets, fault-injection recovery checks.
- Bounded scanner input even in deep mode, ordinary archives use header/backend first; extraction file/byte/free-space budgets stop and clean staging.
- CLI cancellation reaches engine/backend and waits for cleanup. Encoding skip is local. Only wrong-password errors retry credentials.
- Shared outcome drives summary, JSON, history and exit codes; Unicode password truncation, output-aware history behavior, hidden password entry, deterministic noninteractive policy.
- Configurable candidate cap, backend diagnostics, explicit beta docs; Linux x86_64/macOS arm64 build/test/archive/checksum pipeline with mandatory real backend coverage.

GUI, compression, password worker pools, crash resume, Hashcat and crates.io publishing are deferred. This task prepares beta delivery; it does not claim remote CI or a published release without execution evidence.
