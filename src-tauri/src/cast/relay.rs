//! The stream a Chromecast plays: an HTTP server on the LAN address the
//! device reaches us on, serving `/cast/<token>/<n>.mp4` — the provider's
//! stream, remuxed to fragmented MP4 by the embedded FFmpeg (`remux.c`).
//!
//! Provider URLs carry the account's credentials, so they never go to the
//! device: it gets a random token that is only valid for the current item and
//! only from that device's address. Every seek on the TV side is a new URL
//! (`n` counts them) whose start position the relay remembers. One provider
//! connection at a time: a new request ends the previous one (accounts allow
//! one stream).

use std::collections::HashMap;
use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::io::{BufRead, BufReader, Write};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use parking_lot::Mutex;

/// How long the device gets to send its request.
const READ_TIMEOUT: Duration = Duration::from_secs(10);
/// A device that stops reading this long has gone; ends the stream.
const WRITE_TIMEOUT: Duration = Duration::from_secs(30);
/// Pause before a second round of the stream and its mirrors, for an
/// account whose previous provider connection is still closing.
const RETRY_PAUSE: Duration = Duration::from_millis(1500);

#[repr(C)]
struct TpRemux {
    url: *const c_char,
    user_agent: *const c_char,
    referrer: *const c_char,
    start: f64,
    audio_lang: *const c_char,
    live: c_int,
    opaque: *mut c_void,
    write: extern "C" fn(*mut c_void, *const u8, c_int) -> c_int,
    aborted: extern "C" fn(*mut c_void) -> c_int,
    started: extern "C" fn(*mut c_void, f64, f64),
    error: [c_char; 256],
}

unsafe extern "C" {
    fn tp_remux_run(o: *mut TpRemux) -> c_int;
}

/// What one cast item serves.
#[derive(Clone)]
pub struct Source {
    /// the stream, then the same stream on the provider's mirrors
    pub urls: Vec<String>,
    pub user_agent: Option<String>,
    pub referrer: Option<String>,
    pub live: bool,
    /// preferred audio languages, mpv `alang` style ("eng,en")
    pub audio_lang: String,
}

/// Where a request's output really starts (the keyframe at or before the
/// asked position) and the input's length; the receiver's clock starts at 0
/// on every load.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Started {
    pub duration: f64,
    pub first: f64,
}

pub type OnStart = Arc<dyn Fn(u32, Started) + Send + Sync>;

struct Entry {
    source: Source,
    /// the device the token was issued to
    peer: IpAddr,
    /// start position per URL number
    starts: HashMap<u32, f64>,
    /// stops the request that is running
    active: Option<Arc<AtomicBool>>,
    on_start: OnStart,
    /// why the last request failed
    error: Option<String>,
}

#[derive(Default)]
struct Shared {
    entries: HashMap<String, Entry>,
}

pub struct Relay {
    port: u16,
    shared: Arc<Mutex<Shared>>,
}

impl Relay {
    /// Listens on all interfaces (the device connects from the LAN), on an
    /// ephemeral port.
    pub fn start() -> std::io::Result<Self> {
        let listener = TcpListener::bind(("0.0.0.0", 0))?;
        let port = listener.local_addr()?.port();
        let shared: Arc<Mutex<Shared>> = Arc::default();
        let accept = shared.clone();
        std::thread::Builder::new()
            .name("cast-relay".into())
            .spawn(move || {
                for conn in listener.incoming().flatten() {
                    let shared = accept.clone();
                    let _ = std::thread::Builder::new()
                        .name("cast-stream".into())
                        .spawn(move || {
                            if let Err(e) = serve(conn, &shared) {
                                log::debug!("cast relay: {e}");
                            }
                        });
                }
            })?;
        log::info!("cast relay listening on port {port}");
        Ok(Relay { port, shared })
    }

    /// Serves `source` to `peer` under a new token, ending every other one.
    pub fn publish(&self, source: Source, peer: IpAddr, on_start: OnStart) -> String {
        let token = new_token();
        let mut s = self.shared.lock();
        for (_, e) in s.entries.drain() {
            stop(&e);
        }
        s.entries.insert(
            token.clone(),
            Entry {
                source,
                peer,
                starts: HashMap::new(),
                active: None,
                on_start,
                error: None,
            },
        );
        token
    }

    /// Registers URL number `n` of `token`, starting at `start` seconds.
    pub fn position(&self, token: &str, n: u32, start: f64) {
        if let Some(e) = self.shared.lock().entries.get_mut(token) {
            e.starts.insert(n, start);
            e.error = None;
        }
    }

