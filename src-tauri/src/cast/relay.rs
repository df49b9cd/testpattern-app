//! The stream a Chromecast plays: an HTTP server on the LAN address the
//! device reaches us on, serving `/cast/<token>/<n>.mp4` — the provider's
//! stream, remuxed to fragmented MP4 by the embedded FFmpeg (`remux.c`).
//!
//! Provider URLs carry the account's credentials, so they never go to the
//! device: it gets a random token that is only valid for the current item and
//! only from that device's address. Every seek on the TV side is a new URL
//! (`n` counts them) whose start position and tracks the relay remembers.
//! Next to each `<n>.mp4` the relay serves `<n>.vtt`: the subtitle stream
//! picked for it, cue by cue as the remuxer decodes it (the Default Media Receiver shows a
//! WebVTT sidecar through a `<track>` element, whose cues the browser adds
//! while the response is still arriving). One provider
//! connection at a time: a new request ends the previous one (accounts allow
//! one stream).

use std::collections::HashMap;
use std::collections::HashSet;
use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::{BufRead, BufReader, Write};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use parking_lot::{Condvar, Mutex};

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
    audio_index: c_int,
    sub_index: c_int,
    sub_lang: *const c_char,
    opaque: *mut c_void,
    write: extern "C" fn(*mut c_void, *const u8, c_int) -> c_int,
    aborted: extern "C" fn(*mut c_void) -> c_int,
    started: extern "C" fn(*mut c_void, f64, f64),
    stream: extern "C" fn(*mut c_void, *const TpStream),
    cue: extern "C" fn(*mut c_void, c_int, f64, f64, *const c_char),
    error: [c_char; 256],
}

#[repr(C)]
struct TpStream {
    index: c_int,
    kind: c_int,
    codec: *const c_char,
    lang: *const c_char,
    title: *const c_char,
    channels: c_int,
    is_default: c_int,
    castable: c_int,
    selected: c_int,
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
    /// preferred subtitle languages, for `Subtitle::Auto`
    pub sub_lang: String,
}

/// Which subtitle stream a request cues.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Subtitle {
    /// by `Source::sub_lang`, else the one the file marks as default
    Auto,
    #[default]
    Off,
    /// this input stream (by language when it isn't a text subtitle)
    Stream(u32),
}

/// An audio or subtitle stream of the input.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StreamInfo {
    /// input stream index
    pub index: u32,
    pub audio: bool,
    pub codec: String,
    pub lang: Option<String>,
    pub title: Option<String>,
    pub channels: u32,
    pub default: bool,
    /// audio: always; subtitles: text ones (not on live channels)
    pub castable: bool,
    /// the audio stream this request streams / the subtitle stream it cues
    pub selected: bool,
}

/// Where a request's output really starts (the keyframe at or before the
/// asked position), the input's length and its audio/subtitle streams; the
/// receiver's clock starts at 0 on every load.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Started {
    pub duration: f64,
    pub first: f64,
    pub streams: Vec<StreamInfo>,
}

/// What one URL number streams.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Request {
    /// seconds into the item
    pub start: f64,
    /// input audio stream; None: by `Source::audio_lang`
    pub audio: Option<u32>,
    pub subtitle: Subtitle,
}

/// One subtitle cue, on the input's clock.
#[derive(Debug, Clone, PartialEq)]
struct Cue {
    start: f64,
    end: f64,
    /// WebVTT cue text
    text: String,
}

#[derive(Default)]
struct FeedState {
    /// where the output (and so the receiver's clock) starts on the input's;
    /// known once the MP4 request started (it then also cues, or not)
    first: Option<f64>,
    cues: Vec<Cue>,
    /// cues already taken (a repeated request for the same URL decodes them again)
    seen: HashSet<(i64, u64)>,
    /// the MP4 request that fills the feed: a repeated request for the same
    /// URL takes over, and only the newest one ends it
    producer: u64,
    /// no more cues: the MP4 request ended, or a newer URL replaced this one
    done: bool,
}

/// The cues of one URL number, from its MP4 request to its `.vtt` request.
#[derive(Default)]
struct SubFeed {
    state: Mutex<FeedState>,
    changed: Condvar,
}

impl SubFeed {
    fn update(&self, f: impl FnOnce(&mut FeedState)) {
        f(&mut self.state.lock());
        self.changed.notify_all();
    }
}

