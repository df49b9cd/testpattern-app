//! Debug-build-only automation endpoint on 127.0.0.1 (default port 17777).
//! Lets scripts drive and inspect the running app without a desktop session:
//!
//!   GET  /snapshot?path=/tmp/x.png  → renders the window (GL video + webview) to PNG
//!   POST /eval                      → evaluates the body as JS in the webview,
//!                                     responds with the JSON-stringified result
//!   GET  /click?x=10&y=20           → real GTK button press/release on the webview
//!   POST /invoke {cmd, args}        → runs a Tauri command inside the app's webview
//!   GET  /img?u=<url>&w=<px>        → same as the img:// protocol
//!
//! /invoke and /img let the UI run in a plain browser (the desktop app's
//! preview pane) against the real backend during development.
//!   GET  /health
//!
//! Access: local tools (curl, scripts — no `Origin` header) and pages served
//! by the Vite dev server (`ALLOWED_ORIGINS`). Any other web page is refused,
//! including cross-site `<img>`/form tricks and DNS rebinding (`check_request`):
//! this endpoint runs arbitrary JS in the app, which can drive mpv and read
//! stream URLs that embed the account credentials.
//!
//! Never compiled into release builds.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
// only the gtk (Linux) webview paths use channels: the JS runs on the main
// thread there via `with_webview`
#[cfg_attr(not(target_os = "linux"), allow(unused_imports))]
use std::sync::mpsc;
use std::time::Duration;

use tauri::{AppHandle, Manager, Runtime};

/// Pages that may use the bridge: the Vite dev server (`bun run dev`).
const ALLOWED_ORIGINS: &[&str] = &["http://localhost:1420", "http://127.0.0.1:1420"];
const MAX_BODY: usize = 16 << 20;

pub fn start<R: Runtime>(app: &AppHandle<R>) {
    let port: u16 = std::env::var("TP_DEV_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(17777);
    let listener = match TcpListener::bind(("127.0.0.1", port)) {
        Ok(l) => l,
        Err(e) => {
            log::warn!("devtools: cannot bind 127.0.0.1:{port}: {e}");
            return;
        }
    };
    log::info!("devtools listening on http://127.0.0.1:{port}");
    let app = app.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let app = app.clone();
            std::thread::spawn(move || {
                if let Err(e) = handle(stream, &app, port) {
                    log::warn!("devtools: {e}");
                }
            });
        }
    });
}

#[derive(Debug, Default)]
struct Headers {
    host: Option<String>,
    origin: Option<String>,
    /// `Sec-Fetch-Site`: browsers send it on every request, tools don't.
    fetch_site: Option<String>,
    content_length: usize,
}

/// Lets in local tools and the dev server's pages, nothing else.
fn check_request(method: &str, path: &str, port: u16, h: &Headers) -> Result<(), &'static str> {
    // DNS rebinding: a foreign name that resolves to 127.0.0.1
    let host_ok = h.host.as_deref().is_some_and(|host| {
        host == format!("127.0.0.1:{port}") || host == format!("localhost:{port}")
    });
    if !host_ok {
        return Err("unexpected Host header");
    }
    if let Some(origin) = h.origin.as_deref() {
        return if ALLOWED_ORIGINS.contains(&origin) {
            Ok(())
        } else {
            Err("origin not allowed")
        };
    }
    // No Origin: a local tool, or a browser's no-cors load (<img src>, link
    // navigation), which may only reach the read-only endpoints.
    let from_browser = h.fetch_site.as_deref().is_some_and(|s| s != "none");
    let read_only = method == "GET" && matches!(path, "/img" | "/health");
    if from_browser && !read_only {
        Err("cross-site request")
    } else {
        Ok(())
    }
}

