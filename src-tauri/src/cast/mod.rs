//! Chromecast (Google Cast): finds devices on the LAN (mDNS
//! `_googlecast._tcp`, and without it — routers that drop multicast — by
//! address, `probe.rs`), starts Google's Default Media Receiver on one and has
//! it play the current item from the relay (`relay.rs`) — never the provider
//! URL, which carries the account's credentials.
//!
//! One cast session at a time, run by a task that owns the device
//! connection; the UI drives it with `cast_*` commands and follows
//! `cast://status`. While a session runs, everything the user plays goes to
//! the TV (the frontend sends it here instead of to `play`).

pub mod castv2;
mod probe;
mod relay;

use std::collections::BTreeMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tauri::{AppHandle, Emitter, Manager, Runtime, State};
use tokio::sync::mpsc;

use crate::error::{Error, Result};
use crate::playback::{self, PlayRequest};
use crate::player::Player;
use crate::state::AppState;
use crate::{library, settings};
use castv2::{Message, NS_CONNECTION, NS_HEARTBEAT, NS_MEDIA, NS_RECEIVER, RECEIVER};

pub const DEVICES_EVENT: &str = "cast://devices";
/// TVs that worked before (`[{id, name, ip}]`), checked first next time.
const REMEMBERED: &str = "cast.devices";
/// The network sweep runs at most this often.
const SWEEP_EVERY: Duration = Duration::from_secs(60);
pub const STATUS_EVENT: &str = "cast://status";

/// Google's Default Media Receiver: no registration, plays MP4 by URL.
const DEFAULT_RECEIVER: &str = "CC1AD845";
const SERVICE: &str = "_googlecast._tcp.local.";
/// The TV never asked for the stream within this time: it can't reach us.
const UNREACHABLE_AFTER: Duration = Duration::from_secs(20);
/// A live channel paused longer than this restarts at "now" on resume.
const LIVE_PAUSE_LIMIT: Duration = Duration::from_secs(20);

/// The TV list and whether a search is still running (`cast://devices`).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Devices {
    pub devices: Vec<Device>,
    pub searching: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Remembered {
    id: String,
    name: String,
    ip: IpAddr,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Device {
    pub id: String,
    pub name: String,
    pub model: String,
    #[serde(skip)]
    addr: SocketAddr,
    #[serde(skip)]
    fullname: String,
}

/// What the TV shows about the item.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Meta {
    pub title: Option<String>,
    pub subtitle: Option<String>,
    /// artwork URL (http/https only)
    pub image: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub device_id: String,
    pub device_name: String,
    /// connecting | loading | buffering | playing | paused | ended | error (this
    /// item failed, the session goes on) | stopped (the session is over; `error`
    /// says why when it wasn't the user)
    pub state: &'static str,
    /// seconds into the item
    pub position: f64,
    /// 0 when unknown (live)
    pub duration: f64,
    pub live: bool,
    /// the TV's volume, 0…1
    pub volume: f64,
    pub muted: bool,
    pub error: Option<String>,
}

impl Status {
    fn finished(&self) -> bool {
        self.state == "stopped"
    }
}

enum Cmd {
    Load(Box<Item>),
    Play,
    Pause,
    Seek(f64),
    Volume(f64),
    Mute(bool),
    Stop,
    /// the relay's first packet of item `generation`, URL `n`
    Started(u64, u32, relay::Started),
}

struct Item {
    source: relay::Source,
    meta: Meta,
    start: f64,
}

struct Session {
    id: u64,
    device: Device,
    tx: mpsc::UnboundedSender<Cmd>,
    status: Arc<Mutex<Status>>,
}

pub struct Cast {
    relay: Option<relay::Relay>,
    devices: Arc<Mutex<BTreeMap<String, Device>>>,
    browser: Mutex<Option<mdns_sd::ServiceDaemon>>,
    searching: Arc<AtomicBool>,
    last_sweep: Mutex<Option<Instant>>,
    session: Mutex<Option<Session>>,
    next_id: Mutex<u64>,
}

pub fn init<R: Runtime>(app: &AppHandle<R>) {
    let relay = relay::Relay::start().map_err(|e| log::error!("cast relay unavailable: {e}")).ok();
    app.manage(Cast {
        relay,
        devices: Arc::default(),
        browser: Mutex::new(None),
        searching: Arc::default(),
        last_sweep: Mutex::new(None),
        session: Mutex::new(None),
        next_id: Mutex::new(0),
    });
}

