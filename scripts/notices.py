#!/usr/bin/env python3
"""Writes THIRD_PARTY_NOTICES.md: everything the testpattern executable
contains or links, with licenses and the license texts that must accompany
binary distributions (MIT/BSD/Apache/OFL notices).

  scripts/notices.py           # (re)write THIRD_PARTY_NOTICES.md
  scripts/notices.py --check   # exit 1 when the file is out of date

Inputs: `cargo metadata` (crates linked into the Linux x86_64 and macOS
arm64 builds; their license files come from the cargo registry),
node_modules (packages bundled into the UI), and the upstream tags pinned in
scripts/build-media.sh (the media engine is static on both platforms; on
macOS the former Homebrew tail — OpenSSL, libass, lcms2, uchardet and
libass's own dependencies — is static too, PL-110). Needs the crate sources
(any cargo build/fetch) and `bun install`. Standard library only.
"""
import hashlib
import json
import os
import re
import subprocess
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
OUT = os.path.join(ROOT, "THIRD_PARTY_NOTICES.md")
# Both released platforms: the union of the crates each links (the Rust
# crates are resolved per platform without building anything).
TARGETS = ["x86_64-unknown-linux-gnu", "aarch64-apple-darwin"]

# For "A OR B" licenses we comply with the first offered alternative here
# (fewest obligations first).
PREFERENCE = [
    "MIT", "MIT-0", "0BSD", "ISC", "BSD-2-Clause", "BSD-3-Clause", "Zlib", "Unlicense", "CC0-1.0",
    "Apache-2.0", "Apache-2.0 WITH LLVM-exception", "BSL-1.0", "Unicode-3.0", "MPL-2.0", "OFL-1.1",
]
# how license *file names* reveal which license they hold
FILE_HINTS = {
    "MIT": "MIT", "MIT-0": "MIT", "Apache-2.0": "APACHE", "Apache-2.0 WITH LLVM-exception": "APACHE",
    "Unicode-3.0": "UNICODE", "Zlib": "ZLIB", "ISC": "ISC", "BSD-2-Clause": "BSD", "BSD-3-Clause": "BSD",
    "0BSD": "BSD", "MPL-2.0": "MPL", "Unlicense": "UNLICENSE", "CC0-1.0": "CC0", "BSL-1.0": "BOOST",
    "OFL-1.1": "OFL",
}
LICENSE_FILE_PREFIXES = ("LICENSE", "LICENCE", "COPYING", "NOTICE", "UNLICENSE")

# Standard texts for packages that ship no license file of their own.
STANDARD = {
    "MIT": """Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.""",
    "BSD-3-Clause": """Redistribution and use in source and binary forms, with or without
modification, are permitted provided that the following conditions are met:

1. Redistributions of source code must retain the above copyright notice, this
   list of conditions and the following disclaimer.

2. Redistributions in binary form must reproduce the above copyright notice,
   this list of conditions and the following disclaimer in the documentation
   and/or other materials provided with the distribution.

3. Neither the name of the copyright holder nor the names of its
   contributors may be used to endorse or promote products derived from
   this software without specific prior written permission.

THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS"
AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE
IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE
DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR CONTRIBUTORS BE LIABLE
FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL
DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR
SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER
CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY,
OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE
OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.""",
}