fn handle<R: Runtime>(mut stream: TcpStream, app: &AppHandle<R>, port: u16) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request_line = String::new();
    reader.read_line(&mut request_line)?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_owned();
    let target = parts.next().unwrap_or("/").to_owned();

    let mut headers = Headers::default();
    loop {
        let mut line = String::new();
        reader.read_line(&mut line)?;
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            let v = v.trim();
            match k.to_ascii_lowercase().as_str() {
                "content-length" => headers.content_length = v.parse().unwrap_or(0),
                "host" => headers.host = Some(v.to_owned()),
                "origin" => headers.origin = Some(v.to_owned()),
                "sec-fetch-site" => headers.fetch_site = Some(v.to_ascii_lowercase()),
                _ => {}
            }
        }
    }
    let (path, query) = target.split_once('?').unwrap_or((&target, ""));
    if let Err(why) = check_request(&method, path, port, &headers) {
        log::warn!(
            "devtools: refused {method} {path} ({why}; origin {:?})",
            headers.origin
        );
        return respond(&mut stream, 403, "text/plain", why.as_bytes(), None);
    }
    // only allow-listed origins are echoed back (never `*`)
    let cors = headers.origin.as_deref();
    if headers.content_length > MAX_BODY {
        return respond(&mut stream, 413, "text/plain", b"body too large", cors);
    }
    let mut body = vec![0u8; headers.content_length];
    reader.read_exact(&mut body)?;
    let body = String::from_utf8_lossy(&body).into_owned();

    let param = |name: &str| {
        query.split('&').find_map(|kv| {
            let (k, v) = kv.split_once('=')?;
            (k == name).then(|| urlencoding_decode(v))
        })
    };

    if method == "OPTIONS" {
        return respond(&mut stream, 204, "text/plain", b"", cors);
    }
    if (method.as_str(), path) == ("GET", "/img") {
        let (code, ctype, bytes) = image(app, query);
        return respond(&mut stream, code, &ctype, &bytes, cors);
    }

    let (status, response) = match (method.as_str(), path) {
        ("GET", "/health") => (200, "ok".to_owned()),
        ("POST", "/invoke") => match invoke_via_webview(app, &body) {
            Ok(v) => (200, v),
            Err(e) => (500, e),
        },
        ("GET", "/snapshot") => {
            let out = param("path").unwrap_or_else(|| "/tmp/testpattern-snapshot.png".into());
            if !out.starts_with('/') || !out.to_ascii_lowercase().ends_with(".png") {
                (400, "path must be an absolute .png file".to_owned())
            } else {
                match snapshot(app, &out) {
                    Ok(()) => (200, out),
                    Err(e) => (500, e),
                }
            }
        }
        ("GET", "/click") => {
            let x: f64 = param("x").and_then(|v| v.parse().ok()).unwrap_or(10.0);
            let y: f64 = param("y").and_then(|v| v.parse().ok()).unwrap_or(10.0);
            match click(app, x, y) {
                Ok(()) => (200, "clicked".to_owned()),
                Err(e) => (500, e),
            }
        }
        ("POST", "/eval") => match eval(app, &body) {
            Ok(v) => (200, v),
            Err(e) => (500, e),
        },
        // Harness probe: "ready" only once JS actually round-trips through the
        // webview, so the startup check never depends on /snapshot (reload path).
        ("GET", "/eval-ready") => match eval(app, "return 1+1;") {
            Ok(_) => (200, "ready".to_owned()),
            Err(e) => (503, e),
        },
        _ => (404, "not found".to_owned()),
    };
    respond(
        &mut stream,
        status,
        "text/plain; charset=utf-8",
        response.as_bytes(),
        cors,
    )
}

/// `cors` is the request's (already allow-listed) Origin, if any.
fn respond(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
    cors: Option<&str>,
) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        204 => "No Content",
        403 => "Forbidden",
        _ => "ERR",
    };
    let cors = cors
        .map(|o| {
            format!(
                "Access-Control-Allow-Origin: {o}\r\nVary: Origin\r\nAccess-Control-Allow-Methods: GET, POST, OPTIONS\r\n\
                 Access-Control-Allow-Headers: content-type\r\n"
            )
        })
        .unwrap_or_default();
    write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n{cors}\
         Cache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    stream.write_all(body)?;
    stream.flush()
}