pub type OnStart = Arc<dyn Fn(u32, Started) + Send + Sync>;

struct Entry {
    source: Source,
    /// the device the token was issued to
    peer: IpAddr,
    /// what each URL number streams
    requests: HashMap<u32, Request>,
    /// the subtitle cues of each URL number
    feeds: HashMap<u32, Arc<SubFeed>>,
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
            close(&e);
        }
        s.entries.insert(
            token.clone(),
            Entry {
                source,
                peer,
                requests: HashMap::new(),
                feeds: HashMap::new(),
                active: None,
                on_start,
                error: None,
            },
        );
        token
    }

    /// Registers URL number `n` of `token`.
    pub fn position(&self, token: &str, n: u32, request: Request) {
        if let Some(e) = self.shared.lock().entries.get_mut(token) {
            e.requests.insert(n, request);
            let feed = Arc::new(SubFeed::default());
            // only the newest URL's cues are still wanted
            for (_, old) in e.feeds.drain() {
                old.update(|s| s.done = true);
            }
            e.feeds.insert(n, feed);
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
            close(&e);
        }
    }

    pub fn url(&self, local: IpAddr, token: &str, n: u32) -> String {
        format!(
            "http://{}/cast/{token}/{n}.mp4",
            SocketAddr::new(local, self.port)
        )
    }

    /// The subtitles of URL `n`.
    pub fn subtitle_url(&self, local: IpAddr, token: &str, n: u32) -> String {
        format!(
            "http://{}/cast/{token}/{n}.vtt",
            SocketAddr::new(local, self.port)
        )
    }
}

/// Stops the entry's running provider request.
fn stop(e: &Entry) {
    if let Some(a) = &e.active {
        a.store(true, Ordering::Relaxed);
    }
}

/// Stops everything of an entry that goes away, its subtitle responses too.
fn close(e: &Entry) {
    stop(e);
    for feed in e.feeds.values() {
        feed.update(|s| s.done = true);
    }
}

fn new_token() -> String {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).expect("system random source");
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum File {
    Video,
    Subtitles,
}

