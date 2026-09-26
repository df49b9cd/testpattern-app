//! Native playback engine (libmpv). The web UI drives it through the
//! `player_*` commands and listens to `player://event` for state changes.

#[cfg(target_os = "linux")]
mod linux;
pub mod mpv;
mod mpv_sys;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{AppHandle, Emitter, Manager, Runtime, State, WebviewWindow};

use mpv::{EndReason, Event, Mpv};
use mpv_sys::*;

pub const EVENT: &str = "player://event";
pub const USER_AGENT: &str = concat!("testpattern/", env!("CARGO_PKG_VERSION"));

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "kebab-case", rename_all_fields = "camelCase")]
pub enum PlayerEvent {
    Prop { name: String, value: Value },
    StartFile,
    FileLoaded,
    EndFile { reason: EndReason, error: Option<String> },
    Restart,
    Reconnecting { attempt: u32 },
    Log { level: String, prefix: String, text: String },
}

/// The catalog item a file belongs to (movie or episode), so what playback
/// learns about its tracks can be stored for the version picker.
#[derive(Debug, Clone)]
pub struct MediaRef {
    /// "movie" | "episode"
    pub kind: &'static str,
    pub source_id: i64,
    pub id: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoadOptions {
    /// Start position in seconds (resume).
    pub start: Option<f64>,
    /// Live streams get low-latency demuxer settings and auto-reconnect.
    #[serde(default)]
    pub live: bool,
    pub title: Option<String>,
    pub user_agent: Option<String>,
    /// HTTP Referer some streams require (M3U playlists).
    pub referrer: Option<String>,
    #[serde(default)]
    pub paused: bool,
    /// set by `play` (never by the UI)
    #[serde(skip)]
    pub media: Option<MediaRef>,
}

#[derive(Default)]
struct Session {
    url: String,
    options: LoadOptions,
    /// Same stream on mirror servers, tried if the first open fails.
    alternates: Vec<String>,
    reconnects: u32,
    loaded_at: Option<Instant>,
    ever_loaded: bool,
    recording: Option<Recording>,
    /// learned tracks: HDR seen in `video-params`, last summary stored
    hdr: bool,
    track_list: Option<Value>,
    tracks_saved: Option<String>,
}

/// A live recording (mpv `stream-record`). mpv overwrites the target when a
/// new file loads, so each automatic reconnect continues in a new part.
struct Recording {
    /// Path without extension.
    base: PathBuf,
    part: u32,
}

impl Recording {
    fn file(&self) -> PathBuf {
        let name = self.base.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let name = if self.part <= 1 { format!("{name}.ts") } else { format!("{name} (part {}).ts", self.part) };
        self.base.with_file_name(name)
    }
}

/// A file name from a channel title: no path separators or characters
/// Windows/FAT can't store, sensible length.
pub fn file_name_for(title: &str) -> String {
    let cleaned: String = title
        .chars()
        .map(|c| if c.is_control() || r#"/\:*?"<>|"#.contains(c) { ' ' } else { c })
        .collect();
    let words = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    let short: String = words.chars().take(80).collect();
    let short = short.trim_matches(['.', ' ']).to_owned();
    if short.is_empty() { "Live TV".to_owned() } else { short }
}

pub struct Player {
    mpv: Arc<Mpv>,
    session: Arc<Mutex<Option<Session>>>,
}

const OBSERVED: &[(&str, mpv_format)] = &[
    ("pause", MPV_FORMAT_FLAG),
    ("time-pos", MPV_FORMAT_DOUBLE),
    ("duration", MPV_FORMAT_DOUBLE),
    ("paused-for-cache", MPV_FORMAT_FLAG),
    ("cache-buffering-state", MPV_FORMAT_INT64),
    ("demuxer-cache-time", MPV_FORMAT_DOUBLE),
    ("seeking", MPV_FORMAT_FLAG),
    ("seekable", MPV_FORMAT_FLAG),
    ("eof-reached", MPV_FORMAT_FLAG),
    ("idle-active", MPV_FORMAT_FLAG),
    ("core-idle", MPV_FORMAT_FLAG),
    ("volume", MPV_FORMAT_DOUBLE),
    ("mute", MPV_FORMAT_FLAG),
    ("speed", MPV_FORMAT_DOUBLE),
    ("track-list", MPV_FORMAT_NODE),
    ("aid", MPV_FORMAT_NODE),
    ("sid", MPV_FORMAT_NODE),
    ("video-params", MPV_FORMAT_NODE),
    ("video-codec", MPV_FORMAT_STRING),
    ("audio-codec-name", MPV_FORMAT_STRING),
    ("hwdec-current", MPV_FORMAT_STRING),
    ("estimated-vf-fps", MPV_FORMAT_DOUBLE),
    ("chapter-list", MPV_FORMAT_NODE),
    ("sub-delay", MPV_FORMAT_DOUBLE),
    ("audio-delay", MPV_FORMAT_DOUBLE),
    ("video-aspect-override", MPV_FORMAT_STRING),
    ("panscan", MPV_FORMAT_DOUBLE),
    // "" when not recording (T-035)
    ("stream-record", MPV_FORMAT_STRING),
];

impl Player {
    fn new<R: Runtime>(app: &AppHandle<R>) -> Result<Self, mpv::MpvError> {
        // libmpv refuses to start unless numbers are formatted the C way; GTK
        // has already applied the user's locale at this point.
        unsafe { libc::setlocale(libc::LC_NUMERIC, c"C".as_ptr()) };
        let mpv = Arc::new(Mpv::new(&[
            ("vo", "libmpv"),
            ("idle", "yes"),
            ("keep-open", "yes"),
            ("hwdec", "auto-safe"),
            // never block the GTK thread inside mpv_render_context_render
            ("video-timing-offset", "0"),
            ("osd-level", "0"),
            ("input-default-bindings", "no"),
            ("input-vo-keyboard", "no"),
            ("terminal", "no"),
            ("config", "no"),
            ("load-scripts", "no"),
            ("cache", "yes"),
            ("demuxer-max-bytes", "256MiB"),
            ("demuxer-max-back-bytes", "96MiB"),
            ("demuxer-readahead-secs", "30"),
            ("network-timeout", "20"),
            ("user-agent", USER_AGENT),
            ("stream-lavf-o", "reconnect=1,reconnect_streamed=1,reconnect_on_network_error=1,reconnect_delay_max=4"),
            ("audio-client-name", "testpattern"),
            ("sub-auto", "fuzzy"),
            ("deinterlace", "auto"),
            ("hr-seek", "yes"),
            ("volume-max", "150"),
            ("screenshot-format", "png"),
        ])?);
        for (name, format) in OBSERVED {
            mpv.observe(name, *format)?;
        }
        mpv.request_log_messages("warn");
        let (major, minor) = Mpv::api_version();
        log::info!("libmpv client API {major}.{minor}");

        let player = Player { mpv, session: Arc::new(Mutex::new(None)) };
        player.spawn_event_loop(app.clone());
        Ok(player)
    }

    fn spawn_event_loop<R: Runtime>(&self, app: AppHandle<R>) {
        let mpv = self.mpv.clone();
        let session = self.session.clone();
        std::thread::Builder::new()
            .name("mpv-events".into())
            .spawn(move || {
                let mut last_time_emit = Instant::now() - Duration::from_secs(1);
                loop {
                    let Some(ev) = mpv.wait_event(-1.0) else { continue };
                    let out = match ev {
                        Event::Shutdown => break,
                        Event::PropertyChange { name, value } => {
                            // time-pos changes every frame; ~5 Hz is plenty for the UI.
                            if name == "time-pos" {
                                if last_time_emit.elapsed() < Duration::from_millis(200) {
                                    continue;
                                }
                                last_time_emit = Instant::now();
                            }
                            if name == "track-list" || name == "video-params" {
                                Self::learn_tracks(&app, &session, &name, &value);
                            }
                            PlayerEvent::Prop { name, value }
                        }
                        Event::StartFile => PlayerEvent::StartFile,
                        Event::FileLoaded => {
                            if let Some(s) = session.lock().as_mut() {
                                s.loaded_at = Some(Instant::now());
                                s.ever_loaded = true;
                            }
                            PlayerEvent::FileLoaded
                        }
                        Event::PlaybackRestart | Event::Seek => {
                            last_time_emit = Instant::now() - Duration::from_secs(1);
                            if matches!(ev, Event::Seek) {
                                continue;
                            }
                            PlayerEvent::Restart
                        }
                        Event::EndFile { reason, error } => {
                            if let Some(attempt) = Self::maybe_reconnect(&mpv, &session, reason) {
                                PlayerEvent::Reconnecting { attempt }
                            } else {
                                PlayerEvent::EndFile { reason, error }
                            }
                        }
                        Event::Log { prefix, level, text } => {
                            let text = redact(&text);
                            log::debug!("mpv[{prefix}] {level}: {text}");
                            PlayerEvent::Log { level, prefix, text }
                        }
                        Event::Idle | Event::VideoReconfig | Event::Other => continue,
                    };
                    let _ = app.emit(EVENT, out);
                }
                log::info!("mpv event loop finished");
            })
            .expect("spawn mpv event thread");
    }

    /// Remembers the audio/subtitle/video tracks of the playing movie or
    /// episode (`media_info`, shown per version on the detail pages).
    fn learn_tracks<R: Runtime>(app: &AppHandle<R>, session: &Mutex<Option<Session>>, name: &str, value: &Value) {
        let (media, json) = {
            let mut guard = session.lock();
            let Some(s) = guard.as_mut() else { return };
            let Some(media) = s.options.media.clone() else { return };
            if name == "video-params" {
                s.hdr |= matches!(value["gamma"].as_str(), Some("pq" | "hlg"));
            } else {
                s.track_list = Some(value.clone());
            }
            let Some(summary) = s.track_list.as_ref().and_then(|t| crate::works::versions::summarize_tracks(t, s.hdr)) else {
                return;
            };
            let json = summary.to_string();
            if s.tracks_saved.as_deref() == Some(json.as_str()) {
                return;
            }
            s.tracks_saved = Some(json.clone());
            (media, json)
        };
        let app = app.clone();
        // off the mpv event thread: the database writer may be busy (sync)
        std::thread::spawn(move || {
            let Some(st) = app.try_state::<crate::state::AppState>() else { return };
            let conn = st.db.write();
            if let Err(e) = crate::works::versions::save_tracks(&conn, media.source_id, media.kind, &media.id, &json) {
                log::warn!("could not store tracks of {} {}: {e}", media.kind, media.id);
            }
        });
    }

    /// Live streams drop all the time; transparently re-open them a few times.
    fn maybe_reconnect(mpv: &Arc<Mpv>, session: &Mutex<Option<Session>>, reason: EndReason) -> Option<u32> {
        if !matches!(reason, EndReason::Eof | EndReason::Error) {
            return None;
        }
        let mut guard = session.lock();
        let s = guard.as_mut()?;
        // Never opened at all: try the same stream on a mirror server.
        if reason == EndReason::Error && !s.ever_loaded && !s.alternates.is_empty() {
            s.url = s.alternates.remove(0);
            s.reconnects += 1;
            let attempt = s.reconnects;
            log::info!("player: stream failed to open, trying mirror #{attempt}");
            let (url, opts) = (s.url.clone(), file_options(&s.options));
            drop(guard);
            mpv.command_async(&["loadfile", &url, "replace", "-1", &opts]).ok()?;
            return Some(attempt);
        }
        if !s.options.live {
            return None;
        }
        // A stream that played fine for a while earns a fresh retry budget.
        if s.loaded_at.is_some_and(|t| t.elapsed() > Duration::from_secs(30)) {
            s.reconnects = 0;
        }
        if s.reconnects >= 5 {
            if s.recording.take().is_some() {
                let _ = mpv.set_string("stream-record", "");
            }
            return None;
        }
        s.reconnects += 1;
        s.loaded_at = None;
        // keep recording, in a new part (mpv would overwrite the current one)
        if let Some(rec) = s.recording.as_mut() {
            rec.part += 1;
            let _ = mpv.set_string("stream-record", &rec.file().to_string_lossy());
        }
        let attempt = s.reconnects;
        let url = s.url.clone();
        let opts = file_options(&s.options);
        drop(guard);
        let mpv = mpv.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(500 * attempt as u64));
            let _ = mpv.command_async(&["loadfile", &url, "replace", "-1", &opts]);
        });
        Some(attempt)
    }

    pub fn load(&self, url: &str, options: LoadOptions) -> Result<(), mpv::MpvError> {
        self.load_with_fallbacks(url, Vec::new(), options)
    }

    pub fn load_with_fallbacks(
        &self,
        url: &str,
        alternates: Vec<String>,
        options: LoadOptions,
    ) -> Result<(), mpv::MpvError> {
        let opts = file_options(&options);
        // a new stream ends any recording (mpv would record it into the same file)
        let _ = self.mpv.set_string("stream-record", "");
        *self.session.lock() = Some(Session {
            url: url.to_owned(),
            options,
            alternates,
            ..Default::default()
        });
        self.mpv.command(&["loadfile", url, "replace", "-1", &opts])
    }

    /// Records the playing live stream into `dir/<title> <date time>.ts`.
    pub fn start_recording(&self, dir: &Path) -> Result<PathBuf, String> {
        let mut guard = self.session.lock();
        let s = guard.as_mut().filter(|s| s.options.live && s.ever_loaded).ok_or("No live channel is playing")?;
        if let Some(rec) = &s.recording {
            return Ok(rec.file());
        }
        let title = file_name_for(s.options.title.as_deref().unwrap_or(""));
        let stamp = chrono::Local::now().format("%Y-%m-%d %H.%M.%S");
        let rec = Recording { base: dir.join(format!("{title} {stamp}")), part: 1 };
        let file = rec.file();
        self.mpv.set_string("stream-record", &file.to_string_lossy()).map_err(|e| e.to_string())?;
        s.recording = Some(rec);
        Ok(file)
    }

    /// Stops recording; returns the last file written.
    pub fn stop_recording(&self) -> Option<PathBuf> {
        let file = self.session.lock().as_mut().and_then(|s| s.recording.take()).map(|r| r.file());
        let _ = self.mpv.set_string("stream-record", "");
        file
    }

    pub fn mpv(&self) -> &Mpv {
        &self.mpv
    }

    /// A stream is open (playing, paused, or held at its end).
    pub fn busy(&self) -> bool {
        self.session.lock().is_some() && self.mpv.get_json("idle-active") != Some(Value::Bool(true))
    }

    pub fn stop(&self) -> Result<(), mpv::MpvError> {
        *self.session.lock() = None;
        let _ = self.mpv.set_string("stream-record", "");
        self.mpv.command(&["stop"])
    }
}