impl Cast {
    /// A cast session is running (the provider stream belongs to it).
    pub fn busy(&self) -> bool {
        self.session.lock().as_ref().is_some_and(|s| !s.status.lock().finished())
    }

    /// Ends the session, if one runs.
    pub fn stop(&self) {
        if let Some(s) = self.session.lock().take() {
            let _ = s.tx.send(Cmd::Stop);
        }
    }

    fn send(&self, cmd: Cmd) -> Result<()> {
        let guard = self.session.lock();
        let s = guard.as_ref().ok_or_else(|| Error::msg("Not casting"))?;
        s.tx.send(cmd).map_err(|_| Error::msg("The cast session has ended"))
    }

    fn list(&self) -> Devices {
        let mut v: Vec<Device> = self.devices.lock().values().cloned().collect();
        v.sort_by_key(|d| d.name.to_lowercase());
        Devices { devices: v, searching: self.searching.load(Ordering::Relaxed) }
    }

    /// Adds or updates a device; true when the list changed.
    fn found(&self, d: Device) -> bool {
        let mut map = self.devices.lock();
        match map.get_mut(&d.id) {
            // an mDNS entry keeps its service name (for its removal event)
            Some(old) if old.name == d.name && old.addr == d.addr => false,
            Some(old) => {
                log::info!("cast: {} is now at {}", d.name, d.addr);
                old.name = d.name;
                old.addr = d.addr;
                true
            }
            None => {
                log::info!("cast: found {} at {}", d.name, d.addr);
                map.insert(d.id.clone(), d);
                true
            }
        }
    }
}

fn emit_devices<R: Runtime>(app: &AppHandle<R>) {
    if let Some(cast) = app.try_state::<Cast>() {
        let _ = app.emit(DEVICES_EVENT, cast.list());
    }
}

fn remembered(st: &AppState) -> Vec<Remembered> {
    serde_json::from_value(settings::get(&st.db.read(), REMEMBERED)).unwrap_or_default()
}

/// Keeps a TV that worked (or was added by hand) for next time.
fn remember(st: &AppState, d: &Device) {
    let mut list = remembered(st);
    list.retain(|r| r.id != d.id);
    list.insert(0, Remembered { id: d.id.clone(), name: d.name.clone(), ip: d.addr.ip() });
    list.truncate(16);
    if let Err(e) = settings::put(&st.db.write(), REMEMBERED, &json!(list)) {
        log::warn!("cast: cannot remember {}: {e}", d.name);
    }
}

// ------------------------------------------------------------ discovery

fn device_from(s: &mdns_sd::ResolvedService) -> Option<Device> {
    // capabilities bit 0: video out (speakers and speaker groups lack it)
    let ca: u32 = s.get_property_val_str("ca").and_then(|c| c.parse().ok()).unwrap_or(1);
    if ca & 1 == 0 {
        return None;
    }
    let ip = s.addresses.iter().map(|a| a.to_ip_addr()).find(|ip| ip.is_ipv4() && !ip.is_loopback())?;
    let id = probe::normalize_id(s.get_property_val_str("id").unwrap_or(&s.fullname));
    let name = s.get_property_val_str("fn").unwrap_or("Chromecast").to_owned();
    let model = s.get_property_val_str("md").unwrap_or("").to_owned();
    Some(Device { id, name, model, addr: SocketAddr::new(ip, s.port), fullname: s.fullname.clone() })
}

