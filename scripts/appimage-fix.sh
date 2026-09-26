#!/usr/bin/env bash
# Repairs the AppImage Tauri builds (WORKLOG T-049). linuxdeploy bundles the
# build system's libwayland-* as dependencies of GTK; on a desktop with a newer
# Mesa, its EGL driver then loads against those older libraries and fails
# ("Could not create default EGL display: EGL_BAD_PARAMETER"), so the web
# view stays black — the crash behind the GDK_BACKEND=x11 that Tauri's GTK hook
# forces (tauri-apps/tauri#8541), which in turn costs GPU zero-copy. This drops
# the bundled Wayland libraries (every desktop has its own) and the forced X11
# backend, and repacks the image onto its original runtime.
#
#   scripts/appimage-fix.sh <file.AppImage>    (needs squashfs-tools)
set -euo pipefail
img="$(readlink -f "$1")"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
unset APPIMAGE_EXTRACT_AND_RUN
offset="$("$img" --appimage-offset)"
(cd "$tmp" && "$img" --appimage-extract >/dev/null)
root="$tmp/squashfs-root"
hook="$root/apprun-hooks/linuxdeploy-plugin-gtk.sh"

rm -f "$root"/usr/lib/libwayland-{client,cursor,egl,server}.so*
sed -i 's|^export GDK_BACKEND=x11\b.*|# GDK_BACKEND: whatever the desktop uses (scripts/appimage-fix.sh)|' "$hook"
if grep -q '^export GDK_BACKEND=' "$hook" || compgen -G "$root/usr/lib/libwayland-*" >/dev/null; then
  echo "appimage-fix: the GTK hook or the bundled libraries changed — check $hook" >&2
  exit 1
fi

comp="$(unsquashfs -s -o "$offset" "$img" | sed -n 's/^Compression //p')"
head -c "$offset" "$img" >"$tmp/fixed.AppImage"
mksquashfs "$root" "$tmp/image.squashfs" -root-owned -noappend -comp "${comp:-gzip}" -quiet
cat "$tmp/image.squashfs" >>"$tmp/fixed.AppImage"
chmod 755 "$tmp/fixed.AppImage"
mv "$tmp/fixed.AppImage" "$img"
echo "==> $(basename "$img"): bundled libwayland dropped, desktop's GDK backend kept (${comp:-gzip})"
