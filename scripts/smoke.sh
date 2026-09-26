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
check "password kept in the system keyring" "$I const s = (await inv('sources_list'))[0]; return s.passwordInKeyring === true && s.passwordLocked === false;"
check "live categories cleaned" "$I const c = await inv('categories', {kind: 'live'}); return c.length > 50 && c.every(x => !/[ᴬ-ᵡ]/.test(x.title));"
check "channels with now/next" "$I const p = await inv('channels', {query: {withEpg: true, limit: 50}}); return p.total > 100 && p.items.some(c => c.now);"
check "movies paging" "$I const p = await inv('movies', {query: {sort: 'title', offset: 120, limit: 60}}); return p.total > 1000 && p.items.length === 60;"
check "search" "$I const r = await inv('search', {q: 'news', limit: 5}); return r.channels.length > 0;"
check "movie detail" "$I const m = (await inv('movies', {query: {limit: 1}})).items[0]; const d = await inv('movie_detail', {sourceId: m.sourceId, id: m.id}); return d.id === m.id;"
check "series detail with episodes" "$I for (const x of (await inv('series_list', {query: {sort: 'added', limit: 15}})).items) { const d = await inv('series_detail', {sourceId: x.sourceId, id: x.id}); const eps = d.seasons.filter(z => z.season > 0).flatMap(z => z.episodes); if (eps.length >= 3 && eps.slice(0, 3).every(e => e.duration > 60)) { window.__smoke = {d, eps: eps.slice(0, 3)}; return true; } } return false;"

echo "grouping"
check "one entry per title (works)" "$I const src = (await inv('sources_list'))[0].counts; const m = await inv('movies', {query: {limit: 1}}); const s = await inv('series_list', {query: {limit: 1}}); return m.total > 1000 && m.total < src.movies && s.total > 100 && s.total < src.series;"
check "versions: one plays, seasons are their union" "$I const w = (await inv('series_list', {query: {sort: 'added', limit: 300}})).items.find(x => x.versionCount >= 2); let d; for (let i = 0; i < 8; i++) { d = await inv('series_detail', {sourceId: w.sourceId, id: w.id}); if (!d.versionsPending) break; await sleep(1500); } const union = new Set(d.seasons.map(z => z.season)); return d.versions.length === w.versionCount && d.versions.filter(v => v.selected).length === 1 && d.versions.every(v => v.seasons.every(n => union.has(n)));"
check "movie versions carry picture info" "$I const w = (await inv('movies', {query: {sort: 'added', limit: 300}})).items.find(x => x.versionCount >= 2); let d; for (let i = 0; i < 8; i++) { d = await inv('movie_detail', {sourceId: w.sourceId, id: w.id}); if (!d.versionsPending) break; await sleep(1500); } return d.versions.length === w.versionCount && d.versions.filter(v => v.selected).length === 1 && d.versions.some(v => v.video || v.duration);"
check "facets count works and combine" "$I const f = await inv('work_facets', {kind: 'movie'}); const svc = f.service[0]; const q = {facets: [{facet: 'service', value: svc.value}]}; const g = await inv('work_facets', {kind: 'movie', query: q}); const p = await inv('movies', {query: {...q, limit: 1}}); return p.total === svc.count && g.total === svc.count && g.genre.every(x => x.count <= svc.count) && g.service.length > 1;"
check "live channels by country and genre" "$I const nav = await inv('live_nav'); const c = nav.countries[0]; const g = nav.cells.find(x => (x.country ?? null) === (c.code ?? null)); const p = await inv('channels', {query: {grouped: true, country: c.code ?? '', limit: 20}}); const q = await inv('channels', {query: {grouped: true, country: c.code ?? '', genre: g.genre, limit: 1}}); return p.total === c.count && q.total === g.count && p.items.every(x => (x.group?.country ?? null) === (c.code ?? null));"
check "channel feeds grouped, chosen feed listed" "$I const g = (await inv('channels', {query: {grouped: true, limit: 1000}})).items.find(x => x.group.variants > 1); const v = await inv('channel_variants', {key: g.group.key}); return v.length === g.group.variants && v.some(x => x.selected && x.id === g.id);"
check "search lists a channel once" "$I const r = await inv('search', {q: 'sport', limit: 30}); const keys = r.channels.map(c => c.group?.key).filter(Boolean); return keys.length > 0 && new Set(keys).size === keys.length;"
check "TMDB details (with a key)" "$I const t = await inv('tmdb_status'); return !t.configured || (!t.error && (t.known > 0 || t.running));"

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
# record 4 s of live TV into the headless profile, then check the file
REC="$OUT/recordings"
rm -rf "$REC"
file=$(js "$I await inv('settings_set', {key: 'recording.dir', value: '$REC'}); const c = (await inv('channels', {query: {withEpg: true, limit: 1}})).items[0]; await inv('play', {req: {kind: 'live', sourceId: c.sourceId, id: c.id, title: c.title}}); for (let i = 0; i < 30; i++) { await sleep(500); if ((await inv('player_get', {name: 'time-pos'})) > 1) break; } await inv('player_record', {on: true}); await sleep(4000); const f = await inv('player_record', {on: false}); await inv('settings_set', {key: 'recording.dir', value: ''}); return f;" | python3 -c 'import json,sys; print(json.load(sys.stdin) or "")' 2>/dev/null || true)
size=$(stat -c %s "$file" 2>/dev/null || echo 0)
report "live recording writes MPEG-TS" "$([[ $size -gt 100000 && "$(head -c1 "$file" | od -An -tx1 | tr -d ' ')" == 47 ]] && echo 1 || echo 0)" "file '$file', $size bytes"
check "stop" "$I await inv('player_stop'); await sleep(500); return (await inv('player_get', {name: 'idle-active'})) === true;"
# the episode's last seconds through the real UI store (dev builds expose it)
check "next episode offered at the end" "$S const e = s.eps[1]; await window.__TP__.player.getState().play({kind: 'episode', sourceId: s.d.sourceId, id: e.id, title: e.title, subtitle: s.d.title, ext: e.ext, start: e.duration - 5, seriesId: s.d.id, seriesTitle: s.d.title, season: e.season, episode: e.episode}); location.hash = '#/player'; for (let i = 0; i < 34; i++) { await sleep(500); if (/up next in \\d+s/i.test(document.body.innerText)) return true; } return document.body.innerText.slice(0, 120);"
curl -s "$DEV/snapshot?path=$OUT/next-episode.png" >/dev/null
check "finished episode saved as watched" "$S await window.__TP__.player.getState().stop(); location.hash = '#/'; const d = await inv('series_detail', {sourceId: s.d.sourceId, id: s.d.id}); return d.seasons.flatMap(z => z.episodes).find(x => x.id === s.eps[1].id).watched === true;"
js "$S for (const e of s.eps) await inv('history_remove', {kind: 'episode', sourceId: s.d.sourceId, itemId: e.id}); return 1;" >/dev/null

