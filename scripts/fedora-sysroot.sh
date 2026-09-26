#!/usr/bin/env bash
# Rootless build sysroot for Fedora.
#
# Downloads the -devel packages (headers, .pc files, .so symlinks) that the
# build needs from the configured Fedora repos and unpacks them under
# .deps/sysroot. Nothing is installed system-wide: the runtime libraries the
# final binary links against are the ones already installed in /usr/lib64.
#
# If you have root, you can skip this entirely:
#   sudo dnf install $(scripts/fedora-sysroot.sh --print-packages)
#
# Usage: scripts/fedora-sysroot.sh [--print-packages]
set -euo pipefail

PACKAGES=(
  # Tauri (wry / tao / webkit2gtk)
  gtk3-devel webkit2gtk4.1-devel libsoup3-devel javascriptcoregtk4.1-devel
  # mpv (dynamic system deps; FFmpeg + mpv themselves are built from source)
  fribidi-devel libass-devel libplacebo-devel libva-devel libdrm-devel
  pipewire-devel pulseaudio-libs-devel mesa-libEGL-devel libglvnd-devel
  wayland-devel libxkbcommon-devel lcms2-devel uchardet-devel libepoxy-devel
  libdisplay-info-devel
  # FFmpeg
  nasm openssl-devel libdav1d-devel
)

if [[ "${1:-}" == "--print-packages" ]]; then
  echo "${PACKAGES[@]}"
  exit 0
fi

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DEPS="$ROOT/.deps"
RPMS="$DEPS/rpms"
SYSROOT="$DEPS/sysroot"
mkdir -p "$RPMS" "$SYSROOT"

echo "==> downloading ${#PACKAGES[@]} packages (+ missing deps) into $RPMS"
dnf download --resolve --arch=x86_64 --arch=noarch --destdir="$RPMS" "${PACKAGES[@]}" >/dev/null

echo "==> unpacking -devel packages into $SYSROOT"
shopt -s nullglob
for rpm in "$RPMS"/*.rpm; do
  name="$(rpm -qp --qf '%{NAME}' "$rpm" 2>/dev/null)"
  case "$name" in
    *-devel|nasm) ;;
    *) continue ;; # runtime libs come from the host system
  esac
  (cd "$SYSROOT" && rpm2cpio "$rpm" | cpio -idm --quiet 2>/dev/null)
done

echo "==> relocating pkg-config files"
for pc in "$SYSROOT"/usr/lib64/pkgconfig/*.pc "$SYSROOT"/usr/share/pkgconfig/*.pc; do
  # Rewrite absolute /usr paths in variable definitions so includes resolve
  # inside the sysroot; libdir too, so the linker finds the fixed-up .so links.
  sed -i -E "s#^([A-Za-z_]+)=/usr#\1=$SYSROOT/usr#" "$pc"
done

echo "==> pointing .so dev symlinks at the host's runtime libraries"
for link in "$SYSROOT"/usr/lib64/*.so; do
  [[ -L "$link" ]] || continue
  [[ -e "$link" ]] && continue # already resolves inside the sysroot
  target="$(readlink "$link")"
  base="$(basename "$target")"
  if [[ -e "/usr/lib64/$base" ]]; then
    ln -sf "/usr/lib64/$base" "$link"
  else
    # version skew between repo headers and installed runtime: match by SONAME
    stem="$(basename "$link")"
    cand=(/usr/lib64/"$stem".[0-9]*)
    if [[ ${#cand[@]} -gt 0 && -e "${cand[0]}" ]]; then
      ln -sf "${cand[0]}" "$link"
    else
      echo "   warn: no runtime library for $(basename "$link")" >&2
    fi
  fi
done

cat > "$DEPS/env.sh" <<EOF
# Source this before building: . .deps/env.sh
export TP_SYSROOT="$SYSROOT"
export PKG_CONFIG_PATH="$ROOT/third_party/prefix/lib/pkgconfig:$SYSROOT/usr/lib64/pkgconfig:$SYSROOT/usr/share/pkgconfig\${PKG_CONFIG_PATH:+:\$PKG_CONFIG_PATH}"
export PATH="$SYSROOT/usr/bin:$DEPS/venv/bin:\$PATH"
export C_INCLUDE_PATH="$SYSROOT/usr/include\${C_INCLUDE_PATH:+:\$C_INCLUDE_PATH}"
export LIBRARY_PATH="$SYSROOT/usr/lib64\${LIBRARY_PATH:+:\$LIBRARY_PATH}"
EOF
echo "==> done. Run: . .deps/env.sh"
