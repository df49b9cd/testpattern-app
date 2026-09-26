#!/usr/bin/env bash
# Dependency currency audit (WORKLOG.md "Dependency policy"): compares every
# direct crate in src-tauri/Cargo.toml with the newest stable release on
# crates.io, then runs `bun outdated`, `rustup check` and the FFmpeg/mpv tag
# check. Exit code 1 when a crate is behind (except documented GTK3 pins).
#
#   scripts/outdated.sh
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"

echo "==> crates (src-tauri/Cargo.toml → crates.io)"
python3 - "$ROOT/src-tauri/Cargo.toml" <<'EOF'
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
crates=$?

echo "==> npm (bun outdated)"
(cd "$ROOT" && bun outdated 2>&1 | grep -v '^\[' || true)

echo "==> toolchain"
echo "  pinned: $(grep channel "$ROOT/rust-toolchain.toml")"
rustup check 2>/dev/null | grep -E '^stable' || true

echo "==> media engine (scripts/build-media.sh)"
grep -E '^(FFMPEG|MPV)_TAG=' "$ROOT/scripts/build-media.sh" | sed 's/^/  pinned: /'
echo "  newest FFmpeg: $(git ls-remote --tags https://github.com/FFmpeg/FFmpeg 'n*' | sed 's#.*refs/tags/##' | grep -E '^n[0-9]+\.[0-9]+(\.[0-9]+)?$' | sort -V | tail -1)"
echo "  newest mpv:    $(git ls-remote --tags https://github.com/mpv-player/mpv 'v*' | sed 's#.*refs/tags/##' | grep -E '^v[0-9]+\.[0-9]+\.[0-9]+$' | sort -V | tail -1)"
exit $crates
