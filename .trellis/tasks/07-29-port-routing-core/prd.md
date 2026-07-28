# Port routing core types onto feat base

## Parent

`07-29-feat-routing-integration`

## Goal

Land **additive** routing domain types on the clean feat branch from reference commit `c64f77b`, without breaking the existing `ArchiveBackend` stack yet.

## Port from reference

- `crates/smartzip-core/src/routing.rs` (full module)
- exports in `lib.rs`
- errors: `UnsupportedContainer`, `UnsupportedCodec`, `BackendProtocolError` (and password-exhausted wording only if it does not fight feat callers)
- `TaskEventKind::Route(RouteEvent)`
- CONTEXT terms / ADR-003 rewrite + timeline/staging ADRs as documentation of target state (history ADR from feat must remain)

## Constraints

- Do **not** delete feat history types or password APIs.
- Prefer additive error variants; fix call sites only if compile breaks.
- After this task, `cargo check -p smartzip-core` green; full workspace should still build on old archive trait.

## Acceptance

- [ ] `routing.rs` present and exported
- [ ] `TaskEventKind::Route` exists
- [ ] New error variants exist
- [ ] `cargo check -p smartzip-core` green
- [ ] No archive trait rename in this task
