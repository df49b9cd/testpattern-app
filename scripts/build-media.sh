#!/usr/bin/env bash
# Builds the embedded media engine into third_party/prefix, all static:
# FFmpeg (full native decoders) and libmpv (render API over OpenGL), plus the
# libraries whose shared-library names change between distro releases —
# dav1d, libxml2, libplacebo, libdisplay-info — so the executable runs on
# other distros (WORKLOG T-036).
#
# The Tauri binary links all of it statically (src-tauri/build.rs), so
# playback never depends on the distro's codec situation (e.g. Fedora's
# ffmpeg-free has no H.264/HEVC). Only long-stable system libraries (glibc,
# GTK/WebKit, libass, libva, pipewire/pulse/alsa, EGL/drm, OpenSSL, …) stay
# dynamic.
#
# Usage: scripts/build-media.sh [--clean]
# TP_MEDIA_PREFIX / TP_MEDIA_BUILD put the result and the build tree elsewhere
# (scripts/ubuntu-build.sh keeps its container build apart from this host's);
# TP_JOBS limits the parallel compile jobs.
set -euo pipefail

FFMPEG_TAG=n9.0.2
MPV_TAG=v0.41.0
DAV1D_TAG=1.5.4
LIBXML2_TAG=v2.15.4
LIBPLACEBO_TAG=v7.360.1
LIBDISPLAYINFO_TAG=0.4.0

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TP="$ROOT/third_party"
SRC="$TP/src"
BUILD="${TP_MEDIA_BUILD:-$TP/build}"
PREFIX="${TP_MEDIA_PREFIX:-$TP/prefix}"
JOBS="${TP_JOBS:-$(nproc 2>/dev/null || sysctl -n hw.ncpu)}"

[[ -f "$ROOT/.deps/env.sh" ]] && . "$ROOT/.deps/env.sh"
export PKG_CONFIG_PATH="$PREFIX/lib/pkgconfig${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}"

if [[ "${1:-}" == "--clean" ]]; then
  rm -rf "$BUILD" "$PREFIX"
fi
mkdir -p "$SRC" "$BUILD" "$PREFIX/lib"

# Each component's marker (its installed static library) and the markers of
# everything linking it: rebuilding a component rebuilds its dependents.
declare -A MARKER=(
  [dav1d]=libdav1d.a [libxml2]=libxml2.a [ffmpeg]=libavcodec.a
  [libplacebo]=libplacebo.a [libdisplay-info]=libdisplay-info.a [mpv]=libmpv.a
)
declare -A DEPENDENTS=(
  [dav1d]="ffmpeg mpv" [libxml2]="ffmpeg mpv" [ffmpeg]="mpv"
  [libplacebo]="mpv" [libdisplay-info]="mpv" [mpv]=""
)
invalidate() { rm -f "$PREFIX/lib/${MARKER[$1]}"; }
# Options live in this script: when it changes, everything is rebuilt, so a
# build never keeps features it no longer asks for (see the end).
STAMP="$PREFIX/.build-media.sha256"
SCRIPT_SUM="$(sha256sum "$0" | cut -d' ' -f1)"
if [[ "$(cat "$STAMP" 2>/dev/null)" != "$SCRIPT_SUM" ]]; then
  for c in "${!MARKER[@]}"; do invalidate "$c"; done
fi
# after (re)building a component, its dependents are stale
rebuilt() { for d in ${DEPENDENTS[$1]}; do invalidate "$d"; done; }

fetch() { # name url tag [submodules] — re-fetches (and forces a rebuild) when the tag changes
  local name=$1 url=$2 tag=$3 submodules=${4:-} stamp="$SRC/$1.tag" dir="$SRC/$1"
  if [[ -d "$dir" && "$(cat "$stamp" 2>/dev/null)" != "$tag" ]]; then
    echo "==> $name: switching to $tag"
    rm -rf "${SRC:?}/$name" "${BUILD:?}/$name"
    invalidate "$name"
  fi
  if [[ ! -d "$dir" ]]; then
    # Only the tag's commit, and no local tag ref: `clone --depth 1 --branch
    # <annotated tag>` makes git ≥ 2.55 warn "refs/tags/… is not a commit!"
    git init -q "$dir"
    git -C "$dir" remote add origin "$url"
    git -C "$dir" fetch -q --depth 1 --no-tags origin "refs/tags/$tag"
    git -C "$dir" -c advice.detachedHead=false checkout -q FETCH_HEAD
    if [[ -n "$submodules" ]]; then
      git -C "$dir" submodule -q update --init --recursive --depth 1
    fi
    echo "$tag" > "$stamp"
    invalidate "$name"
  fi
}
fetch dav1d https://code.videolan.org/videolan/dav1d.git "$DAV1D_TAG"
fetch libxml2 https://gitlab.gnome.org/GNOME/libxml2.git "$LIBXML2_TAG"
fetch ffmpeg https://github.com/FFmpeg/FFmpeg.git "$FFMPEG_TAG"
fetch libplacebo https://code.videolan.org/videolan/libplacebo.git "$LIBPLACEBO_TAG" submodules
fetch libdisplay-info https://gitlab.freedesktop.org/emersion/libdisplay-info.git "$LIBDISPLAYINFO_TAG"
fetch mpv https://github.com/mpv-player/mpv.git "$MPV_TAG"

