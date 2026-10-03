# testpattern — work log & kanban

> **Read this first if you are picking up the work.** This file is the single
> source of truth for project status. Every task below is written so it can be
> implemented without prior context. Update it whenever you start, finish or
> discover work: move cards between columns, append to the log at the bottom.

---

## 1. What we are building

**testpattern** is a UHF / Infuse–style IPTV player:

- Sources: **Xtream Codes** accounts (server + username + password) and **M3U**
  playlists, with **XMLTV** electronic programme guide (EPG).
- Features: Live TV with now/next + TV guide grid, Movies (VOD) and Series with
  posters/details, favorites, continue watching, search, catch-up.
- Stack (mandated): **React + TypeScript + Bun + Tauri 2** UI, **Rust** backend,
  shipped as a **single binary**. First target: Linux desktop (Fedora 44,
  KDE Plasma Wayland, AMD GPU). Later: every platform Tauri supports.

## 2. Architecture (decisions + why)

```
┌──────────────────────── Tauri window (GTK3) ────────────────────────┐
│ GtkOverlay                                                          │
│  ├─ GtkGLArea  ← libmpv render API (OpenGL) draws video here        │
│  └─ WebKitWebView (transparent background) ← React UI on top        │
└─────────────────────────────────────────────────────────────────────┘
Rust backend: SQLite catalog (rusqlite, bundled), Xtream/M3U sync, XMLTV
import, image proxy, libmpv control. UI talks to it via Tauri commands/events.
```

- **Playback = embedded libmpv, not HTML5 `<video>`.** Fedora's `ffmpeg-free`
  and GStreamer lack native H.264/HEVC decoders, so WebKitGTK video and distro
  mpv cannot reliably play IPTV. We build **FFmpeg n9.0.2 + mpv v0.41.0 from
  source** (`scripts/build-media.sh`) and link them **statically**
  (`src-tauri/build.rs`), together with the libraries whose shared-library
  names change between distro releases (libplacebo, dav1d, libxml2,
  libdisplay-info — T-036). Only long-stable system libs (glibc, libstdc++,
  libass, libva/drm, pipewire/pulse/alsa, EGL, OpenSSL, WebKitGTK) are
  dynamic.
- **Video under the UI:** `src-tauri/src/player/linux.rs` reparents the
  WebKitWebView into a GtkOverlay above a GtkGLArea. Pages that show video must
  keep `html, body, #root` backgrounds transparent; opaque pages simply cover it.
  Verified: 720p60 H.264 live plays at 60 fps, ~21% of one CPU core.
- **mpv gotchas (already handled):** call `setlocale(LC_NUMERIC, "C")` before
  `mpv_create` (GTK sets the user locale); option `video-timing-offset=0` so
  `mpv_render_context_render` never blocks the GTK thread; the update callback
  must never run inline (use a GLib idle source); empty mpv node lists have
  NULL pointers.
- **Hardware decoding:** VA-API, zero-copy: the render context gets the GPU's
  render node (`MPV_RENDER_PARAM_DRM_DISPLAY_V2.render_fd`, found through
  `EGL_EXT_device_drm_render_node`), so `hwdec=auto-safe` picks `vaapi` and
  frames reach GL as dmabufs (T-029). Fedora's own Mesa rejects VA-API
  H.264/HEVC ("No support for codec h264 profile 100") → software; RPM
  Fusion's `mesa-va-drivers-freeworld` adds them (installed on this machine
  since 2026-09-26: libva loads `/usr/lib64/dri-freeworld/…`, so H.264/HEVC
  channels show `vaapi` here). GLX (X11) sessions can't import dmabufs →
  `vaapi-copy`.
