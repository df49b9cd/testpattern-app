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
#   scripts/headless.sh tmdb                   (TMDB key from .env.local, if any)
#   scripts/headless.sh keyring lock|unlock    (the test keyring, no prompt)
#
# TP_HEADLESS_X11=1 at start also runs an Xwayland inside the nested KWin
# (never the desktop's); .deps/headless/x11 then holds its DISPLAY and
# XAUTHORITY for X11 clients — e.g. the AppImage, whose GTK hook forces
# GDK_BACKEND=x11 (T-049), or the app's GLX path.
#
# The session uses its own profile under .deps/headless/{data,cache,config}
# and a throwaway GNOME Keyring (unlocked) as its Secret Service; inspect it
# with `DBUS_SESSION_BUS_ADDRESS=$(cat .deps/headless/dbus) secret-tool …`.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
STATE="$ROOT/.deps/headless"
LOGS="$STATE/logs"
mkdir -p "$LOGS"

alive() { [[ -f "$STATE/$1.pid" ]] && kill -0 "$(cat "$STATE/$1.pid")" 2>/dev/null; }

# Processes attached to the private bus, including everything it started on
# demand (portals, wallet, a11y bus, prompts) — they outlive the bus otherwise.
bus_procs() {
  [[ -f "$STATE/dbus" ]] || return 0
  local addr p
  addr="$(cat "$STATE/dbus")"
  for p in $(pgrep -u "$(id -u)"); do
    [[ $p == "$$" ]] && continue
    grep -qzxF "DBUS_SESSION_BUS_ADDRESS=$addr" "/proc/$p/environ" 2>/dev/null && echo "$p"
  done
  return 0
}

has_owner() { # bus name
  [[ "$(dbus-send --bus="$(cat "$STATE/dbus")" --print-reply=literal --dest=org.freedesktop.DBus / \
    org.freedesktop.DBus.NameHasOwner "string:$1" 2>/dev/null)" == *true* ]]
}

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

stop_all() {
  stop_pid app
  if alive vite; then
    # bun → vite: take the child down with the wrapper we started
    pkill -P "$(cat "$STATE/vite.pid")" 2>/dev/null || true
  fi
  stop_pid vite
  stop_pid kwin
  stop_pid keyring
  local left
  left="$(bus_procs)"
  if [[ -n "$left" ]]; then
    # shellcheck disable=SC2086
    kill $left 2>/dev/null || true
    sleep 1
    left="$(bus_procs)"
    # shellcheck disable=SC2086
    [[ -z "$left" ]] || kill -9 $left 2>/dev/null || true
  fi
  stop_pid dbus
}