fn start_browsing<R: Runtime>(app: &AppHandle<R>, cast: &Cast) -> Result<()> {
    let mut browser = cast.browser.lock();
    if browser.is_some() {
        return Ok(());
    }
    let daemon = mdns_sd::ServiceDaemon::new().map_err(|e| Error::msg(format!("cannot search the network: {e}")))?;
    let events = daemon.browse(SERVICE).map_err(|e| Error::msg(format!("cannot search the network: {e}")))?;
    let devices = cast.devices.clone();
    let app = app.clone();
    std::thread::Builder::new().name("cast-discovery".into()).spawn(move || {
        while let Ok(event) = events.recv() {
            let changed = match event {
                mdns_sd::ServiceEvent::ServiceResolved(s) => match (device_from(&s), app.try_state::<Cast>()) {
                    (Some(d), Some(cast)) => {
                        let mut map = devices.lock();
                        match map.get_mut(&d.id) {
                            // found by address first: now it has a service name
                            Some(old) if old.fullname.is_empty() => {
                                old.fullname = d.fullname;
                                old.model = d.model;
                                false
                            }
                            _ => {
                                drop(map);
                                cast.found(d)
                            }
                        }
                    }
                    _ => false,
                },
                mdns_sd::ServiceEvent::ServiceRemoved(_, fullname) => {
                    let mut map = devices.lock();
                    let before = map.len();
                    map.retain(|_, d| d.fullname != fullname);
                    before != map.len()
                }
                _ => false,
            };
            if changed {
                emit_devices(&app);
            }
        }
    })?;
    *browser = Some(daemon);
    Ok(())
}

/// Looks for TVs by address: the remembered ones, then (at most once a
/// minute) a sweep of the local networks for the Cast port.
fn search<R: Runtime>(app: &AppHandle<R>, cast: &Cast) {
    if cast.searching.swap(true, Ordering::Relaxed) {
        return;
    }
    let sweep = cast.last_sweep.lock().is_none_or(|t| t.elapsed() > SWEEP_EVERY);
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let Some(st) = app.try_state::<AppState>().map(|s| s.inner().clone()) else { return };
        let probe_all = |ips: Vec<IpAddr>| {
            let app = app.clone();
            let http = st.http.clone();
            async move {
                let mut tasks = tokio::task::JoinSet::new();
                for ip in ips {
                    let http = http.clone();
                    tasks.spawn(async move { probe::probe(&http, ip).await });
                }
                while let Some(r) = tasks.join_next().await {
                    if let (Ok(Some(d)), Some(cast)) = (r, app.try_state::<Cast>())
                        && cast.found(d)
                    {
                        emit_devices(&app);
                    }
                }
            }
        };
        probe_all(remembered(&st).into_iter().map(|r| r.ip).collect()).await;
        if sweep {
            let networks = probe::local_networks();
            log::info!("cast: checking {} local network(s) for Cast devices", networks.len());
            let open = probe::sweep(&networks).await;
            probe_all(open).await;
            if let Some(cast) = app.try_state::<Cast>() {
                *cast.last_sweep.lock() = Some(Instant::now());
            }
        }
        if let Some(cast) = app.try_state::<Cast>() {
            cast.searching.store(false, Ordering::Relaxed);
        }
        emit_devices(&app);
    });
}

// ------------------------------------------------------------ session

struct Current {
    generation: u64,
    token: String,
    /// URL number: every (re)load of the item is a new one
    n: u32,
    live: bool,
    meta: Meta,
    /// item position at the start of URL `n`
    offset: f64,
    duration: f64,
    loaded_at: Instant,
    /// the relay answered for URL `n`
    started: bool,
    paused_at: Option<Instant>,
    /// live streams that ended are reloaded a few times
    reloads: u32,
}

struct Conn<R: Runtime> {
    app: AppHandle<R>,
    device: Device,
    w: castv2::Writer,
    local: IpAddr,
    tx: mpsc::UnboundedSender<Cmd>,
    request_id: u64,
    transport: Option<String>,
    app_session: Option<String>,
    media_session: Option<i64>,
    generation: u64,
    current: Option<Current>,
    queued: Option<Box<Item>>,
    /// receiver time of the last media status
    media_time: f64,
    status: Arc<Mutex<Status>>,
}

enum Flow {
    Continue,
    End,
}

impl<R: Runtime> Conn<R> {
    fn next_id(&mut self) -> u64 {
        self.request_id += 1;
        self.request_id
    }

    async fn send(&mut self, destination: &str, namespace: &str, payload: Value) -> std::io::Result<()> {
        self.w.send(&Message::new(destination, namespace, payload.to_string())).await
    }

    async fn request(&mut self, destination: &str, namespace: &str, mut payload: Value) -> std::io::Result<()> {
        payload["requestId"] = json!(self.next_id());
        self.send(destination, namespace, payload).await
    }