# Linked from the operating system at runtime, not shipped in our packages
# (licenses as stated upstream). Keep in sync with src-tauri/build.rs,
# scripts/build-media.sh and `ldd src-tauri/target/release/testpattern`.
# The four libraries that used to be here — libass, OpenSSL, Little CMS 2,
# uchardet — moved to section 1: on macOS they are statically linked now
# (PL-110), and the Linux dynamic linkage is noted in their section-1 rows.
SYSTEM_LIBS = [
    ("glibc (libc, libm)", "C runtime (Linux)", "LGPL-2.1-or-later"),
    ("libgcc_s, libstdc++", "compiler and C++ runtimes (Linux; libplacebo)", "GPL-3.0-or-later WITH GCC-exception-3.1"),
    ("GTK 3, GDK", "window and widgets (Linux)", "LGPL-2.0-or-later"),
    ("gdk-pixbuf", "image loading (Linux)", "LGPL-2.1-or-later"),
    ("GLib, GObject, GIO", "platform library (Linux)", "LGPL-2.1-or-later"),
    ("cairo", "2D drawing (Linux)", "LGPL-2.1-only OR MPL-1.1"),
    ("WebKitGTK, JavaScriptCore (API 4.1)", "web view hosting the UI (Linux)", "LGPL-2.1-only AND BSD-2-Clause (and others)"),
    ("Apple WebKit (WKWebView), JavaScriptCore", "web view hosting the UI (macOS system framework, not redistributed)", "LGPL-2.0-or-later AND BSD (as WebKitGTK)"),
    ("Apple AppKit, CoreText, CoreGraphics, CoreAnimation, OpenGL, VideoToolbox, CoreAudio, AVFoundation, Security (Keychain)", "windowing, fonts, drawing, video decode, audio, keychain (macOS system frameworks)", "Apple system frameworks, not redistributed"),
    ("libsoup 3", "networking for the web view (Linux)", "LGPL-2.0-or-later"),
    ("D-Bus", "desktop IPC (Linux)", "AFL-2.1 OR GPL-2.0-or-later"),
    ("libX11, libXfixes", "X11 support (Linux)", "MIT"),
    ("libEGL (libglvnd)", "OpenGL context for video (Linux)", "MIT-style"),
    ("libdrm, libva, libva-drm", "GPU access, VA-API decoding (Linux)", "MIT"),
    ("libSystem, libz, libiconv, libc++ (macOS)", "C runtime, compression, charset conversion, C++ runtime (libplacebo, HarfBuzz, uchardet)", "Apple PSF (libSystem); Zlib; Apache-2.0 WITH LLVM-exception (libc++)"),
    ("PipeWire client library", "audio output (mpv, Linux)", "MIT"),
    ("PulseAudio client library", "audio output (mpv, Linux)", "LGPL-2.1-or-later"),
    ("ALSA library", "audio output (mpv, Linux)", "LGPL-2.1-or-later"),
]


# ------------------------------------------------------------------ SPDX

def required_licenses(expr):
    """License ids needed to comply with an SPDX expression; each OR is
    resolved to its most preferred alternative."""
    tokens = re.findall(r"\(|\)|[A-Za-z0-9.+\-]+", expr.replace("/", " OR "))
    pos = 0

    def rank(ids):
        return (max(PREFERENCE.index(i) if i in PREFERENCE else len(PREFERENCE) for i in ids), len(ids))

    def atom():
        nonlocal pos
        if tokens[pos] == "(":
            pos += 1
            ids = or_expr()
            pos += 1  # ")"
            return ids
        name = tokens[pos]
        pos += 1
        if pos < len(tokens) and tokens[pos] == "WITH":
            name = f"{name} WITH {tokens[pos + 1]}"
            pos += 2
        return {name}

    def and_expr():
        nonlocal pos
        ids = atom()
        while pos < len(tokens) and tokens[pos] == "AND":
            pos += 1
            ids |= atom()
        return ids

    def or_expr():
        nonlocal pos
        options = [and_expr()]
        while pos < len(tokens) and tokens[pos] == "OR":
            pos += 1
            options.append(and_expr())
        return min(options, key=rank)

    return sorted(or_expr(), key=lambda i: PREFERENCE.index(i) if i in PREFERENCE else 99)


# --------------------------------------------------------- license files

def license_files(directory):
    files = []
    for name in sorted(os.listdir(directory)):
        path = os.path.join(directory, name)
        upper = name.upper()
        if os.path.isfile(path) and upper.startswith(LICENSE_FILE_PREFIXES) and not upper.endswith(".SPDX"):
            files.append(path)
    return files


def pick_files(files, ids):
    """The license files that hold the licenses `ids` (+ any NOTICE files)."""
    notices = [f for f in files if os.path.basename(f).upper().startswith("NOTICE")]
    licenses = [f for f in files if f not in notices]
    chosen = []
    for i in ids:
        hint = FILE_HINTS.get(i)
        chosen += [f for f in licenses if hint and hint in os.path.basename(f).upper()]
    if not chosen:
        # a generic LICENSE/COPYING file usually holds the whole text
        hinted = set(FILE_HINTS.values())
        generic = [f for f in licenses if not any(h in os.path.basename(f).upper() for h in hinted)]
        chosen = generic or licenses
    return sorted(set(chosen)) + notices


def normalized(text):
    lines = [line.rstrip() for line in text.replace("\r\n", "\n").split("\n")]
    while lines and not lines[0]:
        lines.pop(0)
    while lines and not lines[-1]:
        lines.pop()
    return "\n".join(lines)


def read_text(path):
    with open(path, encoding="utf-8", errors="replace") as f:
        return normalized(f.read())


# ------------------------------------------------------------ collectors