case "${1:-status}" in
  start)
    size="${2:-1600x1000}"
    if alive app; then echo "already running"; exit 0; fi
    # leftovers of a session whose app died must not be orphaned
    if alive kwin || alive dbus || alive keyring || alive vite; then stop_all; fi
    sock="tp-headless-$$"
    echo "$sock" >"$STATE/socket"
    # Private session bus so the nested KWin doesn't clash with the real one.
    # What it starts on demand (portals, wallet prompts, …) gets the nested
    # display and the isolated profile: nothing may show up on, or write to,
    # the developer's real desktop and wallet.
    eval "$(env -u DISPLAY WAYLAND_DISPLAY="$sock" XDG_DATA_HOME="$STATE/data" XDG_CACHE_HOME="$STATE/cache" \
      XDG_CONFIG_HOME="$STATE/config" dbus-launch --sh-syntax)"
    echo "$DBUS_SESSION_BUS_PID" >"$STATE/dbus.pid"
    echo "$DBUS_SESSION_BUS_ADDRESS" >"$STATE/dbus"
    # Throwaway keyring as the session's Secret Service (T-046); it owns the
    # name before anything could start the desktop's wallet on this bus.
    mkdir -p -m 700 "$STATE/keyring"
    chmod 700 "$STATE/keyring" # gnome-keyring refuses a readable control dir
    (
      export XDG_DATA_HOME="$STATE/keyring" WAYLAND_DISPLAY="$sock"
      unset DISPLAY
      exec gnome-keyring-daemon --foreground --unlock --components=secrets \
        --control-directory="$STATE/keyring" < <(printf headless)
    ) >"$LOGS/keyring.log" 2>&1 &
    echo $! >"$STATE/keyring.pid"
    for _ in $(seq 1 50); do has_owner org.freedesktop.secrets && break; sleep 0.1; done
    has_owner org.freedesktop.secrets || echo "warning: no keyring on the test bus; see $LOGS/keyring.log" >&2
    xwayland=()
    [[ -n "${TP_HEADLESS_X11:-}" ]] && xwayland=(--xwayland)
    kwin_wayland --virtual "${xwayland[@]}" --socket "$sock" --width "${size%x*}" --height "${size#*x}" \
      --no-lockscreen >"$LOGS/kwin.log" 2>&1 &
    echo $! >"$STATE/kwin.pid"
    for _ in $(seq 1 50); do [[ -S "$XDG_RUNTIME_DIR/$sock" ]] && break; sleep 0.1; done
    rm -f "$STATE/x11"
    if [[ -n "${TP_HEADLESS_X11:-}" ]]; then
      xw=""
      for _ in $(seq 1 50); do
        xw="$(pgrep -a -P "$(cat "$STATE/kwin.pid")" Xwayland || true)"
        [[ -n "$xw" ]] && break
        sleep 0.1
      done
      disp="$(sed -n 's/.*Xwayland \(:[0-9]*\).*/\1/p' <<<"$xw")"
      auth="$(sed -n 's/.* -auth \([^ ]*\).*/\1/p' <<<"$xw")"
      if [[ -n "$disp" ]]; then
        echo "DISPLAY=$disp XAUTHORITY=$auth" >"$STATE/x11"
      else
        echo "warning: no Xwayland in the nested KWin; see $LOGS/kwin.log" >&2
      fi
    fi
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
    # The user's TMDB key (gitignored .env.local) for the isolated profile;
    # sent on stdin so it never shows in a process list.
    set -a; . "$ROOT/.env.local"; set +a
    python3 -c 'import json,os; print(json.dumps({"cmd":"tmdb_set_key","args":{"key":os.environ.get("TP_TMDB_TOKEN") or os.environ.get("TP_TMDB_KEY","")}}))' \
      | curl -s -X POST -H 'content-type: application/json' --data-binary @- http://127.0.0.1:17777/invoke \
      | python3 -c 'import json,sys; r=json.load(sys.stdin); print("tmdb:", r if isinstance(r, str) else {k: r.get(k) for k in ("configured","running","titles","known","error")})'
    ;;
  keyring)
    # what a user's wallet does on its own (screen lock, unlocked in another
    # app), without the prompt a headless session can't answer
    export DBUS_SESSION_BUS_ADDRESS="$(cat "$STATE/dbus")"
    case "${2:-}" in
      lock) secret-tool lock --collection=/org/freedesktop/secrets/collection/login ;;
      unlock)
        python3 - <<'PY'
from gi.repository import Gio, GLib
bus = Gio.bus_get_sync(Gio.BusType.SESSION)
SS, PATH, COLL = "org.freedesktop.secrets", "/org/freedesktop/secrets", "/org/freedesktop/secrets/collection/login"
call = lambda iface, method, args: bus.call_sync(SS, PATH, iface, method, args, None, Gio.DBusCallFlags.NONE, -1, None)
session = call("org.freedesktop.Secret.Service", "OpenSession", GLib.Variant("(sv)", ("plain", GLib.Variant("s", "")))).unpack()[1]
call("org.gnome.keyring.InternalUnsupportedGuiltRiddenInterface", "UnlockWithMasterPassword",
     GLib.Variant("(o(oayays))", (COLL, (session, b"", b"headless", "text/plain"))))
PY
        ;;
      *) echo "usage: $0 keyring lock|unlock" >&2; exit 2 ;;
    esac
    ;;
  stop)
    stop_all
    echo "stopped"
    ;;
  status)
    for p in kwin keyring vite app; do printf "%-7s %s\n" "$p" "$(alive $p && echo running || echo stopped)"; done
    curl -s -m 1 -o /dev/null http://localhost:1420/ && echo "dev server :1420 up" || echo "dev server :1420 down"
    ;;
  *)
    echo "usage: $0 start|stop|status|restart-app|seed|keyring" >&2
    exit 2
    ;;
esac