    async fn media(&mut self, kind: &str) -> std::io::Result<()> {
        let (Some(transport), Some(media)) = (self.transport.clone(), self.media_session) else { return Ok(()) };
        self.request(&transport, NS_MEDIA, json!({"type": kind, "mediaSessionId": media})).await
    }

    fn update(&self, f: impl FnOnce(&mut Status)) {
        let mut s = self.status.lock();
        let before = s.clone();
        f(&mut s);
        if *s != before {
            let _ = self.app.emit(STATUS_EVENT, s.clone());
        }
    }

    /// Publishes a new item on the relay and loads it.
    async fn load_item(&mut self, item: Box<Item>) -> std::io::Result<()> {
        let app = self.app.clone();
        let Some(cast) = app.try_state::<Cast>() else { return Ok(()) };
        let Some(relay) = cast.relay.as_ref() else { return Ok(()) };
        if let Some(old) = self.current.take() {
            relay.unpublish(&old.token);
        }
        self.generation += 1;
        let generation = self.generation;
        let tx = self.tx.clone();
        let live = item.source.live;
        let on_start: relay::OnStart = Arc::new(move |n, s| {
            let _ = tx.send(Cmd::Started(generation, n, s));
        });
        let token = relay.publish(item.source, self.device.addr.ip(), on_start);
        let _ = &cast; // the app borrow ends with this statement
        self.current = Some(Current {
            generation,
            token,
            n: 0,
            live,
            meta: item.meta,
            offset: 0.0,
            duration: 0.0,
            loaded_at: Instant::now(),
            started: false,
            paused_at: None,
            reloads: 0,
        });
        self.update(|s| {
            s.live = live;
            s.duration = 0.0;
            s.error = None;
        });
        self.load_at(item.start).await
    }

    /// (Re)loads the current item from `start` seconds under a new URL.
    async fn load_at(&mut self, start: f64) -> std::io::Result<()> {
        let Some(transport) = self.transport.clone() else { return Ok(()) };
        let app = self.app.clone();
        let Some(cast) = app.try_state::<Cast>() else { return Ok(()) };
        let Some(relay) = cast.relay.as_ref() else { return Ok(()) };
        let Some(cur) = self.current.as_mut() else { return Ok(()) };
        cur.n += 1;
        let start = if cur.live { 0.0 } else { start.max(0.0) };
        relay.position(&cur.token, cur.n, start);
        let url = relay.url(self.local, &cur.token, cur.n);
        cur.offset = start;
        cur.loaded_at = Instant::now();
        cur.started = false;
        cur.paused_at = None;
        let mut metadata = json!({"metadataType": 0, "title": cur.meta.title.clone().unwrap_or_default()});
        if let Some(sub) = cur.meta.subtitle.clone().filter(|s| !s.is_empty()) {
            metadata["subtitle"] = json!(sub);
        }
        if let Some(img) = cur.meta.image.clone().filter(|u| u.starts_with("http://") || u.starts_with("https://")) {
            metadata["images"] = json!([{ "url": img }]);
        }
        let payload = json!({
            "type": "LOAD",
            "sessionId": self.app_session,
            "autoplay": true,
            "currentTime": 0,
            "media": {
                "contentId": url,
                "contentUrl": url,
                "contentType": "video/mp4",
                "streamType": if cur.live { "LIVE" } else { "BUFFERED" },
                "metadata": metadata,
            },
        });
        let _ = &cast;
        self.media_time = 0.0;
        self.update(|s| {
            s.state = "loading";
            s.position = start;
        });
        self.request(&transport, NS_MEDIA, payload).await
    }

    fn position(&self) -> f64 {
        let Some(cur) = &self.current else { return 0.0 };
        let p = cur.offset + self.media_time;
        if cur.duration > 0.0 { p.min(cur.duration) } else { p }
    }

    fn relay_error(&self) -> Option<String> {
        let cur = self.current.as_ref()?;
        self.app.try_state::<Cast>()?.relay.as_ref()?.error(&cur.token)
    }

