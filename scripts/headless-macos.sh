#!/usr/bin/env bash
# macOS counterpart of scripts/headless.sh: runs the debug app against an
# ISOLATED profile under .deps/headless-macos/ (XDG_DATA_HOME / XDG_CACHE_HOME /
# XDG_CONFIG_HOME like headless.sh — the backend reads those on every platform)
# so tests never touch the real profile. macOS has no nested Wayland
# compositor: the app instead opens its window on an offscreen position that
# the user's screens don't contain. Drive it via the devtools server on
# 127.0.0.1:17777 (scripts/dev-run.sh, TP_DEV_MUTE=1) — same as Linux.
#
#   scripts/headless-macos.sh start [WIDTHxHEIGHT]   (default 1600x1000)
#   scripts/headless-macos.sh stop
#   scripts/headless-macos.sh status
#   scripts/headless-macos.sh restart-app            (after a cargo build)
#   scripts/headless-macos.sh seed                   (test account 1 from .env.local)
#   scripts/headless-macos.sh tmdb                   (TMDB key from .env.local, if any)
#
# Secrets go to the login keychain (there is no throwaway Sec keychain per
# test session); the profile id keeps them namespaced, and `stop` removes this
# profile's keychain items so nothing lingers. Requires macOS.
set -euo pipefail
[[ "$(uname -s)" == Darwin ]] || { echo "macOS only; use scripts/headless.sh on Linux" >&2; exit 1; }
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
STATE="$ROOT/.deps/headless-macos"
LOGS="$STATE/logs"
mkdir -p "$LOGS"

alive() { [[ -f "$STATE/$1.pid" ]] && kill -0 "$(cat "$STATE/$1.pid")" 2>/dev/null; }

app_pid_file() {
  # the rust binary is the (grand-)child of dev-run.sh: match it so `stop`
  # works after the wrapper was re-exec'd
  pgrep -f "target/debug/testpattern" 2>/dev/null | head -1 || true
}

start_app() {
  (
    export TP_DEV_MUTE=1 TP_DEV_AO=null
    export XDG_DATA_HOME="$STATE/data" XDG_CACHE_HOME="$STATE/cache" XDG_CONFIG_HOME="$STATE/config"
    exec "$ROOT/scripts/dev-run.sh"
  ) >"$LOGS/app.log" 2>&1 &
  echo $! >"$STATE/app.pid"
  local probe='return document.readyState === "complete" && !!window.__TAURI_INTERNALS__ && (document.getElementById("root")?.childElementCount ?? 0) > 0;'
  for _ in $(seq 1 150); do
    if [[ "$(curl -s -m 2 -X POST --data "$probe" http://127.0.0.1:17777/eval 2>/dev/null)" == "true" ]]; then
      echo "app ready" && return 0
    fi
    sleep 0.2
  done
  echo "app did not come up; see $LOGS/app.log" >&2
  return 1
}

stop_app() {
  stop_pid app || true
  # catch the binary if it detached from the wrapper
  local p; p="$(app_pid_file)"; [[ -n "$p" ]] && kill "$p" 2>/dev/null || true
}

stop_pid() {
  if alive "$1"; then
    kill "$(cat "$STATE/$1.pid")" 2>/dev/null || true
    for _ in $(seq 1 25); do alive "$1" || break; sleep 0.2; done
    alive "$1" && kill -9 "$(cat "$STATE/$1.pid")" 2>/dev/null || true
  fi
  rm -f "$STATE/$1.pid"
}

scrub_keychain() {
  # remove this profile's keychain items (label prefix testpattern:, profile
  # attribute keeps them namespaced; `secrets.rs` uses the `security` cli too)
  while security find-generic-password -l "testpattern:" -g >/dev/null 2>&1; do
    security delete-generic-password -l "testpattern:" >/dev/null 2>&1 || break
  done || true
}

case "${1:-status}" in
  start)
    if alive app; then echo "already running"; exit 0; fi
    # reuse a dev server that is already up; only servers we start get stopped
    if curl -s -m 1 -o /dev/null http://localhost:1420/; then
      echo "reusing the dev server already running on :1420"
      rm -f "$STATE/vite.pid"
    else
      (cd "$ROOT" && exec bun run dev) >"$LOGS/vite.log" 2>&1 &
      echo $! >"$STATE/vite.pid"
      for _ in $(seq 1 75); do curl -s -m 1 -o /dev/null http://localhost:1420/ && break; sleep 0.2; done
    fi
    start_app
    ;;
  restart-app) stop_app; start_app ;;
  seed)
    set -a; . "$ROOT/.env.local"; set +a
    body=$(python3 -c 'import json,os; print(json.dumps({"cmd":"source_add","args":{"input":{"kind":"xtream","name":"Test account 1","url":os.environ["TP_XTREAM_SERVER"],"username":os.environ["TP_XTREAM_USER_1"],"password":os.environ["TP_XTREAM_PASS_1"],"altUrls":os.environ.get("TP_XTREAM_MIRRORS","").split(",")}}}))')
    curl -s -X POST -H 'content-type: application/json' --data "$body" http://127.0.0.1:17777/invoke >/dev/null
    for _ in $(seq 1 120); do
      state=$(curl -s -X POST -H 'content-type: application/json' --data '{"cmd":"sources_list","args":{}}' http://127.0.0.1:17777/invoke)
      if echo "$state" | grep -q '"syncing":false' && echo "$state" | grep -q '"programmes":[1-9]'; then
        echo "seeded"
        [[ -n "${TP_TMDB_TOKEN:-${TP_TMDB_KEY:-}}" ]] && "$0" tmdb
        exit 0
      fi
      sleep 1
    done
    echo "seed did not finish" >&2; exit 1
    ;;
  tmdb)
    set -a; . "$ROOT/.env.local"; set +a
    python3 -c 'import json,os; print(json.dumps({"cmd":"tmdb_set_key","args":{"key":os.environ.get("TP_TMDB_TOKEN") or os.environ.get("TP_TMDB_KEY","")}}))' \
      | curl -s -X POST -H 'content-type: application/json' --data-binary @- http://127.0.0.1:17777/invoke \
      | python3 -c 'import json,sys; r=json.load(sys.stdin); print("tmdb:", r if isinstance(r, str) else {k: r.get(k) for k in ("configured","running","titles","known","error")})'
    ;;
  stop)
    stop_app
    if alive vite; then pkill -P "$(cat "$STATE/vite.pid")" 2>/dev/null || true; fi
    stop_pid vite
    scrub_keychain
    echo "stopped"
    ;;
  status)
    for p in vite app; do printf "%-7s %s\n" "$p" "$(alive $p && echo running || echo stopped)"; done
    curl -s -m 1 -o /dev/null http://localhost:1420/ && echo "dev server :1420 up" || echo "dev server :1420 down"
    ;;
  *) echo "usage: $0 start|stop|status|restart-app|seed|tmdb" >&2; exit 2 ;;
esac
