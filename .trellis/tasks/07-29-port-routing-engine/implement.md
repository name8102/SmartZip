# Implementation

- `170eb57 feat(engine): retarget workflows to archive executor`
- All engine workflow/access generics use `ArchiveExecutor`; no `ArchiveBackend` or `BackendCapabilities` remains in engine sources.
- The old `extract_with_progress` seam was removed. Extraction uses plain executor `extract`; the engine still emits its indeterminate extraction progress event.
- `begin_task()` is called at the real recursive extraction entry.
- Integration fakes and history tests use `BackendRouter`/`from_config` construction.