/// `/cast/<token>/<n>.mp4` | `.vtt` → (token, n, file)
fn parse_path(path: &str) -> Option<(&str, u32, File)> {
    let rest = path.strip_prefix("/cast/")?;
    let (token, file) = rest.split_once('/')?;
    let (n, kind) = if let Some(n) = file.strip_suffix(".mp4") {
        (n, File::Video)
    } else {
        (file.strip_suffix(".vtt")?, File::Subtitles)
    };
    let n = n.parse().ok()?;
    (token.len() == 32 && token.bytes().all(|b| b.is_ascii_hexdigit())).then_some((token, n, kind))
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
    let Some((token, n, file)) = parse_path(path) else {
        return respond(&mut conn, "404 Not Found");
    };
    let headers = match file {
        File::Video => STREAM_HEADERS,
        File::Subtitles => VTT_HEADERS,
    };
    if method == "HEAD" {
        let known = shared
            .lock()
            .entries
            .get(token)
            .is_some_and(|e| e.peer == peer || peer.is_loopback());
        return if known {
            conn.write_all(headers.as_bytes())
        } else {
            respond(&mut conn, "404 Not Found")
        };
    }
    if file == File::Subtitles {
        let feed = shared
            .lock()
            .entries
            .get(token)
            .filter(|e| e.peer == peer || peer.is_loopback())
            .and_then(|e| e.feeds.get(&n).cloned());
        return match feed {
            Some(feed) => serve_subtitles(conn, &feed, n),
            None => respond(&mut conn, "404 Not Found"),
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
                // this request fills the URL's cues from now on (under the
                // shared lock, so the one it replaced can't end them first)
                let feed = e.feeds.get(&n).map(|f| {
                    let mut st = f.state.lock();
                    st.producer += 1;
                    st.done = false;
                    (f.clone(), st.producer)
                });
                (
                    e.source.clone(),
                    e.requests.get(&n).copied().unwrap_or_default(),
                    feed,
                    flag,
                    e.on_start.clone(),
                )
            })
    };
    let Some((source, request, feed, flag, on_start)) = job else {
        return respond(&mut conn, "404 Not Found");
    };

    conn.set_write_timeout(Some(WRITE_TIMEOUT))?;
    let start = request.start;
    log::info!("cast relay: stream #{n} from {start:.1} s to {peer}");
    let mut sink = Sink {
        conn,
        sent: false,
        stop: flag,
        on_start: Some(Box::new(move |s| on_start(n, s))),
        streams: Vec::new(),
        feed: feed.as_ref().map(|(f, _)| f.clone()),
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
            sink.streams.clear();
            result = run_remux(&source, url, request, &mut sink);
            // once data went out the stream is this one, whatever happened
            if result.is_ok() || sink.sent {
                break 'rounds;
            }
        }
    }
    if let Some((feed, producer)) = &feed {
        feed.update(|s| {
            if s.producer == *producer {
                s.done = true;
            }
        });
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

/// The `.vtt` response headers: a stream of unknown length, like the video's.
const VTT_HEADERS: &str = "HTTP/1.1 200 OK\r\nContent-Type: text/vtt; charset=utf-8\r\nAccess-Control-Allow-Origin: *\r\n\
                           Cache-Control: no-store\r\nConnection: close\r\n\r\n";

/// How long `<n>.vtt` waits for its video request to start before it ends
/// empty.
const SUBTITLE_WAIT: Duration = Duration::from_secs(30);

/// Serves the picked subtitle stream of URL `n` as WebVTT, each cue as soon as
/// the remuxer decoded it, until the video request ends.
fn serve_subtitles(mut conn: TcpStream, feed: &SubFeed, n: u32) -> std::io::Result<()> {
    conn.set_write_timeout(Some(WRITE_TIMEOUT))?;
    conn.write_all(VTT_HEADERS.as_bytes())?;
    conn.write_all(b"WEBVTT\n\n")?;
    let deadline = Instant::now() + SUBTITLE_WAIT;
    let mut st = feed.state.lock();
    // the output's start is known once the video started
    let first = loop {
        match st.first {
            Some(first) => break first,
            None if st.done || Instant::now() >= deadline => return Ok(()),
            None => {
                feed.changed.wait_until(&mut st, deadline);
            }
        }
    };
    log::info!("cast relay: subtitles #{n}");
    let mut next = 0;
    loop {
        let mut out = String::new();
        for cue in &st.cues[next..] {
            out.push_str(&vtt_cue(cue.start - first, cue.end - first, &cue.text));
        }
        next = st.cues.len();
        let done = st.done;
        if !out.is_empty() {
            // never write while holding the lock: the remuxer would wait on the TV
            drop(st);
            conn.write_all(out.as_bytes())?;
            conn.flush()?;
            st = feed.state.lock();
            continue;
        }
        if done {
            return Ok(());
        }
        feed.changed.wait_for(&mut st, Duration::from_secs(1));
    }
}

/// One WebVTT cue block (times on the receiver's clock); empty when the cue
/// ended before the output starts.
fn vtt_cue(start: f64, end: f64, text: &str) -> String {
    if end <= 0.0 || text.is_empty() {
        return String::new();
    }
    let start = start.max(0.0);
    let end = end.max(start + 0.001);
    format!("{} --> {}\n{text}\n\n", vtt_time(start), vtt_time(end))
}

fn vtt_time(t: f64) -> String {
    let ms = (t * 1000.0).round() as u64;
    format!(
        "{:02}:{:02}:{:02}.{:03}",
        ms / 3_600_000,
        ms / 60_000 % 60,
        ms / 1000 % 60,
        ms % 1000
    )
}

/// An FFmpeg ASS event (`ReadOrder,Layer,Style,Name,MarginL,MarginR,MarginV,
/// Effect,Text`) → WebVTT cue text: italic/bold/underline kept, other
/// override tags dropped, `\N` line breaks, `<`/`&` escaped, no blank lines
/// (they would end the cue).
pub(super) fn ass_to_vtt(event: &str) -> String {
    let text = event.splitn(9, ',').nth(8).unwrap_or(event);
    let mut out = String::new();
    let mut open: Vec<char> = Vec::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '{' => {
                let mut block = String::new();
                for c in chars.by_ref() {
                    if c == '}' {
                        break;
                    }
                    block.push(c);
                }
                for tag in block.split('\\').skip(1) {
                    let mut t = tag.chars();
                    let (Some(k), rest) = (t.next(), t.as_str()) else {
                        continue;
                    };
                    if !matches!(k, 'i' | 'b' | 'u') || !matches!(rest, "0" | "1") {
                        continue;
                    }
                    let on = rest == "1";
                    if on && !open.contains(&k) {
                        open.push(k);
                        out.push_str(&format!("<{k}>"));
                    } else if !on && let Some(pos) = open.iter().position(|&o| o == k) {
                        // close the tags opened after it too, then reopen them
                        let reopen: Vec<char> = open.drain(pos..).collect();
                        for o in reopen.iter().rev() {
                            out.push_str(&format!("</{o}>"));
                        }
                        for &o in &reopen[1..] {
                            open.push(o);
                            out.push_str(&format!("<{o}>"));
                        }
                    }
                }
            }
            '\\' => match chars.peek() {
                Some('N' | 'n') => {
                    chars.next();
                    out.push('\n');
                }
                Some('h') => {
                    chars.next();
                    out.push(' ');
                }
                _ => out.push('\\'),
            },
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '&' => out.push_str("&amp;"),
            '\r' => {}
            c => out.push(c),
        }
    }
    for o in open.iter().rev() {
        out.push_str(&format!("</{o}>"));
    }
    out.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