# VA-API frames go straight to GL (T-029): a local VP9 clip (made once with
# the system ffmpeg) must decode as hwdec "vaapi", not "vaapi-copy"/"no".
# Needs a GPU render node and a GPU that decodes VP9 (most since ~2017).
clip="$OUT/vp9.webm"
if [[ ! -s "$clip" ]] && command -v ffmpeg >/dev/null; then
  ffmpeg -hide_banner -loglevel error -y -f lavfi -i "testsrc2=size=1280x720:rate=30" -t 6 \
    -c:v libvpx-vp9 -deadline realtime -cpu-used 8 -b:v 2M "$clip" 2>/dev/null || rm -f "$clip"
fi
if [[ -s "$clip" ]] && compgen -G "/dev/dri/renderD*" >/dev/null; then
  python3 -m http.server 18556 --bind 127.0.0.1 --directory "$OUT" >/dev/null 2>&1 &
  srv=$!
  trap 'kill $srv 2>/dev/null || true' EXIT
  for _ in $(seq 1 20); do curl -s -m 1 -o /dev/null "http://127.0.0.1:18556/" && break; sleep 0.1; done
  check "GPU decodes without copying frames (vaapi)" "$I await inv('player_load', {url: 'http://127.0.0.1:18556/vp9.webm', options: {title: 'vp9'}}); for (let i = 0; i < 30; i++) { await sleep(250); if ((await inv('player_get', {name: 'time-pos'})) > 1) break; } const h = await inv('player_get', {name: 'hwdec-current'}); await inv('player_stop'); return h === 'vaapi' || 'hwdec-current = ' + h;"
  kill $srv 2>/dev/null || true
else
  echo "  - GPU decoding check skipped (no render node or no VP9 encoder)"
fi

echo "hardening"
code=$(curl -s -o /dev/null -w '%{http_code}' -m 10 -X POST -H 'Origin: https://evil.example' --data 'return 1' "$E")
report "devtools refuses foreign web pages" "$([[ $code == 403 ]] && echo 1 || echo 0)" "HTTP $code"
check "webview cannot spawn processes via mpv" "$I try { await inv('player_command', {args: ['run', 'true']}); return 'allowed'; } catch (e) { return /not allowed/.test(String(e)); }"

echo
echo "passed $pass, failed $fail  (snapshots in $OUT)"
[[ $fail -eq 0 ]]
