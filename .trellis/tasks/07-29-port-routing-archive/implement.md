# Implementation

- Ported the reference capability-aware router and adapter stack in `f717003`.
- `ArchiveExecutor` is the engine seam; `ArchiveAdapter` is the per-installation seam; profiles are composed from config and adapter profile data.
- Extract routing uses the router's isolated temporary-attempt commit path so failed fallback attempts cannot leak output.
- Encoding auto-detection uses the current feat `smartzip-encoding` detector through a local native-ZIP helper because this branch does not expose the reference-only `decode_name_auto` symbol.
- Verification: `cargo check -p smartzip-archive`, `cargo test -p smartzip-archive` (60 passed).
