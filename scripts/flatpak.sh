#!/usr/bin/env bash
# Flatpak build (WORKLOG T-057) from packaging/flatpak/dev.testpattern.app.yml:
# GNOME runtime, the media engine built inside the SDK, the working tree as the
# app's source. Uses flatpak-builder when installed, else Flathub's
# org.flatpak.Builder app; the runtimes come from Flathub (per user).
# Everything it writes lives in .deps/flatpak/.
#
#   scripts/flatpak.sh [install]  build + install for this user → flatpak run dev.testpattern.app
#   scripts/flatpak.sh bundle     build + single-file bundle → .deps/flatpak/testpattern.flatpak
#                                 (install elsewhere: flatpak install --user testpattern.flatpak)
#   scripts/flatpak.sh pins       check that the manifest's media sources match build-media.sh
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
APP=dev.testpattern.app
MANIFEST="$ROOT/packaging/flatpak/$APP.yml"
OUT="$ROOT/.deps/flatpak"

# The git sources of the media-engine module must be the tags build-media.sh
# pins (the module writes those tags as its fetch stamps, so a mismatch would
# silently build other code than the script's).
pins() {
  local bad=0 var name tag pinned
  while read -r var name; do
    pinned=$(sed -n "s/^$var=//p" "$ROOT/scripts/build-media.sh")
    tag=$(awk -v d="dest: third_party/src/$name" '
      /- type: git/ { t = "" } /^ *tag: / { t = $2 } $0 ~ d"$" { print t }' "$MANIFEST")
    if [[ "$tag" != "$pinned" ]]; then
      echo "flatpak: $name is $tag in $(basename "$MANIFEST"), build-media.sh pins $pinned (update tag + commit)" >&2
      bad=1
    fi
  done <<'EOF'
DAV1D_TAG dav1d
LIBXML2_TAG libxml2
FFMPEG_TAG ffmpeg
LIBPLACEBO_TAG libplacebo
LIBDISPLAYINFO_TAG libdisplay-info
MPV_TAG mpv
EOF
  local toolchain bun
  toolchain="$(sed -n 's/^channel *= *"\(.*\)"/\1/p' "$ROOT/rust-toolchain.toml")"
  bun=$(python3 -c "import json, sys; print(json.load(open(sys.argv[1]))['packageManager'].removeprefix('bun@'))" "$ROOT/package.json")
  grep -q "/rustc-$toolchain-x86_64" "$MANIFEST" ||
    { echo "flatpak: the manifest's Rust is not $toolchain (rust-toolchain.toml)" >&2; bad=1; }
  grep -q "/bun-v$bun/" "$MANIFEST" ||
    { echo "flatpak: the manifest's bun is not $bun (package.json)" >&2; bad=1; }
  return $bad
}

builder() {
  if command -v flatpak-builder >/dev/null; then
    flatpak-builder "$@"
  else
    flatpak info --user org.flatpak.Builder >/dev/null 2>&1 ||
      flatpak install --user -y --noninteractive flathub org.flatpak.Builder
    flatpak run --user org.flatpak.Builder "$@"
  fi
}

build() { # extra flatpak-builder args…
  pins
  flatpak remote-add --user --if-not-exists flathub https://dl.flathub.org/repo/flathub.flatpakrepo
  mkdir -p "$OUT"
  builder --user --force-clean --ccache --install-deps-from=flathub \
    --state-dir="$OUT/state" --jobs="${TP_JOBS:-$(nproc)}" "$@" "$OUT/build" "$MANIFEST"
}

case "${1:-install}" in
  install)
    build --install
    echo "==> installed: flatpak run $APP"
    ;;
  bundle)
    build --repo="$OUT/repo"
    flatpak build-bundle --runtime-repo=https://dl.flathub.org/repo/flathub.flatpakrepo \
      "$OUT/repo" "$OUT/testpattern.flatpak" "$APP"
    ls -l "$OUT/testpattern.flatpak"
    ;;
  pins)
    pins && echo "flatpak: manifest pins match build-media.sh, rust-toolchain.toml and package.json"
    ;;
  *)
    echo "usage: $0 [install|bundle|pins]" >&2
    exit 2
    ;;
esac