/// Masks credentials in stream URLs mpv may log
/// (`/live/<user>/<pass>/…`, `username=…&password=…`).
pub fn redact(text: &str) -> String {
    static PATH: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"/(live|movie|series|timeshift)/[^/\s]+/[^/\s]+/").unwrap()
    });
    static QUERY: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"(?i)\b(username|password)=[^&\s]*").unwrap());
    let t = PATH.replace_all(text, "/$1/***/***/");
    QUERY.replace_all(&t, "$1=***").into_owned()
}

/// Per-file option string for `loadfile`.
pub fn file_options(o: &LoadOptions) -> String {
    let mut opts = vec![format!("pause={}", if o.paused { "yes" } else { "no" })];
    if let Some(start) = o.start.filter(|s| *s > 0.0) {
        opts.push(format!("start={start:.1}"));
    }
    if let Some(title) = &o.title {
        // mpv's key=value list escaping: %<len>%<text>
        opts.push(format!("force-media-title=%{}%{}", title.len(), title));
    }
    if let Some(ua) = &o.user_agent {
        opts.push(format!("user-agent=%{}%{}", ua.len(), ua));
    }
    if let Some(referrer) = &o.referrer {
        opts.push(format!("referrer=%{}%{}", referrer.len(), referrer));
    }
    if o.live {
        // Faster zapping: probe less, keep a modest buffer, no EOF hold.
        opts.extend([
            "demuxer-lavf-analyzeduration=1".to_owned(),
            "demuxer-lavf-probesize=1000000".to_owned(),
            "demuxer-readahead-secs=10".to_owned(),
            "keep-open=no".to_owned(),
        ]);
    }
    opts.join(",")
}

