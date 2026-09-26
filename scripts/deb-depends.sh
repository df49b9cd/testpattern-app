#!/usr/bin/env bash
# Checks bundle.linux.deb.depends in src-tauri/tauri.conf.json against what
# Debian's own tooling (dpkg-shlibdeps) derives from the release executable.
# Run it on the release baseline (Ubuntu 24.04: scripts/ubuntu-build.sh, CI):
# package names and minimum versions come from that system's shlibs files.
#
#   scripts/deb-depends.sh <executable>           exit 1 when the list differs
#   scripts/deb-depends.sh <executable> --print   the derived list, as JSON
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
bin="$(readlink -f "$1")"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
mkdir -p "$tmp/debian"
printf 'Source: x\n\nPackage: x\nArchitecture: any\n' >"$tmp/debian/control"
derived="$(cd "$tmp" && dpkg-shlibdeps -O -e"$bin" 2>/dev/null | sed -n 's/^shlibs:Depends=//p')"
python3 - "$ROOT/src-tauri/tauri.conf.json" "$derived" "${2:-}" <<'EOF'
import json, sys

conf, derived, mode = sys.argv[1:4]
want = sorted(d.strip() for d in derived.split(",") if d.strip())
if not want:
    sys.exit("dpkg-shlibdeps derived nothing — is this a Debian/Ubuntu system with dpkg-dev?")
if mode == "--print":
    print(json.dumps(want, indent=2))
    sys.exit(0)
have = sorted(json.load(open(conf))["bundle"]["linux"]["deb"].get("depends", []))
if want == have:
    print(f"deb depends match dpkg-shlibdeps ({len(want)} packages)")
    sys.exit(0)
print("bundle.linux.deb.depends differs from dpkg-shlibdeps "
      "(update it from `scripts/deb-depends.sh <executable> --print`):")
for d in sorted(set(want) - set(have)):
    print("  missing:", d)
for d in sorted(set(have) - set(want)):
    print("  extra:  ", d)
sys.exit(1)
EOF
