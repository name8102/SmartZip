SmartZip 0.1.0-beta.1 for Linux x86_64 (Ubuntu 24.04+) and macOS arm64 (macOS 14+).

Desktop archives include GUI + CLI; CLI-only archives remain available. Verify the SHA-256 file, install 7z/7zz separately, and run `smartzip doctor`. See the included desktop-beta.md for installation, upgrade, removal, dependencies and recovery boundaries. macOS applications are ad-hoc signed, not notarized.

This beta provides intelligent extraction, archive browsing and bounded text/image previews, persistent execution and interrupted-commit recovery. It fixes legacy ZIP names, unverified listing-password hints, long-prefix scanning and duplicate inputs across concurrent roots. Compression creation is outside this release.

Back up configuration and the database before upgrading. Schema v7 migration is forward-only. Recovery starts at the next extraction entrypoint and may repeat incomplete archive nodes; temporary passwords are not persisted in execution snapshots. Resource limits are periodic checks, not an OS sandbox. Password storage is plaintext. Linux commit crash tests do not establish power-loss durability or macOS native GUI acceptance.
