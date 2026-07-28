# Retarget engine to ArchiveExecutor without losing history

## Parent

`07-29-feat-routing-integration`

## Depends on

`07-29-port-routing-archive`

## Goal

Feat engine behavior stays; backend injection becomes `ArchiveExecutor` only.

## Must preserve

- `TaskHistoryRecorder` injection and file-grain recording
- Header-first root detection / business container skip / enqueue-all embedded findings
- Interactive prompters and event sink behavior

## Must change

- All `B: ArchiveBackend` → `B: ArchiveExecutor`
- `backend.begin_task()` at the real extract entry used by all wrappers
- Extract call sites: **one** progress design consistent with archive port
- Test fakes: implement `ArchiveExecutor`, no `capabilities()` mocks
- Replace test `BackendRouter::locate()` / raw sevenzip assumptions with router construction that matches new API (even if config is empty/auto)

## Forbidden

- Keeping parallel `extract` and `extract_with_progress` engine paths “for compatibility”
- Importing both old and new backend traits

## Acceptance

- [ ] `cargo check -p smartzip-engine` green
- [ ] History integration tests compile
- [ ] No `ArchiveBackend` / `BackendCapabilities` in engine
