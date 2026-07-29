# Implementation

- CLI backend creation is centralized in `build_backend()`.
- Global `--config`, `--backend`, and `--verbose-routing` flags are wired to `SmartZipConfig` and `BackendRouter::from_config`.
- Detect/list/extract/encoding-preview all consume the same router instance.
- Existing feat command surface, history flags, JSON output, and file-aware commands remain unchanged.
- Verification: `cargo test -p smartzip-cli` passed.
