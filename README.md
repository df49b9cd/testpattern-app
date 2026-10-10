# testpattern

A UHF / Infuse–style IPTV player for the desktop: Live TV with a real TV
guide, Movies and Series with rich artwork, favorites, continue watching and
search, for **Xtream Codes** accounts and **M3U** playlists (+ XMLTV).

- **UI:** React 19 + TypeScript + Tailwind 4 (Vite, Bun) inside Tauri 2
- **Backend:** Rust (SQLite catalog, Xtream/M3U sync, XMLTV import, artwork cache)
- **Playback:** libmpv + FFmpeg built from source and **statically linked**, rendered
  through OpenGL underneath the web UI — plays H.264/HEVC/AV1, AAC/AC3/EAC3/DTS,
  MPEG-TS/HLS/MKV regardless of the distro's codec packages
- **Ships as one executable** (~62 MB) — .deb, .rpm and AppImage
- **Hardware decoding** through VA-API with zero-copy on Wayland; **source
  passwords** live in the desktop keyring (KWallet, GNOME Keyring)
- **One entry per title:** the copies a provider lists of a film or show
  (Netflix, Apple TV+, Blu-ray, Nordic, 4K Dolby Vision, …) become *versions*
  with their seasons, picture, audio and subtitles; one row per TV channel
  with its feeds (HD, RAW, HEVC, 4K…); browse by service, genre, language,
  quality, decade, country and live genre
- **Optional TMDB metadata** with your own free API key (Settings →
  Metadata): genres for every movie, original languages, collections, TV
  networks. This product uses the TMDB API but is not endorsed or certified
  by TMDB.

## Build (Fedora)

```bash
sudo dnf install $(scripts/fedora-sysroot.sh --print-packages)   # or: scripts/fedora-sysroot.sh (rootless)
scripts/build-media.sh          # static FFmpeg + libmpv (≈1 min)
bun install
bun run tauri dev               # development
bun run tauri build             # release binary + rpm/deb for this machine
scripts/ubuntu-build.sh         # portable release: deb + AppImage + rpm (glibc ≥ 2.39)
```

The portable release is built in an Ubuntu 24.04 container (podman), so it
runs on Ubuntu 24.04+, Debian 13, Fedora 40+ and similar — see WORKLOG.md §3.

If you used the rootless sysroot, run `. .deps/env.sh` before building.

**GPU decoding on Fedora:** the stock Mesa drivers decode AV1 and VP9 on AMD
GPUs but not H.264/HEVC — which most IPTV streams use, so those are decoded
in software (fine on current CPUs). RPM Fusion's driver adds them — enable
its *free* repository first (Fedora's preinstalled RPM Fusion entries only
cover NVIDIA and Steam), see
[rpmfusion.org/Howto/Multimedia](https://rpmfusion.org/Howto/Multimedia):

```bash
sudo dnf install https://mirrors.rpmfusion.org/free/fedora/rpmfusion-free-release-$(rpm -E %fedora).noarch.rpm
sudo dnf install mesa-va-drivers-freeworld
```

The player's stats show which decoder is in use.

## Test

```bash
scripts/check.sh                                   # unit tests, clippy, typecheck (also CI)
scripts/headless.sh start && scripts/headless.sh seed && scripts/smoke.sh
```

The smoke test needs a provider account: copy `.env.example` to the
gitignored `.env.local` and fill it in (optionally with a TMDB key:
`TP_TMDB_TOKEN`, see `.env.example`). To make sure those credentials never
reach a commit, enable the repository's hook once per clone:

```bash
git config core.hooksPath scripts/git-hooks
```

## Release

Tag a commit on main as `v<version>` — `package.json`,
`src-tauri/tauri.conf.json` and `src-tauri/Cargo.toml` must all carry that
version (the release job enforces it). CI builds the Linux
.deb/.rpm/AppImage and the macOS .dmg, writes `SHA256SUMS`, attests each
asset's build provenance and opens a **draft** release with notes; review
it and publish on GitHub. macOS assets are signed and notarized when the
repository secrets are configured (see WORKLOG.md §3 "Releases"), ad-hoc
signed otherwise.

## Status and roadmap

See **[WORKLOG.md](WORKLOG.md)** — architecture decisions, how everything fits
together, the kanban board with self-contained task descriptions, and the
dated work log.

## License

GPL-3.0-or-later (the embedded FFmpeg and mpv are GPL builds). testpattern
ships no content; use sources you are entitled to.
