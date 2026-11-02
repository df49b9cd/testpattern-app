#!/usr/bin/env bash
# Runs the debug build against a running `bun run dev` (port 1420).
# Optional: TP_DEV_AUTOPLAY=live:<stream_id> | movie:<id>.<ext> autoplays a
# stream from test account 1 in .env.local (credentials never leave this box).
# TP_APP_BIN=<command> runs another build (e.g. scripts/csp-check.sh) — with
# arguments, split on spaces: "scripts/ubuntu-build.sh run-app".
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
read -ra app <<<"${TP_APP_BIN:-${CARGO_TARGET_DIR:-$ROOT/src-tauri/target}/debug/testpattern}"
exec "${app[@]}" "$@"