    async fn on_message(&mut self, m: Message) -> std::io::Result<Flow> {
        let p: Value = serde_json::from_str(&m.payload).unwrap_or(Value::Null);
        let kind = p["type"].as_str().unwrap_or("");
        match (m.namespace.as_str(), kind) {
            (NS_HEARTBEAT, "PING") => {
                self.send(&m.source, NS_HEARTBEAT, json!({"type": "PONG"})).await?;
            }
            (NS_CONNECTION, "CLOSE") if Some(&m.source) == self.transport.as_ref() => {
                self.update(|s| {
                    s.state = "stopped";
                    s.error = None;
                });
                return Ok(Flow::End);
            }
            (NS_RECEIVER, "RECEIVER_STATUS") => {
                let st = &p["status"];
                if let Some(level) = st["volume"]["level"].as_f64() {
                    let muted = st["volume"]["muted"].as_bool().unwrap_or(false);
                    self.update(|s| {
                        s.volume = level;
                        s.muted = muted;
                    });
                }
                let ours = st["applications"].as_array().and_then(|apps| {
                    apps.iter().find(|a| a["appId"].as_str() == Some(DEFAULT_RECEIVER)).map(|a| {
                        (a["transportId"].as_str().unwrap_or("").to_owned(), a["sessionId"].as_str().unwrap_or("").to_owned())
                    })
                });
                match (ours, &self.transport) {
                    (Some((transport, session)), None) if !transport.is_empty() => {
                        self.send(&transport, NS_CONNECTION, json!({"type": "CONNECT"})).await?;
                        self.transport = Some(transport);
                        self.app_session = Some(session);
                        if let Some(item) = self.queued.take() {
                            self.load_item(item).await?;
                        }
                    }
                    // another app took over the TV, or the remote closed ours
                    (None, Some(_)) => {
                        self.update(|s| {
                            s.state = "stopped";
                            s.error = None;
                        });
                        return Ok(Flow::End);
                    }
                    _ => {}
                }
            }
            (NS_RECEIVER, "LAUNCH_ERROR") => {
                self.update(|s| {
                    s.state = "stopped";
                    s.error = Some("The TV could not start its media player".into());
                });
                return Ok(Flow::End);
            }
            (NS_MEDIA, "MEDIA_STATUS") => {
                if let Some(ms) = p["status"].as_array().and_then(|a| a.first()) {
                    return self.on_media_status(ms.clone()).await;
                }
            }
            (NS_MEDIA, "LOAD_FAILED" | "INVALID_REQUEST") => {
                let error = self.relay_error().unwrap_or_else(|| "The TV could not play this stream".into());
                self.update(|s| {
                    s.state = "error";
                    s.error = Some(error);
                });
            }
            _ => {}
        }
        Ok(Flow::Continue)
    }

    async fn on_media_status(&mut self, ms: Value) -> std::io::Result<Flow> {
        if let Some(id) = ms["mediaSessionId"].as_i64() {
            self.media_session = Some(id);
        }
        if let Some(t) = ms["currentTime"].as_f64() {
            self.media_time = t;
        }
        let state = ms["playerState"].as_str().unwrap_or("");
        let reason = ms["idleReason"].as_str().unwrap_or("");
        let position = self.position();
        let live = self.current.as_ref().is_some_and(|c| c.live);
        match (state, reason) {
            ("PLAYING", _) => {
                if let Some(c) = self.current.as_mut() {
                    c.paused_at = None;
                }
                self.update(|s| {
                    s.state = "playing";
                    s.position = position;
                    s.error = None;
                });
            }
            ("PAUSED", _) => {
                if let Some(c) = self.current.as_mut() {
                    c.paused_at.get_or_insert_with(Instant::now);
                }
                self.update(|s| {
                    s.state = "paused";
                    s.position = position;
                });
            }
            ("BUFFERING", _) => self.update(|s| {
                s.state = "buffering";
                s.position = position;
            }),
            ("IDLE", "FINISHED" | "CANCELLED") if live => {
                // the channel's stream dropped: tune in again a few times
                let retry = self.current.as_mut().map(|c| {
                    c.reloads += 1;
                    c.reloads <= 3
                });
                if retry == Some(true) {
                    self.load_at(0.0).await?;
                } else {
                    // a stream that ended without an error of its own (e.g. a
                    // file, or a receiver that closed early) is still "playing"
                    let error = self.relay_error();
                    self.update(|s| match error {
                        Some(e) => {
                            s.state = "error";
                            s.error = Some(e);
                        }
                        None => {
                            s.state = "playing";
                            s.error = None;
                        }
                    });
                }
            }
            ("IDLE", "FINISHED") => self.update(|s| {
                s.state = "ended";
                s.position = s.duration.max(position);
            }),
            ("IDLE", "ERROR") => {
                let error = self.relay_error().unwrap_or_else(|| "The TV could not play this stream".into());
                self.update(|s| {
                    s.state = "error";
                    s.error = Some(error);
                });
            }
            _ => {}
        }
        Ok(Flow::Continue)
    }

