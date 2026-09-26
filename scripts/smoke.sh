#!/usr/bin/env bash
# End-to-end smoke test against the real provider (test account 1).
# Drives the debug app in the invisible headless session (scripts/headless.sh)
# through the devtools server and checks the main flows. Plays at most ONE
# stream at a time (the account allows a single connection).
#
#   scripts/smoke.sh            # starts/seeds the headless session if needed
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
H="$ROOT/scripts/headless.sh"
DEV=http://127.0.0.1:17777
E=$DEV/eval
OUT="$ROOT/.deps/headless/smoke"
mkdir -p "$OUT"

pass=0
fail=0
report() { # name, ok (0/1), detail
  if [[ "$2" == 1 ]]; then
    printf '  \033[32m✓\033[0m %s\n' "$1"
    pass=$((pass + 1))
  else
    printf '  \033[31m✗\033[0m %s → %s\n' "$1" "${3:0:200}"
    fail=$((fail + 1))
  fi
}
check() { # name, js (must return true; /eval gives up after 20 s)
  local res
  res=$(curl -s -m 60 -X POST --data "$2" "$E" || echo "curl-failed")
  if [[ "$res" == "true" ]]; then report "$1" 1; else report "$1" 0 "$res"; fi
}
js() { curl -s -m 60 -X POST --data "$1" "$E"; }

curl -s -m 2 $DEV/health >/dev/null 2>&1 || "$H" start
state=$(js "return (await window.__TAURI_INTERNALS__.invoke('sources_list')).length")
[[ "$state" == "0" ]] && "$H" seed

I="const inv = window.__TAURI_INTERNALS__.invoke; const sleep = (ms) => new Promise(r => setTimeout(r, ms));"
# the smoke series: first recent series with >= 3 episodes that have durations
S="$I const s = window.__smoke;"

echo "catalog"
check "source synced with EPG" "$I const s = (await inv('sources_list'))[0]; return !!s && s.counts.channels > 1000 && s.counts.programmes > 0 && !s.syncError;"
check "live categories cleaned" "$I const c = await inv('categories', {kind: 'live'}); return c.length > 50 && c.every(x => !/[ᴬ-ᵡ]/.test(x.title));"
check "channels with now/next" "$I const p = await inv('channels', {query: {withEpg: true, limit: 50}}); return p.total > 100 && p.items.some(c => c.now);"
check "movies paging" "$I const p = await inv('movies', {query: {sort: 'title', offset: 120, limit: 60}}); return p.total > 1000 && p.items.length === 60;"
check "search" "$I const r = await inv('search', {q: 'news', limit: 5}); return r.channels.length > 0;"
check "movie detail" "$I const m = (await inv('movies', {query: {limit: 1}})).items[0]; const d = await inv('movie_detail', {sourceId: m.sourceId, id: m.id}); return d.id === m.id;"
check "series detail with episodes" "$I for (const x of (await inv('series_list', {query: {sort: 'added', limit: 15}})).items) { const d = await inv('series_detail', {sourceId: x.sourceId, id: x.id}); const eps = d.seasons.filter(z => z.season > 0).flatMap(z => z.episodes); if (eps.length >= 3 && eps.slice(0, 3).every(e => e.duration > 60)) { window.__smoke = {d, eps: eps.slice(0, 3)}; return true; } } return false;"

echo "library"
# what SeriesDetail.tsx sends when marking a never-played episode
META="const meta = (e) => ({seriesId: s.d.id, season: e.season, episode: e.episode, title: s.d.title, subtitle: e.title, image: s.d.cover, ext: e.ext, duration: e.duration});"
js "$S for (const e of s.eps) await inv('history_remove', {kind: 'episode', sourceId: s.d.sourceId, itemId: e.id}); return 1;" >/dev/null
check "unplayed episode can be marked watched" "$S $META await inv('mark_watched', {kind: 'episode', sourceId: s.d.sourceId, itemId: s.eps[0].id, watched: true, meta: meta(s.eps[0])}); const d = await inv('series_detail', {sourceId: s.d.sourceId, id: s.d.id}); return d.seasons.flatMap(z => z.episodes).find(e => e.id === s.eps[0].id).watched === true;"
check "up next offers the following episode" "$S const u = (await inv('up_next', {})).filter(x => x.series.id === s.d.id); return u.length === 1 && u[0].episode.id === s.eps[1].id;"
js "$I await inv('source_sync', {id: (await inv('sources_list'))[0].id}); return 1;" >/dev/null
sleep 2
for _ in $(seq 1 90); do [[ "$(js "$I return (await inv('sources_list'))[0].syncing;")" == "false" ]] && break; sleep 2; done
check "up next survives a catalog sync" "$S const u = (await inv('up_next', {})).filter(x => x.series.id === s.d.id); return u.length === 1 && u[0].episode.id === s.eps[1].id;"

