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
# On Darwin the same prefix also carries the macOS static tail (PL-110):
# OpenSSL, FreeType, FriBidi, HarfBuzz, libunibreak, libass, lcms2 and
# uchardet — the libraries the app used to take as Homebrew dylibs and
# everything those pull in — so the .dmg's executable links nothing outside
# Apple's system (src-tauri/build.rs links them statically there). On Linux
# these stay the distro's shared libraries and are not built at all.
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
# macOS static tail (PL-110): Darwin-only, everything the app used to take
# from Homebrew as shared libraries, plus their own dependencies — so the
# .dmg links nothing outside Apple's system libraries.
OPENSSL_TAG=openssl-3.6.5
FREETYPE_TAG=VER-2-14-3
FRIBIDI_TAG=v1.0.17
HARFBUZZ_TAG=14.6.0
LIBASS_TAG=0.17.5
LCMS2_TAG=lcms2.19.1
UCHARDET_TAG=v0.0.8
# libunibreak's git checkout needs autoreconf; its release tarball ships the
# generated configure, so it is fetched as a checksum-pinned tarball instead
# (the tag matches upstream's git tag).
LIBUNIBREAK_TAG=libunibreak_8_0
LIBUNIBREAK_URL=https://github.com/adah1972/libunibreak/releases/download/libunibreak_8_0/libunibreak-8.0.tar.gz
LIBUNIBREAK_SHA256=9c4fad6e517338a098373acc9f35579ae2c325e6446666fb9ac2666ba15ceba4

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TP="$ROOT/third_party"
SRC="$TP/src"
BUILD="${TP_MEDIA_BUILD:-$TP/build}"
PREFIX="${TP_MEDIA_PREFIX:-$TP/prefix}"
JOBS="${TP_JOBS:-$(nproc 2>/dev/null || sysctl -n hw.ncpu || echo 4)}"

[[ -f "$ROOT/.deps/env.sh" ]] && . "$ROOT/.deps/env.sh"
export PKG_CONFIG_PATH="$PREFIX/lib/pkgconfig${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}"

# Binaries must run on the oldest supported macOS (tauri.conf.json
# bundle.macOS.minimumSystemVersion): clang, meson, cmake and OpenSSL's
# Configure all take their deployment target from this. Rust's own macOS
# floor (11.0) is lower, so the effective minimum stays 13.0.
if [ "$(uname)" = "Darwin" ]; then
  export MACOSX_DEPLOYMENT_TARGET="${MACOSX_DEPLOYMENT_TARGET:-13.0}"
fi

if [[ "${1:-}" == "--clean" ]]; then
  rm -rf "$BUILD" "$PREFIX"
fi
mkdir -p "$SRC" "$BUILD" "$PREFIX/lib"

# Each component's marker (its installed static library) and the markers of
# everything linking it: rebuilding a component rebuilds its dependents.
declare -A MARKER=(
  [dav1d]=libdav1d.a [libxml2]=libxml2.a [ffmpeg]=libavcodec.a
  [libplacebo]=libplacebo.a [libdisplay-info]=libdisplay-info.a [mpv]=libmpv.a
  [openssl]=libssl.a [freetype]=libfreetype.a [fribidi]=libfribidi.a
  [harfbuzz]=libharfbuzz.a [libunibreak]=libunibreak.a [libass]=libass.a
  [lcms2]=liblcms2.a [uchardet]=libuchardet.a
)
declare -A DEPENDENTS=(
  [dav1d]="ffmpeg mpv" [libxml2]="ffmpeg mpv" [ffmpeg]="mpv"
  [libplacebo]="mpv" [libdisplay-info]="mpv" [mpv]=""
  [openssl]="ffmpeg mpv" [freetype]="libass" [fribidi]="libass"
  [harfbuzz]="libass" [libunibreak]="libass" [libass]="mpv"
  [lcms2]="mpv libplacebo" [uchardet]="mpv"
)
invalidate() { rm -f "$PREFIX/lib/${MARKER[$1]}"; }
# Options live in this script: when it changes, everything is rebuilt, so a
# build never keeps features it no longer asks for (see the end).
# (macOS has no sha256sum; shasum ships with the OS.)
sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | cut -d' ' -f1
  else shasum -a 256 "$1" | cut -d' ' -f1; fi
}
STAMP="$PREFIX/.build-media.sha256"
SCRIPT_SUM="$(sha256_of "$0")"
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
fetch_tar() { # name url sha256 tag tarball — verified release tarball → $SRC/$name
  local name=$1 url=$2 sha=$3 tag=$4 file="$SRC/$5" stamp="$SRC/$1.tag" dir="$SRC/$1"
  if [[ -d "$dir" && "$(cat "$stamp" 2>/dev/null)" != "$tag" ]]; then
    echo "==> $name: switching to $tag"
    rm -rf "${SRC:?}/$name" "$file"
    invalidate "$name"
  fi
  if [[ ! -d "$dir" ]]; then
    curl -fsSL -o "$file" "$url"
    [[ "$(sha256_of "$file")" == "$sha" ]] || { echo "==> $name: sha256 mismatch for $file" >&2; exit 1; }
    mkdir -p "$dir"
    tar -xf "$file" -C "$dir" --strip-components=1
    echo "$tag" >"$stamp"
    invalidate "$name"
  fi
}
fetch dav1d https://code.videolan.org/videolan/dav1d.git "$DAV1D_TAG"
fetch libxml2 https://gitlab.gnome.org/GNOME/libxml2.git "$LIBXML2_TAG"
fetch ffmpeg https://github.com/FFmpeg/FFmpeg.git "$FFMPEG_TAG"
fetch libplacebo https://code.videolan.org/videolan/libplacebo.git "$LIBPLACEBO_TAG" submodules
fetch libdisplay-info https://gitlab.freedesktop.org/emersion/libdisplay-info.git "$LIBDISPLAYINFO_TAG"
fetch mpv https://github.com/mpv-player/mpv.git "$MPV_TAG"