    async fn on_cmd(&mut self, cmd: Cmd) -> std::io::Result<Flow> {
        match cmd {
            Cmd::Load(item) => {
                if self.transport.is_some() {
                    self.load_item(item).await?;
                } else {
                    self.queued = Some(item);
                }
            }
            Cmd::Play => {
                let stale = self
                    .current
                    .as_ref()
                    .is_some_and(|c| c.live && c.paused_at.is_some_and(|t| t.elapsed() > LIVE_PAUSE_LIMIT));
                let ended = self.status.lock().state == "ended";
                if stale || ended {
                    // live: back to "now"; VOD at the end: from the start
                    self.load_at(0.0).await?;
                } else {
                    self.media("PLAY").await?;
                }
            }
            Cmd::Pause => self.media("PAUSE").await?,
            Cmd::Seek(t) => {
                let Some(cur) = &self.current else { return Ok(Flow::Continue) };
                if !cur.live {
                    let end = if cur.duration > 0.0 { (cur.duration - 1.0).max(0.0) } else { f64::MAX };
                    self.load_at(t.clamp(0.0, end)).await?;
                }
            }
            Cmd::Volume(level) => {
                self.request(RECEIVER, NS_RECEIVER, json!({"type": "SET_VOLUME", "volume": {"level": level.clamp(0.0, 1.0)}}))
                    .await?;
                self.update(|s| s.volume = level.clamp(0.0, 1.0));
            }
            Cmd::Mute(muted) => {
                self.request(RECEIVER, NS_RECEIVER, json!({"type": "SET_VOLUME", "volume": {"muted": muted}})).await?;
                self.update(|s| s.muted = muted);
            }
            Cmd::Stop => {
                if let Some(session) = self.app_session.clone() {
                    let _ = self.request(RECEIVER, NS_RECEIVER, json!({"type": "STOP", "sessionId": session})).await;
                }
                self.update(|s| {
                    s.state = "stopped";
                    s.error = None;
                });
                return Ok(Flow::End);
            }
            Cmd::Started(generation, n, started) => {
                if let Some(cur) = self.current.as_mut().filter(|c| c.generation == generation && c.n == n) {
                    cur.started = true;
                    cur.offset = started.first;
                    if started.duration > 0.0 {
                        cur.duration = started.duration;
                    }
                    let (duration, position) = (cur.duration, self.position());
                    self.update(|s| {
                        s.duration = duration;
                        s.position = position;
                    });
                }
            }
        }
        Ok(Flow::Continue)
    }

    async fn on_tick(&mut self) -> std::io::Result<Flow> {
        let state = self.status.lock().state;
        if matches!(state, "playing" | "buffering") {
            self.media("GET_STATUS").await?;
        }
        let unreachable = self.current.as_ref().is_some_and(|c| {
            !c.started && state == "loading" && c.loaded_at.elapsed() > UNREACHABLE_AFTER
        });
        if unreachable {
            let error = self.relay_error().unwrap_or_else(|| {
                "The TV can't reach this computer. Make sure both are on the same network and no firewall blocks \
                 incoming connections."
                    .into()
            });
            self.update(|s| {
                s.state = "error";
                s.error = Some(error);
            });
            if let Some(c) = self.current.as_mut() {
                c.started = true; // report once
            }
        }
        Ok(Flow::Continue)
    }
}