/// Runs `window.__TAURI_INTERNALS__.invoke(cmd, args)` in the app's webview.
/// Body: `{"cmd": "...", "args": {...}}`. Tauri errors come back as
/// `{"error": ...}` with status 500.
fn invoke_via_webview<R: Runtime>(app: &AppHandle<R>, body: &str) -> Result<String, String> {
    let req: serde_json::Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    let cmd = req["cmd"].as_str().ok_or("missing cmd")?;
    let args = req.get("args").cloned().unwrap_or(serde_json::json!({}));
    let js = format!(
        "try {{ return {{ ok: await window.__TAURI_INTERNALS__.invoke({}, {}) }}; }} catch (e) {{ return {{ error: e }}; }}",
        serde_json::to_string(cmd).unwrap(),
        args
    );
    let out = eval(app, &js)?;
    let v: serde_json::Value = serde_json::from_str(&out).map_err(|e| e.to_string())?;
    match v.get("error") {
        Some(err) => Err(match err {
            serde_json::Value::String(s) => s.clone(),
            other => other.to_string(),
        }),
        None => Ok(v
            .get("ok")
            .cloned()
            .unwrap_or(serde_json::Value::Null)
            .to_string()),
    }
}

fn image<R: Runtime>(app: &AppHandle<R>, query: &str) -> (u16, String, Vec<u8>) {
    let Some(st) = app.try_state::<crate::state::AppState>() else {
        return (503, "text/plain".into(), Vec::new());
    };
    let uri = format!("img://localhost/?{query}");
    let Ok(request) = tauri::http::Request::builder().uri(uri).body(Vec::new()) else {
        return (400, "text/plain".into(), Vec::new());
    };
    let response =
        tauri::async_runtime::block_on(crate::images::serve(st.inner().clone(), request));
    let ctype = response
        .headers()
        .get("Content-Type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_owned();
    (response.status().as_u16(), ctype, response.into_body())
}

fn urlencoding_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
                if let Ok(b) = u8::from_str_radix(hex, 16) {
                    out.push(b);
                    i += 3;
                    continue;
                }
                out.push(b'%');
            }
            b'+' => out.push(b' '),
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(target_os = "linux")]
fn snapshot<R: Runtime>(app: &AppHandle<R>, out: &str) -> Result<(), String> {
    use gtk::prelude::*;
    let (tx, rx) = mpsc::channel();
    let out = out.to_owned();
    let handle = app.clone();
    app.run_on_main_thread(move || {
        let result = (|| -> Result<(), String> {
            let window = handle.get_webview_window("main").ok_or("no main window")?;
            let gtk_window = window.gtk_window().map_err(|e| e.to_string())?;
            let (w, h) = (gtk_window.allocated_width(), gtk_window.allocated_height());
            let surface = gtk::cairo::ImageSurface::create(gtk::cairo::Format::ARgb32, w, h)
                .map_err(|e| e.to_string())?;
            {
                let cr = gtk::cairo::Context::new(&surface).map_err(|e| e.to_string())?;
                gtk_window.draw(&cr);
            }
            let mut file = std::fs::File::create(&out).map_err(|e| e.to_string())?;
            surface.write_to_png(&mut file).map_err(|e| e.to_string())
        })();
        let _ = tx.send(result);
    })
    .map_err(|e| e.to_string())?;
    rx.recv_timeout(Duration::from_secs(10))
        .map_err(|e| e.to_string())?
}

