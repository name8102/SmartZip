# Implementation and verification

- Upgrade the single gpui-kit dependency from 0.6.1 to 0.8.0. Root now owns its presentation layers; remove the obsolete manual dialog layer.
- Separate application/worker lifetime (`workspace.rs`), native composition/action routing (`shell.rs`), navigation, shared quick options and the selected-task inspector. Keep the engine/queue/data ownership contracts.
- Replace the old screen stacking with native tabs, menus, resizable panes, virtualized directory/task lists and semantic selection controls. Settings/appearance/integration and history/recovery are grouped destinations.
- Use 16px default body/control text, 14px metadata, static Noto Sans CJK SC Regular, higher-contrast secondary text, aligned icon rails and consistent left-aligned content/40px controls. OFL and copyright ship in source, macOS bundles, Linux desktop packages/local installs and Nix packages.
- Defaults are no longer permanently expanded; the task inspector appears contextually and yields to pending decisions. Its dragged width is retained.
- Fix two demonstrated preview defects: scanner-selected byte-zero archives remain readable; bounded p7zip member reads use C.UTF-8 so Chinese selectors return actual contents. The Unicode regression fails with the previous C locale and passes with installed 7z and 7zz.

## Static and backend proof

- `nix develop -c cargo check --offline -p smartzip-gui`
- `nix develop -c cargo build --offline -p smartzip-gui`
- `nix develop -c cargo test --offline -p smartzip-gui`: 58 unit tests + 7 worker tests passed; 9 explicitly backend-gated tests remain ignored in this command.
- `nix develop -c cargo test --offline -p smartzip-gui --test runtime_worker real_backend_detect_list_test_extract_preserve_source -- --include-ignored`: passed against a real backend.
- `nix develop -c cargo test --offline -p smartzip-archive --test member_read`: 6 passed, including Unicode reads with both installed command variants.
- Python macOS package/installer tests: 4 passed; local desktop installer tests: 5 passed. These test bundle/install logic on Linux, not macOS native rendering.
- Python compile checks for changed packaging scripts; Nix package syntax parse; Rust formatting and diff whitespace checks.

## Native runtime and visual proof

Use isolated profiles/databases under `.work/gui-proof/`; no user archives, passwords, configuration or associations were changed. Actual Wayland window inspection was followed by isolated Xvfb rendering with the Mesa EGL vendor to avoid moving the user's pointer.

Inspected 1180×780 desktop, 760×640 narrow and 420×650 quick windows. Exercised archive listing, directory navigation, mixed/long Chinese-English names, text member preview, held task configuration, start, existing-output decision/rename, successful extraction, history selection/detail, password insertion/masking, settings, appearance, recovery, system integration display, and dark/light/reduced-motion controls. System association registration was not executed. The actual input focus ring was captured; macOS-specific window/input/association acceptance remains for that platform.

Local captures and logs are in `.work/gui-proof/`. Representative captures: `after/unicode-preview-verified.png`, `after/task-desktop-final.png`, `after/task-narrow-final.png`, `after/settings-narrow-final.png`, `after/dark-verified.png`, `after/quick-small-final.png`, `after/latest-history-detail.png`. The baseline, observed defects and repair comparisons are recorded in `.work/gui-proof/comparison.md` and `critique.md`. The web-oriented visual scanner only found old HTML mockups; native source/runtime evidence was used for acceptance instead.

## GUI cleanup (2026-10-22)

- Save the redesign independently in `2b1d09b` before cleanup.
- Remove five obsolete test-only library APIs; exercise the production pagination, batch mutation and configuration APIs in the retained safety tests instead.
- Replace the unused raw-TOML list editor path with explicit list metadata. Build each editor once and initialize only the active list's input entities. Remove the obsolete raw-array editing test.
- Keep preferences and settings drafts in Workspace rather than mirroring their state in View. Remove the unused confirmation severity argument and route migration preview results through the visible workspace message instead of an unread buffer.
- Move backend acceptance cases into `runtime::worker_tests`; stop importing runtime source into a second test binary and running its seven unit tests twice. Update the CI backend-test filter. GUI dependencies retain actual production uses, so none are removed.
- Verification: 66 distinct GUI tests, including nine real-backend cases, passed. The `--include-ignored` run passed 64; two recycling cases initially hit `/tmp` trash-directory permissions, then passed with isolated writable `TMPDIR` and `XDG_DATA_HOME` under `.work/gui-cleanup-proof/test-env/` using the `recycl` filter. No production workaround was added.
- `cargo build --offline --locked -p smartzip-gui` passed. Native Xvfb smoke verification used an isolated configuration/database: backend and password-source editing/saving, draft retention through quick/full mode changes, dirty-exit confirmation, queue-options close persistence, sidebar collapse persistence and clean exit. Captures and logs are local under `.work/gui-cleanup-proof/`.
- Current backend-only command: `nix develop -c cargo test --offline --locked -p smartzip-gui runtime::worker_tests -- --ignored`.
