# Implementation

- Ported the reference capability-aware router and adapter stack in `f717003`.
- `ArchiveExecutor` is the engine seam; `ArchiveAdapter` is the per-installation seam; profiles are composed from config and adapter profile data.
- Route events are emitted directly into the caller's `TaskEvent` sink; no router-side event buffer remains.
- `OutputMaterializer` owns extraction staging. The router invokes adapters in that directory and clears/ verifies contents between retryable attempts; it never creates nested attempt directories.
- Encoding auto-detection uses the current feat `smartzip-encoding` detector through a local native-ZIP helper because this branch does not expose the reference-only `decode_name_auto` symbol.
- Verification: `cargo check -p smartzip-archive`, `cargo test -p smartzip-archive` (60 passed).
