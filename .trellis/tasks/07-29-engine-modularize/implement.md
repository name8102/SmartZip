# Implementation

- `8751f43 refactor(engine): split workflow capabilities into modules`
- `28a0a25 refactor(engine): isolate recursive extraction workflow`
- Public `SmartZipEngine` entrypoints remain in `src/lib.rs`; capability code is in access, encoding, nested, policy, password, event, backend, workflow, and extract-workflow modules.
- Existing history injection and file-grain behavior were preserved.
- Verification: `cargo test -p smartzip-engine` passed before and after the executor port.