# License texts of everything linked into the executable, kept in the repo
# for THIRD_PARTY_NOTICES.md (scripts/notices.py) — refreshed from the exact
# sources built here, so a tag bump shows up as a diff.
LIC="$ROOT/packaging/licenses"
mkdir -p "$LIC"
cp "$SRC/ffmpeg/LICENSE.md" "$LIC/ffmpeg.md"
cp "$SRC/mpv/Copyright" "$LIC/mpv.txt"
cp "$SRC/dav1d/COPYING" "$LIC/dav1d.txt"
cp "$SRC/libxml2/Copyright" "$LIC/libxml2.txt"
cp "$SRC/libplacebo/LICENSE" "$LIC/libplacebo.txt"
cp "$SRC/libplacebo/3rdparty/fast_float/LICENSE-MIT" "$LIC/fast_float.txt"
cp "$SRC/libplacebo/3rdparty/glad/LICENSE" "$LIC/glad.txt"
cp "$SRC/libdisplay-info/LICENSE" "$LIC/libdisplay-info.txt"

# Our prefix first: the rootless sysroot carries the distro's headers of the
# same libraries (possibly other versions) and must not shadow ours.
EXTRA_CFLAGS="-I$PREFIX/include"
EXTRA_LDFLAGS="-L$PREFIX/lib"
if [[ -n "${TP_SYSROOT:-}" ]]; then
  EXTRA_CFLAGS="$EXTRA_CFLAGS -I$TP_SYSROOT/usr/include"
  EXTRA_LDFLAGS="$EXTRA_LDFLAGS -L$TP_SYSROOT/usr/lib64"
fi

built() { [[ -f "$PREFIX/lib/${MARKER[$1]}" ]]; }

meson_static() { # name [meson options...] — static meson build into $PREFIX
  local name=$1
  shift
  echo "==> building $name $(cat "$SRC/$name.tag")"
  rm -rf "${BUILD:?}/$name"
  meson setup "$BUILD/$name" "$SRC/$name" \
    --prefix="$PREFIX" --libdir=lib --buildtype=release -Ddefault_library=static \
    -Dc_args="$EXTRA_CFLAGS" -Dc_link_args="$EXTRA_LDFLAGS" "$@" \
    >"$BUILD/$name-configure.log"
  meson compile -C "$BUILD/$name" -j "$JOBS" >"$BUILD/$name-make.log" 2>&1
  meson install -C "$BUILD/$name" >/dev/null
  rebuilt "$name"
}

# ------------------------------------------------ FFmpeg's static deps
built dav1d || meson_static dav1d \
  -Denable_tools=false -Denable_tests=false -Denable_examples=false -Denable_docs=false

# only FFmpeg's DASH demuxer uses libxml2: parser + tree, nothing else
built libxml2 || meson_static libxml2 \
  -Dpython=disabled -Dicu=disabled -Dzlib=disabled -Dreadline=disabled -Dhistory=disabled \
  -Dhttp=disabled -Dmodules=disabled -Ddocs=disabled -Dcatalog=disabled -Dschematron=disabled

# ---------------------------------------------------------------- FFmpeg
# VA-API/libdrm (and mpv's drm/libplacebo-GL plumbing) are Linux-only; on
# macOS FFmpeg auto-detects VideoToolbox and mpv's gl-cocoa covers the render
# API (the app's own NSOpenGLView drives it — no cocoa-cb/swift build).
if [ "$(uname)" = "Darwin" ]; then
  FFMPEG_HW=(--enable-videotoolbox --enable-audiotoolbox)
  PLACEBO_PLAT=()
  MPV_PLAT=(-Dgl=enabled -Dgl-cocoa=enabled -Dcocoa=enabled -Dcoreaudio=enabled -Davfoundation=enabled \
    -Ddrm=disabled -Degl=disabled -Dvaapi=disabled -Dvaapi-drm=disabled \
    -Dpipewire=disabled -Dpulse=disabled -Dalsa=disabled -Daudiounit=disabled \
    -Dmacos-cocoa-cb=disabled -Dswift-build=enabled)