# Darwin-only sources of the static tail
if [ "$(uname)" = Darwin ]; then
  fetch openssl https://github.com/openssl/openssl.git "$OPENSSL_TAG"
  fetch freetype https://github.com/freetype/freetype.git "$FREETYPE_TAG"
  fetch fribidi https://github.com/fribidi/fribidi.git "$FRIBIDI_TAG"
  fetch harfbuzz https://github.com/harfbuzz/harfbuzz.git "$HARFBUZZ_TAG"
  fetch libass https://github.com/libass/libass.git "$LIBASS_TAG"
  fetch lcms2 https://github.com/mm2/Little-CMS.git "$LCMS2_TAG"
  fetch uchardet https://gitlab.freedesktop.org/uchardet/uchardet.git "$UCHARDET_TAG"
  fetch_tar libunibreak "$LIBUNIBREAK_URL" "$LIBUNIBREAK_SHA256" "$LIBUNIBREAK_TAG" libunibreak-8.0.tar.gz
fi

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

# the static tail's licenses (also committed: Linux builds don't fetch these
# sources, but THIRD_PARTY_NOTICES.md covers both platforms)
if [ "$(uname)" = Darwin ]; then
  cp "$SRC/openssl/LICENSE.txt" "$LIC/openssl.txt"
  cp "$SRC/freetype/docs/FTL.TXT" "$LIC/freetype.txt"
  cp "$SRC/fribidi/COPYING" "$LIC/fribidi.txt"
  cp "$SRC/harfbuzz/COPYING" "$LIC/harfbuzz.txt"
  cp "$SRC/libunibreak/LICENCE" "$LIC/libunibreak.txt"
  cp "$SRC/libass/COPYING" "$LIC/libass.txt"
  cp "$SRC/lcms2/LICENSE" "$LIC/lcms2.txt"
  cp "$SRC/uchardet/COPYING" "$LIC/uchardet.txt"
fi

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
  # stdout of meson setup is chatty; its error text goes to stderr AND to the
  # configure.log. CI swallows the former — keep a copy visible on failure.
  meson setup "$BUILD/$name" "$SRC/$name" \
    --prefix="$PREFIX" --libdir=lib --buildtype=release -Ddefault_library=static \
    -Dc_args="$EXTRA_CFLAGS" -Dc_link_args="$EXTRA_LDFLAGS" "$@" \
    >"$BUILD/$name-configure.log" 2>&1
  local rc=$?
  if [[ $rc -ne 0 ]]; then
    echo "==> $name configure FAILED (rc=$rc), meson log follows:" >&2
    tail -60 "$BUILD/$name-configure.log" >&2 || true
    echo "==> end $name configure failure" >&2
    return $rc
  fi
  meson compile -C "$BUILD/$name" -j "$JOBS" >"$BUILD/$name-make.log" 2>&1
  local rc=$?
  if [[ $rc -ne 0 ]]; then
    echo "==> $name build FAILED (rc=$rc); logs follow:" >&2
    tail -40 "$BUILD/$name-make.log" >&2 || true
    echo "==> end $name build failure" >&2
    return $rc
  fi
  meson install -C "$BUILD/$name" >/dev/null
  rebuilt "$name"
}

fail_dump() { # component — tail its build logs and give up
  echo "==> $1 FAILED — log tails follow:" >&2
  local l
  for l in "$BUILD/$1"-*.log; do
    [[ -f "$l" ]] && { echo "--- $l" >&2; tail -n 30 "$l" >&2; }
  done
  exit 1
}