async fn run<R: Runtime>(
    app: AppHandle<R>,
    device: Device,
    mut rx: mpsc::UnboundedReceiver<Cmd>,
    tx: mpsc::UnboundedSender<Cmd>,
    status: Arc<Mutex<Status>>,
) {
    let emit = |s: &Status| {
        let _ = app.emit(STATUS_EVENT, s.clone());
    };
    let (mut reader, writer, local) = match castv2::connect(device.addr).await {
        Ok(c) => c,
        Err(e) => {
            log::warn!("cast: cannot connect to {} at {}: {e}", device.name, device.addr);
            let mut s = status.lock();
            s.state = "stopped";
            s.error = Some(format!("Can't connect to {}", device.name));
            emit(&s);
            return;
        }
    };
    log::info!("cast: connected to {} at {}", device.name, device.addr);
    if let Some(st) = app.try_state::<AppState>() {
        remember(st.inner(), &device);
    }
    let mut c = Conn {
        app: app.clone(),
        device,
        w: writer,
        local,
        tx,
        request_id: 0,
        transport: None,
        app_session: None,
        media_session: None,
        generation: 0,
        current: None,
        queued: None,
        media_time: 0.0,
        status,
    };
    let setup = async {
        c.send(RECEIVER, NS_CONNECTION, json!({"type": "CONNECT"})).await?;
        c.request(RECEIVER, NS_RECEIVER, json!({"type": "LAUNCH", "appId": DEFAULT_RECEIVER})).await
    };
    let mut result = setup.await;

    let mut heartbeat = tokio::time::interval(Duration::from_secs(5));
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    let mut last_heard = Instant::now();
    while result.is_ok() {
        let flow = tokio::select! {
            m = reader.next() => match m {
                Ok(m) => {
                    last_heard = Instant::now();
                    c.on_message(m).await
                }
                Err(e) => Err(e),
            },
            cmd = rx.recv() => match cmd {
                Some(cmd) => c.on_cmd(cmd).await,
                None => Ok(Flow::End),
            },
            _ = heartbeat.tick() => {
                if last_heard.elapsed() > Duration::from_secs(20) {
                    Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "the TV stopped answering"))
                } else {
                    c.send(RECEIVER, NS_HEARTBEAT, json!({"type": "PING"})).await.map(|_| Flow::Continue)
                }
            }
            _ = tick.tick() => c.on_tick().await,
        };
        match flow {
            Ok(Flow::Continue) => {}
            Ok(Flow::End) => break,
            Err(e) => result = Err(e),
        }
    }
    if let Err(e) = result {
        log::warn!("cast: connection to {} lost: {e}", c.device.name);
        c.update(|s| {
            if !s.finished() {
                s.state = "stopped";
                s.error = Some(format!("Lost the connection to {}", c.device.name));
            }
        });
    }
    if let Some(cur) = c.current.take()
        && let Some(relay) = app.try_state::<Cast>().as_ref().and_then(|cast| cast.relay.as_ref())
    {
        relay.unpublish(&cur.token);
    }
    if let Some(transport) = c.transport.clone() {
        let _ = c.send(&transport, NS_CONNECTION, json!({"type": "CLOSE"})).await;
    }
    let _ = c.send(RECEIVER, NS_CONNECTION, json!({"type": "CLOSE"})).await;
    log::info!("cast: session with {} ended", c.device.name);
}

// ------------------------------------------------------------ commands

/// The cast devices found so far; the first call starts looking
/// (updates follow as `cast://devices`).
#[tauri::command]
pub fn cast_devices<R: Runtime>(app: AppHandle<R>, cast: State<'_, Cast>) -> Result<Devices> {
    if cast.relay.is_none() {
        return Err(Error::msg("Casting is unavailable: no network port could be opened"));
    }
    // mDNS is optional: the search by address finds TVs without it
    if let Err(e) = start_browsing(&app, &cast) {
        log::warn!("cast: {e}");
    }
    search(&app, &cast);
    Ok(cast.list())
}

/// Adds a TV by its IP address (for networks where it can't be found).
#[tauri::command]
pub async fn cast_add_device<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
    cast: State<'_, Cast>,
    address: String,
) -> Result<Device> {
    let ip = probe::parse_address(&address)
        .ok_or_else(|| Error::msg("Enter the TV's IP address, like 192.168.1.20"))?;
    let device = probe::probe(&state.http, ip)
        .await
        .ok_or_else(|| Error::msg(format!("No Chromecast answered at {ip}. Check the address in the Google Home app.")))?;
    remember(state.inner(), &device);
    if cast.found(device.clone()) {
        emit_devices(&app);
    }
    Ok(device)
}