/// Synthesizes a primary-button press + release on the webview and runs it
/// through GTK's normal event dispatch (exercises native handlers too).
#[cfg(target_os = "linux")]
fn click<R: Runtime>(app: &AppHandle<R>, x: f64, y: f64) -> Result<(), String> {
    use glib::translate::{ToGlibPtr, ToGlibPtrMut};
    use gtk::prelude::*;
    let window = app.get_webview_window("main").ok_or("no main window")?;
    let (tx, rx) = mpsc::channel();
    window
        .with_webview(move |pw| {
            let webview: webkit2gtk::WebView = pw.inner();
            let result = (|| -> Result<(), String> {
                let gdk_window = webview.window().ok_or("webview not realized")?;
                let seat = gdk_window.display().default_seat().ok_or("no seat")?;
                let pointer = seat.pointer().ok_or("no pointer")?;
                for kind in [gdk::EventType::ButtonPress, gdk::EventType::ButtonRelease] {
                    let mut ev = gdk::Event::new(kind);
                    unsafe {
                        let raw: *mut gdk::ffi::GdkEvent = ev.to_glib_none_mut().0;
                        let b = raw as *mut gdk::ffi::GdkEventButton;
                        (*b).window = gdk_window.to_glib_full();
                        (*b).send_event = 1;
                        (*b).time = gtk::current_event_time();
                        (*b).x = x;
                        (*b).y = y;
                        (*b).button = 1;
                        (*b).x_root = x;
                        (*b).y_root = y;
                        gdk::ffi::gdk_event_set_device(raw, pointer.to_glib_none().0);
                    }
                    gtk::main_do_event(&mut ev);
                }
                Ok(())
            })();
            let _ = tx.send(result);
        })
        .map_err(|e| e.to_string())?;
    rx.recv_timeout(Duration::from_secs(10))
        .map_err(|e| e.to_string())?
}

#[cfg(not(target_os = "linux"))]
fn click<R: Runtime>(_app: &AppHandle<R>, _x: f64, _y: f64) -> Result<(), String> {
    Err("click is only implemented on Linux".into())
}

#[cfg(not(target_os = "linux"))]
fn snapshot<R: Runtime>(_app: &AppHandle<R>, _out: &str) -> Result<(), String> {
    Err("snapshot is only implemented on Linux".into())
}

#[cfg(target_os = "linux")]
fn eval<R: Runtime>(app: &AppHandle<R>, js: &str) -> Result<String, String> {
    use javascriptcore::ValueExt;
    use webkit2gtk::WebViewExt;
    let window = app.get_webview_window("main").ok_or("no main window")?;
    let (tx, rx) = mpsc::channel::<Result<String, String>>();
    // Wrap so the result is always JSON text; async functions are awaited.
    let script = format!(
        "(async () => {{ try {{ const r = await (async () => {{ {js} }})(); return JSON.stringify(r ?? null); }} catch (e) {{ return JSON.stringify({{error: String(e && e.stack || e)}}); }} }})()"
    );
    window
        .with_webview(move |pw| {
            let webview: webkit2gtk::WebView = pw.inner();
            let tx = tx.clone();
            webview.call_async_javascript_function(
                &format!("return await {script};"),
                None,
                None,
                None,
                None::<&gtk::gio::Cancellable>,
                move |res| {
                    let out = match res {
                        Ok(v) => Ok(v.to_str().to_string()),
                        Err(e) => Err(e.to_string()),
                    };
                    let _ = tx.send(out);
                },
            );
        })
        .map_err(|e| e.to_string())?;
    rx.recv_timeout(Duration::from_secs(20))
        .map_err(|e| e.to_string())?
}

