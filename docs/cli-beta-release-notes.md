SmartZip CLI beta for Linux x86_64 (Ubuntu 24.04+) and macOS arm64 (macOS 14+).

Install a current 7z/7zz separately, verify the archive against its .sha256 file, unpack and run `smartzip doctor`. Optional unrar provides additional RAR diagnostics. Installation, dynamic dependencies, output recovery, budgets, exit codes and known limitations are documented in the included cli-beta.md.

This beta adds recoverable overwrite commits, bounded scans, dynamic extraction budgets, Ctrl+C cleanup, consistent batch status and unattended policies. Password storage is plaintext with Unix mode 0600. Extraction budgets are periodic checks, not an OS sandbox. There is no automatic crash recovery. GUI and compression are outside this release.
