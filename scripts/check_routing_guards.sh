#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

patterns=(
	'ArchiveBackend'
	'BackendCapabilities'
	'BackendRouter::locate'
	'BackendRouter::new'
	'route_events'
	'clear_route_events'
	'extract_with_progress'
	'ExtractionProgressCallback'
	'fn capabilities\\(&self\\) -> BackendCapabilities'
)

for pattern in "${patterns[@]}"; do
	if rg -n "$pattern" crates --glob '*.rs'; then
		printf 'routing guard failed: %s\n' "$pattern" >&2
		exit 1
	fi
done

printf 'routing guards: clean\n'
