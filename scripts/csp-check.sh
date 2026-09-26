#!/usr/bin/env bash
# Checks the Content-Security-Policy (tauri.conf.json app.security.csp)
# against the real UI. Dev builds load from Vite, where Tauri cannot enforce
# the CSP, so this builds a debug binary that serves the bundled assets like a
# release build (`tauri build --debug`: CSP injected, devtools server kept),
# runs it in the headless session, walks every page, opens the lazy-loaded
# license texts, previews one live channel (one stream), and fails on any CSP
# violation (the UI records them in window.__TP_CSP__, see src/main.tsx).
#
#   scripts/csp-check.sh        # ~2 min the first time, then incremental
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
H="$ROOT/scripts/headless.sh"
E=http://127.0.0.1:17777/eval
BIN_DIR="$ROOT/src-tauri/target/prodassets"
[[ -f "$ROOT/.deps/env.sh" ]] && . "$ROOT/.deps/env.sh"

echo "==> building the UI + a debug binary with bundled assets"
(cd "$ROOT" && CARGO_TARGET_DIR="$BIN_DIR" bun run tauri build --debug --no-bundle >"$ROOT/.deps/headless/logs/csp-build.log" 2>&1) ||
  { echo "build failed: .deps/headless/logs/csp-build.log" >&2; exit 1; }

curl -s -m 2 -o /dev/null http://127.0.0.1:17777/health 2>/dev/null || "$H" start >/dev/null
TP_APP_BIN="$BIN_DIR/debug/testpattern" "$H" restart-app >/dev/null
# back to the normal dev binary afterwards
trap '"$H" restart-app >/dev/null 2>&1 || true' EXIT

js() { curl -s -m 60 -X POST --data "$1" "$E"; }
[[ "$(js 'return location.protocol')" == '"tauri:"' ]] || { echo "not serving bundled assets" >&2; exit 1; }
js 'window.__TP_CSP__.length = 0; return 1;' >/dev/null

echo "==> walking the UI"
for page in "#/" "#/live" "#/guide" "#/movies" "#/series" "#/search?q=news" "#/settings" "#/onboarding?add=1"; do
  js "location.hash = '$page'; await new Promise(r => setTimeout(r, 2500)); return 1;" >/dev/null
  printf '  %-20s %s violations so far\n' "$page" "$(js 'return window.__TP_CSP__.length')"
done
js "const s = (await window.__TAURI_INTERNALS__.invoke('series_list', {query: {limit: 1}})).items[0]; location.hash = '#/series/' + s.sourceId + '/' + s.id; await new Promise(r => setTimeout(r, 2500)); return 1;" >/dev/null
js "location.hash = '#/settings'; await new Promise(r => setTimeout(r, 1200)); for (const label of ['License', 'Third-party notices']) { [...document.querySelectorAll('button')].find(b => b.innerText === label).click(); for (let i = 0; i < 40 && !document.querySelector('[role=dialog] pre'); i++) await new Promise(r => setTimeout(r, 250)); window.dispatchEvent(new KeyboardEvent('keydown', {key: 'Escape'})); await new Promise(r => setTimeout(r, 300)); } return 1;" >/dev/null
echo "  license dialogs      $(js 'return window.__TP_CSP__.length') violations so far"
js "const inv = window.__TAURI_INTERNALS__.invoke; location.hash = '#/live?list=all'; await new Promise(r => setTimeout(r, 2000)); [...document.querySelectorAll('[role=button]')].filter(r => r.querySelector('img'))[1].click(); for (let i = 0; i < 30; i++) { await new Promise(r => setTimeout(r, 500)); if ((await inv('player_get', {name: 'time-pos'})) > 2) break; } await inv('player_stop'); location.hash = '#/'; return 1;" >/dev/null
echo "  live preview         $(js 'return window.__TP_CSP__.length') violations so far"

if [[ "$(js 'return window.__TP_CSP__.length')" != "0" ]]; then
  # "<count>× <directive>: <scheme://host>" per distinct violation
  js "const c = {}; for (const v of window.__TP_CSP__) { const i = v.indexOf(': '); let k = v.slice(0, i + 2); try { const u = new URL(v.slice(i + 2)); k += u.protocol + '//' + u.host; } catch { k += v.slice(i + 2); } c[k] = (c[k] || 0) + 1; } return Object.entries(c).map(([k, n]) => n + '× ' + k).join('\n');" |
    python3 -c 'import json,sys; print("CSP violations:\n  " + json.load(sys.stdin).replace("\n", "\n  "))' >&2
  exit 1
fi
echo "no CSP violations"