/// Creates the player and hooks its video surface into `window`.
pub fn init<R: Runtime>(app: &AppHandle<R>, window: &WebviewWindow<R>) -> Result<(), String> {
    let player = Player::new(app).map_err(|e| e.to_string())?;
    #[cfg(target_os = "linux")]
    linux::attach(window, player.mpv.clone()).map_err(|e| e.to_string())?;
    #[cfg(not(target_os = "linux"))]
    let _ = window;
    app.manage(player);
    Ok(())
}

// The webview only gets playback controls. mpv itself can spawn processes
// (`run`, `subprocess`), load scripts and write files (`stream-record`,
// `screenshot-to-file`, …); none of that is reachable through these commands.

/// Properties the UI may change.
const UI_PROPERTIES: &[&str] = &[
    "pause", "volume", "mute", "speed", "aid", "sid", "secondary-sid", "audio", "sub", "vid", "video",
    "sub-visibility", "sub-delay", "audio-delay", "sub-scale", "sub-pos", "video-aspect-override", "panscan",
    "video-zoom", "video-pan-x", "video-pan-y", "video-rotate", "video-margin-ratio-left", "video-margin-ratio-right",
    "video-margin-ratio-top", "video-margin-ratio-bottom", "brightness", "contrast", "saturation", "gamma", "hue",
    "deinterlace", "time-pos", "percent-pos", "chapter", "loop-file", "ab-loop-a", "ab-loop-b",
];

