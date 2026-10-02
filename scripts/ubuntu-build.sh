#!/usr/bin/env bash
# Portable Linux release (WORKLOG T-049): builds the media engine and the app
# inside an Ubuntu 24.04 container (podman), so the executable needs glibc
# 2.39 rather than this machine's, and packages that one binary as .deb,
# AppImage and .rpm. The pinned Rust toolchain, the crate cache, bun and
# node_modules come from this machine; the image only adds Ubuntu's -dev
# packages (packaging/ubuntu/). Everything it writes lives in .deps/ubuntu24/.
#
#   scripts/ubuntu-build.sh [release]  bundles → .deps/ubuntu24/target/release/bundle/
#   scripts/ubuntu-build.sh debug      debug binary (loads the UI from `bun run dev`)
#   scripts/ubuntu-build.sh run-app    runs that debug binary in the container — as
#                                      TP_APP_BIN for scripts/headless.sh (WORKLOG §3)
#   scripts/ubuntu-build.sh shell      a shell in the build environment
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
IMAGE=localhost/testpattern-build:ubuntu24.04
OUT="$ROOT/.deps/ubuntu24"
TOOLCHAIN="$(sed -n 's/^channel *= *"\(.*\)"/\1/p' "$ROOT/rust-toolchain.toml")"
RUST="${RUSTUP_HOME:-$HOME/.rustup}/toolchains/$TOOLCHAIN-x86_64-unknown-linux-gnu"
BUN="$(readlink -f "$(command -v bun)" 2>/dev/null || realpath "$(command -v bun)")"
[[ -d "$RUST" ]] || { echo "Rust $TOOLCHAIN missing: rustup toolchain install $TOOLCHAIN" >&2; exit 1; }

# Packaging engine: podman by default (Fedora), docker when selected with
# CONTAINER_ENGINE=docker or when podman is unusable (e.g. a macOS dev
# machine running colima/docker, where the container is an emulated x86_64
# Ubuntu; keep TP_JOBS/CARGO_BUILD_JOBS low there).
CONTAINER_ENGINE="${CONTAINER_ENGINE:-podman}"
if ! command -v "$CONTAINER_ENGINE" >/dev/null 2>&1 \
   || { [[ "$CONTAINER_ENGINE" == podman ]] && ! podman machine inspect >/dev/null 2>&1 \
        && [[ "$(uname -s)" == Darwin ]] && command -v docker >/dev/null 2>&1; }; then
  command -v docker >/dev/null 2>&1 && CONTAINER_ENGINE=docker
fi

# bind mounts require a supported filesystem on both sides; --userns=keep-id is a
# podman-only flag mapping the caller's uid; docker runs as its VM's root unless
# -u is given (then /work must be writable for the uid).
UID_="$(id -u)"; GID_="$(id -g)"

APP_ENV=()
if [[ "${1:-}" == run-app ]]; then
  # scripts/headless.sh starts us with the app's environment (isolated
  # profile, private session bus, nested display). That goes into the
  # container; podman itself needs this user's normal one — its image store
  # lives under XDG_DATA_HOME, and it talks to systemd on the session bus.
  bus="${DBUS_SESSION_BUS_ADDRESS#unix:path=}"
  bus="${bus%%,*}"
  mkdir -p -m 700 "$OUT/runtime"
  APP_ENV=(--device /dev/dri
    -v "$OUT/runtime:/run/tp" -e XDG_RUNTIME_DIR=/run/tp
    -v "$XDG_RUNTIME_DIR/$WAYLAND_DISPLAY:/run/tp/$WAYLAND_DISPLAY"
    -e "WAYLAND_DISPLAY=$WAYLAND_DISPLAY" -e "GDK_BACKEND=${GDK_BACKEND:-wayland}"
    -v "$bus:$bus" -e "DBUS_SESSION_BUS_ADDRESS=$DBUS_SESSION_BUS_ADDRESS"
    -v "$ROOT/.deps/headless:$ROOT/.deps/headless"
    -e "XDG_DATA_HOME=$XDG_DATA_HOME" -e "XDG_CACHE_HOME=$XDG_CACHE_HOME" -e "XDG_CONFIG_HOME=$XDG_CONFIG_HOME"
    -e "TP_DEV_MUTE=${TP_DEV_MUTE:-}" -e "TP_DEV_AO=${TP_DEV_AO:-}")
  unset XDG_DATA_HOME XDG_CACHE_HOME XDG_CONFIG_HOME
  export DBUS_SESSION_BUS_ADDRESS="unix:path=$XDG_RUNTIME_DIR/bus"