    /// Why the last request of `token` failed, if it did.
    pub fn error(&self, token: &str) -> Option<String> {
        self.shared
            .lock()
            .entries
            .get(token)
            .and_then(|e| e.error.clone())
    }

    pub fn unpublish(&self, token: &str) {
        if let Some(e) = self.shared.lock().entries.remove(token) {
            stop(&e);
        }
    }

    pub fn url(&self, local: IpAddr, token: &str, n: u32) -> String {
        format!(
            "http://{}/cast/{token}/{n}.mp4",
            SocketAddr::new(local, self.port)
        )
    }
}

fn stop(e: &Entry) {
    if let Some(a) = &e.active {
        a.store(true, Ordering::Relaxed);
    }
}

fn new_token() -> String {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).expect("system random source");
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// `/cast/<token>/<n>.mp4` → (token, n)
fn parse_path(path: &str) -> Option<(&str, u32)> {
    let rest = path.strip_prefix("/cast/")?;
    let (token, file) = rest.split_once('/')?;
    let n = file.strip_suffix(".mp4")?.parse().ok()?;
    (token.len() == 32 && token.bytes().all(|b| b.is_ascii_hexdigit())).then_some((token, n))
}

fn respond(conn: &mut TcpStream, status: &str) -> std::io::Result<()> {
    write!(
        conn,
        "HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    )
}

/// An endless stream: no length, no ranges.
const STREAM_HEADERS: &str = "HTTP/1.1 200 OK\r\nContent-Type: video/mp4\r\nAccess-Control-Allow-Origin: *\r\n\
                              Cache-Control: no-store\r\nConnection: close\r\n\r\n";

fn serve(mut conn: TcpStream, shared: &Mutex<Shared>) -> std::io::Result<()> {
    conn.set_read_timeout(Some(READ_TIMEOUT))?;
    let peer = conn.peer_addr()?.ip();
    let mut reader = BufReader::new(conn.try_clone()?);
    let mut request = String::new();
    reader.read_line(&mut request)?;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 || line.trim().is_empty() {
            break;
        }
    }
    let mut parts = request.split_whitespace();
    let (method, path) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    if method != "GET" && method != "HEAD" {
        return respond(&mut conn, "405 Method Not Allowed");
    }
    let Some((token, n)) = parse_path(path) else {
        return respond(&mut conn, "404 Not Found");
    };
    if method == "HEAD" {
        let known = shared
            .lock()
            .entries
            .get(token)
            .is_some_and(|e| e.peer == peer || peer.is_loopback());
        return if known {
            conn.write_all(STREAM_HEADERS.as_bytes())
        } else {
            respond(&mut conn, "404 Not Found")
        };
    }
    let job = {
        let mut s = shared.lock();
        s.entries
            .get_mut(token)
            .filter(|e| e.peer == peer || peer.is_loopback())
            .map(|e| {
                // one provider connection at a time
                stop(e);
                let flag = Arc::new(AtomicBool::new(false));
                e.active = Some(flag.clone());
                (
                    e.source.clone(),
                    e.starts.get(&n).copied().unwrap_or(0.0),
                    flag,
                    e.on_start.clone(),
                )
            })
    };
    let Some((source, start, flag, on_start)) = job else {
        return respond(&mut conn, "404 Not Found");
    };

    conn.set_write_timeout(Some(WRITE_TIMEOUT))?;
    log::info!("cast relay: stream #{n} from {start:.1} s to {peer}");
    let mut sink = Sink {
        conn,
        sent: false,
        stop: flag,
        on_start: Some(Box::new(move |s| on_start(n, s))),
    };
    let mut result = Err(String::new());
    // the stream, then its mirrors; a second round after `RETRY_PAUSE`
    'rounds: for round in 0..2 {
        if round > 0 {
            std::thread::sleep(RETRY_PAUSE);
        }
        for url in &source.urls {
            if sink.stop.load(Ordering::Relaxed) {
                break 'rounds;
            }
            result = run_remux(&source, url, start, &mut sink);
            // once data went out the stream is this one, whatever happened
            if result.is_ok() || sink.sent {
                break 'rounds;
            }
        }
    }
    let stopped = sink.stop.load(Ordering::Relaxed);
    match result {
        Ok(()) => log::info!(
            "cast relay: stream #{n} {}",
            if stopped { "stopped" } else { "ended" }
        ),
        Err(_) if stopped => log::info!("cast relay: stream #{n} stopped"),
        Err(e) => {
            let e = crate::player::redact(&e);
            log::warn!("cast relay: stream #{n}: {e}");
            if let Some(entry) = shared.lock().entries.get_mut(token) {
                entry.error = Some(e);
            }
            if !sink.sent {
                respond(&mut sink.conn, "502 Bad Gateway")?;
            }
        }
    }
    Ok(())
}

