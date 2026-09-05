# Changelog

## 0.1.0-beta.1 (unreleased)

- Preserve the existing integrity-test command, volume diagnostics and history work on main.
- Recover old output after a failed overwrite commit; retain and report backups when concurrent changes prevent recovery.
- Bound embedded scans and enforce file, byte, free-space and nested-candidate budgets.
- Wire Ctrl+C through prompts, task contexts and external process cleanup; forward progress.
- Stop password retries on non-password errors; keep encoding skips local to a batch.
- Align extraction JSON, history and exit status; fix detect/preview failure exits, Unicode password listing and extraction to a new destination.
- Add configurable unattended policies, hidden password input, private database permissions and doctor.
- Add Linux x86_64/macOS arm64 CLI build, real-backend acceptance, binary archives, SHA-256 and gated beta prereleases.

See `docs/cli-beta.md` for requirements, limitations and the script interface. A workflow definition is not evidence of a successful platform run.