fi

# the image, rebuilt whenever its definition changes
stamp="$(cat "$ROOT/packaging/ubuntu/Containerfile" "$ROOT/packaging/ubuntu/packages.txt" | { sha256sum 2>/dev/null || shasum -a 256; } | cut -c1-12)"
LABEL_F='{{index .Labels "tp.stamp"}}'
[[ "$CONTAINER_ENGINE" == docker ]] && LABEL_F='{{index .Config.Labels "tp.stamp"}}'
PLATFORM=()
# an arm64 docker host (Apple Silicon) would otherwise build/pull arm64 images,
# but the mounted Rust toolchain is x86_64
[[ "$CONTAINER_ENGINE" == docker && "$(uname -m)" == arm64 ]] && PLATFORM=(--platform linux/amd64)
if [[ "$("$CONTAINER_ENGINE" image inspect -f "$LABEL_F" "$IMAGE" 2>/dev/null)" != "$stamp" ]]; then
  "$CONTAINER_ENGINE" build "${PLATFORM[@]}" --label "tp.stamp=$stamp" -t "$IMAGE" "$ROOT/packaging/ubuntu" >&2
fi
mkdir -p "$OUT/home" "$OUT/cargo"

USERNS=(--userns=keep-id --security-opt label=disable)
if [[ "$CONTAINER_ENGINE" == docker ]]; then
  # no keep-id/SELinux under docker; root inside is fine on macOS (mounts map to
  # the caller); on a Linux docker host run as the caller instead
  USERNS=(); [[ "$(uname -s)" != Darwin ]] && USERNS=(-u "$UID_:$GID_")
fi

EXTRA=()
container() { # [exec] command… — in the build container, as the calling user
  local how=""
  if [[ $1 == exec ]]; then how=exec; shift; fi
  $how "$CONTAINER_ENGINE" run --rm --init --network=host "${USERNS[@]}" "${PLATFORM[@]}" \
    -v "$ROOT:/work" -w /work \
    -v "$RUST:/rust:ro" -v "$BUN:/usr/local/bin/bun:ro" -v "$BUN:/usr/local/bin/bunx:ro" \
    -v "$OUT/cargo:/cargo" -v "$HOME/.cargo/registry:/cargo/registry" \
    -e HOME=/work/.deps/ubuntu24/home -e CARGO_HOME=/cargo -e CARGO_NET_OFFLINE=true \
    -e PATH=/rust/bin:/usr/local/bin:/usr/bin:/bin \
    -e TP_MEDIA_PREFIX=/work/.deps/ubuntu24/prefix -e TP_MEDIA_BUILD=/work/.deps/ubuntu24/media-build \
    -e CARGO_TARGET_DIR=/work/.deps/ubuntu24/target -e TP_JOBS -e CARGO_BUILD_JOBS \
    "${EXTRA[@]}" "$IMAGE" "$@"
}

case "${1:-release}" in
  release)
    container bash -euc '
      scripts/build-media.sh
      # the AppImage tools are AppImages themselves, and containers have no FUSE
      APPIMAGE_EXTRACT_AND_RUN=1 bun run tauri build --bundles deb,appimage,rpm
      scripts/appimage-fix.sh "$CARGO_TARGET_DIR"/release/bundle/appimage/*.AppImage
      bin=$CARGO_TARGET_DIR/release/testpattern
      echo "==> the executable needs $(objdump -T "$bin" | grep -oE "GLIBC_[0-9.]+" | sort -Vu | tail -1)"
      scripts/deb-depends.sh "$bin"
    '
    ls -l "$OUT"/target/release/bundle/deb/*.deb "$OUT"/target/release/bundle/appimage/*.AppImage \
      "$OUT"/target/release/bundle/rpm/*.rpm
    ;;
  debug)
    container bash -euc 'scripts/build-media.sh && cd src-tauri && cargo build'
    ;;
  run-app)
    shift
    EXTRA=("${APP_ENV[@]}")
    container exec /work/.deps/ubuntu24/target/debug/testpattern "$@"
    ;;
  shell)
    EXTRA=(-it)
    container exec bash
    ;;
  *)
    echo "usage: $0 [release|debug|run-app|shell]" >&2
    exit 2
    ;;
esac
