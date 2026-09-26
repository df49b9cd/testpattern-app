#!/usr/bin/env bash
# Builds the embedded media engine: FFmpeg (static, full native decoders) and
# libmpv (static, render API over OpenGL) into third_party/prefix.
#
# The Tauri binary links both statically, so playback never depends on the
# distro's codec situation (e.g. Fedora's ffmpeg-free has no H.264/HEVC).
# Only ubiquitous system libraries (libass, libplacebo, libva, pipewire, ...)
# are linked dynamically.
#
# Usage: scripts/build-media.sh [--clean]
set -euo pipefail

FFMPEG_TAG=n9.0.2
MPV_TAG=v0.41.0

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TP="$ROOT/third_party"
SRC="$TP/src"
BUILD="$TP/build"
PREFIX="$TP/prefix"
JOBS="$(nproc)"

[[ -f "$ROOT/.deps/env.sh" ]] && . "$ROOT/.deps/env.sh"
export PKG_CONFIG_PATH="$PREFIX/lib/pkgconfig${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}"

if [[ "${1:-}" == "--clean" ]]; then
  rm -rf "$BUILD" "$PREFIX"
fi
mkdir -p "$SRC" "$BUILD" "$PREFIX"

fetch() { # name url tag — re-clones (and forces a rebuild) when the tag changes
  local stamp="$SRC/$1.tag"
  if [[ -d "$SRC/$1" && "$(cat "$stamp" 2>/dev/null)" != "$3" ]]; then
    echo "==> $1: switching to $3"
    rm -rf "${SRC:?}/$1" "${BUILD:?}/$1"
    rm -f "$PREFIX/lib/libmpv.a" "$PREFIX/lib/libavcodec.a"
  fi
  if [[ ! -d "$SRC/$1" ]]; then
    git -c advice.detachedHead=false clone -q --depth 1 --branch "$3" "$2" "$SRC/$1"
    echo "$3" > "$stamp"
  fi
}
fetch ffmpeg https://github.com/FFmpeg/FFmpeg.git "$FFMPEG_TAG"
fetch mpv https://github.com/mpv-player/mpv.git "$MPV_TAG"

EXTRA_CFLAGS=""
EXTRA_LDFLAGS=""
if [[ -n "${TP_SYSROOT:-}" ]]; then
  EXTRA_CFLAGS="-I$TP_SYSROOT/usr/include"
  EXTRA_LDFLAGS="-L$TP_SYSROOT/usr/lib64"
fi

# ---------------------------------------------------------------- FFmpeg
if [[ ! -f "$PREFIX/lib/libavcodec.a" ]]; then
  echo "==> building FFmpeg $FFMPEG_TAG"
  mkdir -p "$BUILD/ffmpeg"
  (
    cd "$BUILD/ffmpeg"
    "$SRC/ffmpeg/configure" \
      --prefix="$PREFIX" \
      --pkg-config-flags=--static \
      --extra-cflags="$EXTRA_CFLAGS" \
      --extra-ldflags="$EXTRA_LDFLAGS" \
      --enable-static --disable-shared --enable-pic \
      --disable-programs --disable-doc --disable-debug \
      --enable-gpl --enable-version3 \
      --disable-avdevice --disable-indevs --disable-outdevs \
      --enable-network --enable-openssl \
      --enable-libdav1d --enable-libxml2 --enable-zlib \
      --enable-vaapi --enable-libdrm \
      --disable-vulkan --disable-cuda --disable-cuvid --disable-nvenc \
      --disable-nvdec --disable-ffnvcodec --disable-amf --disable-vdpau \
      --disable-xlib --disable-libxcb --disable-sdl2 \
      --disable-lzma --disable-bzlib \
      --disable-encoders --enable-encoder=png,mjpeg \
      >"$BUILD/ffmpeg-configure.log"
    make -j"$JOBS" >"$BUILD/ffmpeg-make.log" 2>&1
    make install >/dev/null
  )
fi

# ------------------------------------------------------------------- mpv
if [[ ! -f "$PREFIX/lib/libmpv.a" ]]; then
  echo "==> building mpv $MPV_TAG"
  rm -rf "$BUILD/mpv"
  meson setup "$BUILD/mpv" "$SRC/mpv" \
    --prefix="$PREFIX" --libdir=lib --buildtype=release \
    -Dc_args="$EXTRA_CFLAGS" -Dc_link_args="$EXTRA_LDFLAGS" \
    -Ddefault_library=static \
    -Dlibmpv=true -Dcplayer=false -Dgpl=true \
    -Dgl=enabled -Dplain-gl=enabled -Degl=enabled \
    -Ddrm=enabled -Dvaapi=enabled -Dvaapi-drm=enabled \
    -Dx11=disabled -Dwayland=disabled -Dvulkan=disabled \
    -Ddmabuf-wayland=disabled -Dvdpau=disabled \
    -Dpipewire=enabled -Dpulse=enabled -Dalsa=enabled \
    -Dlua=disabled -Djavascript=disabled -Dlibarchive=disabled \
    -Dlibavdevice=disabled -Drubberband=disabled -Dzimg=disabled \
    -Duchardet=enabled -Dlcms2=enabled \
    -Dcdda=disabled -Ddvdnav=disabled -Dlibbluray=disabled \
    -Dmanpage-build=disabled -Dhtml-build=disabled -Dpdf-build=disabled \
    >"$BUILD/mpv-configure.log"
  meson compile -C "$BUILD/mpv" -j "$JOBS" >"$BUILD/mpv-make.log" 2>&1
  meson install -C "$BUILD/mpv" >/dev/null
fi

echo "==> media engine ready in $PREFIX"
pkg-config --modversion libavcodec mpv | paste -sd' ' | sed 's/^/    libavcodec + mpv: /'
