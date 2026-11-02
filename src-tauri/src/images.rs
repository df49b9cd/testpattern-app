//! `img://` protocol: fetches remote artwork (posters, logos, backdrops),
//! downsizes it to the requested width and caches it on disk.
//!
//!   img://localhost/?u=<url-encoded remote url>&w=<max width px>
//!
//! (Windows webviews address it as http://img.localhost/?u=...)

use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};
use std::time::{Duration, SystemTime};

use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType;
use image::{GenericImageView, ImageReader, Limits};
use serde::Serialize;
use tauri::http::{Request, Response, StatusCode};
use tauri::{Manager, Runtime, State, UriSchemeContext, UriSchemeResponder};
use tokio::sync::Semaphore;

use crate::error::{Error, Result};
use crate::settings;
use crate::state::AppState;
use crate::util::fnv1a;

/// Dead links are remembered this long (`<key>.miss` markers).
const MISS_TTL: Duration = Duration::from_secs(86400);
/// Default size limit of the artwork cache in MB (setting `cache.imagesMb`).
pub const DEFAULT_CACHE_MB: u64 = 1024;

/// Bounds concurrent remote downloads so a poster wall can't flood the
/// provider (or our socket pool).
static FETCH_SLOTS: LazyLock<Arc<Semaphore>> = LazyLock::new(|| Arc::new(Semaphore::new(16)));

pub fn handle<R: Runtime>(
    ctx: UriSchemeContext<'_, R>,
    request: Request<Vec<u8>>,
    responder: UriSchemeResponder,
) {
    let app = ctx.app_handle().clone();
    tauri::async_runtime::spawn(async move {
        let response = match app.try_state::<AppState>() {
            Some(st) => serve(st.inner().clone(), request).await,
            None => status(StatusCode::SERVICE_UNAVAILABLE),
        };
        responder.respond(response);
    });
}

fn status(code: StatusCode) -> Response<Vec<u8>> {
    Response::builder()
        .status(code)
        .header("Access-Control-Allow-Origin", "*")
        .body(Vec::new())
        .unwrap()
}

fn content_type(bytes: &[u8]) -> &'static str {
    match bytes {
        [0xff, 0xd8, ..] => "image/jpeg",
        [0x89, b'P', b'N', b'G', ..] => "image/png",
        [b'G', b'I', b'F', ..] => "image/gif",
        [
            b'R',
            b'I',
            b'F',
            b'F',
            _,
            _,
            _,
            _,
            b'W',
            b'E',
            b'B',
            b'P',
            ..,
        ] => "image/webp",
        b if b.windows(4).take(256).any(|w| w == b"<svg") => "image/svg+xml",
        _ => "application/octet-stream",
    }
}

fn ok(bytes: Vec<u8>) -> Response<Vec<u8>> {
    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", content_type(&bytes))
        .header("Cache-Control", "public, max-age=604800, immutable")
        .header("Access-Control-Allow-Origin", "*")
        .body(bytes)
        .unwrap()
}

fn params(request: &Request<Vec<u8>>) -> Option<(String, u32)> {
    let query = request.uri().query()?;
    let mut url = None;
    let mut width = 0u32;
    for (k, v) in url::form_urlencoded::parse(query.as_bytes()) {
        match k.as_ref() {
            "u" => url = Some(v.into_owned()),
            "w" => width = v.parse().unwrap_or(0),
            _ => {}
        }
    }
    let url = url.filter(|u| u.starts_with("http://") || u.starts_with("https://"))?;
    // quantize widths so near-identical requests share cache entries
    let width = match width {
        0 => 0,
        w => w.div_ceil(80) * 80,
    };
    Some((url, width.min(3840)))
}