else
  FFMPEG_HW=(--enable-vaapi --enable-libdrm)
  PLACEBO_PLAT=(-Ddrm=enabled -Dvaapi=enabled -Dvaapi-drm=enabled)
  MPV_PLAT=(-Dgl=enabled -Dplain-gl=enabled -Degl=enabled \
    -Ddrm=enabled -Dvaapi=enabled -Dvaapi-drm=enabled \
    -Dpipewire=enabled -Dpulse=enabled -Dalsa=enabled)
fi

if ! built ffmpeg; then
  echo "==> building FFmpeg $FFMPEG_TAG"
  rm -rf "$BUILD/ffmpeg"
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
      "${FFMPEG_HW[@]}" \
      --disable-vulkan --disable-cuda --disable-cuvid --disable-nvenc \
      --disable-nvdec --disable-ffnvcodec --disable-amf --disable-vdpau \
      --disable-xlib --disable-libxcb --disable-sdl2 \
      --disable-lzma --disable-bzlib \
      --disable-encoders --enable-encoder=png,mjpeg \
      >"$BUILD/ffmpeg-configure.log"
    make -j"$JOBS" >"$BUILD/ffmpeg-make.log" 2>&1
    make install >/dev/null
  )
  rebuilt ffmpeg
fi

# --------------------------------------------------- mpv's static deps
built libplacebo || meson_static libplacebo \
  -Dvulkan=disabled -Dopengl=enabled -Dd3d11=disabled -Dglslang=disabled -Dshaderc=disabled \
  -Dlcms=enabled -Ddovi=enabled -Dlibdovi=disabled -Dunwind=disabled -Dxxhash=disabled \
  -Ddemos=false -Dtests=false -Dbench=false -Dfuzz=false \
  "${PLACEBO_PLAT[@]}"

# needed by mpv's drm feature, which VA-API decoding requires (no X11/Wayland here)
if [ "$(uname)" != "Darwin" ]; then
  built libdisplay-info || meson_static libdisplay-info
fi

# ------------------------------------------------------------------- mpv
if ! built mpv; then
  echo "==> building mpv $MPV_TAG"
  rm -rf "$BUILD/mpv"
  meson setup "$BUILD/mpv" "$SRC/mpv" \
    --prefix="$PREFIX" --libdir=lib --buildtype=release \
    -Dc_args="$EXTRA_CFLAGS" -Dc_link_args="$EXTRA_LDFLAGS" \
    -Ddefault_library=static \
    -Dlibmpv=true -Dcplayer=false -Dgpl=true \
    -Dx11=disabled -Dwayland=disabled -Dvulkan=disabled \
    -Ddmabuf-wayland=disabled -Dvdpau=disabled \
    "${MPV_PLAT[@]}" \
    -Dlua=disabled -Djavascript=disabled -Dlibarchive=disabled \
    -Dlibavdevice=disabled -Drubberband=disabled -Dzimg=disabled \
    -Duchardet=enabled -Dlcms2=enabled -Djpeg=disabled \
    -Dcdda=disabled -Ddvdnav=disabled -Dlibbluray=disabled \
    -Dmanpage-build=disabled -Dhtml-build=disabled -Dpdf-build=disabled \
    >"$BUILD/mpv-configure.log"
  meson compile -C "$BUILD/mpv" -j "$JOBS" >"$BUILD/mpv-make.log" 2>&1
  # libmpv.a is only usable if it holds the mpv_* symbols; mpv's Swift
  # step writes osdep/mac/swift.o as its own ar archive, which `ar rcs`
  # then stores as a 0-byte member, and the stub passes ninja's mtime test
  # on a re-run. Detect and redo from the object list of the STATIC_LINKER
  # rule with `libtool -static` (which understands nested archives).
  if ! nm "$BUILD/mpv/libmpv.a" 2>/dev/null | grep -q _mpv_create; then
    echo "==> libmpv.a stub detected; re-archiving with libtool (swift.o archive content)"
    objs=$(find "$BUILD/mpv/libmpv.a.p" -name '*.o' -o -name '*.m.o')
    libtool -static -o "$BUILD/mpv/libmpv.a" "$BUILD/mpv/osdep/mac/swift.o" $objs
  fi
  meson install -C "$BUILD/mpv" >/dev/null
fi

echo "$SCRIPT_SUM" >"$STAMP"
echo "==> media engine ready in $PREFIX"
if [ "$(uname)" = "Darwin" ]; then
  PKGCFG_MODS="libavcodec mpv libplacebo dav1d libxml-2.0"
else
  PKGCFG_MODS="libavcodec mpv libplacebo dav1d libxml-2.0 libdisplay-info"
fi
pkg-config --modversion $PKGCFG_MODS | paste -sd' ' - |
  sed "s/^/    $PKGCFG_MODS: /"