echo "playback"
check "live channel plays" "$I location.hash = '#/'; const c = (await inv('channels', {query: {withEpg: true, limit: 2}})).items; await inv('play', {req: {kind: 'live', sourceId: c[0].sourceId, id: c[0].id}}); for (let i = 0; i < 30; i++) { await sleep(500); const t = await inv('player_get', {name: 'time-pos'}); if (t > 2) return true; } return false;"
curl -s "$DEV/snapshot?path=$OUT/live.png" >/dev/null
check "zap to next channel" "$I const c = (await inv('channels', {query: {withEpg: true, limit: 2}})).items; await inv('play', {req: {kind: 'live', sourceId: c[1].sourceId, id: c[1].id}}); await sleep(1500); for (let i = 0; i < 30; i++) { await sleep(500); const t = await inv('player_get', {name: 'time-pos'}); const title = await inv('player_get', {name: 'media-title'}); if (t > 1 && title) return true; } return false;"
check "movie resumes at 5:00" "$I await inv('player_stop'); const m = (await inv('movies', {query: {limit: 1}})).items[0]; await inv('play', {req: {kind: 'movie', sourceId: m.sourceId, id: m.id, start: 300}}); for (let i = 0; i < 40; i++) { await sleep(500); const t = await inv('player_get', {name: 'time-pos'}); if (t >= 299 && t < 330) return true; } return false;"
check "progress lands in continue watching" "$I const m = (await inv('movies', {query: {limit: 1}})).items[0]; const d = await inv('player_get', {name: 'duration'}); await inv('history_update', {entry: {kind: 'movie', sourceId: m.sourceId, itemId: m.id, title: m.title, position: 305, duration: d || 6000}}); const cw = await inv('continue_watching', {}); return cw.some(h => h.itemId === m.id);"
check "stop" "$I await inv('player_stop'); await sleep(500); return (await inv('player_get', {name: 'idle-active'})) === true;"
# the episode's last seconds through the real UI store (dev builds expose it)
check "next episode offered at the end" "$S const e = s.eps[1]; await window.__TP__.player.getState().play({kind: 'episode', sourceId: s.d.sourceId, id: e.id, title: e.title, subtitle: s.d.title, ext: e.ext, start: e.duration - 5, seriesId: s.d.id, seriesTitle: s.d.title, season: e.season, episode: e.episode}); location.hash = '#/player'; for (let i = 0; i < 34; i++) { await sleep(500); if (/up next in \\d+s/i.test(document.body.innerText)) return true; } return document.body.innerText.slice(0, 120);"
curl -s "$DEV/snapshot?path=$OUT/next-episode.png" >/dev/null
check "finished episode saved as watched" "$S await window.__TP__.player.getState().stop(); location.hash = '#/'; const d = await inv('series_detail', {sourceId: s.d.sourceId, id: s.d.id}); return d.seasons.flatMap(z => z.episodes).find(x => x.id === s.eps[1].id).watched === true;"
js "$S for (const e of s.eps) await inv('history_remove', {kind: 'episode', sourceId: s.d.sourceId, itemId: e.id}); return 1;" >/dev/null

echo "hardening"
code=$(curl -s -o /dev/null -w '%{http_code}' -m 10 -X POST -H 'Origin: https://evil.example' --data 'return 1' "$E")
report "devtools refuses foreign web pages" "$([[ $code == 403 ]] && echo 1 || echo 0)" "HTTP $code"
check "webview cannot spawn processes via mpv" "$I try { await inv('player_command', {args: ['run', 'true']}); return 'allowed'; } catch (e) { return /not allowed/.test(String(e)); }"

echo
echo "passed $pass, failed $fail  (snapshots in $OUT)"
[[ $fail -eq 0 ]]