fn property_allowed(name: &str) -> bool {
    UI_PROPERTIES.contains(&name)
}

/// mpv commands the UI may run (property-changing ones only on `UI_PROPERTIES`).
fn command_allowed(args: &[String]) -> bool {
    match args.first().map(String::as_str) {
        Some("seek" | "revert-seek" | "frame-step" | "frame-back-step" | "sub-seek" | "sub-step") => true,
        Some("set" | "add" | "multiply" | "cycle" | "cycle-values") => args.get(1).is_some_and(|p| property_allowed(p)),
        _ => false,
    }
}

/// Properties that expose full stream URLs (with credentials) are not readable.
fn readable(name: &str) -> bool {
    !matches!(name, "path" | "stream-open-filename" | "stream-path") && !name.starts_with("playlist")
}

type Res<T> = Result<T, String>;

#[tauri::command]
pub fn player_load(player: State<'_, Player>, url: String, options: Option<LoadOptions>) -> Res<()> {
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err("only http(s) streams can be loaded".into());
    }
    player.load(&url, options.unwrap_or_default()).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn player_stop(player: State<'_, Player>) -> Res<()> {
    player.stop().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn player_command(player: State<'_, Player>, args: Vec<String>) -> Res<()> {
    if !command_allowed(&args) {
        return Err(format!("mpv command not allowed: {}", args.first().map_or("", String::as_str)));
    }
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    player.mpv.command(&args).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn player_set(player: State<'_, Player>, name: String, value: Value) -> Res<()> {
    if !property_allowed(&name) {
        return Err(format!("mpv property not allowed: {name}"));
    }
    player.mpv.set_json(&name, &value).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn player_get(player: State<'_, Player>, name: String) -> Option<Value> {
    readable(&name).then(|| player.mpv.get_json(&name)).flatten()
}

#[cfg(test)]
mod tests {
    use super::{command_allowed, property_allowed, readable, redact};

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn webview_gets_playback_controls_only() {
        // what the UI sends (src/pages/Player.tsx, src/stores/player.ts)
        assert!(command_allowed(&args(&["seek", "30", "relative"])));
        assert!(command_allowed(&args(&["cycle", "audio"])));
        assert!(command_allowed(&args(&["cycle", "sub"])));
        assert!(property_allowed("video-margin-ratio-left"));
        assert!(property_allowed("video-aspect-override"));
        // process spawning, scripts, file writes, arbitrary loads
        for bad in [&["run", "sh", "-c", "id"][..], &["subprocess"], &["load-script", "/tmp/x.lua"], &["loadfile", "file:///etc/passwd"]] {
            assert!(!command_allowed(&args(bad)), "{bad:?}");
        }
        assert!(!command_allowed(&args(&["set", "stream-record", "/home/u/.bashrc"])));
        assert!(!command_allowed(&args(&["cycle-values", "stream-record", "/tmp/a", ""])));
        assert!(!command_allowed(&args(&["no-osd", "seek", "10"])));
        assert!(!command_allowed(&[]));
        assert!(!property_allowed("input-ipc-server"));
        assert!(!readable("path") && !readable("playlist/0/filename") && readable("time-pos"));
    }

    #[test]
    fn recording_file_names() {
        assert_eq!(super::file_name_for("UK: BBC One / HD"), "UK BBC One HD");
        assert_eq!(super::file_name_for(" <?> "), "Live TV");
        assert_eq!(super::file_name_for(&"x".repeat(200)).len(), 80);
        let mut rec = super::Recording { base: "/v/BBC One 2026-09-26 14.00.00".into(), part: 1 };
        assert_eq!(rec.file().to_string_lossy(), "/v/BBC One 2026-09-26 14.00.00.ts");
        rec.part = 2;
        assert_eq!(rec.file().to_string_lossy(), "/v/BBC One 2026-09-26 14.00.00 (part 2).ts");
    }

    #[test]
    fn per_file_options_escape_values() {
        let o = super::LoadOptions {
            start: Some(300.0),
            title: Some("Tom, Jerry & Co".into()),
            user_agent: Some("Mozilla/5.0 (X11; Linux)".into()),
            referrer: Some("https://site.example/?a=1,b=2".into()),
            ..Default::default()
        };
        assert_eq!(
            super::file_options(&o),
            "pause=no,start=300.0,force-media-title=%15%Tom, Jerry & Co,user-agent=%24%Mozilla/5.0 (X11; Linux),\
             referrer=%29%https://site.example/?a=1,b=2"
        );
    }

    #[test]
    fn redacts_credentials() {
        assert_eq!(
            redact("Failed to open http://h.tv/live/alice/s3cret/123.ts."),
            "Failed to open http://h.tv/live/***/***/123.ts."
        );
        assert_eq!(
            redact("http://h.tv/player_api.php?username=alice&password=s3cret&action=x"),
            "http://h.tv/player_api.php?username=***&password=***&action=x"
        );
    }
}
