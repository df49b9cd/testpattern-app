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
mod commands;
mod discovery;
mod probe;
mod relay;
mod session;

pub use commands::*;
use discovery::{search, start_browsing};
use session::run;

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
use crate::settings;
use crate::state::{AppState, read_blocking, write_blocking};
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
    let relay = relay::Relay::start()
        .map_err(|e| log::error!("cast relay unavailable: {e}"))
        .ok();
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
        self.session
            .lock()
            .as_ref()
            .is_some_and(|s| !s.status.lock().finished())
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
        s.tx.send(cmd)
            .map_err(|_| Error::msg("The cast session has ended"))
    }

    fn list(&self) -> Devices {
        let mut v: Vec<Device> = self.devices.lock().values().cloned().collect();
        v.sort_by_key(|d| d.name.to_lowercase());
        Devices {
            devices: v,
            searching: self.searching.load(Ordering::Relaxed),
        }
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

fn remembered(conn: &rusqlite::Connection) -> Vec<Remembered> {
    serde_json::from_value(settings::get(conn, REMEMBERED)).unwrap_or_default()
}

/// Keeps a TV that worked (or was added by hand) for next time.
async fn remember(st: &AppState, d: &Device) {
    let entry = Remembered {
        id: d.id.clone(),
        name: d.name.clone(),
        ip: d.addr.ip(),
    };
    let saved = write_blocking(st, move |conn| {
        let mut list = remembered(conn);
        list.retain(|r| r.id != entry.id);
        list.insert(0, entry);
        list.truncate(16);
        settings::put(conn, REMEMBERED, &json!(list))
    })
    .await;
    if let Err(e) = saved {
        log::warn!("cast: cannot remember {}: {e}", d.name);
    }
}
