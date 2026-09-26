use std::path::PathBuf;
use std::process::Command;

fn main() {
    tauri_build::build();

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux") {
        link_media_engine();
    }
}

/// Links the embedded media engine built by `scripts/build-media.sh`:
/// libmpv + FFmpeg statically, their (ubiquitous) system dependencies
/// dynamically. The result is a single self-contained executable whose codec
/// support does not depend on the distro's FFmpeg build.
fn link_media_engine() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let prefix = std::env::var("TP_MEDIA_PREFIX")
        .map(PathBuf::from)
        .unwrap_or_else(|_| manifest.join("../third_party/prefix"));
    let prefix = prefix.canonicalize().unwrap_or_else(|_| {
        panic!(
            "media engine not found at {} — run scripts/build-media.sh first",
            prefix.display()
        )
    });
    let pkgconfig = prefix.join("lib/pkgconfig");

    println!("cargo:rerun-if-env-changed=TP_MEDIA_PREFIX");
    println!("cargo:rerun-if-env-changed=TP_SYSROOT");
    println!("cargo:rerun-if-env-changed=PKG_CONFIG_PATH");
    println!("cargo:rerun-if-changed={}", prefix.join("lib/libmpv.a").display());

    // Rootless dev setups (scripts/fedora-sysroot.sh) keep -devel symlinks here.
    if let Ok(sysroot) = std::env::var("TP_SYSROOT") {
        println!("cargo:rustc-link-search=native={sysroot}/usr/lib64");
    }

    let pkg_path = match std::env::var("PKG_CONFIG_PATH") {
        Ok(p) if !p.is_empty() => format!("{}:{p}", pkgconfig.display()),
        _ => pkgconfig.display().to_string(),
    };
    let pkg_config = |args: &[&str]| -> Vec<String> {
        let out = Command::new("pkg-config")
            .args(args)
            .env("PKG_CONFIG_PATH", &pkg_path)
            .output()
            .expect("pkg-config not found");
        if !out.status.success() {
            panic!(
                "pkg-config {args:?} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        String::from_utf8(out.stdout)
            .unwrap()
            .split_whitespace()
            .map(str::to_owned)
            .collect()
    };

    // Link order matters for static archives: dependents before dependencies.
    // Besides mpv/FFmpeg, the libraries whose shared-library names change
    // between distro releases are static too (scripts/build-media.sh).
    const STATIC: &[&str] = &[
        "mpv",
        "placebo",
        "display-info",
        "avfilter",
        "avformat",
        "avcodec",
        "swscale",
        "swresample",
        "avutil",
        "xml2",
        "dav1d",
    ];

    // FFmpeg's private deps (dav1d, va, ssl, xml2, ...) + mpv's public deps
    // (libass, libplacebo, pipewire, ...); everything not in STATIC is dynamic.
    let mut flags = pkg_config(&[
        "--static",
        "--libs",
        "libavfilter",
        "libavformat",
        "libavcodec",
        "libswscale",
        "libswresample",
        "libavutil",
    ]);
    flags.extend(pkg_config(&["--libs", "mpv"]));

    let mut search: Vec<String> = vec![prefix.join("lib").display().to_string()];
    let mut dylibs: Vec<String> = Vec::new();
    let mut args: Vec<String> = Vec::new();
    for flag in flags {
        if let Some(dir) = flag.strip_prefix("-L") {
            if !search.iter().any(|s| s == dir) {
                search.push(dir.to_owned());
            }
        } else if let Some(lib) = flag.strip_prefix("-l") {
            if !STATIC.contains(&lib) && !dylibs.iter().any(|d| d == lib) {
                dylibs.push(lib.to_owned());
            }
        } else if flag == "-pthread" && !args.contains(&flag) {
            args.push(flag);
        }
    }

    // libplacebo has C++ parts; its pkg-config file doesn't name the C++
    // runtime, which its shared build used to pull in by itself.
    if !dylibs.iter().any(|d| d == "stdc++") {
        dylibs.push("stdc++".to_owned());
    }

    for dir in &search {
        println!("cargo:rustc-link-search=native={dir}");
    }
    for lib in STATIC {
        println!("cargo:rustc-link-lib=static:-bundle={lib}");
    }
    for lib in &dylibs {
        println!("cargo:rustc-link-lib=dylib={lib}");
    }
    for arg in &args {
        println!("cargo:rustc-link-arg={arg}");
    }
}