/// macOS: WKWebView's `evaluateJavaScript` completion path loses the result
/// of an async script here (the completion value comes back empty from wry's
/// NSJSONSerialization serialization of a Promise), so the Linux-style
/// "read the completion value" contract cannot work. Instead the wrapper
/// ships the result back to Rust through Tauri's own IPC — a channel that is
/// proven to work in this webview — and the caller waits on it. The script
/// itself is still dispatched with `eval_with_callback` (fire-and-forget);
/// only the *return path* changes.
///
/// The JS posts to the devtools-only `__devtools_eval_result` command —
/// registered only in debug builds, as devtools itself is. Any page load could
/// in principle invoke it, but only the Vite dev server's origin can even
/// reach /eval, and the result is keyed by an eval id that is only valid for
/// one outstanding call.
#[cfg(target_os = "macos")]
fn eval<R: Runtime>(app: &AppHandle<R>, js: &str) -> Result<String, String> {
    let window = app.get_webview_window("main").ok_or("no main window")?;
    let id = {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(1);
        NEXT.fetch_add(1, Ordering::Relaxed)
    };
    let (tx, rx) = mpsc::channel::<Result<String, String>>();
    eval_bus::register(id, tx);
    if window
        .eval_with_callback(&eval_wrapper(id, js), |_| {})
        .is_err()
    {
        eval_bus::unregister(id);
        return Err("evaluateJavaScript dispatch failed".into());
    }
    let r = rx.recv_timeout(Duration::from_secs(25));
    eval_bus::unregister(id);
    r.map_err(|e| format!("evaluateJavaScript completion never fired: {e}"))?
}

/// The JS wrapper for the macOS eval path (a pure fn so tests can assert on
/// its output): awaits the user JS, then posts the JSON string back over
/// Tauri IPC. The post retries a bounded 200 times (50ms apart, ~10s: the
/// Rust side's IPC handler may lag the webview's script start) and, when all
/// attempts fail, posts once more with an error result so the caller fails
/// fast instead of timing out blind. `eval_with_callback`'s callback is
/// ignored on macOS (see above), so nothing else reports the outcome.
#[cfg(target_os = "macos")]
fn eval_wrapper(id: u64, js: &str) -> String {
    format!(
        "(async () => {{ let out; try {{ const r = await (async () => {{ {js} }})(); out = JSON.stringify(r ?? null); }} catch (e) {{ out = JSON.stringify({{error: String(e && e.stack || e)}}); }} const post = (result) => window.__TAURI_INTERNALS__.invoke('__devtools_eval_result', {{ id: {id}, result }}); let done = false; for (let i = 0; i < 200 && !done; i++) {{ try {{ await post(out); done = true; }} catch (_) {{ await new Promise(r => setTimeout(r, 50)); }} }} if (!done) {{ try {{ await post(JSON.stringify({{error: 'devtools /eval: result IPC failed after 200 retries'}})); }} catch (_) {{}} }} }})(); 'ok'"
    )
}

#[cfg(target_os = "macos")]
mod eval_bus {
    use std::collections::HashMap;
    use std::sync::mpsc::Sender;
    use std::sync::{Mutex, OnceLock};

    type Tx = Sender<Result<String, String>>;

    fn channels() -> &'static Mutex<HashMap<u64, Tx>> {
        static C: OnceLock<Mutex<HashMap<u64, Tx>>> = OnceLock::new();
        C.get_or_init(|| Mutex::new(HashMap::new()))
    }

    pub fn register(id: u64, tx: Tx) {
        channels().lock().unwrap().insert(id, tx);
    }

    pub fn unregister(id: u64) {
        channels().lock().unwrap().remove(&id);
    }

    pub fn resolve(id: u64, result: String) -> bool {
        let tx = { channels().lock().unwrap().get(&id).cloned() };
        match tx {
            Some(tx) => tx.send(Ok(result)).is_ok(),
            None => false,
        }
    }
}