struct Sink {
    conn: TcpStream,
    /// the response headers went out
    sent: bool,
    stop: Arc<AtomicBool>,
    on_start: Option<Box<dyn FnOnce(Started)>>,
    /// the input's audio/subtitle streams, reported before `started`
    streams: Vec<StreamInfo>,
    /// where this URL number's subtitle cues go
    feed: Option<Arc<SubFeed>>,
}

// The callbacks run on the thread inside `tp_remux_run`, which
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
    if let Some(feed) = &sink.feed {
        feed.update(|s| {
            s.first.get_or_insert(first);
        });
    }
    if let Some(f) = sink.on_start.take() {
        f(Started {
            duration,
            first,
            streams: std::mem::take(&mut sink.streams),
        });
    }
}

/// A C string from remux.c (NULL or empty: none).
///
/// # Safety
/// `p` is NULL or a NUL-terminated string valid for the call.
unsafe fn opt_str(p: *const c_char) -> Option<String> {
    if p.is_null() {
        return None;
    }
    // SAFETY: non-null, and NUL-terminated per the caller's contract.
    let s = unsafe { CStr::from_ptr(p) }
        .to_string_lossy()
        .trim()
        .to_owned();
    (!s.is_empty()).then_some(s)
}

extern "C" fn sink_stream(opaque: *mut c_void, s: *const TpStream) {
    // SAFETY: `opaque` is the `&mut Sink` `run_remux` lent to this call (see
    // above); no other reference to it is live while the callback runs.
    let sink = unsafe { &mut *(opaque as *mut Sink) };
    // SAFETY: remux.c passes a pointer to a live `tp_stream` (same layout,
    // `repr(C)`) whose strings are NULL or NUL-terminated for the call.
    let s = unsafe { &*s };
    // SAFETY: as above, the strings live for the duration of the call.
    let (codec, lang, title) = unsafe { (opt_str(s.codec), opt_str(s.lang), opt_str(s.title)) };
    sink.streams.push(StreamInfo {
        index: s.index.max(0) as u32,
        audio: s.kind == 1,
        codec: codec.unwrap_or_default(),
        lang,
        title,
        channels: s.channels.max(0) as u32,
        default: s.is_default != 0,
        castable: s.castable != 0,
        selected: s.selected != 0,
    });
}

extern "C" fn sink_cue(
    opaque: *mut c_void,
    _index: c_int,
    start: f64,
    end: f64,
    ass: *const c_char,
) {
    // SAFETY: `opaque` is the `&mut Sink` `run_remux` lent to this call (see
    // above), read here only.
    let sink = unsafe { &*(opaque as *const Sink) };
    let Some(feed) = &sink.feed else { return };
    // SAFETY: remux.c passes the decoder's NUL-terminated ASS line, valid for the call.
    let Some(event) = (unsafe { opt_str(ass) }) else {
        return;
    };
    let text = ass_to_vtt(&event);
    if text.is_empty() || !start.is_finite() || !end.is_finite() {
        return;
    }
    let mut h = DefaultHasher::new();
    text.hash(&mut h);
    let key = ((start * 1000.0).round() as i64, h.finish());
    feed.update(|s| {
        if s.seen.insert(key) {
            s.cues.push(Cue { start, end, text });
        }
    });
}