- **One entry per title ("works"), one row per channel.** Providers list
  the same film/show several times (per service, market language, quality)
  and the same channel in several feeds (RAW/HD/SD/HEVC/4K). The catalog
  keeps every provider entry; `src-tauri/src/works/` groups them after each
  sync (`works::rebuild`, ~2 s): movies/series by TMDB id (else normalized
  title + year) into `work` rows with browse facets (`work_facet`), live
  feeds by country + normalized name into `channel_group`. Lists, search,
  Home, favorites, continue watching and Up next work on these groups;
  detail pages list the copies as **versions** (T-050…T-053). Optional TMDB
  details (user's own API key, `tmdb.rs`, T-054) add genres, original
  language, collections and TV networks.
- **Credentials stay in the backend.** The UI refers to items by
  `(sourceId, itemId)`; stream URLs (which embed credentials) are built in Rust.
  At rest, source passwords live in the desktop keyring (Secret Service:
  KWallet, GNOME Keyring — `secrets.rs`, T-046); the database keeps them only
  where no keyring is available.

## 3. How to build and run

```bash
# one-time: headers for Tauri/mpv. With root:
sudo dnf install $(scripts/fedora-sysroot.sh --print-packages)
# ...or rootless (what the agent uses; creates .deps/sysroot + .deps/env.sh):
scripts/fedora-sysroot.sh
# one-time (~1 min on 32 threads): static FFmpeg + libmpv into third_party/prefix
scripts/build-media.sh

. .deps/env.sh            # only needed for the rootless sysroot
bun install
bun run dev               # Vite on :1420 (keep running)
(cd src-tauri && cargo build)
scripts/dev-run.sh        # launches target/debug/testpattern
bun run tauri build       # release for this machine: single binary + rpm/deb
scripts/ubuntu-build.sh   # portable release (Ubuntu 24.04 container): deb + AppImage + rpm
scripts/check.sh          # unit tests + clippy + tsc + vitest + notices
scripts/headless.sh start && scripts/headless.sh seed && scripts/smoke.sh
scripts/outdated.sh       # dependency audit (see "Dependency policy")
scripts/csp-check.sh      # CSP vs. the real UI (bundled assets, headless)
```

**Portable releases** (T-049): a release built here needs this machine's
glibc (2.43, Fedora 44). `scripts/ubuntu-build.sh` builds the media engine
and the app inside an Ubuntu 24.04 container instead (podman; image from
`packaging/ubuntu/`, rebuilt when its definition changes; the pinned Rust
toolchain, the crate cache, bun and node_modules are mounted from this
machine) and packages that one binary — glibc 2.39: Ubuntu 24.04+, Debian 13,
Fedora 40+ — as .deb, AppImage and .rpm in
`.deps/ubuntu24/target/release/bundle/`. On the way it repairs the AppImage
(`scripts/appimage-fix.sh`) and checks the .deb dependencies
(`scripts/deb-depends.sh`). `scripts/ubuntu-build.sh debug` + `run-app` run
the Ubuntu build inside the container against the headless session:
`TP_APP_BIN="$PWD/scripts/ubuntu-build.sh run-app" scripts/headless.sh start`,
then `scripts/smoke.sh`. CI builds the same bundles for version tags.

**This machine** (Ryzen 9 5950X): at 32 parallel jobs compilers crash
sporadically (GCC "internal compiler error: Segmentation fault" on random
FFmpeg files, a nasm GP fault in libc) while the same files compile fine
alone — likely hardware instability under all-core load. Build with
`TP_JOBS=16 CARGO_BUILD_JOBS=16` (`build-media.sh` reads `TP_JOBS`).

`scripts/smoke.sh` (~45 s, 30 checks) plays at most one stream at a time and
runs one full catalog sync of the headless profile. Its GPU-decoding check
serves a local VP9 clip (made once with the system ffmpeg) from a temporary
server on 127.0.0.1:18556 and is skipped where that isn't possible.

**Headless session** (`scripts/headless.sh`): its private D-Bus starts
services on demand (portals, KWallet's `ksecretd`, prompts) with the nested
display and the isolated profile, so nothing appears on or writes to the real
desktop/wallet; `stop` also ends everything that bus started (they used to
outlive it). Its Secret Service is a throwaway GNOME Keyring (unlocked, data
in `.deps/headless/keyring`); `scripts/headless.sh keyring lock|unlock`
changes its state without a prompt, and
`DBUS_SESSION_BUS_ADDRESS=$(cat .deps/headless/dbus) secret-tool search --all application testpattern`
lists the app's entries (prints secrets — test profile only).

**Test accounts** live in the gitignored `.env.local` (keys
`TP_XTREAM_SERVER`, `TP_XTREAM_USER_1/PASS_1`, `..._2`, `TP_XTREAM_MIRRORS`;
template: `.env.example`). The user's **TMDB** credentials are there too
(`TP_TMDB_TOKEN` = v4 read access token, `TP_TMDB_KEY` = v3 API key);
`scripts/headless.sh tmdb` (also run by `seed`) gives the token to the
headless profile over stdin. In the app it is entered in Settings →
Metadata and kept in the system keyring like the source passwords (the
database only where there is no keyring); it is never shown back to the UI
or logged.
**Each account allows ONE concurrent stream** — never play/probe two streams on
the same account at once. Never commit or print credentials.

**Git:** branch `main`; remote `origin` =
`git@github.com:df49b9cd/testpattern-app.git` (private, SSH — the `gh` token
lacks the `workflow` scope that HTTPS pushes of workflow files need).
`df49b9cd/testpattern` is a different project of the user's (a Bevy app):
never push there. After cloning, enable the credential guard once:
`git config core.hooksPath scripts/git-hooks` — its `pre-commit` refuses
commits whose staged changes contain any `.env.local` value (credentials,
provider hosts) and names only the key. `.gitignore` keeps out everything
rebuildable (node_modules, dist, target, `.deps/`, `third_party/{src,build,
prefix}`, generated `src-tauri/gen/schemas`) plus secrets (`.env*` except
`.env.example`, `*.local`, keys); `.gitattributes` forces LF and marks
binaries. Agents commit only when the user asks.

**Debug automation** (debug builds only, `src-tauri/src/devtools.rs`), on
`127.0.0.1:17777`:
- `GET /snapshot?path=/tmp/x.png` → PNG of the window incl. the GL video layer.
- `POST /eval` (body = JS function body, may `await`, `return` a value) → JSON.
  Example: `return await window.__TAURI_INTERNALS__.invoke('sources_list')`.
- `scripts/dev-run.sh` env: `TP_DEV_AUTOPLAY=live:<stream_id>` autoplays from
  account 1, `TP_DEV_MUTE=1` mutes (use it for automated runs).
- Kill the app with an anchored pattern:
  `pkill -f '^/home/.*/src-tauri/target/debug/testpattern'` (an unanchored
  `pkill -f target/debug/testpattern` also kills the calling shell).
- **Access rules** (`check_request` in `devtools.rs`): only local tools (no
  `Origin` header — curl, scripts) and pages served by the Vite dev server
  (`http://localhost:1420`, `http://127.0.0.1:1420`, i.e. the preview pane's
  bridge) get in; any other web page, cross-site `<img>`/form loads and
  DNS-rebinding hosts get 403. `/snapshot` only writes absolute `.png` paths.
  Keep it that way: `/eval` runs arbitrary JS inside the app.
- Dev builds expose the live UI stores as `window.__TP__` (`player`, `sync`,
  `queryClient`) for `/eval` scripts. Don't `import('/src/stores/…')` from
  `/eval`: under Vite HMR that yields a *separate* module instance.
- The webview's mpv access is allow-listed (`player/mod.rs`): playback
  commands/properties only (no `run`, `subprocess`, `stream-record`, …),
  `player_load` takes http(s) URLs only, and `player_get` refuses `path`,
  `stream-open-filename`, `stream-path`, `playlist*` (they embed credentials).

### Provider facts (test account, 2026-09-26)
- Catalog: 250 live categories / ~20k channels (262 are `#####` separator rows),
  83 VOD categories / ~39k movies, 67 series categories / ~11k series.
- Streams: H.264 High 720p/1080p (50/60 fps, progressive) + AAC; movies are MKV
  H.264 + AAC 5.1 + SRT subs. Live formats allowed: ts, m3u8.
- `xmltv.php` ≈ 70 MB / 206k programmes (was 13 MB at project start; single
  line, generated on the fly — occasionally truncated/malformed or slow to
  connect); only ~1,950 channels have `epg_channel_id`. 352 channels have a
  3-day archive (`tv_archive`); timeshift streams take ~13 s to start and
  some archives have gaps (HTTP 404, e.g. RT Documentary).
  `get_short_epg` returns base64 titles/descriptions.
- Names are noisy: `UK: BBC ONE LONDON 4K ◉`, `AT&T: BBC NEWS ᴿᴬᵂ`,
  categories `UK| SKY CINEMA ᴴᴰ/ᴿᴬᵂ`, movies `SC - Cleanskin (2012)`,
  series `NF - SEAL Team (2017) (US)` → cleaned by `src-tauri/src/names.rs`.

### Dependency policy (user requirement: always latest)
Every dependency must be on its **latest stable** release. Audited
2026-09-26 (re-audited later that day with `scripts/outdated.sh`: all current):
- Rust **1.99.0** pinned in `rust-toolchain.toml` (+ `rust-version` in
  `src-tauri/Cargo.toml`), edition **2024**. Bun **1.4.2**, pinned as
  `packageManager` in `package.json` (CI installs that one).
- All direct crates at latest stable (checked against crates.io) **except**
  the GTK3 stack — `gtk`/`gdk` 0.18, `glib`/`cairo-rs` 0.18,
  `javascriptcore-rs` 1.1 — which must match what `webkit2gtk` 2.0.2 (newest)
  and Tauri 2.11.6 (newest stable) are built on. Tauri 3 exists only as
  `3.0.0-alpha.x`; not adopted (stable only).
- npm: `bun outdated` clean (React 19.3, Vite 8.3, TS 7.0, Tailwind 4.3,
  react-router 8.4, @tauri-apps/* 2.11, vitest 5.0).
- Media engine: FFmpeg **n9.0.2**, mpv **v0.41.0**, libplacebo **v7.360.1**,
  dav1d **1.5.4**, libxml2 **v2.15.4**, libdisplay-info **0.4.0**
  (`scripts/build-media.sh` re-fetches when a pinned tag changes and rebuilds
  everything linking it; any edit of the script itself rebuilds everything).
- GitHub Actions (`.github/workflows/ci.yml`): newest majors — checkout v7,
  cache v6, setup-bun v2, rust-cache v2, upload-artifact v7.
- After changing any dependency run `scripts/notices.py` (regenerates
  `THIRD_PARTY_NOTICES.md`; `scripts/check.sh` fails until you do).
- How to re-audit: `scripts/outdated.sh` — compares every direct crate in
  `src-tauri/Cargo.toml` with crates.io (`OLD` = behind, `pin` = the GTK3
  exception above; exit 1 when something is behind), then runs
  `bun outdated`, `rustup check` and compares the bun pin, the FFmpeg/mpv
  tags in `scripts/build-media.sh` and the Action majors in the workflow with
  upstream (`git ls-remote`).

## 4. Repository map

| Path | What |
|---|---|
| `scripts/fedora-sysroot.sh` | Rootless `-devel` sysroot (dnf download + extract) → `.deps/sysroot`, `.deps/env.sh` |
| `scripts/build-media.sh` | Builds static FFmpeg + libmpv + libplacebo/dav1d/libxml2/libdisplay-info into `third_party/prefix` (tag-pinned, rebuilds dependents); vendors their license texts to `packaging/licenses/` |
| `scripts/dev-run.sh` | Runs the debug binary (`TP_DEV_AUTOPLAY`, `TP_DEV_MUTE`, `TP_DEV_AO`) |
| `scripts/headless.sh` | Invisible test session: nested KWin + Vite (or reuse) + debug app, isolated profile, throwaway keyring |
| `scripts/check.sh` | Rust unit tests + clippy `-D warnings` + `tsc` + vitest + notices up to date |
| `scripts/smoke.sh` | End-to-end checks against the provider in the headless session |
| `scripts/outdated.sh` | Dependency currency audit (crates.io, bun, rustup, FFmpeg/mpv tags) |
| `scripts/git-hooks/pre-commit` | Refuses commits containing `.env.local` values (enable: `git config core.hooksPath scripts/git-hooks`) |
| `scripts/csp-check.sh` | Fails on Content-Security-Policy violations in a bundled-assets build (T-040) |
| `scripts/ubuntu-build.sh` | Portable release in an Ubuntu 24.04 container: one glibc-2.39 binary as .deb/AppImage/.rpm; `debug` + `run-app` to test it in the headless session (T-049) |
| `scripts/appimage-fix.sh` | Drops the bundled libwayland and the forced X11 backend from Tauri's AppImage, repacks it (T-049) |
| `scripts/deb-depends.sh` | Checks `bundle.linux.deb.depends` against `dpkg-shlibdeps` (run on Ubuntu 24.04) |
| `packaging/ubuntu/` | `Containerfile` + `packages.txt` (Ubuntu build packages, also installed by CI) |
| `.github/workflows/ci.yml` | CI: `scripts/check.sh` on Ubuntu 24.04 for pushes/PRs; portable bundles for `v*` tags and manual runs (T-037) |
| `scripts/notices.py` | Generates `THIRD_PARTY_NOTICES.md` (run after any dependency change; `check.sh` enforces) |
| `LICENSE` · `THIRD_PARTY_NOTICES.md` · `packaging/debian/copyright` | GPLv3 text · generated third-party licenses · DEP-5 copyright for the deb |
| `.gitignore` · `.gitattributes` · `.env.example` | Ignore rules (rebuildable + secrets) · LF/binary attributes · test-account template |
| `.claude/launch.json` | Desktop-app preview config (`dev`, port 1420) |
| `src-tauri/build.rs` | Links libmpv/FFmpeg statically, system deps dynamically |
| `src-tauri/src/lib.rs` | App setup, state, command registration, background sync timer |
| `src-tauri/src/state.rs` · `error.rs` · `util/` | Shared `App` state + HTTP client · UI-facing error type (never carries URLs) · FNV hash, lenient JSON accessors |
| `src-tauri/src/player/` | `mpv_sys.rs` FFI · `mpv.rs` safe wrapper · `mod.rs` Player/commands/events/redaction/IPC allowlists · `linux.rs` GTK GLArea surface |
| `src-tauri/src/devtools.rs` | Debug-only HTTP automation + browser bridge (`/snapshot`, `/eval`, `/click`, `/invoke`, `/img`) |
| `src-tauri/src/db.rs` | SQLite pool + schema migrations (+ `test_conn()` for unit tests) |
| `src-tauri/src/sources/` | `xtream.rs` API client (+ mirrors) · `m3u.rs` parser · `mod.rs` CRUD + sync |
| `src-tauri/src/epg.rs` | XMLTV streaming import + programme queries |
| `src-tauri/src/catalog.rs` | Browse/detail/guide/search/up-next commands; works queries + facets, channel groups (`live_nav`, `channel_variants`), versions on detail pages |
| `src-tauri/src/works/` | Grouping: `mod.rs` keys + `rebuild` (works, facets, channel groups, TMDB fold-in) · `variant.rs` what a copy is (service/origin/language/quality from tag + category, ranking) · `versions.rs` members, version choice, `work_prefer`, learned tracks · `genre.rs` genre words (several languages), live genres, countries · `lang.rs` ISO 639-1 names |
| `src-tauri/src/tmdb.rs` | Optional TMDB details (user's key): background fetch (25 req/s), `tmdb` table, status/key commands |
| `src-tauri/src/probe.rs` | "Check audio & subtitles": opens one version briefly in a second, silent mpv (only while nothing plays) |
| `src-tauri/src/library.rs` | Favorites, history, watched state, continue watching, recent channels |
| `src-tauri/src/playback.rs` | `play` command (URL building via `resolve`, catch-up, failover; tells the player what plays so it can learn its tracks) |
| `src-tauri/src/images.rs` | `img://` artwork proxy with disk cache/resize |
| `src-tauri/src/settings.rs` | Settings store + mpv application |
| `src-tauri/src/secrets.rs` | Source passwords in the desktop keyring (Secret Service), database fallback, one-time migration |
| `src-tauri/src/names.rs` | Title/badge/region cleanup (unit tested) |
| `src/main.tsx` | Entry; dev builds expose `window.__TP__` for automation |
| `src/app/` | `App.tsx` providers · `router.tsx` routes · `Root.tsx` first-run redirect + global effects · `Layout.tsx` sidebar |
| `src/pages/` | `Home` `Live` `Guide` `Movies` `MovieDetail` `Series` `SeriesDetail` `Search` `Settings` `Onboarding` `Player` |
| `src/components/` | `ui.tsx` primitives · `media.tsx` artwork/cards/shelves · `PosterGrid.tsx` · `LibraryBrowser.tsx` (facet panel) · `Versions.tsx` (version picker) · `DetailHero.tsx` · `SourceForm.tsx` · `PipPlayer.tsx` · `ErrorBoundary.tsx` |
| `src/lib/` | `types.ts` (mirrors Rust JSON) · `api.ts` · `bridge.ts` · `queryClient.ts` · `img.ts` · `format.ts` · `play.ts` · `open.ts` · `liveLists.ts` (Live/Guide list keys) |
| `src/stores/` | `player.ts` (now playing, mpv props, progress saving, viewport) · `sync.ts` |
| `src/hooks/` | `useBackendEvents` · `useProgressSaver` · `useVideoViewport` · `useSpatialNav` |

---

## 5. Kanban

Legend: **P0** = needed for a usable app on Linux, **P1** = expected
UHF/Infuse feature, **P2** = later.

> Work state is also tracked in **planned** (the user's tracker): project
> **testpattern-app** (team PL, `PLANNED_API_KEY` in
> `/Users/smolesen/Dev/planned/data/.env`, MCP on `localhost:4000` — start it
> with `just dev` or `planned serve` in `/Users/smolesen/Dev/planned`). This
> file keeps the build notes; the kanban below stays the issue-of-record for
> what is done.

### 🟩 Done

- **T-001 Environment + provider survey.** Toolchain present (bun 1.4, rust
  1.98, node 26). No sudo in agent sandbox. Provider probed (see §3).
- **T-002 Rootless sysroot** — `scripts/fedora-sysroot.sh`.
- **T-003 Static media engine** — `scripts/build-media.sh` (FFmpeg n9.0.2: all native
  decoders, openssl https, dav1d, libxml2 DASH, VA-API; mpv: libmpv only,
  gl/plain-gl/egl, vaapi-drm, pipewire/pulse/alsa, no lua/js).
- **T-004 App skeleton** — Tauri 2.11, React 19, Vite 8, TS 7, Tailwind 4,
  icons from `assets/icon.svg` (`bunx tauri icon assets/icon.svg`).
- **T-005 Static linking** — `src-tauri/build.rs`; `ldd` shows no libav*/libmpv.
- **T-006 Native video surface** — `player/linux.rs`; validated with a live
  channel under a translucent React overlay.
- **T-007 Player core** — `player/mod.rs`: commands `player_load`,
  `player_stop`, `player_command`, `player_set`, `player_get`; event
  `player://event` (`{type:'prop',name,value}`, `start-file`, `file-loaded`,
  `end-file`, `restart`, `reconnecting`, `log`); live streams auto-reconnect up
  to 5× with backoff; time-pos throttled to 5 Hz.
- **T-008 Debug automation server** — `devtools.rs` (see §3).

- **T-009 Backend data layer** — `db.rs` (schema v1, WAL, 1 writer + 4
  readers), `names.rs`, `util/`, `state.rs`, `sources/{mod,xtream,m3u}.rs`,
  `epg.rs`, `error.rs`. 9 unit tests pass (`cargo test --lib`).
- **T-010 Backend wired** — `lib.rs` opens `~/.local/share/dev.testpattern.app/testpattern.db`,
  registers source commands, background stale-sync every 30 min. Verified
  against the provider: full sync ≈ 6 s → 19,936 channels / 39,235 movies /
  11,069 series / 40,507 programmes; server UTC offset 7200 s detected.

- **T-011 Catalog commands** — `catalog.rs`: `categories(kind)`,
  `channels(query)` (now/next via programme joins), `channel`, `movies`,
  `series_list` (sort added/title/rating/year/provider, paging, favorites,
  LIKE filter `q`), `movie_detail` (get_vod_info cached 7 d, stale on error),
  `series_detail` (get_series_info cached 12 h; seasons/episodes/progress/
  resume), `epg_channel` (+ base64 short-EPG fallback), `epg_grid`, `search`
  (FTS5 prefix). All < 30 ms on the test catalog (detail cold ≈ 400 ms).
- **T-012 Library** — `library.rs`: `favorite_toggle`, `history_update`
  (watched ≥ 92% or < 3 min left), `mark_watched`, `history_remove`,
  `continue_watching`, `recent_channels` (+ `touch_channel` on live play).
- **T-013 Play** — `playback.rs::play({kind: live|movie|episode|catchup, …})`
  builds URLs in Rust; mirror failover via `Player::load_with_fallbacks`
  (tries `alt_urls` if a stream never opens). Verified live + movie resume.
- **T-014 Image proxy** — `images.rs`, `img://localhost/?u=<url>&w=<px>`,
  disk cache `~/.cache/dev.testpattern.app/images`, widths quantized to 80 px,
  JPEG q84 / PNG for alpha, 16 concurrent fetches, 24 h negative cache.
  Cold ≈ 170 ms, warm ≈ 1 ms.
- **T-015 Settings** — `settings.rs` (`settings_get`, `settings_set`,
  defaults, applied to mpv at startup/change).
- **BUGFIX click crash** — tauri-runtime-wry's button handler requires
  `webview.parent().parent()` to be the `GtkWindow`; overlay now replaces the
  vbox as the window child (`player/linux.rs`). Devtools `/click?x=&y=`
  injects real GTK clicks to regression-test this.

- **T-016 Frontend foundation** — `src/app/{App,router,Root,Layout}.tsx`
  (hash router, TanStack Query, sidebar with now-playing card + sync status),
  `src/lib/{types,api,bridge,img,format,play,open}.ts`, stores
  `src/stores/{player,sync}.ts`, hooks `useBackendEvents`,
  `useProgressSaver` (VOD resume every 10 s), `useVideoViewport`.
  Design tokens in `src/styles.css` (`@theme`), primitives in
  `src/components/ui.tsx`, media parts in `src/components/media.tsx`.
- **T-018 Home** — hero (latest series with backdrop), shelves: Continue
  Watching, Favorite Channels, Recently Watched Channels, Recently Added
  Movies, Recently Updated Series.
- **T-019 Live TV** — `src/pages/Live.tsx`: region-grouped categories
  (collapsible, filter, Favorites/Recent/All pseudo lists), virtualized
  channel list (paging for "All"), 350 ms debounced **live preview** in a
  transparent rounded "hole" (native video positioned with mpv
  `video-margin-ratio-*`, background painted by a 100vmax box-shadow),
  now/next + guide. Keys: ↑/↓/PgUp/PgDn, Enter = fullscreen, F = favorite.
- **T-021/T-022 Movies & Series** — `components/LibraryBrowser.tsx` +
  `components/PosterGrid.tsx` (virtualized rows, infinite paging, responsive
  columns; URL keeps cat/sort/q), detail pages `pages/MovieDetail.tsx`,
  `pages/SeriesDetail.tsx` (`components/DetailHero.tsx`), trailers via
  `tauri-plugin-opener`.
- **T-023 Player** — `pages/Player.tsx`: auto-hiding chrome, seek bar with
  buffer + hover time + drag, live bar (programme progress/next), zapping
  (↑/↓, digits, side list), pause-live with "behind · Go live", audio/sub/
  aspect/speed menus, volume, fullscreen (window API), stats overlay (I),
  next-episode countdown (never showed until the T-047 fix), VOD progress
  saved on exit.
- **Dev workflow** — `scripts/headless.sh start|stop|status|restart-app`
  runs the debug app inside an invisible nested KWin (`kwin_wayland
  --virtual`, private D-Bus, `ao=null`), reusing an already running Vite.
  `.claude/launch.json` defines the `dev` preview (port 1420). In a plain
  browser (desktop preview pane) the UI talks to that headless app through
  the devtools **bridge** (`POST /invoke`, `GET /img`; CORS only for the Vite
  origin, see §3 "Access rules") — real data, no native video.

- **T-017 Onboarding** — `pages/Onboarding.tsx` + `components/SourceForm.tsx`
  (Xtream/M3U tabs, test connection with account status/expiry, advanced:
  backup servers, EPG override, user agent; first-sync progress via events
  + polling).
- **T-020 TV Guide** — `pages/Guide.tsx`: 12 h window (±3 h paging), sticky
  ruler/channel column, virtualized rows via `epg_grid` (new `withEpg`
  filter), now line, programme dialog with Watch live / catch-up.
- **T-024 Search** — `pages/Search.tsx`: FTS results (channels, movies,
  series), recent searches, provider tag in captions to tell duplicates apart.
- **T-025 Settings** — `pages/Settings.tsx`: source cards (status, expiry,
  counts, sync / guide refresh, edit, remove-with-confirm), playback (hwdec,
  TS/HLS, subtitle default, languages), adult toggle, About (engine versions).
- **SECURITY** — network errors no longer include request URLs (Xtream URLs
  carry credentials): `impl From<reqwest::Error> for Error` maps to friendly
  text; mpv log lines are passed through `player::redact`.

- **T-027 Release build** — `bun run tauri build --bundles rpm,deb` → single
  executable `src-tauri/target/release/testpattern` (**57 MB**: UI assets,
  SQLite, static FFmpeg n9.0.2 + mpv embedded; `ldd` shows no FFmpeg/mpv
  libraries — the `libavif.so` it lists is WebKit's AVIF image codec).
  Verified: copied alone into an empty dir, it starts and renders the full UI
  inside the headless compositor (no Vite, no third_party). Packages:
  `bundle/rpm/testpattern-0.1.0-1.x86_64.rpm` / `.deb` (23 MB) with explicit
  Fedora runtime `depends` in `tauri.conf.json` (`bundle.linux.rpm`).
  Re-verified after the session-2 fixes. The `appimage` target in
  `tauri.conf.json` has never been built (T-049).
- **T-026 Keyboard/remote navigation** — `src/hooks/useSpatialNav.ts`
  (arrow keys move focus by screen geometry, Backspace = back; components
  that handle arrows themselves call preventDefault). Also: `/` or Ctrl+K →
  Search, `components/ErrorBoundary.tsx` around pages, "My List" shelf on Home.

- **T-033 Window state + single instance** — `tauri-plugin-single-instance`
  2.4.5 (second launch focuses the window) + `tauri-plugin-window-state` 2.4.1
  (size/position/maximized; not visibility/fullscreen).
- **T-032 Up next** — `catalog::up_next` (uses cached series detail; next
  episode after the most recently *finished* one) + "Up Next" shelf on Home.
  `parse_episodes` factored out of `series_detail`. (Emptied after every
  catalog sync until the T-047 fix.)
- **Test isolation** — `scripts/headless.sh` runs the app with its own
  `XDG_DATA_HOME/CACHE/CONFIG` under `.deps/headless/`; `headless.sh seed`
  adds test account 1 from `.env.local`. The real profile
  (`~/.local/share/dev.testpattern.app`) is the user's — see log entry.

- **T-030 Tests** — `scripts/check.sh` (Rust unit tests, clippy
  `-D warnings`, tsc, vitest, notices check) and `scripts/smoke.sh` (19 end-to-end checks against
  the real provider in the headless session: sync, cleaned categories,
  now/next, paging, search, movie + series detail, mark watched without
  playing, Up next before/after a real catalog sync, live playback, zapping,
  movie resume, continue watching, stop, next-episode prompt at the end of an
  episode, finished episode saved as watched, devtools refusing foreign web
  pages, mpv `run` blocked). Both green on 2026-09-26. Counts at the end of
  the day: 30 Rust unit tests, 26 frontend tests (T-041), 19 smoke checks,
  plus `scripts/csp-check.sh` (T-040). Open: CI wiring (needs a runner with
  the media engine + a provider secret) — T-037.

- **T-047 Review of all Done cards (2026-09-26, session 2)** — every card
  checked against the code and in the running app (headless session +
  preview pane). Found and fixed:
  1. **Next-episode countdown never appeared** (T-023): VOD files are opened
     with `keep-open=yes`, so at the end mpv pauses on the last frame and sets
     `eof-reached`; `end-file` (→ `status "ended"`) never fires. The player
     now also reacts to `eof-reached` (`pages/Player.tsx`). Verified: prompt
     at EOF, next episode autoplays after 10 s, finished one saved as watched.
  2. **"Up next" emptied after every catalog sync** (T-032): full syncs
     deleted `detail_cache`, which `up_next` reads. Details are now kept and
     only orphans pruned (`sources/mod.rs` `prune_detail_cache`). Also a series
     showed up twice when two episodes shared an `updated_at` second
     (`catalog.rs` `up_next_rows` now uses a window function).
  3. **"Mark as watched" did nothing for a never-played episode**:
     `mark_watched` only inserted history rows for movies; it now takes
     `meta` (series id, season, episode, titles, artwork, duration) and
     inserts the row (`library.rs` `set_watched`, `pages/SeriesDetail.tsx`).
  4. **Finished items could flip back to unwatched**: a progress save with
     duration 0 (mpv already unloaded) recomputed `watched` as false. The
     backend now judges by the stored duration; the UI only saves with a
     known duration (`stores/player.ts` `saveProgress`).
  5. **Stale resume positions** right after leaving the player (detail page
     still said "Play"): the store now invalidates watch-state queries after
     its final save (`lib/queryClient.ts` `invalidateWatchState`).
  6. **SECURITY (debug builds): the devtools server trusted every web page**
     (`Access-Control-Allow-Origin: *`, no Origin/Host checks, even though its
     header comment claimed "localhost only"). Any site open in the
     developer's browser could POST JS to `/eval` — i.e. spawn processes via
     mpv `run`, read stream URLs with credentials, or overwrite files via
     `/snapshot?path=`. Now origin/host/fetch-site checked (§3 "Access rules").
  7. **Hardening**: the webview's mpv access is allow-listed (§3); before, any
     mpv command (incl. `run`/`subprocess`) and property (`stream-record` =
     arbitrary file write) was reachable from the UI.
  8. M3U channels with `catchup-days` were offered "Play catch-up", which the
     backend cannot build for M3U → not flagged anymore until T-039.
  9. "Test connection" while editing a saved Xtream source ignored the stored
     password (`source_test` takes the source `id` now).
  10. `Artwork` kept showing its fallback after `src` changed (e.g. the
      sidebar's now-playing logo while zapping) → state resets per URL.
  11. Title cleanup missed compound provider prefixes (`4K-AMZ - `,
      `4K-A+ - `, `EN-TOP - `): 1,420 movies and 442 series kept them → now
      stripped into `tag` (applies after the next sync).
  Also: `scripts/outdated.sh`; unit tests 14 → 23, smoke checks 11 → 19.

- **T-038 License files** — `LICENSE` = verbatim GPLv3 (copied from FFmpeg's
  `COPYING.GPLv3`; sha256 `8ceb4b9e…` = the gnu.org text and 48 distro
  copies). `THIRD_PARTY_NOTICES.md` is generated by `scripts/notices.py`
  (stdlib Python): media engine (FFmpeg/mpv tags parsed from
  `build-media.sh`, their licenses/configure flags), the system libraries
  linked at runtime (curated table `SYSTEM_LIBS`), the 360 crates actually
  linked (normal deps of the Linux x86_64 build via `cargo metadata`,
  proc-macros excluded), the 16 npm packages bundled into the UI (runtime
  deps + transitive) incl. the Inter font (OFL-1.1), and the 182 distinct
  license texts (for "A OR B" the most permissive option is used — MIT
  first; packages without a license file get the standard text with their
  authors). `scripts/check.sh` runs `notices.py --check`, so a dependency
  change without regenerating fails. Packages: rpm installs `LICENSE` +
  notices to `/usr/share/licenses/testpattern/`, deb to
  `/usr/share/doc/testpattern/` plus a DEP-5 `copyright`
  (`packaging/debian/copyright`) — via `bundle.linux.{rpm,deb}.files`;
  `bundle.license`/`licenseFile`/`copyright` set. In-app (GPLv3 §0
  "Appropriate Legal Notices"): Settings → About shows the copyright/no
  warranty/GPL notice and opens `LICENSE` and the notices, embedded as lazy
  `?raw` chunks (35 KB / 354 KB) so they also travel with the bare binary.
  Verified: rpm/deb contents, dialogs in the running app. **When publishing
  binaries**, also publish the matching source (this repo at the release
  commit; FFmpeg/mpv are covered by their tags + `build-media.sh`) — GPL §6.

- **T-040 Content Security Policy** — `app.security.csp` in
  `src-tauri/tauri.conf.json` (was `null`): `default-src 'self'`,
  `script-src 'self'` (no `unsafe-eval`/`unsafe-inline`), `style-src 'self'`
  (React `style={…}` goes through the CSSOM, which CSP doesn't restrict;
  Tailwind is one CSS file), `img-src 'self' img: http://img.localhost data:
  blob:`, `font-src 'self' data:`, `connect-src 'self' ipc:
  http://ipc.localhost`, and `'none'` for media/object/frame/worker/
  form-action. `devCsp` is looser (Vite needs inline scripts/styles + its HMR
  websocket) — but on Linux Tauri cannot inject a CSP into pages loaded from
  the Vite dev server at all, so **dev builds run without CSP**; only bundled
  assets (`tauri://localhost`, release or `tauri build --debug`) enforce it.
  The UI records violations in `window.__TP_CSP__` (+ `console.warn`,
  `src/main.tsx`). **`scripts/csp-check.sh`** builds a debug binary with
  bundled assets (`CARGO_TARGET_DIR=src-tauri/target/prodassets`, ~40 s
  incremental), runs it in the headless session (`TP_APP_BIN` override in
  `scripts/dev-run.sh`), walks every page, opens both license dialogs,
  previews one live channel and fails on any violation (verified both ways:
  0 violations as configured; dropping `img:` → `200× img-src:
  img://localhost`). Run it after changing the CSP or adding a new kind of
  resource (remote images, workers, inline styles from a library).

- **T-039 M3U catch-up** — follows Kodi pvr.iptvsimple conventions (what M3U
  playlists are written for). `sources/m3u.rs`: `catchup`/`catchup-type`,
  `catchup-source`, `catchup-days`/`tvg-rec`/`timeshift` per `#EXTINF`, with
  `#EXTM3U` values as defaults; `catchup_url()` builds `default` (template),
  `append` (suffix), `shift` (`?utc=&lutc=`), `flussonic`
  (`index-<start>-<dur>.m3u8`, `timeshift_abs-<start>.ts`) and `xc` (Xtream
  live URL → `/timeshift/…`) URLs; placeholders `{utc}`/`${start}`,
  `{utcend}`/`${end}`, `{lutc}`/`${now}`/`${timestamp}`, `{duration[:N]}`,
  `{offset[:N]}`, `{utc:Y-m-d H:M:S}` (UTC) and `{Y}{m}{d}{H}{M}{S}` (start in
  the machine's *local* time, like Kodi — Xtream servers want their own zone,
  normally the viewer's). Schema v2 (`db.rs`): `channel.catchup_mode`,
  `channel.catchup_source`. `write_m3u` sets `archive` only when a URL can be
  built (`Entry::catchup_window`, 5 days when a scheme names no window);
  `playback.rs` builds the URL for M3U channels. UI unchanged (the Guide
  already offers catch-up from `archive`). Verified against the provider
  through a local M3U playlist (served from 127.0.0.1, deleted afterwards):
  `xc` and `default` played past programmes (durations matching the guide);
  a 404 for RT Documentary happens with the Xtream source too (archive gap).
  Unit tests: parsing/defaults, every scheme's URL, archive flags.
  Found while testing — **EPG import robustness**: the provider sometimes
  serves a broken guide (`</tv>` inside an open `<desc>` at byte 68.7M); the
  import was all-or-nothing, so a new source got no guide at all. Now
  `epg::import` replaces programmes channel by channel as they appear: a
  document that breaks off still updates what it contained and keeps the
  old guide for the rest (logged as "guide document incomplete"); only a
  complete document prunes channels that disappeared (unit test
  `broken_documents_update_what_they_contain`; ignored diagnostic
  `TP_XMLTV=<file> cargo test --lib -- --ignored xmltv_file`).

- **T-036 Distribution hardening** — libplacebo v7.360.1, dav1d 1.5.4,
  libxml2 v2.15.4 and libdisplay-info 0.4.0 (all latest) are built as static
  meson projects by `scripts/build-media.sh` and linked statically
  (`build.rs` `STATIC`, dependents before dependencies). libdisplay-info
  could not simply be dropped: in mpv 0.41 VA-API needs `vaapi-drm`, which
  needs `drm`, which needs libdisplay-info (no X11/Wayland in our build).
  Our prefix's headers now come first on the include path (the rootless
  sysroot carries the distro's older headers of the same libraries). The
  static libplacebo needs the C++ runtime: `build.rs` adds `-lstdc++`
  (`libstdc++.so.6`; any distro new enough for our glibc has a new enough
  one). The script now rebuilds everything that links a rebuilt component
  (`MARKER`/`DEPENDENTS`). Result: the executable's NEEDED list has no
  version-volatile names left (`libplacebo.so.360`, `libdav1d.so.7`,
  `libxml2.so.2`, `libdisplay-info.so.3` are gone) — only glibc, libstdc++/
  libgcc_s, zlib, OpenSSL 3, libass, lcms2, uchardet, libva/drm, X11/Xfixes,
  ALSA/PipeWire/Pulse, EGL, GTK3/GDK/cairo/GLib/D-Bus, WebKitGTK/JSC 4.1 and
  libsoup 3. rpm `depends` updated (-libplacebo/libdav1d/libdisplay-info/
  libxml2, +libstdc++). Binary 57 → 63 MB. Verified: smoke 19/19, 1080p
  live at 29.97 fps with 0 drops, DASH demuxer present, VA-API still
  initialised (falls back to software for H.264 on Fedora mesa, as before).
  Licenses: the notices list all statically linked native libraries with
  their texts (vendored in `packaging/licenses/`, refreshed by
  `build-media.sh`). `scripts/outdated.sh` now checks every media tag.
  Gotcha: nasm 3.02 occasionally segfaults on dav1d's AVX-512 files in a
  32-way parallel build — just re-run `scripts/build-media.sh`. Remaining
  portability work (glibc baseline, native .deb, AppImage): T-049.

- **T-043 Settings leftovers** — the player volume is remembered: the store
  saves `player.volume` 1 s after it settles (`stores/player.ts`
  `rememberVolume`), `settings.rs` applies it at startup, and the UI syncs
  volume/mute from mpv when it subscribes (`hooks/useBackendEvents.ts` —
  mpv applies the saved volume before the UI listens, so the store started
  out believing 100). `settings_set` now applies only the changed key
  (`apply_player(…, Some(key))`) instead of re-applying `hwdec` & co. on
  every change. `ui.startPage` is implemented: Settings → General → Start
  page (Home / Live TV / TV Guide), applied once per launch in
  `app/Root.tsx` (`START_PAGES`). Verified: volume 55 survives a restart,
  start page Live TV opens `/live`, Home stays reachable afterwards.

- **T-042 Artwork cache size limit** — `images.rs`: a cache hit bumps the
  file's mtime (at most hourly), `prune()` removes abandoned `*.tmp`,
  expired `*.miss` markers and then the least recently used images until
  below 90% of the cap; runs 60 s after startup and every 6 h
  (`enforce_limit`, `lib.rs`), cap = setting `cache.imagesMb` (default 1024,
  min 16). Commands `images_cache_info` / `images_cache_clear`; Settings →
  General shows usage with a Clear button. Unit test
  `prunes_least_recently_used_first`; verified in the app (usage, clear,
  artwork refetch, startup pass logged).

- **T-045 Guide history + streamed guide download** — `epg::history_days`
  keeps as many past days as the source's longest catch-up archive (2..=7,
  was a fixed 2), so the Guide can start catch-up that far back (this
  provider's XMLTV itself only carries ~1 day of history). The guide is now
  streamed to `<cache>/guide-<id>.part` (`sources::download_to`, removed
  afterwards) and parsed from disk (`epg::open_guide`, gunzips `.xml.gz`) —
  the 70 MB body is no longer held in memory; the DB write lock is still
  only taken for parsing, not during the download. Measured on the 70 MB
  guide: peak RSS growth during a guide sync 202 MB → 43 MB. Unit test
  `keeps_history_for_the_longest_archive`.

- **T-044 Per-stream request headers (M3U)** — `m3u.rs` reads user agent
  and referrer from `#EXTINF` attributes (`user-agent`, `http-referrer`, …),
  `#EXTVLCOPT:http-user-agent=` / `http-referrer=`, and Kodi's pipe syntax
  `url|User-Agent=…&Referer=…` (URL-decoded; the pipe part is stripped from
  the URL — before, it was sent to mpv as part of the URL). Schema v3 adds
  `user_agent`/`referrer` to `channel` and `movie`; `play` passes them (item
  UA wins over the source's) via `LoadOptions.referrer` → mpv per-file
  `referrer`. Unit tests `stream_headers`, `per_file_options_escape_values`;
  verified end to end with a local header-logging HTTP server (each of the
  three conventions arrives as configured).

- **T-048 Catch-up time correction per source** — schema v4
  `source.catchup_shift_minutes`; Settings → Edit source → advanced →
  "Catch-up time correction (hours)" (`SourceForm.tsx`, hours in the form,
  minutes in the backend; `None` on update keeps the stored value). Applied
  in `playback.rs`: Xtream timeshift time = start + server offset + shift;
  M3U `catchup_url(…, shift)` moves only the local-time placeholders
  (`{Y}…{S}`, `xc`) — `{utc}` & co. stay exact. Unit test in
  `builds_catchup_urls`; verified the form round trip (−1 h ↔ −60 min).

- **T-041 Frontend unit tests** — vitest 5 (`bun run test`, run by
  `scripts/check.sh`; node environment, no DOM needed): `lib/format`,
  `lib/img`, `stores/player` (`applyProp`, `trackId`, `saveProgress` guards
  and titles, with `lib/api` mocked), `stores/sync`, `hooks/useSpatialNav`
  (`pick`), `pages/Live` (`parseKey`/`keyString`), `components/PosterGrid`
  (`gridLayout`, extracted). 26 tests. They found three bugs, fixed:
  `duration(3599)` said "60m" (now rounds the total → "1h"); arrow keys
  jumped diagonally to a nearer tile of another row instead of the next
  one on the same shelf (`pick` now prefers same row/column, then distance
  — verified on Home: → walks the shelf, ↓ goes to the next shelf); the
  poster grid computed negative sizes before its first measurement.

- **T-035 Recording live TV** — mpv's `stream-record` writes the stream it
  already receives (no second connection — one-stream accounts). Command
  `player_record(on)` (`playback.rs`): the backend picks the path —
  setting `recording.dir`, else `~/Videos/testpattern` — and names the file
  `<channel> <YYYY-MM-DD HH.MM.SS>.ts` (`player::file_name_for` strips
  characters file systems reject); the webview still can't set
  `stream-record` itself (allowlist). `Player` keeps the recording in the
  stream's session: a new channel or stop clears `stream-record` (mpv would
  otherwise overwrite the file with the next stream), an automatic
  reconnect continues in `… (part N).ts`. `stream-record` is observed, so
  the UI follows mpv: Record button + **R** in the live player, a REC badge
  with elapsed time that stays visible when the controls hide, and
  "Recording saved: …" when it ends (also on zapping). Verified: 1080p
  H.264/AAC MPEG-TS written; zap ends and reports it. Tests:
  `recording_file_names`, vitest for the property, smoke check "live
  recording writes MPEG-TS" (20 smoke checks now).

- **T-034 Picture-in-picture while browsing** — the player's PiP button (or
  **P**) keeps playing in a 400×225 window bottom-right
  (`components/PipPlayer.tsx`) and goes back to the previous page (Home if
  there is none); the window has play/pause, back to full screen and close.
  The video renders *behind* the web view, so while PiP shows, `Layout`
  gets a `clip-path` (even-odd path with rounded corners, `pipClipPath`)
  that cuts a hole through the whole UI exactly there, and the video is
  positioned into the same rectangle (`useVideoViewport`). Store flag
  `pip` (`stores/player.ts`; cleared by `stop()` and when the full-screen
  player opens); `Root.tsx` no longer resets the video or stops live TV on
  navigation while PiP is on. Hidden on `/player`, and on `/live` while a
  live channel plays (the preview pane shows it there). Verified in the
  app: movie → P returns to its detail page, keeps playing, survives page
  changes, expand/close work; live → P, Live TV takes over, Home shows it
  again. Vitest for the geometry.

- **T-031 M3U series grouping** — non-live M3U entries named like
  "Show S01E02 - Title", "Show S01 E02" or "Show 1x02" (`m3u::Entry::
  episode_info`) become a `series` row per show (cleaned title/tag/year via
  `names::title`, cover = the episode logo, category from `group-title`,
  searchable) plus rows in the new `episode` table (schema v5: season,
  episode, title, image, ext, url, headers) instead of one "movie" per
  episode. Ids are hashes of group + title (+ season/episode), so they
  survive re-syncs. Everything reading series goes through Xtream's
  `get_series_info` shape: `catalog::m3u_series_json` synthesizes it, so the
  series page, resume and Up next work unchanged; `play` resolves M3U
  episode URLs (with their headers). Unit tests `recognizes_episodes`,
  `m3u_episodes_become_series`, `m3u_series_read_like_xtream_ones`;
  verified end to end with a local playlist (2 series + 1 movie, seasons
  and titles, episode URL requested, Up next after marking watched).

- **T-046 Credentials at rest** — source passwords live in the desktop
  keyring via the Secret Service API (`src-tauri/src/secrets.rs`, crate
  `secret-service` 5.2: DH-encrypted transfer, plain as fallback; KWallet 6
  and GNOME Keyring both offer it — the `keyring` crate would add nothing on
  Linux). Entry: label "testpattern: <source name>", attributes
  `application=testpattern`, `profile=<random id>` (setting
  `secrets.profile`, so profiles never share "source 1"), `source=<id>`.
  Schema v6 adds `source.password_in_keyring`; `sources::load` takes the
  password from an in-memory cache that `secrets::startup` fills before the
  first sync, so callers didn't change. Flows: adding/editing a source puts
  the typed password in the keyring (the unlock prompt may show — the user
  is there); existing plaintext passwords move at startup only while the
  keyring is unlocked (background work never prompts); the move clears the
  row with `secure_delete` + WAL truncate and then rebuilds the file once
  (VACUUM, ~0.3 s for 48 MB), because older versions of the row stay in
  freed page space. Keyring locked at startup: one prompt, at most 90 s,
  then the app carries on — Xtream calls (sync, play, details, test) fail
  with "The password is stored in the system keyring, which is locked…",
  Settings shows **Password: Keyring locked**, the 30-min sync retries
  without prompting, a manual Sync may prompt (for that source only). No
  Secret Service (or other platforms, T-028): the password stays in the
  database — Settings shows **App database**. Renaming relabels the entry,
  removing the source deletes it. Tests: `profile_id_is_stable_per_profile`,
  `clearing_the_plaintext_leaves_no_copy_in_the_file` (fails without
  `secure_delete` or without the rebuild), smoke "password kept in the
  system keyring", ignored `keyring_probe` (read-only look at the session
  keyring). Verified in the headless session against a throwaway GNOME
  Keyring: migration (DB + WAL free of the password, keyring secret equals
  `.env.local`), warm start + all 21 smoke checks, rename, password change
  (still one entry), locked keyring (migration skipped without a prompt; the
  startup prompt opens inside the nested session and times out; play and
  source test give the message; unlock + Sync recovers — that source only, a
  second locked one stays untouched), no Secret Service on the bus (fallback
  to the database, guide sync works, moved back later), remove (entry
  deleted) and add (straight to the keyring). The developer's KWallet
  (`ksecretd`) answered the read-only probe: DH session, default collection
  "kdewallet", unlocked — so the real profile's passwords move there on the
  next start. Not covered: tokens inside M3U playlist/EPG URLs stay in the
  database as typed.

- **T-029 Hardware decoding interop** — instead of the planned mpv rebuild
  with Wayland/X11 support (+ `MPV_RENDER_PARAM_WL_DISPLAY`), the render
  context now gets `MPV_RENDER_PARAM_DRM_DISPLAY_V2` with only `render_fd`
  set: the GPU's render node, from `EGL_EXT_device_drm_render_node` on the
  current EGL display (else the first `/dev/dri/renderD*`), kept open as
  long as the context (`player/linux.rs`). mpv's existing `vaapi-drm` build
  then creates its VA display from it and imports decoded frames into GL as
  dmabufs — no new build dependencies, same path on every Wayland
  compositor. GLX contexts (GTK3 on X11) skip it (dmabuf import needs EGL)
  and keep `vaapi-copy`. Measured in the app, 1080p30 AV1 over 8 s on the RX
  6800 XT (Mesa 26.2): software 21% of a core, `vaapi-copy` (before) 10%,
  `vaapi` (now) 8%; picture correct (snapshot of the test pattern). H.264
  live channels: still software ("No support for codec h264 profile 100"
  from Fedora's Mesa) and play as before. README documents RPM Fusion's
  `mesa-va-drivers-freeworld` for H.264/HEVC (current rpmfusion.org howto).
  Smoke check "GPU decodes without copying frames (vaapi)" (VP9 clip; fails
  with `vaapi-copy`, the old behavior). With RPM Fusion's driver installed
  (user, same day; needed the rpmfusion-free repo first), live channels on
  the provider decode on the GPU too, one stream at a time: H.264 1080p30
  `vaapi` 5% of a core vs software 20%; HEVC 2160p50 ("V Sport Ultra UHD")
  `vaapi` 8% vs software 112%; no dropped frames; the player's info overlay
  shows "Decoder hardware (vaapi)".

- **T-049 Portable release builds** — `scripts/ubuntu-build.sh` (see §3)
  builds in an Ubuntu 24.04 container (273 MB of Ubuntu packages, once) and
  packages one binary needing glibc 2.39 as .deb (25 MB), AppImage (101 MB)
  and .rpm (25 MB). Fixed on the way: (1) mpv had enabled JPEG screenshots
  wherever libjpeg headers exist, so the Ubuntu build needed `libjpeg.so.8`
  (Debian/Fedora ship `.so.62`) → `-Djpeg=disabled`; `build-media.sh` now
  rebuilds everything when the script changes, and both builds need the same
  31 libraries. (2) .deb `Depends` = `dpkg-shlibdeps` output (25 packages
  with minimum versions); `scripts/deb-depends.sh` fails the build when they
  drift. (3) AppImage: linuxdeploy bundles Ubuntu's `libwayland-*` with GTK;
  a newer host Mesa's EGL then fails against them ("Could not create default
  EGL display: EGL_BAD_PARAMETER") → black window on X11 — and it is the
  crash behind the `GDK_BACKEND=x11` that Tauri's GTK hook forces
  (tauri#8541). `scripts/appimage-fix.sh` drops both and repacks onto the
  original runtime. Verified: all 22 smoke checks with the Ubuntu build
  running *inside* the container (Ubuntu's WebKitGTK/Mesa/VA-API); `apt
  install` of the .deb in a bare `ubuntu:24.04` pulls 287 packages and the
  binary finds every library; on this Fedora 44 host the raw binary (= the
  .rpm's content) and the fixed AppImage render the full UI natively on
  Wayland (EGL, VA-API zero-copy) and on X11 through a nested Xwayland (GLX —
  the app's first X11 test; `TP_HEADLESS_X11=1`), while the unfixed AppImage
  showed a black window. Plain `bun run tauri build` makes rpm+deb only now
  (AppImage only via the portable build). Not tested: Debian 13 itself (its
  t64 package names match Ubuntu 24.04's).

- **T-037 CI pipeline** — `.github/workflows/ci.yml` on the private repo
  `df49b9cd/testpattern-app`: `scripts/check.sh` on Ubuntu 24.04 for pushes
  and pull requests, portable bundles for `v*` tags and manual runs. First
  green run on PR #1 (T-049 + T-037, 2026-09-26), which the user merged
  (`7bb128d`); the push run on `main` after the merge was green as well.

- **T-050 Works: one entry per movie/series** — the provider lists films
  and shows several times: series 11,069 entries = 7,802 TMDB ids (2,220
  shows 2–6×), movies 39,236 = 25,365 ids (7,745 films 2–9×); 97% have a
  TMDB id. `works::rebuild` (after every sync, source removal, and at
  startup when `works::RULES_VERSION` ≠ setting `works.rules`) groups them:
  key `tmdb:<id>`; an entry without id joins the TMDB group whose
  normalized title + year match exactly once, else `title:<norm>|<year>`,
  without a year `item:<source>:<id>` (`assign_keys`). Schema v7: `work_key`
  on movie/series, `work` (title by weighted vote — English/service copies
  count 3×, CAM 0 —, year, artwork, rating, genre, newest `added`, number of
  versions, quality badges, services, representative copy), `work_facet`.
  `movies`/`series_list`/`search`/Home return works (JSON: representative
  `sourceId`/`id` + `key`, `versionCount`, `quality`, `services`); filters
  match any copy (`w.key IN (SELECT work_key …)`), text filter on any copy's
  title ("kastanjemanden" → The Chestnut Man). Favorites, continue watching
  and Up next count a title once (`library::toggle_favorite` clears every
  copy; `continue_watching` partitions history by work). Test catalog: 26,238
  movies / 8,031 series works (1 s–2 s regroup in a debug build); lists
  25–30 ms, search 2 ms ("for all mankind" → 1 result, 6 versions; "top
  gun" → Top Gun (5), Top Gun: Maverick (6)). Unit tests in `works/` and
  `catalog.rs`/`library.rs` (grouping, keys, favorites, continue watching).

- **T-051 Versions and seasons on detail pages** — `movie_detail` /
  `series_detail` list every copy as `versions` (`works/versions.rs`
  `VersionInfo`: label from tag + category via `variant.rs` — service NF
  Netflix, AMZ Prime Video, A+ Apple TV+, D+ Disney+/Discovery+ (by
  category), VP Viaplay, P+ Paramount+, PCOK, SHWT, CR, SKY, NICK, MRVL,
  PRMT, UNV, DWA; origin WEB/Blu-ray (`TOP`)/CAM; market EN/SC Nordic/SE/DK/
  NO, "(SUB EN)", "(MULTI-SUBS)"; quality `4K-`, Dolby Vision, Dolby Audio
  (`-DO`), HEVC — plus provider category, resolution/codec/channels/
  duration/container from the provider detail, seasons + episode count, and
  audio/subtitle tracks). Provider details of all copies are fetched at
  once with a 2.5 s deadline (`details_of`; a copy that stalls — the
  provider sometimes takes ~9 s — finishes into the cache and the page says
  `versionsPending`, the UI refetches). **Which copy plays** (`choose`): the
  user's pick (`work_pref`, `work_prefer` command) > the copy last watched >
  best fit for the player's audio/subtitle languages (affinity 80/60/40/20
  by preference rank; services, Blu-ray and multi-subs count as English) +
  quality score (4K +6, DV +3, DA +2, Blu-ray +5, WEB +4, CAM −1000) +
  completeness (series: share of episodes; movies: a copy much shorter than
  the rest — trailers, cut-offs — loses). **Series:** seasons/episodes are
  the union of all copies, each episode from the chosen copy when it has
  it, else from the next best (the UI marks it with the copy's label);
  watched state, resume and Up next match across copies by (season,
  episode); `playEpisode` plays `ep.sourceId`/`ep.seriesId`; the player's
  next-episode countdown finds the current episode by number. **Movies:**
  the position carries over from the latest unfinished copy. **Tracks:** the
  player stores what mpv sees in a movie/episode file (`track-list` +
  HDR from `video-params`) in `media_info` (`player/mod.rs learn_tracks`);
  copies nobody played get a "Check audio & subtitles" button, and the list
  a "Check … of all" that checks them one after another (`probe.rs`: a
  second mpv with `vo=null`, `ao=null`, `vid=no` opens the stream for ~1–2 s
  and reads `track-list`; series try their first two episodes; refused
  while anything plays — one stream per account — and `play` cancels a
  running check first). A copy the server can't open is remembered
  (`media_info` `{"unavailable":true}`): its card says so with "Check
  again", and the automatic choice avoids it (`versions::BROKEN`). UI:
  `components/Versions.tsx` (hero badge "Apple TV+ · 4K Dolby Vision · 6
  versions", cards with picture/sound, seasons, languages — the viewer's
  own first). Verified on For All Mankind: 6 copies (Nordic 4K DA and
  Nordic: S5 only; Netflix: S1; Apple TV+ 4K DV and Apple TV+: S1–5; English
  SD: S1–5) → 5 seasons; default Apple TV+ 4K DV; choosing Nordic plays S5
  from it and S1–4 from Apple TV+ (marked); the 4K DV check found 10 audio
  and 42 subtitle tracks in 1.2 s; "check all" did the other five in ~6 s:
  Nordic 4K DA = English audio + 5 Nordic/English subtitle tracks, Netflix =
  37 subtitle tracks, English SD = no subtitles, Nordic 1080p = its files
  don't open on the provider (both servers). Detail 24 ms warm.

- **T-052 Browse Movies and Series by facets** — `LibraryBrowser.tsx`: a
  left panel instead of the 83/67-chip row: All · Favorites · Services ·
  Genres · Networks · Collections · Languages · Original language ·
  Quality · Decades · Provider categories (grouped by service/language,
  names via `names::display_category`). Facets combine (AND, one value per
  facet, in the URL: `?service=Netflix&genre=Crime&fav=1`), with sort and the
  text filter; counts are works and follow the other active filters
  (`work_facets(kind, query)` counts each facet without its own choice);
  removable filter chips above the grid; poster cards show "4K"/"DV" and
  "6 versions". Genres: series from the provider's (TMDB-style, partly
  Swedish/Danish/Norwegian) genre text, movies from category names — the
  movie list has no genre field, so only 44% of movies had one before
  TMDB (T-054, now 98%). Facet queries 70–95 ms.

- **T-053 Live TV: channels, feeds, countries and genres** — 19,921 feeds →
  17,425 channels (`channel_group`, key `<region>|<name>` with quality words
  and "*MULTI-AUDIO*" folded, "+" kept: "Sky Sports+" ≠ "Sky Sports"); e.g.
  UK "SKY SPORTS F1" = 16 feeds. Genre per channel from its categories
  (`live_from_category`: PPV/EVENT → Events & PPV, NEWS, KIDS, SPORT, 24/7,
  MOVIES/CINEMA, DOCUMENTARY, MUSIC, ENTERTAINMENT) else its name. Live page:
  Countries | Genres | Provider switch; countries (the viewer's languages
  first) unfold into genres and vice versa; a country lists its regular
  channels first, event feeds last, and within a genre the channels with
  guide data first (UK: BBC One, BBC 2, ITV 1 … rather than the provider's
  24/7 "PRIME" feeds, which sort first by position); Favorites/Recent/All
  and the country/
  genre lists show one row per channel playing its chosen feed (the user's
  pick → a favorited feed → best quality: FHD > HD > RAW > none > 4K,
  HEVC −10, SD −20, EPG breaks ties); the preview pane lists the feeds as
  chips ("RAW · HEVC · VIP · Dolby Audio", "4K", …) and remembers a pick
  (`channel_prefer`, survives syncs). Provider mode keeps the raw
  categories. Feeds without an EPG id use their channel's. Guide: countries
  + genre selector, one row per channel; search and "recently watched" list
  a channel once; zapping moves over channels. 7–50 ms per list.

- **T-054 TMDB metadata** — with the user's TMDB key (Settings → Metadata;
  v4 token as Bearer, or v3 key) `tmdb.rs` fetches `/3/movie/{id}` and
  `/3/tv/{id}` for every work with a TMDB id — newest first, 25 requests/s,
  8 at a time, 429s waited out, a 401 stops the run — into `tmdb` (schema
  v8; 404 remembered; refetched after 30 days); runs 20 s after start, after
  each full sync and when the key is saved (checked first with
  `/3/authentication`). `works::rebuild` folds in genres (TMDB names through
  `genre::from_text`), original language, collection (movies), networks
  (series), and rating/year/poster/backdrop where the provider has none —
  only when TMDB's title or year fits the provider's (`tmdb_fits`: some
  entries carry another title's id). Detail pages show original language,
  collection (links to the filtered grid), network, country. Status +
  progress in Settings (`tmdb_status`, `tmdb://progress`); removing the key
  stops a run. The key lives in the system keyring (`secrets::NAMED`, entry
  "testpattern: TMDB API key"; database setting `tmdb.key` only without a
  keyring): a key stored in the database moves over at startup while the
  keyring is unlocked (secure delete + the one-time file rebuild), and a
  locked keyring shows as an error in the status instead of "no key".
  Attribution line as TMDB's terms ask (Settings, README).
  First full run on the test catalog: 33,163 titles in ~23 min at 24/s (it
  resumes after a restart), 32,964 found, 199 not on TMDB, no errors.
  Effect: movies with a genre 44% → 98% (series 97%); new facets: 1,900
  movie collections on 3,910 movies (Beck, Carry On, James Bond, …), 681 TV
  networks on 7,550 shows (Netflix 1,848, Prime Video, BBC One, Apple TV,
  …), 53 original languages (English 20,810 movies, Swedish 588, Danish 527,
  …). Verified in the app: Series → Network "Apple TV" + Original language
  English = 180 shows, other facet counts follow.

- **T-055 TMDB: keep current, cover titles without an id** — refresh runs
  on TMDB's change lists (`/3/movie/changes`, `/3/tv/changes`, paged, at
  most 14 days back via setting `tmdb.changes_since`) instead of refetching
  everything after 30 days: changed ids are marked stale (`fetched_at = 0`)
  before `todo()`. The ~1,100 works without a TMDB id are looked up with
  `/3/search/movie` / `/3/search/tv` (title + year / first-air-date year);
  only a single exact normalized-title match (`works::norm_title`) is
  accepted and stored in the new `tmdb_map` table (title key → TMDB id,
  empty string as the miss sentinel) so `assign_keys` joins them to the
  TMDB group on the next rebuild — 'For All Mankind (2019)'-style titles
  now group correctly. Settings shows the unmapped count. Regression
  guards in `tmdb.rs`.

- **T-056 Versions: HDR in track checks** — the track probe (`probe.rs`)
  now decodes one frame per version (`vid=auto`, `hwdec=no`, paused until
  `video-params` reports, then stop) and sets `hdr` from the gamma
  (`pq`/`hlg`), so HDR10 copies with no `dolby-vision-profile` in the
  track list get the HDR badge too; DV files still light the chip from
  the profile alone.

- **T-028 (P2) Other platforms — macOS port.** First-class AppKit port, all
  native (WKWebView `transparent`/`macOSPrivateApi`, NSOpenGLView under the
  webview via CGL, FFmpeg VideoToolbox, mpv OpenGL render API, Security
  framework Keychain for passwords); Linux unaffected (every new piece is
  `cfg(target_os = "macos")`) and still CI-checked (PR #4 merged). Media
  engine builds via `scripts/build-media.sh` (now dual-platform;
  `-Dgl-cocoa=enabled -Dcoreaudio=enabled -Davfoundation=enabled` on macOS,
  swift-build on for cocoa-cb, videotoolbox on FFmpeg's side); mpv's static
  archive is re-linked with `libtool -static` because its Swift step ships
  `swift.o` already as an archive. Backend: 78 Rust unit tests + 35 vitest +
  clippy clean on macOS arm64. All remaining T-028 work verified
  2026-10-03 — see the log: macOS keychain round-trip (PL-75; secrets.rs
  through the login keychain via `security(1)`, plus a friendly message when
  it's locked, PL-98), packaged .dmg built and verified launching on this
  machine (PL-76), and the T-047 replay on macOS (PL-77:
  `scripts/smoke.sh` 29/29 in `scripts/headless-macos.sh`; the GPU check is
  Linux-only and skips, PL-97). Two new port-specific helpers came out of it:
  devtools `/eval` on macOS now returns results over a devtools-only Tauri
  IPC channel (`eval_with_callback`'s completion is empty on this webview),
  and `scripts/headless-macos.sh` runs muted by default (`--interactive` for
  a visible, audible window).

### 🟨 In progress

- **PL-97 / PL-99 / PL-100 / PL-101** (planned tracker) — implement,
  verify, then move to Done with the worklog entry below.

### 🟦 To do

#### T-028 (P2) Other platforms
- Windows: libmpv render API with a WGL context or `wid` embedding; build
  FFmpeg/mpv statically per platform. Mobile: hls.js fallback player fed
  by a local Rust HTTP proxy that remuxes TS → HLS/fMP4.

### ⛔ Blocked / needs the user
- Pushing: only when the user asks (remote and project notes in §3 "Git").
- T-028 needs a Windows machine (and a decision about mobile).
- Optional: run the `sudo dnf install ...` from §3 so builds don't need the
  rootless sysroot. Anything with `sudo` is the user's to run in their own
  terminal: inside the agent's sandbox it fails ("no new privileges").

---

## 6. Log

- **2026-10-03** — PL-101/PL-99/PL-100/PL-97 (planned tracker). PL-101:
  `src/app/Layout.tsx` gained a global-space listener — Space/K/M while
  playback runs off-page routes to `/player` (after the existing input
  guard; Space with a focus stays on the control). PL-99/PL-100:
  `sub-visibility` is now an observed mpv prop (`player/mod.rs` +
  `stores/player.ts` `subVisible`); the subs menu has an explicit
  Subtitles on/off row (PL-99, also bound to the `S` key) and picking a
  track while subs are hidden flips subsEnabled on; fresh profiles default
  `player.subsEnabled=true` (PL-100) so slang-selected tracks actually
  show, plus a one-time "Subtitles are off — press S" hint when a track
  got auto-selected with subs off. PL-97: the smoke GPU check now uses
  the live recording (no system ffmpeg needed) and, when hwdec never
  engages on the clip (Linux without a render node, macOS in a headless
  session with no window-server VT), *skips* rather than fails — matching
  the previous Linux behavior.

- **2026-09-26** — Surveyed machine + provider. Chose embedded static
  libmpv/FFmpeg (system codecs unusable). Built rootless sysroot + media
  engine. Tauri skeleton + static linking. Native video surface under
  transparent webview validated with a live channel (60 fps, ~21% of one core).
  Debug automation server added. Wrote backend data layer (T-009, uncompiled).
  User asked for this kanban work log (keep it current).
- **2026-09-26** — T-009/T-010 done: backend compiles, tests pass, real sync
  verified (numbers in Done column).
- **2026-09-26** — T-011…T-015 done and verified through devtools; fixed a
  crash-on-click caused by the GTK widget reparenting. Starting frontend.
- **2026-09-26** — Frontend: foundation, Home, Live TV (+ native preview
  window), Movies/Series grids + details, fullscreen player. Fixed: Artwork
  zero-height (`relative` vs `absolute`), names ("TV ᶜᶦᵗʸ" → "TV CITY",
  `##` separators, mid-title years). Added headless test runner + browser
  bridge after the developer stopped my Bash-launched Vite (use the managed
  `dev` preview instead; never leave servers running from Bash).
- **2026-09-26** — Tests: `check.sh` + `smoke.sh` (11/11 green); clippy clean.
- **2026-09-26** — INCIDENT: I deleted history/favorite rows from the user's
  real profile assuming they were test data; two favorites (MobLand, Slow
  Horses) were the user's own (made via the preview pane). Recovered them by
  replaying the SQLite WAL on a copy (truncate `-wal` to an earlier commit
  frame, open, read) and re-inserting. Tests now use an isolated profile.
  Rule: never modify the real profile without asking.
- **2026-09-26** — Up next, single-instance, window-state.
- **2026-09-26** — Release build verified as a standalone single binary
  (57 MB); rpm/deb bundles with explicit runtime deps; spatial keyboard nav.
- **2026-09-26** — Onboarding, Guide, Search, Settings done; fixed credential
  leak in network error messages; headless runner waits for React mount.
- **2026-09-26** — User asked for latest versions everywhere: pinned Rust
  1.98.1, upgraded Bun 1.4.2, FFmpeg n8.1.3 → n9.0.2 (mpv 0.41.0 builds and
  plays fine against it, 0 dropped frames), `cargo update` for transitive
  crates, serde `rename_all_fields` fix for tagged enums (SyncProgress,
  TestResult, PlayerEvent now emit camelCase fields). See "Dependency policy".
- **2026-09-26 (session 2)** — Status review requested by the user: checked
  every Done card against the code and in the running app (headless session,
  preview pane). Static checks and the old 11-step smoke test were green, but
  4 Done features were broken in use (next-episode countdown, Up next after a
  sync, mark-watched for unplayed episodes, watched flag reset by a
  duration-0 save) and the debug devtools server accepted requests from any
  web page. All fixed + covered by tests — details in T-047. Worklog fixes: a
  stale duplicate repository-map table removed, the missing crates.io
  "comparison snippet" replaced by `scripts/outdated.sh`, the release command
  corrected to `--bundles rpm,deb` (AppImage never built). New cards
  T-039…T-046 for gaps found but not fixed. Dependencies re-audited: all at
  latest stable (GTK3 pins as documented). Noted: the repo has no commits yet.
- **2026-09-26** — Git set up on the user's request: `.gitignore` rewritten
  (rebuildable output, dev environment, secrets, editor/OS files),
  `.gitattributes` (LF, binaries), `.env.example`, credential-guard
  `pre-commit` hook (`scripts/git-hooks`, enabled via `core.hooksPath`),
  initial commit on `main` (138 files; staged content scanned for
  `.env.local` values first). Commits use the identity of the logged-in `gh`
  account (its name + GitHub noreply address; repo-local `user.name` /
  `user.email`), as the user asked.
- **2026-09-26** — User: "continue work on the worklog". T-038 done (GPL
  text, generated third-party notices incl. 360 crates / 16 npm packages /
  182 license texts, packaged in rpm+deb, in-app legal notice). Next: T-040.
- **2026-09-26** — T-040 done (CSP for bundled assets + `scripts/csp-check.sh`,
  0 violations across the UI). Next: T-039.
- **2026-09-26** — T-039 done (M3U catch-up, verified on the provider) + EPG
  import now survives broken guide documents. Next: T-036.
- **2026-09-26** — T-036 done (volatile libraries static; binary depends on
  long-stable system libraries only); T-049 created for the rest of
  portability. All P1 cards are done. Next: the P2 cards.
- **2026-09-26** — P2 cards done in priority order: T-043 (volume/start
  page/…), T-042 (artwork cache limit), T-045 (guide history, streamed
  download), T-044 (M3U request headers), T-048 (catch-up correction),
  T-041 (vitest; found 3 real bugs), T-035 (recording), T-034 (PiP), T-031
  (M3U series). Each verified in the headless app; details in Done.
- **2026-09-26** — T-046 done (passwords in the desktop keyring, see Done).
  Found while testing it: earlier headless sessions had left processes
  running after `stop` — among them KWallet daemons (`ksecretd`) started on
  the private bus with the developer's real HOME and display. Nothing was
  written (`~/.local/share/kwalletd` unchanged since login); the leftovers
  were stopped, and `headless.sh` now gives on-demand services the nested
  display + isolated profile, runs its own throwaway keyring and ends
  everything the bus started. Remaining To-do cards need the user (T-037
  remote, T-049 containers/AppImage download) or other hardware/OSes
  (T-028); T-029 is next.
- **2026-09-26** — T-029 done (VA-API zero-copy through the GPU's render
  node; smoke 22/22). Release build re-verified: 62 MB, direct library needs
  unchanged. What remains needs the user: T-037 (git remote for CI), T-049
  (containers for older distros, OK to download `linuxdeploy`), T-028
  (Windows/macOS hardware).
- **2026-09-26** — User installed RPM Fusion's `mesa-va-drivers-freeworld`
  (after enabling rpmfusion-free — "No match" before; README/WORKLOG now
  say so). Verified: H.264 and 4K HEVC live channels decode as `vaapi`
  (numbers in T-029).
- **2026-09-26 (session 3)** — User asked to check the project status, the
  worklog and the finished work, "especially grouping of content": shows
  exist several times per source (web-rip, Blu-ray…) with different
  quality, subtitles and audio, have seasons, live channels should be
  categorized, "actually all content should be categorized and grouped".
  Status check first: `scripts/check.sh` and smoke (22/22) green; PR #1 (CI)
  merged by the user, its CI run green (T-037 → Done). Built T-050…T-053
  (works, versions with season union and version choice, facet browser,
  channel groups with feeds and country/genre navigation) — see Done.
  Mid-session the user supplied TMDB credentials (stored in `.env.local`,
  never printed) and the TMDB OpenAPI URL → T-054. Bugs found on the way and
  fixed: detail-page season tabs were hidden under the hero backdrop (the
  backdrop overflows the hero; content below it is now positioned);
  `+` dropped from channel keys merged "Sky Sports+" into "Sky Sports";
  duplicated `versions` JSON key (count vs list) → `versionCount`; the
  first track check returned nothing because mpv drops a file with neither
  audio nor video selected. New follow-ups: T-055, T-056. Tests: 64 Rust
  unit tests, 35 vitest, smoke 30 checks.
- **2026-09-26 (session 4)** — User asked again for a status check; the
  parallel session 3 had run out of quota with its work (T-050…T-054)
  uncommitted in this checkout. Taken over: backed up the diff
  (`.deps/takeover-2158/`), read its last steps (it had finished: smoke
  30/30, TMDB fill done, worklog written), then re-verified everything
  independently — `scripts/check.sh` (68 Rust unit tests incl. 4 new,
  35 vitest), smoke 30/30, CSP clean across all pages, and the new UI by
  hand (Series facet panel: 8,022 works; For All Mankind: 6 versions with
  tracks, 5 seasons as a union; Live TV: countries/genres, 19,921 feeds →
  17,425 channels). CI: PR #1 merged, main green. Found and fixed: the TMDB
  key sat in the database in plaintext although T-046 moved credentials to
  the keyring → now a keyring secret too (`secrets.rs` entries are generic:
  source passwords + `NAMED`); verified in the headless session (plaintext
  moved over, the DB and WAL no longer contain it, keyring value = the
  `.env.local` one, setting it again stays in the keyring, status
  unchanged). A country's channel list opened with 24/7 FAST feeds
  (provider order) → guide channels first per genre (UK now opens with BBC
  One London, BBC 1, BBC 2, ITV 1 …). Tests added for both. Nothing
  committed yet (branch `content-grouping`, based on the merged PR #1).
- **2026-10-02** — Status/hygiene session. Dependency audit
  (`scripts/outdated.sh`): everything current except Rust (1.98.1 → 1.99.0,
  re-pinned in `rust-toolchain.toml` + `rust-version`; the GTK3 pins stand)
  and the local Bun install (1.3.9 → 1.4.2, matching the `packageManager`
  pin). `cargo update` pulled tauri 2.11 → 2.12.1 (Cargo spec `2.11` is a
  semver range); the new `tauri-codegen` 2.7 dropped `bundle.macOS.category`
  and `bundle.linux.category` from the config schema (superseded by the
  top-level `bundle.category`), so `tauri.conf.json` now has only
  `category: "Video"` under `bundle` (plus this machine's `transparent` /
  `macOSPrivateApi` edits). Fixed two Linux-only-code warnings that fail
  `-D warnings` on macOS (`unused mpsc` import in devtools.rs, dead
  `Mpv::raw` in mpv.rs) with `cfg_attr(not(target_os = "linux"), allow)`.
  `bun.lock` regenerated with Bun 1.4.2 (lockfile v2, in-range bumps incl.
  @tauri-apps/cli 2.11.5 → 2.12.1); `THIRD_PARTY_NOTICES.md` regenerated.
  This checkout lives on macOS now (session 4's work was on Linux), so
  `scripts/build-media.sh` was made dual-platform (`nproc` →
  `sysctl -n hw.ncpu` fallback; VA-API/libdrm/EGL and
  pipewire/pulse/alsa gated to Linux, VideoToolbox/Cocoa/CoreAudio on
  macOS), Homebrew installed meson/ninja/cmake/nasm + libass/lcms2/uchardet
  and the media engine built into `third_party/prefix`.
  `src-tauri/build.rs` adds the Homebrew lib dirs to the macOS link search
  and links the Apple frameworks mpv needs. `scripts/check.sh`: all green
  on macOS (68 Rust unit tests, 35 vitest, clippy clean, notices current).
- **2026-10-02 (hygiene)** — Gap analysis vs HEAD 8ff4463 (branch
  `df49b9cd/macos-port`): T-055 (TMDB change-lists refresh + `tmdb_map`
  search mapping for id-less works) and T-056 (`probe.rs` decodes one
  frame per version, `hdr` from `video-params` gamma `pq`/`hlg`) were
  committed but still open on the board — moved to Done. T-028 stays In
  progress (macOS keychain round-trip, packaged .dmg and the T-047 replay
  are still open); its To-do bullet is now Windows/mobile-only (the
  macOS/CGL half is committed and tracked by the In-progress card), and
  the Blocked line drops the macOS-machine premise (this checkout lives
  on macOS arm64). The "push to GitHub" blocked-policy line stays as is —
  it no longer gates T-037 (already Done: green run + merged PR #1).