/// Devtools IPC command (debug builds only): the eval wrapper posts its JSON
/// result here and the waiting eval thread picks it up.
#[cfg(target_os = "macos")]
#[tauri::command]
pub fn __devtools_eval_result(id: u64, result: String) -> bool {
    eval_bus::resolve(id, result)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn eval<R: Runtime>(_app: &AppHandle<R>, _js: &str) -> Result<String, String> {
    Err("eval is only implemented on Linux and macOS".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(host: &str, origin: Option<&str>, fetch_site: Option<&str>) -> Headers {
        Headers {
            host: Some(host.into()),
            origin: origin.map(Into::into),
            fetch_site: fetch_site.map(Into::into),
            content_length: 0,
        }
    }

    #[test]
    fn lets_in_local_tools_and_the_dev_server() {
        // curl / scripts
        assert!(check_request("POST", "/eval", 17777, &req("127.0.0.1:17777", None, None)).is_ok());
        assert!(
            check_request(
                "GET",
                "/snapshot",
                17777,
                &req("localhost:17777", None, None)
            )
            .is_ok()
        );
        // the UI preview in a plain browser (bridge.ts)
        let preview = req(
            "127.0.0.1:17777",
            Some("http://localhost:1420"),
            Some("cross-site"),
        );
        assert!(check_request("POST", "/invoke", 17777, &preview).is_ok());
        assert!(check_request("OPTIONS", "/invoke", 17777, &preview).is_ok());
        // artwork <img> from the preview: no Origin on no-cors loads
        assert!(
            check_request(
                "GET",
                "/img",
                17777,
                &req("127.0.0.1:17777", None, Some("cross-site"))
            )
            .is_ok()
        );
    }

    #[test]
    fn refuses_other_web_pages() {
        let evil = req(
            "127.0.0.1:17777",
            Some("https://evil.example"),
            Some("cross-site"),
        );
        assert!(check_request("POST", "/eval", 17777, &evil).is_err());
        assert!(check_request("OPTIONS", "/eval", 17777, &evil).is_err());
        assert!(
            check_request(
                "POST",
                "/eval",
                17777,
                &req("127.0.0.1:17777", Some("null"), None)
            )
            .is_err()
        );
        // <img src="http://127.0.0.1:17777/snapshot?path=…"> on a foreign page
        let img = req("127.0.0.1:17777", None, Some("cross-site"));
        assert!(check_request("GET", "/snapshot", 17777, &img).is_err());
        assert!(check_request("GET", "/click", 17777, &img).is_err());
        // DNS rebinding: the page's own host name resolving to 127.0.0.1
        let rebound = req("attacker.example:17777", None, Some("same-origin"));
        assert!(check_request("POST", "/eval", 17777, &rebound).is_err());
        assert!(check_request("GET", "/health", 17777, &Headers::default()).is_err());
    }

    #[cfg(target_os = "macos")]
    mod eval_bus_tests {
        use super::*;
        use std::time::Duration;

        #[test]
        fn resolve_sends_to_the_registered_waiter() {
            let (tx, rx) = mpsc::channel::<Result<String, String>>();
            eval_bus::register(1, tx);
            assert!(eval_bus::resolve(1, "\"ok\"".into()));
            assert_eq!(
                rx.recv_timeout(Duration::from_secs(1)).unwrap().unwrap(),
                "\"ok\""
            );
            eval_bus::unregister(1);
        }

        #[test]
        fn resolve_an_unknown_id_returns_false() {
            assert!(!eval_bus::resolve(424242, "\"nope\"".into()));
        }

        #[test]
        fn resolve_after_unregister_returns_false() {
            let (tx, _rx) = mpsc::channel::<Result<String, String>>();
            eval_bus::register(2, tx);
            eval_bus::unregister(2);
            assert!(!eval_bus::resolve(2, "\"late\"".into()));
        }

        #[test]
        fn wrapper_embeds_the_id_the_script_and_the_retry_bound() {
            let w = eval_wrapper(7, "return 1+1;");
            assert!(w.contains("id: 7"));
            assert!(w.contains("return 1+1;"));
            assert!(w.contains("i < 200"));
            assert!(w.contains("__devtools_eval_result"));
            assert!(w.contains("result IPC failed after 200 retries"));
        }
    }
}