/// Streams `url` from `start` seconds into the sink until it ends, the TV
/// hangs up or the request is stopped.
fn run_remux(source: &Source, url: &str, request: Request, sink: &mut Sink) -> Result<(), String> {
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
    let sub_lang = cstr(&source.sub_lang)?;
    let mut o = TpRemux {
        url: url.as_ptr(),
        user_agent: ua.as_ptr(),
        referrer: referrer.as_ref().map_or(std::ptr::null(), |s| s.as_ptr()),
        start: request.start,
        audio_lang: audio_lang.as_ptr(),
        live: source.live as c_int,
        audio_index: request.audio.map_or(-1, |i| i as c_int),
        sub_index: match request.subtitle {
            Subtitle::Auto => -1,
            Subtitle::Off => -2,
            Subtitle::Stream(i) => i as c_int,
        },
        sub_lang: sub_lang.as_ptr(),
        opaque: sink as *mut Sink as *mut c_void,
        write: sink_write,
        aborted: sink_aborted,
        started: sink_started,
        stream: sink_stream,
        cue: sink_cue,
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
    use super::{File, ass_to_vtt, parse_path, vtt_cue};

    #[test]
    fn only_token_urls_are_served() {
        let t = "0123456789abcdef0123456789abcdef";
        assert_eq!(
            parse_path(&format!("/cast/{t}/3.mp4")),
            Some((t, 3, File::Video))
        );
        assert_eq!(
            parse_path(&format!("/cast/{t}/3.vtt")),
            Some((t, 3, File::Subtitles))
        );
        assert_eq!(parse_path(&format!("/cast/{t}/3.srt")), None);
        assert_eq!(parse_path(&format!("/cast/{t}/x.mp4")), None);
        assert_eq!(parse_path("/cast/../../etc/passwd"), None);
        assert_eq!(parse_path("/cast/short/1.mp4"), None);
        assert_eq!(parse_path(&format!("/img/{t}/1.mp4")), None);
    }

    #[test]
    fn ass_events_become_vtt_text() {
        assert_eq!(
            ass_to_vtt("0,0,Default,,0,0,0,,Hello, world"),
            "Hello, world"
        );
        assert_eq!(
            ass_to_vtt("1,0,Default,,0,0,0,,{\\i1}Who's there?{\\i0}\\NMe."),
            "<i>Who's there?</i>\nMe."
        );
        assert_eq!(
            ass_to_vtt("2,0,Default,,0,0,0,,{\\an8\\pos(10,20)}Top"),
            "Top"
        );
        assert_eq!(
            ass_to_vtt("3,0,Default,,0,0,0,,a < b & c"),
            "a &lt; b &amp; c"
        );
        assert_eq!(ass_to_vtt("4,0,Default,,0,0,0,,{\\b1}open"), "<b>open</b>");
        assert_eq!(ass_to_vtt("5,0,Default,,0,0,0,,one\\N\\Ntwo"), "one\ntwo");
        assert_eq!(
            ass_to_vtt("6,0,Default,,0,0,0,,{\\b1}{\\i1}x{\\b0}y{\\i0}"),
            "<b><i>x</i></b><i>y</i>"
        );
    }

    #[test]
    fn cues_are_timed_on_the_receivers_clock() {
        assert_eq!(
            vtt_cue(1.5, 3.25, "Hi"),
            "00:00:01.500 --> 00:00:03.250\nHi\n\n"
        );
        assert_eq!(
            vtt_cue(3661.0, 3662.0, "x"),
            "01:01:01.000 --> 01:01:02.000\nx\n\n"
        );
        // started before the output: shown from 0
        assert_eq!(
            vtt_cue(-2.0, 1.0, "x"),
            "00:00:00.000 --> 00:00:01.000\nx\n\n"
        );
        // over before the output starts
        assert_eq!(vtt_cue(-5.0, -1.0, "x"), "");
    }

    /// The relay end to end over loopback: a local file with two audio and
    /// two SubRip streams (`TP_CAST_FILE`), `<n>.mp4` with the asked audio
    /// stream and `<n>.vtt` with the picked subtitles.
    /// `TP_CAST_FILE=/path/t.mkv cargo test --lib relay_serves_tracks -- --ignored`
    #[test]
    #[ignore]
    fn relay_serves_tracks() {
        use super::{Relay, Request, Source, Started, Subtitle};
        use std::io::{Read, Write};
        use std::sync::{Arc, Mutex};
        let file = std::env::var("TP_CAST_FILE").expect("TP_CAST_FILE");
        let relay = Relay::start().unwrap();
        let started: Arc<Mutex<Option<Started>>> = Arc::default();
        let s2 = started.clone();
        let source = Source {
            urls: vec![file],
            user_agent: None,
            referrer: None,
            live: false,
            audio_lang: "eng".into(),
            sub_lang: "fin".into(),
        };
        let lo = "127.0.0.1".parse().unwrap();
        let token = relay.publish(
            source,
            lo,
            Arc::new(move |_, s| *s2.lock().unwrap() = Some(s)),
        );
        // Auto: no Finnish, so the stream the file marks as default (Danish)
        relay.position(
            &token,
            1,
            Request {
                start: 0.0,
                audio: Some(2),
                subtitle: Subtitle::Auto,
            },
        );
        let fetch = |url: String| {
            std::thread::spawn(move || {
                let rest = url.strip_prefix("http://").unwrap();
                let (host, path) = rest.split_once('/').unwrap();
                let mut c = std::net::TcpStream::connect(host).unwrap();
                write!(c, "GET /{path} HTTP/1.1\r\nHost: {host}\r\n\r\n").unwrap();
                let mut out = Vec::new();
                c.read_to_end(&mut out).unwrap();
                out
            })
        };
        let vtt = fetch(relay.subtitle_url(lo, &token, 1));
        std::thread::sleep(std::time::Duration::from_millis(100));
        let mp4 = fetch(relay.url(lo, &token, 1));
        let mp4 = mp4.join().unwrap();
        let vtt = String::from_utf8(vtt.join().unwrap()).unwrap();
        let st = started.lock().unwrap().clone().expect("started");
        eprintln!("{st:#?}\n{vtt}");
        let audio: Vec<_> = st
            .streams
            .iter()
            .filter(|s| s.audio)
            .map(|s| (s.index, s.selected))
            .collect();
        assert_eq!(audio, vec![(1, false), (2, true)]);
        let subs: Vec<_> = st
            .streams
            .iter()
            .filter(|s| !s.audio)
            .map(|s| (s.index, s.castable, s.default, s.selected))
            .collect();
        assert_eq!(subs, vec![(3, true, false, false), (4, true, true, true)]);
        assert!(mp4.len() > 10_000, "mp4 {} bytes", mp4.len());
        assert!(vtt.contains("text/vtt"));
        assert!(vtt.contains("WEBVTT"));
        assert!(
            vtt.contains("00:00:02.000 --> 00:00:04.000\nHej fra dansk"),
            "{vtt}"
        );
        assert!(!vtt.contains("English"));
        if let Ok(out) = std::env::var("TP_CAST_OUT") {
            std::fs::write(
                out,
                &mp4[mp4.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4..],
            )
            .unwrap();
        }

        // a seek: URL 2 from 10 s, English
        *started.lock().unwrap() = None;
        relay.position(
            &token,
            2,
            Request {
                start: 10.0,
                audio: Some(1),
                subtitle: Subtitle::Stream(3),
            },
        );
        let mp4 = fetch(relay.url(lo, &token, 2));
        let vtt = fetch(relay.subtitle_url(lo, &token, 2));
        let (mp4, vtt) = (
            mp4.join().unwrap(),
            String::from_utf8(vtt.join().unwrap()).unwrap(),
        );
        let st = started.lock().unwrap().clone().expect("started");
        eprintln!("first {}\n{vtt}", st.first);
        assert!(mp4.len() > 10_000);
        assert_eq!(st.first, 10.0, "keyframe every 2 s");
        // on the receiver's clock (0 = 10 s); the cue at 1-3 s is over
        assert!(
            vtt.contains("00:00:02.000 --> 00:00:04.500\nSecond line\ntwo rows\n\n"),
            "{vtt}"
        );
        assert!(!vtt.contains("Hello"));
    }
}
