#!/usr/bin/env bash
# Static checks: Rust unit tests + clippy, TypeScript typecheck + unit tests
# (vitest), and that THIRD_PARTY_NOTICES.md matches the current dependencies.
# (End-to-end: scripts/smoke.sh)
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
[[ -f "$ROOT/.deps/env.sh" ]] && . "$ROOT/.deps/env.sh"
echo "==> cargo test"
(cd "$ROOT/src-tauri" && cargo test --lib --quiet)
echo "==> cargo clippy"
(cd "$ROOT/src-tauri" && cargo clippy --quiet --all-targets -- -D warnings)
echo "==> tsc"
(cd "$ROOT" && bunx tsc --noEmit -p .)
echo "==> vitest"
(cd "$ROOT" && bun run --silent test)
echo "==> third-party notices"
python3 "$ROOT/scripts/notices.py" --check
echo "all checks passed"