/// Plays `req` on a TV: on `device_id` (starting a session there), or on the
/// TV already being cast to. Local playback stops — the account streams to
/// one place.
#[tauri::command]
pub async fn cast_play<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
    player: State<'_, Player>,
    cast: State<'_, Cast>,
    device_id: Option<String>,
    req: PlayRequest,
    meta: Option<Meta>,
) -> Result<()> {
    let st = state.inner().clone();
    let (stream, audio_lang) = {
        let conn = st.db.read();
        (playback::resolve(&conn, &req)?, settings::get_str(&conn, "player.audioLang"))
    };
    if let Some((title, logo)) = stream.channel.clone() {
        library::touch_channel(&st.db.write(), req.source_id, &req.id, &title, logo.as_deref())?;
    }
    if crate::probe::running() {
        tokio::task::spawn_blocking(crate::probe::cancel).await.map_err(|e| Error::msg(e.to_string()))?;
    }
    let _ = player.stop();

    let mut urls = vec![stream.url];
    urls.extend(stream.alternates);
    let mut meta = meta.unwrap_or_default();
    meta.title = meta.title.or(req.title.clone());
    let item = Box::new(Item {
        source: relay::Source {
            urls,
            user_agent: stream.user_agent,
            referrer: stream.referrer,
            live: stream.live,
            audio_lang,
        },
        meta,
        start: req.start.unwrap_or(0.0),
    });

    let mut session = cast.session.lock();
    let running = session.as_ref().filter(|s| !s.status.lock().finished());
    let reuse = match (&device_id, running) {
        (None, Some(_)) => true,
        (Some(id), Some(s)) => &s.device.id == id,
        (None, None) => return Err(Error::msg("Not casting")),
        (Some(_), None) => false,
    };
    if reuse {
        let s = session.as_ref().expect("running session");
        log::info!("cast: {} {}:{} to {}", req.kind, req.source_id, req.id, s.device.name);
        return s.tx.send(Cmd::Load(item)).map_err(|_| Error::msg("The cast session has ended"));
    }

    let id = device_id.unwrap_or_default();
    let device = cast.devices.lock().get(&id).cloned().ok_or_else(|| Error::msg("That TV is no longer on the network"))?;
    if let Some(old) = session.take() {
        let _ = old.tx.send(Cmd::Stop);
    }
    let (tx, rx) = mpsc::unbounded_channel();
    let status = Arc::new(Mutex::new(Status {
        device_id: device.id.clone(),
        device_name: device.name.clone(),
        state: "connecting",
        live: item.source.live,
        volume: 1.0,
        ..Default::default()
    }));
    let _ = app.emit(STATUS_EVENT, status.lock().clone());
    log::info!("cast: {} {}:{} to {}", req.kind, req.source_id, req.id, device.name);
    let _ = tx.send(Cmd::Load(item));
    let sid = {
        let mut n = cast.next_id.lock();
        *n += 1;
        *n
    };
    *session = Some(Session { id: sid, device: device.clone(), tx: tx.clone(), status: status.clone() });
    drop(session);
    let app2 = app.clone();
    tauri::async_runtime::spawn(async move {
        run(app2.clone(), device, rx, tx, status).await;
        if let Some(cast) = app2.try_state::<Cast>() {
            let mut s = cast.session.lock();
            if s.as_ref().is_some_and(|s| s.id == sid) {
                *s = None;
            }
        }
    });
    Ok(())
}

/// play | pause | seek (seconds) | volume (0…1) | mute (0/1)
#[tauri::command]
pub fn cast_control(cast: State<'_, Cast>, action: String, value: Option<f64>) -> Result<()> {
    let v = value.unwrap_or(0.0);
    let cmd = match action.as_str() {
        "play" => Cmd::Play,
        "pause" => Cmd::Pause,
        "seek" => Cmd::Seek(v),
        "volume" => Cmd::Volume(v),
        "mute" => Cmd::Mute(v != 0.0),
        other => return Err(Error::msg(format!("unknown cast action {other}"))),
    };
    cast.send(cmd)
}

/// Ends casting (the TV's player closes).
#[tauri::command]
pub fn cast_stop(cast: State<'_, Cast>) -> Result<()> {
    cast.stop();
    Ok(())
}

/// The running session, if any.
#[tauri::command]
pub fn cast_status(cast: State<'_, Cast>) -> Option<Status> {
    cast.session.lock().as_ref().map(|s| s.status.lock().clone()).filter(|s| !s.finished())
}