pub async fn serve(st: AppState, request: Request<Vec<u8>>) -> Response<Vec<u8>> {
    let Some((url, width)) = params(&request) else {
        return status(StatusCode::BAD_REQUEST);
    };
    let dir: PathBuf = st.cache_dir.join("images");
    let key = format!("{:016x}", fnv1a(format!("{url}|{width}").as_bytes()));
    let path = dir.join(&key);
    let miss = dir.join(format!("{key}.miss"));

    if let Ok(bytes) = tokio::fs::read(&path).await {
        touch(path);
        return ok(bytes);
    }
    // remember dead links for a day instead of hammering them
    if let Ok(meta) = tokio::fs::metadata(&miss).await
        && meta
            .modified()
            .ok()
            .and_then(|m| m.elapsed().ok())
            .is_some_and(|age| age < MISS_TTL)
    {
        return status(StatusCode::NOT_FOUND);
    }

    let fetched = async {
        let _slot = FETCH_SLOTS.clone().acquire_owned().await.ok()?;
        let r = st
            .http
            .get(&url)
            .timeout(Duration::from_secs(20))
            .send()
            .await
            .ok()?;
        if !r.status().is_success() {
            return None;
        }
        r.bytes().await.ok().map(|b| b.to_vec())
    }
    .await;

    let Some(original) = fetched.filter(|b| !b.is_empty()) else {
        let _ = tokio::fs::create_dir_all(&dir).await;
        let _ = tokio::fs::write(&miss, b"").await;
        return status(StatusCode::NOT_FOUND);
    };

    let processed = tokio::task::spawn_blocking(move || shrink(original, width))
        .await
        .ok();
    let Some(bytes) = processed else {
        return status(StatusCode::INTERNAL_SERVER_ERROR);
    };
    let _ = tokio::fs::create_dir_all(&dir).await;
    let tmp = dir.join(format!("{key}.tmp"));
    if tokio::fs::write(&tmp, &bytes).await.is_ok() {
        let _ = tokio::fs::rename(&tmp, &path).await;
    }
    ok(bytes)
}

/// Downscales to `width` (keeping aspect) and re-encodes; anything we can't
/// decode (SVG, exotic formats) is passed through untouched.
fn shrink(original: Vec<u8>, width: u32) -> Vec<u8> {
    let mut limits = Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    let reader = match ImageReader::new(Cursor::new(&original)).with_guessed_format() {
        Ok(mut r) => {
            r.limits(limits);
            r
        }
        Err(_) => return original,
    };
    let Ok(img) = reader.decode() else {
        return original;
    };
    let (w, h) = img.dimensions();
    let needs_resize = width > 0 && w > width;
    let is_jpeg = content_type(&original) == "image/jpeg";
    if !needs_resize && is_jpeg {
        return original;
    }
    let img = if needs_resize {
        let nh = ((h as f64) * (width as f64 / w as f64)).round().max(1.0) as u32;
        img.resize_exact(width, nh, FilterType::Triangle)
    } else {
        img
    };
    let mut out = Vec::new();
    let encoded = if img.color().has_alpha() {
        img.write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png)
            .is_ok()
    } else {
        let rgb = img.to_rgb8();
        JpegEncoder::new_with_quality(&mut out, 84)
            .encode_image(&rgb)
            .is_ok()
    };
    if encoded && !out.is_empty() {
        out
    } else {
        original
    }
}

// ------------------------------------------------------------ cache size

/// Marks a cache entry as recently used: eviction goes by modification time
/// (bumped at most hourly, so scrolling a poster wall doesn't mean a write
/// per image).
fn touch(path: PathBuf) {
    tokio::task::spawn_blocking(move || {
        let fresh = std::fs::metadata(&path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|m| m.elapsed().ok())
            .is_some_and(|age| age < Duration::from_secs(3600));
        if !fresh && let Ok(f) = std::fs::File::options().write(true).open(&path) {
            let _ = f.set_modified(SystemTime::now());
        }
    });
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CacheStats {
    pub files: u64,
    pub bytes: u64,
}

/// Trims the artwork cache in `dir`: leftover temp files and expired
/// "not found" markers go, then the least recently used images until the
/// cache is below 90% of `cap` bytes. Returns what is left.
pub fn prune(dir: &Path, cap: u64, now: SystemTime) -> std::io::Result<CacheStats> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(CacheStats::default()),
        Err(e) => return Err(e),
    };
    let mut images = Vec::new();
    for entry in entries.flatten() {
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        let modified = meta.modified().unwrap_or(now);
        let age = now.duration_since(modified).unwrap_or_default();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let temp = name.ends_with(".tmp");
        let marker = name.ends_with(".miss");
        if (temp && age > Duration::from_secs(3600)) || (marker && age > MISS_TTL) {
            let _ = std::fs::remove_file(entry.path());
        } else if !temp && !marker {
            images.push((modified, meta.len(), entry.path()));
        }
    }
    let mut stats = CacheStats {
        files: images.len() as u64,
        bytes: images.iter().map(|i| i.1).sum(),
    };
    if stats.bytes > cap {
        images.sort_by_key(|i| i.0); // least recently used first
        let target = cap / 10 * 9;
        for (_, len, path) in &images {
            if stats.bytes <= target {
                break;
            }
            if std::fs::remove_file(path).is_ok() {
                stats.bytes -= len;
                stats.files -= 1;
            }
        }
    }
    Ok(stats)
}