struct Sink {
    conn: TcpStream,
    /// the response headers went out
    sent: bool,
    stop: Arc<AtomicBool>,
    on_start: Option<Box<dyn FnOnce(Started)>>,
}

// The three callbacks run on the thread inside `tp_remux_run`, which
// `run_remux` blocks on; `opaque` is its `&mut Sink`, exclusively borrowed
// for the whole call, and remux.c never calls them concurrently.

extern "C" fn sink_write(opaque: *mut c_void, buf: *const u8, len: c_int) -> c_int {
    // SAFETY: `opaque` is the `&mut Sink` `run_remux` lent to this call (see
    // above); no other reference to it is live while the callback runs.
    let sink = unsafe { &mut *(opaque as *mut Sink) };
    // SAFETY: FFmpeg's AVIO write callback passes `len` initialized bytes at
    // `buf`, valid for the duration of the call.
    let data = unsafe { std::slice::from_raw_parts(buf, len.max(0) as usize) };
    if sink.stop.load(Ordering::Relaxed) {
        return -1;
    }
    if !sink.sent {
        sink.sent = true;
        if sink.conn.write_all(STREAM_HEADERS.as_bytes()).is_err() {
            return -1;
        }
    }
    if sink.conn.write_all(data).is_err() {
        -1
    } else {
        0
    }
}

extern "C" fn sink_aborted(opaque: *mut c_void) -> c_int {
    // SAFETY: `opaque` is the `&mut Sink` `run_remux` lent to this call (see
    // above), read here only.
    let sink = unsafe { &*(opaque as *const Sink) };
    sink.stop.load(Ordering::Relaxed) as c_int
}

extern "C" fn sink_started(opaque: *mut c_void, duration: f64, first: f64) {
    // SAFETY: `opaque` is the `&mut Sink` `run_remux` lent to this call (see
    // above); no other reference to it is live while the callback runs.
    let sink = unsafe { &mut *(opaque as *mut Sink) };
    if let Some(f) = sink.on_start.take() {
        f(Started { duration, first });
    }
}

/// Streams `url` from `start` seconds into the sink until it ends, the TV
/// hangs up or the request is stopped.
fn run_remux(source: &Source, url: &str, start: f64, sink: &mut Sink) -> Result<(), String> {
    let cstr = |s: &str| CString::new(s).map_err(|_| "invalid stream address".to_owned());
    let url = cstr(url)?;
    let ua = cstr(
        source
            .user_agent
            .as_deref()
            .unwrap_or(crate::player::USER_AGENT),
    )?;
    let referrer = source.referrer.as_deref().map(cstr).transpose()?;
    let audio_lang = cstr(&source.audio_lang)?;
    let mut o = TpRemux {
        url: url.as_ptr(),
        user_agent: ua.as_ptr(),
        referrer: referrer.as_ref().map_or(std::ptr::null(), |s| s.as_ptr()),
        start,
        audio_lang: audio_lang.as_ptr(),
        live: source.live as c_int,
        opaque: sink as *mut Sink as *mut c_void,
        write: sink_write,
        aborted: sink_aborted,
        started: sink_started,
        error: [0; 256],
    };
    // SAFETY: `o` matches remux.c's `tp_remux` layout (`repr(C)`); the C
    // strings and `sink` outlive the call, which is synchronous and keeps no
    // pointer past its return; `error` is a zeroed buffer remux.c fills with
    // a NUL-terminated message (snprintf).
    let ret = unsafe { tp_remux_run(&mut o) };
    if ret == 0 {
        return Ok(());
    }
    // SAFETY: `error` started zeroed and remux.c only writes it with
    // snprintf into its 256 bytes, so it is always NUL-terminated.
    let msg = unsafe { CStr::from_ptr(o.error.as_ptr()) }
        .to_string_lossy()
        .into_owned();
    Err(if msg.is_empty() {
        format!("remux failed ({ret})")
    } else {
        msg
    })
}

#[cfg(test)]
mod tests {
    use super::parse_path;

    #[test]
    fn only_token_urls_are_served() {
        let t = "0123456789abcdef0123456789abcdef";
        assert_eq!(parse_path(&format!("/cast/{t}/3.mp4")), Some((t, 3)));
        assert_eq!(parse_path(&format!("/cast/{t}/x.mp4")), None);
        assert_eq!(parse_path("/cast/../../etc/passwd"), None);
        assert_eq!(parse_path("/cast/short/1.mp4"), None);
        assert_eq!(parse_path(&format!("/img/{t}/1.mp4")), None);
    }
}
