#!/usr/bin/env bash
# Dependency currency audit (WORKLOG.md "Dependency policy"): compares every
# direct crate in src-tauri/Cargo.toml — its requirement *and* the version
# locked in Cargo.lock — with the newest stable release on crates.io, lists
# the transitive crates `cargo update` would move, runs `bun outdated`,
# compares the pinned Rust toolchain with `rustup check`, and compares the
# bun pinned in package.json, every media-engine tag in scripts/build-media.sh
# and every GitHub Action major in .github/workflows with upstream. Exit code
# 1 when something is behind (except the documented GTK3 crate pins).
#
#   scripts/outdated.sh
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"

echo "==> crates (src-tauri/Cargo.toml + Cargo.lock → crates.io)"
crates=0
python3 - "$ROOT/src-tauri/Cargo.toml" "$ROOT/src-tauri/Cargo.lock" <<'EOF' || crates=1
import json, re, sys, tomllib, urllib.request

# Pinned on purpose: must match what webkit2gtk/Tauri are built on
# (see the comment in Cargo.toml and WORKLOG.md "Dependency policy").
PINNED = {"gtk", "gdk", "glib", "cairo-rs", "javascriptcore-rs"}

manifest = tomllib.load(open(sys.argv[1], "rb"))
locked = {}
for p in tomllib.load(open(sys.argv[2], "rb")).get("package", []):
    locked.setdefault(p["name"], []).append(p["version"])
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

def key(version):
    return [int(x) for x in re.findall(r"\d+", version.split("+")[0].split("-")[0])][:3]

behind = 0
for name, spec in sorted(deps.items()):
    req = spec if isinstance(spec, str) else spec.get("version", "")
    # the version this build uses: the newest locked one the requirement accepts
    lock = max((v for v in locked.get(name, []) if compatible(req, v)), key=key, default="?")
    try:
        v = latest(name)
    except Exception as e:  # network hiccup: report, don't fail the audit
        print(f"  ?  {name:30} {req:10} locked {lock:10} (lookup failed: {e})")
        continue
    if not compatible(req, v):
        # the requirement excludes the newest release: Cargo.toml needs a bump
        mark = "pin" if name in PINNED else "OLD"
    elif lock == "?" or key(lock) < key(v):
        # allowed but not used yet: `cargo update -p <name>`
        mark = "upd"
    else:
        mark = "ok"
    if mark in ("OLD", "upd"):
        behind += 1
    print(f"  {mark:3} {name:30} {req:10} locked {lock:10} latest {v}")
sys.exit(1 if behind else 0)
EOF
# transitive crates within the current requirements (what `cargo update` would move)
moves=$(cd "$ROOT/src-tauri" && cargo update --dry-run 2>&1 | grep -E '^ +(Updating|Downgrading|Adding|Removing) .* v[0-9]' || true)
if [[ -n "$moves" ]]; then
  echo "  upd cargo update would change $(wc -l <<<"$moves") lock entries:"
  sed 's/^ */      /' <<<"$moves"
  crates=1
else
  echo "  ok  Cargo.lock: nothing for cargo update to move"
fi

echo "==> npm (bun outdated)"
npm_out=$(cd "$ROOT" && bun outdated 2>&1 | grep -v '^\[' || true)
echo "$npm_out"
npm=0
# any table row besides the header ("| Package | Current | …") is a package behind
awk '/^\| / && !/^\| Package /{found=1} END{exit !found}' <<<"$npm_out" && npm=1

echo "==> toolchain"
rust_old=0
rust_pinned=$(sed -n 's/^channel *= *"\(.*\)"/\1/p' "$ROOT/rust-toolchain.toml")
# rustup reports the newest stable either as "update available: A -> B" or "Up to date : B"
rust_newest=$( (rustup check 2>/dev/null || true) | sed -n 's/^stable-[^ ]* - .*\(->\|: \) *\([0-9][0-9.]*\).*/\2/p' | head -1)
mark=ok
[[ -n "$rust_newest" && "$rust_pinned" != "$rust_newest" ]] && { mark=OLD; rust_old=1; }
printf '  %-3s rust (rust-toolchain.toml) %-10s latest stable %s\n' "$mark" "$rust_pinned" "${rust_newest:-?}"
bun_old=0
bun_pinned=$(python3 -c "import json, sys; print(json.load(open(sys.argv[1])).get('packageManager', '').removeprefix('bun@'))" "$ROOT/package.json")
bun_newest=$(git ls-remote --tags https://github.com/oven-sh/bun 2>/dev/null | sed -n 's#.*refs/tags/bun-v\([0-9]*\.[0-9]*\.[0-9]*\)$#\1#p' | sort -V | tail -1)
mark=ok
[[ -n "$bun_newest" && "$bun_pinned" != "$bun_newest" ]] && { mark=OLD; bun_old=1; }
printf '  %-3s bun (package.json) %-10s latest %s — installed %s\n' "$mark" "$bun_pinned" "${bun_newest:-?}" "$(bun --version)"

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
echo "==> GitHub Actions (.github/workflows)"
actions=0
for uses in $(grep -ho 'uses: *[^ ]*@v[0-9][^ ]*' "$ROOT"/.github/workflows/*.yml | sed 's/uses: *//' | sort -u); do
  repo=${uses%@*}
  major=${uses#*@}
  newest=$(git ls-remote --tags "https://github.com/$repo" 2>/dev/null | grep -v '\^{}' | sed 's#.*refs/tags/##' |
    grep -E '^v[0-9]+\.[0-9]+\.[0-9]+$' | sort -V | tail -1)
  mark=ok
  [[ -n "$newest" && "${newest%%.*}" != "$major" ]] && { mark=OLD; actions=1; }
  printf '  %-3s %-26s %-5s latest %s\n' "$mark" "$repo" "$major" "${newest:-?}"
done
exit $((crates | npm | rust_old | media | bun_old | actions))
