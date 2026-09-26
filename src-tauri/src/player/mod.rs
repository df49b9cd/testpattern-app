//! Native playback engine (libmpv). The web UI drives it through the
//! `player_*` commands and listens to `player://event` for state changes.

#[cfg(target_os = "linux")]
mod linux;
pub mod mpv;
mod mpv_sys;

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
    #[serde(default)]
    pub paused: bool,
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
            return None;
        }
        s.reconnects += 1;
        s.loaded_at = None;
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
        *self.session.lock() = Some(Session {
            url: url.to_owned(),
            options,
            alternates,
            ..Default::default()
        });
        self.mpv.command(&["loadfile", url, "replace", "-1", &opts])
    }

    pub fn mpv(&self) -> &Mpv {
        &self.mpv
    }

    pub fn stop(&self) -> Result<(), mpv::MpvError> {
        *self.session.lock() = None;
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
fn file_options(o: &LoadOptions) -> String {
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
