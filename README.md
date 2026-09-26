# testpattern

A UHF / Infuse–style IPTV player for the desktop: Live TV with a real TV
guide, Movies and Series with rich artwork, favorites, continue watching and
search, for **Xtream Codes** accounts and **M3U** playlists (+ XMLTV).

- **UI:** React 19 + TypeScript + Tailwind 4 (Vite, Bun) inside Tauri 2
- **Backend:** Rust (SQLite catalog, Xtream/M3U sync, XMLTV import, artwork cache)
- **Playback:** libmpv + FFmpeg built from source and **statically linked**, rendered
  through OpenGL underneath the web UI — plays H.264/HEVC/AV1, AAC/AC3/EAC3/DTS,
  MPEG-TS/HLS/MKV regardless of the distro's codec packages
- **Ships as one executable** (~57 MB) — see `bun run tauri build`

## Build (Fedora)

```bash
sudo dnf install $(scripts/fedora-sysroot.sh --print-packages)   # or: scripts/fedora-sysroot.sh (rootless)
scripts/build-media.sh          # static FFmpeg + libmpv (≈1 min)
bun install
bun run tauri dev               # development
bun run tauri build --bundles rpm,deb   # release binary + rpm/deb
```

If you used the rootless sysroot, run `. .deps/env.sh` before building.

## Test

```bash
scripts/check.sh                                   # unit tests, clippy, typecheck
scripts/headless.sh start && scripts/headless.sh seed && scripts/smoke.sh
```

The smoke test needs a provider account: copy `.env.example` to the
gitignored `.env.local` and fill it in. To make sure those credentials never
reach a commit, enable the repository's hook once per clone:

```bash
git config core.hooksPath scripts/git-hooks
```

## Status and roadmap

See **[WORKLOG.md](WORKLOG.md)** — architecture decisions, how everything fits
together, the kanban board with self-contained task descriptions, and the
dated work log.

## License

GPL-3.0-or-later (the embedded FFmpeg and mpv are GPL builds). testpattern
ships no content; use sources you are entitled to.
