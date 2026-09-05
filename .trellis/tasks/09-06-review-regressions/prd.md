# Recent regression review and fixes

User requested review and repair of recent commits, reporting compiler warnings
and archives renamed to `.jpg` being skipped. Clear defects may be fixed directly;
reproduction before every fix is not required.

Initial review baseline: `4830f73...291bc3f`. User subsequently requested all-branch
regression archaeology and selective restoration of existing implementations.
The history review therefore includes the pre-clean-landing main and feat lines.

## Acceptance

- Remove project compiler warnings on the installed Rust 1.98 toolchain without
  suppressing lints or weakening validation.
- Preserve archive recognition with disguised extensions; scan explicit roots
  through their archive boundaries and continue looking for later payloads. Apply
  efficiency limits only to nested discoveries; keep original candidate identity separate from backend input paths.
- Restore direct password extraction from existing history, with no full test
  pass before extraction; preserve temporary-output cleanup, cancellation and budgets.
- Use the mother filename for one payload and mother-1/mother-2 for multiple payloads.
- Continue scanning from a recognized archive end until a search window is empty.
- Repair confirmed behavioral regressions found by review, with focused coverage.
- Run workspace checks, retained tests, routing guards, and real CLI verification.

## Workflow

Implementation authorized by the user's review-and-fix request. The documented
`.trellis/scripts/task.py` is absent in this checkout, so metadata is maintained
directly using the existing task directory format.
