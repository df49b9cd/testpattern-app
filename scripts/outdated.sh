#!/usr/bin/env bash
# Dependency currency audit (WORKLOG.md "Dependency policy"): compares every
# direct crate in src-tauri/Cargo.toml with the newest stable release on
# crates.io, then runs `bun outdated`, `rustup check` and compares every
# media-engine tag in scripts/build-media.sh with upstream. Exit code 1
# when something is behind (except the documented GTK3 crate pins).
#
#   scripts/outdated.sh
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"

echo "==> crates (src-tauri/Cargo.toml → crates.io)"
crates=0
python3 - "$ROOT/src-tauri/Cargo.toml" <<'EOF' || crates=1
import json, re, sys, tomllib, urllib.request

# Pinned on purpose: must match what webkit2gtk/Tauri are built on
# (see the comment in Cargo.toml and WORKLOG.md "Dependency policy").
PINNED = {"gtk", "gdk", "glib", "cairo-rs", "javascriptcore-rs"}

manifest = tomllib.load(open(sys.argv[1], "rb"))
deps = {}
for table in ("dependencies", "build-dependencies"):
    deps.update(manifest.get(table, {}))
for target in manifest.get("target", {}).values():
    deps.update(target.get("dependencies", {}))

def latest(name):
    req = urllib.request.Request(
        f"https://crates.io/api/v1/crates/{name}",
        headers={"User-Agent": "testpattern-dependency-audit (local script)"},
    )
    with urllib.request.urlopen(req, timeout=20) as r:
        return json.load(r)["crate"]["max_stable_version"]

def compatible(req, version):
    """Does the caret requirement `req` accept `version`?"""
    want = [int(x) for x in re.findall(r"\d+", req)]
    have = [int(x) for x in re.findall(r"\d+", version)][:3]
    # the first non-zero component (plus everything before it) must match
    for i, w in enumerate(want):
        if have[i] != w:
            return False
        if w != 0:
            return True
    return True

behind = 0
for name, spec in sorted(deps.items()):
    req = spec if isinstance(spec, str) else spec.get("version", "")
    try:
        v = latest(name)
    except Exception as e:  # network hiccup: report, don't fail the audit
        print(f"  ?  {name:30} {req:10} (lookup failed: {e})")
        continue
    ok = compatible(req, v)
    mark = "ok" if ok else ("pin" if name in PINNED else "OLD")
    if mark == "OLD":
        behind += 1
    print(f"  {mark:3} {name:30} {req:10} latest {v}")
sys.exit(1 if behind else 0)
EOF

echo "==> npm (bun outdated)"
(cd "$ROOT" && bun outdated 2>&1 | grep -v '^\[' || true)

echo "==> toolchain"
echo "  pinned: $(grep channel "$ROOT/rust-toolchain.toml")"
rustup check 2>/dev/null | grep -E '^stable' || true

echo "==> media engine (scripts/build-media.sh)"
media=0
while read -r var repo pattern; do
  pinned=$(sed -n "s/^$var=//p" "$ROOT/scripts/build-media.sh")
  newest=$(git ls-remote --tags "$repo" 2>/dev/null | grep -v '\^{}' | sed 's#.*refs/tags/##' | grep -E "$pattern" | sort -V | tail -1)
  mark=ok
  [[ -n "$newest" && "$pinned" != "$newest" ]] && { mark=OLD; media=1; }
  printf '  %-3s %-20s %-10s latest %s\n' "$mark" "$var" "$pinned" "${newest:-?}"
done <<'EOF'
FFMPEG_TAG https://github.com/FFmpeg/FFmpeg ^n[0-9]+\.[0-9]+(\.[0-9]+)?$
MPV_TAG https://github.com/mpv-player/mpv ^v[0-9]+\.[0-9]+\.[0-9]+$
DAV1D_TAG https://code.videolan.org/videolan/dav1d.git ^[0-9]+\.[0-9]+\.[0-9]+$
LIBXML2_TAG https://gitlab.gnome.org/GNOME/libxml2.git ^v[0-9]+\.[0-9]+\.[0-9]+$
LIBPLACEBO_TAG https://code.videolan.org/videolan/libplacebo.git ^v[0-9]+\.[0-9]+\.[0-9]+$
LIBDISPLAYINFO_TAG https://gitlab.freedesktop.org/emersion/libdisplay-info.git ^[0-9]+\.[0-9]+\.[0-9]+$
EOF
exit $((crates | media))