# ------------------------------------------------ FFmpeg's static deps
built dav1d || meson_static dav1d \
  -Denable_tools=false -Denable_tests=false -Denable_examples=false -Denable_docs=false

# only FFmpeg's DASH demuxer uses libxml2: parser + tree, nothing else
built libxml2 || meson_static libxml2 \
  -Dpython=disabled -Dicu=disabled -Dzlib=disabled -Dreadline=disabled -Dhistory=disabled \
  -Dhttp=disabled -Dmodules=disabled -Ddocs=disabled -Dcatalog=disabled -Dschematron=disabled

# ------------------------------------------- macOS static tail (PL-110)
# OpenSSL, libass, lcms2 and uchardet — the libraries the macOS build used to
# take as Homebrew dylibs — and everything libass pulls in (FreeType, FriBidi,
# HarfBuzz, libunibreak), all static into the prefix, before FFmpeg (which
# needs the static OpenSSL for HTTPS). src-tauri/build.rs links exactly this
# set on macOS; Linux keeps taking these from the distro, dynamically.
if [ "$(uname)" = Darwin ]; then
  # OpenSSL has its own configure; system perl is enough — it vendors the
  # Text::Template module it needs. install_dev = libraries + headers + .pc.
  if ! built openssl; then
    echo "==> building OpenSSL $OPENSSL_TAG"
    rm -rf "$BUILD/openssl"; mkdir -p "$BUILD/openssl"
    (
      cd "$BUILD/openssl"
      case "$(uname -m)" in
        arm64) target=darwin64-arm64-cc ;;
        *) target=darwin64-x86_64-cc ;;
      esac
      "$SRC/openssl/Configure" --prefix="$PREFIX" --libdir=lib \
        no-shared no-tests no-apps no-docs "$target" \
        >"$BUILD/openssl-configure.log"
      make -j"$JOBS" build_libs >"$BUILD/openssl-make.log" 2>&1
      make install_dev >/dev/null
    ) || fail_dump openssl
    rebuilt openssl
  fi

  # FreeType: plain glyph rendering for libass — no compressed-font formats
  # (zlib/png/bzip2/brotli all off), no harfbuzz autohint integration.
  built freetype || meson_static freetype \
    -Dbrotli=disabled -Dbzip2=disabled -Dpng=disabled -Dzlib=disabled -Dharfbuzz=disabled

  built fribidi || meson_static fribidi -Ddocs=false -Dtests=false -Dbin=false

  # the shaper libass needs; every optional backend and utility off
  built harfbuzz || meson_static harfbuzz \
    -Dicu=disabled -Dglib=disabled -Dgobject=disabled -Dcairo=disabled -Dchafa=disabled \
    -Dpng=disabled -Dzlib=disabled -Dfreetype=disabled -Dcoretext=disabled \
    -Dtests=disabled -Dutilities=disabled -Ddocs=disabled -Dintrospection=disabled \
    -Dsubset=disabled -Dbenchmark=disabled -Draster=disabled -Dvector=disabled -Dgpu=disabled

  if ! built libunibreak; then
    echo "==> building libunibreak $LIBUNIBREAK_TAG"
    rm -rf "$BUILD/libunibreak"; mkdir -p "$BUILD/libunibreak"
    (
      cd "$BUILD/libunibreak"
      "$SRC/libunibreak/configure" --prefix="$PREFIX" --disable-shared --enable-static \
        >"$BUILD/libunibreak-configure.log"
      make -j"$JOBS" >"$BUILD/libunibreak-make.log" 2>&1
      make install >/dev/null
    ) || fail_dump libunibreak
    rebuilt libunibreak
  fi

  # CoreText provides the system fonts (fontconfig is Linux-only)
  built libass || meson_static libass \
    -Dfontconfig=disabled -Dtest=disabled -Dcompare=disabled \
    -Dprofile=disabled -Dfuzz=disabled -Dcheckasm=disabled

  if ! built lcms2; then
    echo "==> building Little CMS 2 $LCMS2_TAG"
    rm -rf "$BUILD/lcms2"
    {
      cmake -S "$SRC/lcms2" -B "$BUILD/lcms2" \
        -DCMAKE_INSTALL_PREFIX="$PREFIX" -DCMAKE_BUILD_TYPE=Release \
        -DCMAKE_OSX_DEPLOYMENT_TARGET="$MACOSX_DEPLOYMENT_TARGET" \
        -DBUILD_SHARED_LIBS=OFF -DLCMS2_BUILD_SHARED=OFF -DLCMS2_BUILD_STATIC=ON \
        -DLCMS2_BUILD_TOOLS=OFF -DLCMS2_BUILD_TESTS=OFF \
        -DLCMS2_WITH_JPEG=OFF -DLCMS2_WITH_TIFF=OFF -DLCMS2_WITH_ZLIB=OFF \
        >"$BUILD/lcms2-configure.log" 2>&1
      cmake --build "$BUILD/lcms2" -j "$JOBS" >"$BUILD/lcms2-make.log" 2>&1
      cmake --install "$BUILD/lcms2" >>"$BUILD/lcms2-make.log" 2>&1
    } || fail_dump lcms2
    rebuilt lcms2
  fi

  if ! built uchardet; then
    echo "==> building uchardet $UCHARDET_TAG"
    rm -rf "$BUILD/uchardet"
    {
      # uchardet's cmake_minimum_required(3.1) predates CMake 4's policy purge
      cmake -S "$SRC/uchardet" -B "$BUILD/uchardet" \
        -DCMAKE_INSTALL_PREFIX="$PREFIX" -DCMAKE_BUILD_TYPE=Release \
        -DCMAKE_OSX_DEPLOYMENT_TARGET="$MACOSX_DEPLOYMENT_TARGET" \
        -DCMAKE_POLICY_VERSION_MINIMUM=3.5 \
        -DBUILD_SHARED_LIBS=OFF -DBUILD_BINARY=OFF \
        >"$BUILD/uchardet-configure.log" 2>&1
      cmake --build "$BUILD/uchardet" -j "$JOBS" >"$BUILD/uchardet-make.log" 2>&1
      cmake --install "$BUILD/uchardet" >>"$BUILD/uchardet-make.log" 2>&1
    } || fail_dump uchardet
    rebuilt uchardet
  fi
