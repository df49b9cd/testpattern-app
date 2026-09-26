//! `img://` protocol: fetches remote artwork (posters, logos, backdrops),
//! downsizes it to the requested width and caches it on disk.
//!
//!   img://localhost/?u=<url-encoded remote url>&w=<max width px>
//!
//! (Windows webviews address it as http://img.localhost/?u=...)

use std::io::Cursor;
use std::path::PathBuf;
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType;
use image::{GenericImageView, ImageReader, Limits};
use tauri::http::{Request, Response, StatusCode};
use tauri::{Manager, Runtime, UriSchemeContext, UriSchemeResponder};
use tokio::sync::Semaphore;

use crate::state::AppState;
use crate::util::fnv1a;

/// Bounds concurrent remote downloads so a poster wall can't flood the
/// provider (or our socket pool).
static FETCH_SLOTS: LazyLock<Arc<Semaphore>> = LazyLock::new(|| Arc::new(Semaphore::new(16)));

pub fn handle<R: Runtime>(ctx: UriSchemeContext<'_, R>, request: Request<Vec<u8>>, responder: UriSchemeResponder) {
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
        [b'R', b'I', b'F', b'F', _, _, _, _, b'W', b'E', b'B', b'P', ..] => "image/webp",
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
        return ok(bytes);
    }
    // remember dead links for a day instead of hammering them
    if let Ok(meta) = tokio::fs::metadata(&miss).await
        && meta.modified().ok().and_then(|m| m.elapsed().ok()).is_some_and(|age| age < Duration::from_secs(86400)) {
            return status(StatusCode::NOT_FOUND);
        }

    let fetched = async {
        let _slot = FETCH_SLOTS.clone().acquire_owned().await.ok()?;
        let r = st.http.get(&url).timeout(Duration::from_secs(20)).send().await.ok()?;
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

    let processed = tokio::task::spawn_blocking(move || shrink(original, width)).await.ok();
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
    let Ok(img) = reader.decode() else { return original };
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
        img.write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png).is_ok()
    } else {
        let rgb = img.to_rgb8();
        JpegEncoder::new_with_quality(&mut out, 84).encode_image(&rgb).is_ok()
    };
    if encoded && !out.is_empty() { out } else { original }
}