def crates():
    """The union, over TARGETS, of the crates each platform's build links."""
    # All platforms' crate sources must be present for their license files:
    # a Linux checkout never downloads macOS-only crates during a build, so
    # fetch the whole lockfile first (cached; a no-op after the first run).
    subprocess.run(["cargo", "fetch", "--locked"], cwd=os.path.join(ROOT, "src-tauri"),
                   check=True, capture_output=True)
    linked = {}
    for target in TARGETS:
        meta = json.loads(subprocess.run(
            ["cargo", "metadata", "--format-version", "1", "--locked", "--filter-platform", target],
            cwd=os.path.join(ROOT, "src-tauri"), check=True, capture_output=True, text=True,
        ).stdout)
        packages = {p["id"]: p for p in meta["packages"]}
        nodes = {n["id"]: n for n in meta["resolve"]["nodes"]}
        root = meta["resolve"]["root"]
        # normal (linked) dependencies only: no build/dev dependencies
        seen, stack = set(), [root]
        while stack:
            node = stack.pop()
            if node in seen:
                continue
            seen.add(node)
            for dep in nodes[node]["deps"]:
                if any(kind["kind"] is None for kind in dep["dep_kinds"]):
                    stack.append(dep["pkg"])
        seen.discard(root)
        for pid in seen:
            p = packages[pid]
            kinds = {k for t in p["targets"] for k in t["kind"]}
            if kinds & {"lib", "rlib", "cdylib", "staticlib"} == set() and "proc-macro" in kinds:
                continue  # runs in the compiler only, never linked
            linked.setdefault((p["name"], p["version"]), {
                "name": p["name"],
                "version": p["version"],
                "license": p["license"] or "SEE LICENSE FILE",
                "authors": [re.sub(r"\s*<[^>]*>", "", a) for a in p.get("authors") or []],
                "dir": os.path.dirname(p["manifest_path"]),
            })
    return sorted(linked.values(), key=lambda c: (c["name"], c["version"]))


def npm_packages():
    with open(os.path.join(ROOT, "package.json")) as f:
        todo = list(json.load(f)["dependencies"])
    seen = {}
    while todo:
        name = todo.pop()
        if name in seen:
            continue
        directory = os.path.join(ROOT, "node_modules", name)
        with open(os.path.join(directory, "package.json")) as f:
            pkg = json.load(f)
        author = pkg.get("author")
        if isinstance(author, dict):
            author = author.get("name")
        seen[name] = {
            "name": name,
            "version": pkg["version"],
            "license": pkg.get("license") or "SEE LICENSE FILE",
            "authors": [author] if author else [],
            "dir": directory,
        }
        todo += list((pkg.get("dependencies") or {}).keys())
    return sorted(seen.values(), key=lambda p: p["name"])