fi

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
      ${FFMPEG_HW[@]+"${FFMPEG_HW[@]}"} \
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
# PLACEBO_PLAT is empty on macOS; "${arr[@]}" on bash 4.x still passes one
# empty positional, which meson rejects as an unknown option. Use "${arr[@]+...}"
# so an array that is empty expands to nothing on every supported shell.
built libplacebo || meson_static libplacebo \
  -Dvulkan=disabled -Dopengl=enabled -Dd3d11=disabled -Dglslang=disabled -Dshaderc=disabled \
  -Dlcms=enabled -Ddovi=enabled -Dlibdovi=disabled -Dunwind=disabled -Dxxhash=disabled \
  -Ddemos=false -Dtests=false -Dbench=false -Dfuzz=false ${PLACEBO_PLAT[@]+"${PLACEBO_PLAT[@]}"}

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
    ${MPV_PLAT[@]+"${MPV_PLAT[@]}"} \
    -Dlua=disabled -Djavascript=disabled -Dlibarchive=disabled \
    -Dlibavdevice=disabled -Drubberband=disabled -Dzimg=disabled \
    -Duchardet=enabled -Dlcms2=enabled -Djpeg=disabled \
    -Dcdda=disabled -Ddvdnav=disabled -Dlibbluray=disabled \
    -Dmanpage-build=disabled -Dhtml-build=disabled -Dpdf-build=disabled \
    >"$BUILD/mpv-configure.log"
  meson compile -C "$BUILD/mpv" -j "$JOBS" >"$BUILD/mpv-make.log" 2>&1
  # macOS-only: if libmpv.a ended up empty (Swift-compiled delivery of swift.o
  # being an ar archive, mimicking the statically linked but stubbed state a
  # lingering inc/Darwin cache left behind), rebuild it via libtool's archive
  # catalyst. Linux never runs this branch.
  if [ "$(uname)" = "Darwin" ] && ! nm "$BUILD/mpv/libmpv.a" 2>/dev/null | grep -q _mpv_create; then
    echo "==> libmpv.a stub detected; re-archiving with libtool (swift.o archive content)"
    mapfile -t objs < <(find "$BUILD/mpv/libmpv.a.p" -name '*.o' -o -name '*.m.o')
    libtool -static -o "$BUILD/mpv/libmpv.a" "$BUILD/mpv/osdep/mac/swift.o" "${objs[@]}"
  fi
  meson install -C "$BUILD/mpv" >/dev/null
fi

echo "$SCRIPT_SUM" >"$STAMP"
echo "==> media engine ready in $PREFIX"
if [ "$(uname)" = "Darwin" ]; then
  PKGCFG_MODS="libavcodec mpv libplacebo dav1d libxml-2.0 \
openssl freetype2 fribidi harfbuzz libunibreak libass lcms2 uchardet"
else
  PKGCFG_MODS="libavcodec mpv libplacebo dav1d libxml-2.0 libdisplay-info"
fi
pkg-config --modversion $PKGCFG_MODS | paste -sd' ' - |
  sed "s/^/    $PKGCFG_MODS: /"
