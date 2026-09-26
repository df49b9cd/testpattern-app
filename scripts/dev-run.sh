#!/usr/bin/env bash
# Runs the debug build against a running `bun run dev` (port 1420).
# Optional: TP_DEV_AUTOPLAY=live:<stream_id> | movie:<id>.<ext> autoplays a
# stream from test account 1 in .env.local (credentials never leave this box).
# TP_APP_BIN=<path> runs another build (e.g. scripts/csp-check.sh).
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
[[ -f "$ROOT/.deps/env.sh" ]] && . "$ROOT/.deps/env.sh"
if [[ -n "${TP_DEV_AUTOPLAY:-}" && -f "$ROOT/.env.local" ]]; then
  set -a; . "$ROOT/.env.local"; set +a
  kind="${TP_DEV_AUTOPLAY%%:*}"; id="${TP_DEV_AUTOPLAY#*:}"
  case "$kind" in
    live) export TP_DEV_AUTOPLAY_URL="$TP_XTREAM_SERVER/live/$TP_XTREAM_USER_1/$TP_XTREAM_PASS_1/$id.ts" ;;
    movie) export TP_DEV_AUTOPLAY_URL="$TP_XTREAM_SERVER/movie/$TP_XTREAM_USER_1/$TP_XTREAM_PASS_1/$id" ;;
  esac
fi
exec "${TP_APP_BIN:-$ROOT/src-tauri/target/debug/testpattern}" "$@"
