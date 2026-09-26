#!/usr/bin/env bash
# Runs the debug app invisibly for automated testing: a nested KWin on a
# virtual framebuffer (own D-Bus session) + Vite + the app, with silent audio.
# Nothing appears on the developer's desktop; drive it via the devtools
# server on 127.0.0.1:17777 (see WORKLOG.md §3).
#
#   scripts/headless.sh start [WIDTHxHEIGHT]   (default 1600x1000)
#   scripts/headless.sh stop
#   scripts/headless.sh status
#   scripts/headless.sh restart-app            (after a cargo build)
#   scripts/headless.sh seed                   (add test account 1 from .env.local)
#
# The session uses its own profile under .deps/headless/{data,cache,config}.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
STATE="$ROOT/.deps/headless"
LOGS="$STATE/logs"
mkdir -p "$LOGS"

alive() { [[ -f "$STATE/$1.pid" ]] && kill -0 "$(cat "$STATE/$1.pid")" 2>/dev/null; }

start_app() {
  (
    export WAYLAND_DISPLAY="$(cat "$STATE/socket")" GDK_BACKEND=wayland
    export DBUS_SESSION_BUS_ADDRESS="$(cat "$STATE/dbus")"
    export TP_DEV_MUTE=1 TP_DEV_AO=null
    # isolated profile: tests never touch the developer's real library
    export XDG_DATA_HOME="$STATE/data" XDG_CACHE_HOME="$STATE/cache" XDG_CONFIG_HOME="$STATE/config"
    exec "$ROOT/scripts/dev-run.sh"
  ) >"$LOGS/app.log" 2>&1 &
  echo $! >"$STATE/app.pid"
  # ready = devtools server up *and* the React app mounted in the webview
  local probe='return document.readyState === "complete" && !!window.__TAURI_INTERNALS__ && (document.getElementById("root")?.childElementCount ?? 0) > 0;'
  for _ in $(seq 1 150); do
    if [[ "$(curl -s -m 2 -X POST --data "$probe" http://127.0.0.1:17777/eval 2>/dev/null)" == "true" ]]; then
      echo "app ready"
      return 0
    fi
    sleep 0.2
  done
  echo "app did not come up; see $LOGS/app.log" >&2
  return 1
}

stop_pid() {
  if alive "$1"; then
    kill "$(cat "$STATE/$1.pid")" 2>/dev/null || true
    for _ in $(seq 1 25); do alive "$1" || break; sleep 0.2; done
    alive "$1" && kill -9 "$(cat "$STATE/$1.pid")" 2>/dev/null || true
  fi
  rm -f "$STATE/$1.pid"
}

case "${1:-status}" in
  start)
    size="${2:-1600x1000}"
    if alive app; then echo "already running"; exit 0; fi
    # private session bus so the nested KWin doesn't clash with the real one
    eval "$(dbus-launch --sh-syntax)"
    echo "$DBUS_SESSION_BUS_PID" >"$STATE/dbus.pid"
    echo "$DBUS_SESSION_BUS_ADDRESS" >"$STATE/dbus"
    sock="tp-headless-$$"
    echo "$sock" >"$STATE/socket"
    kwin_wayland --virtual --socket "$sock" --width "${size%x*}" --height "${size#*x}" --no-lockscreen \
      >"$LOGS/kwin.log" 2>&1 &
    echo $! >"$STATE/kwin.pid"
    for _ in $(seq 1 50); do [[ -S "$XDG_RUNTIME_DIR/$sock" ]] && break; sleep 0.1; done
    # Reuse a dev server that is already up (e.g. one the developer started);
    # only servers we start ourselves get stopped again.
    if curl -s -m 1 -o /dev/null http://localhost:1420/; then
      echo "reusing the dev server already running on :1420"
      rm -f "$STATE/vite.pid"
    else
      (cd "$ROOT" && exec bun run dev) >"$LOGS/vite.log" 2>&1 &
      echo $! >"$STATE/vite.pid"
      for _ in $(seq 1 50); do curl -s -m 1 -o /dev/null http://localhost:1420/ && break; sleep 0.2; done
    fi
    start_app
    ;;
  restart-app)
    stop_pid app
    start_app
    ;;
  seed)
    # Adds test account 1 (from the gitignored .env.local) to the isolated
    # profile and waits for the first sync.
    set -a; . "$ROOT/.env.local"; set +a
    body=$(python3 -c 'import json,os; print(json.dumps({"cmd":"source_add","args":{"input":{"kind":"xtream","name":"Test account 1","url":os.environ["TP_XTREAM_SERVER"],"username":os.environ["TP_XTREAM_USER_1"],"password":os.environ["TP_XTREAM_PASS_1"],"altUrls":os.environ.get("TP_XTREAM_MIRRORS","").split(",")}}}))')
    curl -s -X POST -H 'content-type: application/json' --data "$body" http://127.0.0.1:17777/invoke >/dev/null
    for _ in $(seq 1 120); do
      state=$(curl -s -X POST -H 'content-type: application/json' --data '{"cmd":"sources_list","args":{}}' http://127.0.0.1:17777/invoke)
      if echo "$state" | grep -q '"syncing":false' && echo "$state" | grep -q '"programmes":[1-9]'; then echo "seeded"; exit 0; fi
      sleep 1
    done
    echo "seed did not finish" >&2; exit 1
    ;;
  stop)
    stop_pid app
    if alive vite; then
      # bun → vite: take the child down with the wrapper we started
      pkill -P "$(cat "$STATE/vite.pid")" 2>/dev/null || true
    fi
    stop_pid vite
    stop_pid kwin
    stop_pid dbus
    echo "stopped"
    ;;
  status)
    for p in kwin vite app; do printf "%-5s %s\n" "$p" "$(alive $p && echo running || echo stopped)"; done
    curl -s -m 1 -o /dev/null http://localhost:1420/ && echo "dev server :1420 up" || echo "dev server :1420 down"
    ;;
  *)
    echo "usage: $0 start|stop|status|restart-app" >&2
    exit 2
    ;;
esac