fn cache_dir(st: &AppState) -> PathBuf {
    st.cache_dir.join("images")
}

/// Applies the size limit (setting `cache.imagesMb`) in the background.
pub async fn enforce_limit(st: AppState) {
    let cap_mb = {
        let conn = st.db.read();
        settings::get(&conn, "cache.imagesMb")
            .as_u64()
            .unwrap_or(DEFAULT_CACHE_MB)
            .max(16)
    };
    let dir = cache_dir(&st);
    match tokio::task::spawn_blocking(move || prune(&dir, cap_mb << 20, SystemTime::now())).await {
        Ok(Ok(s)) => log::info!(
            "artwork cache: {} images, {} MB (limit {cap_mb} MB)",
            s.files,
            s.bytes >> 20
        ),
        Ok(Err(e)) => log::warn!("artwork cache: {e}"),
        Err(e) => log::warn!("artwork cache: {e}"),
    }
}

#[tauri::command]
pub async fn images_cache_info(state: State<'_, AppState>) -> Result<CacheStats> {
    let dir = cache_dir(state.inner());
    // a cap nothing reaches: just count
    tokio::task::spawn_blocking(move || prune(&dir, u64::MAX, SystemTime::now()))
        .await
        .map_err(|e| Error::msg(e.to_string()))?
        .map_err(Error::from)
}

#[tauri::command]
pub async fn images_cache_clear(state: State<'_, AppState>) -> Result<CacheStats> {
    let dir = cache_dir(state.inner());
    tokio::task::spawn_blocking(move || prune(&dir, 0, SystemTime::now()))
        .await
        .map_err(|e| Error::msg(e.to_string()))?
        .map_err(Error::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prunes_least_recently_used_first() {
        let dir = std::env::temp_dir().join(format!("tp-images-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let now = SystemTime::now();
        let file = |name: &str, kb: usize, age_s: u64| {
            let p = dir.join(name);
            std::fs::write(&p, vec![0u8; kb * 1024]).unwrap();
            std::fs::File::options()
                .write(true)
                .open(&p)
                .unwrap()
                .set_modified(now - Duration::from_secs(age_s))
                .unwrap();
        };
        file("old", 40, 3 * 86400);
        file("mid", 40, 86400);
        file("new", 40, 60);
        file("gone.tmp", 1, 7200); // abandoned download
        file("dead.miss", 0, 2 * 86400); // expired "not found"
        file("fresh.miss", 0, 60);

        // under the limit: only the leftovers go
        assert_eq!(
            prune(&dir, 1 << 30, now).unwrap(),
            CacheStats {
                files: 3,
                bytes: 120 * 1024
            }
        );
        assert!(
            !dir.join("gone.tmp").exists()
                && !dir.join("dead.miss").exists()
                && dir.join("fresh.miss").exists()
        );
        // 100 KB limit → down to 90 KB: the least recently used image goes
        assert_eq!(
            prune(&dir, 100 * 1024, now).unwrap(),
            CacheStats {
                files: 2,
                bytes: 80 * 1024
            }
        );
        assert!(!dir.join("old").exists() && dir.join("mid").exists() && dir.join("new").exists());
        // clear
        assert_eq!(prune(&dir, 0, now).unwrap(), CacheStats::default());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