# Built from source and linked statically (scripts/build-media.sh): name,
# tag variable, license, what for, upstream, license text in packaging/licenses
NATIVE = [
    ("FFmpeg", "FFMPEG_TAG", "GPL-3.0-or-later (configured `--enable-gpl --enable-version3`)", "decoding, demuxing",
     "https://github.com/FFmpeg/FFmpeg/tree/{tag}", "ffmpeg.md"),
    ("mpv (libmpv)", "MPV_TAG", "GPL-2.0-or-later (configured `-Dgpl=true`)", "playback engine",
     "https://github.com/mpv-player/mpv/tree/{tag}", "mpv.txt"),
    ("libplacebo", "LIBPLACEBO_TAG", "LGPL-2.1-or-later", "video rendering (mpv)",
     "https://code.videolan.org/videolan/libplacebo/-/tree/{tag}", "libplacebo.txt"),
    ("fast_float (in libplacebo)", "LIBPLACEBO_TAG", "MIT OR Apache-2.0 OR BSL-1.0 (used under MIT)", "number parsing",
     "https://code.videolan.org/videolan/libplacebo/-/tree/{tag}/3rdparty", "fast_float.txt"),
    ("glad loader (in libplacebo)", "LIBPLACEBO_TAG", "MIT; generated from Khronos specs, Apache-2.0", "OpenGL loading",
     "https://code.videolan.org/videolan/libplacebo/-/tree/{tag}/3rdparty", "glad.txt"),
    ("dav1d", "DAV1D_TAG", "BSD-2-Clause", "AV1 decoding (FFmpeg)",
     "https://code.videolan.org/videolan/dav1d/-/tree/{tag}", "dav1d.txt"),
    ("libxml2", "LIBXML2_TAG", "MIT", "DASH manifests (FFmpeg)",
     "https://gitlab.gnome.org/GNOME/libxml2/-/tree/{tag}", "libxml2.txt"),
    ("libdisplay-info", "LIBDISPLAYINFO_TAG", "MIT", "display EDID parsing (mpv; Linux only)",
     "https://gitlab.freedesktop.org/emersion/libdisplay-info/-/tree/{tag}", "libdisplay-info.txt"),
    # The macOS static tail (PL-110). On Linux these four stay the distro's
    # shared libraries (not shipped); on macOS they — and libass's own
    # dependencies below — are built statically into the app.
    ("OpenSSL", "OPENSSL_TAG", "Apache-2.0", "TLS for HTTPS streams (FFmpeg; macOS static, Linux system)",
     "https://github.com/openssl/openssl/tree/{tag}", "openssl.txt"),
    ("libass", "LIBASS_TAG", "ISC", "subtitle rendering (mpv; macOS static, Linux system)",
     "https://github.com/libass/libass/tree/{tag}", "libass.txt"),
    ("Little CMS 2", "LCMS2_TAG", "MIT", "color management (mpv; macOS static, Linux system)",
     "https://github.com/mm2/Little-CMS/tree/{tag}", "lcms2.txt"),
    ("uchardet", "UCHARDET_TAG", "MPL-1.1 OR GPL-2.0-or-later OR LGPL-2.0-or-later", "subtitle charset detection (mpv; macOS static, Linux system)",
     "https://gitlab.freedesktop.org/uchardet/uchardet/-/tree/{tag}", "uchardet.txt"),
    ("FreeType", "FREETYPE_TAG", "FTL OR GPL-2.0-or-later (used under the FTL)", "glyph rendering (libass; macOS only)",
     "https://github.com/freetype/freetype/tree/{tag}", "freetype.txt"),
    ("FriBidi", "FRIBIDI_TAG", "LGPL-2.1-or-later", "bidirectional text (libass; macOS only)",
     "https://github.com/fribidi/fribidi/tree/{tag}", "fribidi.txt"),
    ("HarfBuzz", "HARFBUZZ_TAG", "Old MIT", "text shaping (libass; macOS only)",
     "https://github.com/harfbuzz/harfbuzz/tree/{tag}", "harfbuzz.txt"),
    ("libunibreak", "LIBUNIBREAK_TAG", "Zlib", "Unicode line breaking (libass; macOS only)",
     "https://github.com/adah1972/libunibreak/releases/tag/{tag}", "libunibreak.txt"),
]


def native_libraries():
    with open(os.path.join(ROOT, "scripts", "build-media.sh")) as f:
        script = f.read()
    out = []
    for name, var, license_, use, url, text in NATIVE:
        tag = re.search(rf"^{var}=(\S+)", script, re.M).group(1)
        out.append({"name": name, "tag": tag, "license": license_, "use": use, "url": url.format(tag=tag),
                    "text": read_text(os.path.join(ROOT, "packaging", "licenses", text))})
    return out


# ------------------------------------------------------------- document

def texts_for(components):
    """Groups components by the license text(s) they need: {text: (ids, [labels])}."""
    groups = {}
    for c in components:
        ids = required_licenses(c["license"]) if c["license"] != "SEE LICENSE FILE" else []
        files = pick_files(license_files(c["dir"]), ids)
        label = f"{c['name']} {c['version']}"
        if files:
            parts = [read_text(f) for f in files]
        else:
            holder = ", ".join(c["authors"]) or f"the {c['name']} authors"
            parts = []
            for i in ids:
                if i in STANDARD:
                    parts.append(f"Copyright (c) {holder}\n\n{STANDARD[i]}")
                else:
                    # reuse the text another component ships (e.g. MPL-2.0)
                    parts.append(f"@{i}")
            label += " (ships no license file: standard text)"
        for text in parts:
            groups.setdefault(text, (ids, []))[1].append(label)
    # "@<id>" placeholders: attach to a group whose sole license is that id
    for text in [t for t in groups if t.startswith("@")]:
        license_id = text[1:]
        ids, labels = groups.pop(text)
        target = next((t for t, (i, _) in groups.items() if i == [license_id] and not t.startswith("@")), None)
        if target is None:
            raise SystemExit(f"notices: no {license_id} text available for {labels}")
        groups[target][1].extend(labels)
    return groups


def fenced(text):
    """A code block whose fence is longer than any backtick run in `text`."""
    fence = "`" * max(3, 1 + max((len(run) for run in re.findall(r"`+", text)), default=0))
    return f"{fence}text\n{text}\n{fence}"


def table(rows, header):
    out = ["| " + " | ".join(header) + " |", "|" + "---|" * len(header)]
    out += ["| " + " | ".join(str(c).replace("|", "\\|") for c in r) + " |" for r in rows]
    return "\n".join(out)


def document():
    native = native_libraries()
    rust = crates()
    npm = npm_packages()
    chosen = lambda c: " AND ".join(required_licenses(c["license"])) if c["license"] != "SEE LICENSE FILE" else "see text"

    doc = [f"""# Third-party notices

testpattern is free software, licensed under the GNU General Public License,
version 3 or (at your option) any later version — see `LICENSE`. This file
lists the third-party software that the testpattern executable (Linux x86_64
and macOS arm64 builds) contains or links, with the license texts that must
accompany it.

_Generated by `scripts/notices.py` from `src-tauri/Cargo.lock`,
`node_modules` and `scripts/build-media.sh`; do not edit by hand.
`scripts/check.sh` fails when this file is out of date._

## 1. Media engine and native libraries (statically linked)

{table([(n["name"], n["tag"], n["license"], n["use"], n["url"]) for n in native],
       ["Component", "Version", "License", "Used for", "Source"])}

FFmpeg is copyright (c) the FFmpeg developers; mpv is copyright (c) the mpv
developers and the MPlayer/mplayer2 contributors; the other libraries'
copyright notices are part of their license texts in section 5. All are
compiled from the unmodified upstream release tags above by
`scripts/build-media.sh`, which records every build option — the tagged
sources plus that script are their complete corresponding source. As a
combined work, the testpattern executable is distributed under
GPL-3.0-or-later (full text in `LICENSE`). libplacebo is linked statically
under the LGPL: the complete source of testpattern is available under the
GPL, so it can be rebuilt against a modified libplacebo. Libraries marked
"macOS only" are built on macOS (on Linux their role is served by the
system libraries of section 2); a "macOS static, Linux system" library is
static on macOS and loaded from the system on Linux.

## 2. System libraries (dynamically linked, not included)

Loaded from the operating system at runtime; they are not part of the
testpattern packages and keep their own licenses:

{table(SYSTEM_LIBS, ["Library", "Used for", "License"])}

## 3. Rust crates (statically linked)

{len(rust)} crates compiled into the executable (the union of the Linux and
macOS builds; build-time-only crates such as procedural macros are not
included). Where a crate offers a choice of licenses, "Used under" names the
one this distribution relies on.

{table([(c["name"], c["version"], c["license"], chosen(c)) for c in rust],
       ["Crate", "Version", "Declared license", "Used under"])}

## 4. JavaScript packages and fonts (bundled into the user interface)

{table([(p["name"], p["version"], p["license"], chosen(p)) for p in npm],
       ["Package", "Version", "Declared license", "Used under"])}

The Inter typeface (`@fontsource-variable/inter`) is embedded in the user
interface under the SIL Open Font License 1.1 (text in section 5).

## 5. License texts

Native libraries first (section 1), then each distinct text of the crates
and packages (sections 3 and 4) with the components it covers."""]

    for n in native:
        doc.append(f"### {n['name']} {n['tag']}\n\n{fenced(n['text'])}")

    groups = texts_for(rust + npm)
    ordered = sorted(groups.items(), key=lambda kv: (kv[1][0][:1], sorted(kv[1][1])[0].lower()))
    for n, (text, (ids, labels)) in enumerate(ordered, 1):
        title = " AND ".join(ids) or "license"
        doc.append(f"### 5.{n} {title}\n\nApplies to: {', '.join(sorted(set(labels), key=str.lower))}.\n\n"
                   f"{fenced(text)}")
    return "\n\n".join(doc) + "\n"


def main():
    new = document()
    if "--check" in sys.argv:
        try:
            with open(OUT, encoding="utf-8") as f:
                current = f.read()
        except FileNotFoundError:
            current = ""
        if current != new:
            print("THIRD_PARTY_NOTICES.md is out of date — run scripts/notices.py", file=sys.stderr)
            return 1
        return 0
    with open(OUT, "w", encoding="utf-8") as f:
        f.write(new)
    print(f"wrote {os.path.relpath(OUT, ROOT)} ({len(new) // 1024} KB, sha256 {hashlib.sha256(new.encode()).hexdigest()[:12]})")
    return 0


if __name__ == "__main__":
    sys.exit(main())
