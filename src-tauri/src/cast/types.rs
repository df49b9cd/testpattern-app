//! The cast session's shared shapes: devices, status, the commands the
//! session task takes, and the managed `Cast` state.

use crate::error::{Error, Result};
use crate::settings;
use crate::state::{AppState, write_blocking};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager, Runtime};
use tokio::sync::mpsc;

use super::relay;

pub const DEVICES_EVENT: &str = "cast://devices";
/// TVs that worked before (`[{id, name, ip}]`), checked first next time.
pub(super) const REMEMBERED: &str = "cast.devices";
/// The network sweep runs at most this often.
pub(super) const SWEEP_EVERY: Duration = Duration::from_secs(60);
pub const STATUS_EVENT: &str = "cast://status";

/// Google's Default Media Receiver: no registration, plays MP4 by URL.
pub(super) const DEFAULT_RECEIVER: &str = "CC1AD845";
pub(super) const SERVICE: &str = "_googlecast._tcp.local.";
/// The TV never asked for the stream within this time: it can't reach us.
pub(super) const UNREACHABLE_AFTER: Duration = Duration::from_secs(20);
/// A live channel paused longer than this restarts at "now" on resume.
pub(super) const LIVE_PAUSE_LIMIT: Duration = Duration::from_secs(20);

/// The TV list and whether a search is still running (`cast://devices`).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Devices {
    pub devices: Vec<Device>,
    pub searching: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct Remembered {
    pub(super) id: String,
    pub(super) name: String,
    pub(super) ip: IpAddr,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Device {
    pub id: String,
    pub name: String,
    pub model: String,
    #[serde(skip)]
    pub(super) addr: SocketAddr,
    #[serde(skip)]
    pub(super) fullname: String,
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

/// Where a cast session is (`cast://status`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CastState {
    #[default]
    Connecting,
    Loading,
    Buffering,
    Playing,
    Paused,
    Ended,
    /// this item failed; the session goes on
    Error,
    /// the session is over; `Status::error` says why when it wasn't the user
    Stopped,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub device_id: String,
    pub device_name: String,
    pub state: CastState,
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
    pub(super) fn finished(&self) -> bool {
        self.state == CastState::Stopped
    }
}

pub(super) enum Cmd {
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

pub(super) struct Item {
    pub(super) source: relay::Source,
    pub(super) meta: Meta,
    pub(super) start: f64,
}

pub(super) struct Session {
    pub(super) id: u64,
    pub(super) device: Device,
    pub(super) tx: mpsc::UnboundedSender<Cmd>,
    pub(super) status: Arc<Mutex<Status>>,
}

pub struct Cast {
    pub(super) relay: Option<relay::Relay>,
    pub(super) devices: Arc<Mutex<BTreeMap<String, Device>>>,
    pub(super) browser: Mutex<Option<mdns_sd::ServiceDaemon>>,
    pub(super) searching: Arc<AtomicBool>,
    pub(super) last_sweep: Mutex<Option<Instant>>,
    pub(super) session: Mutex<Option<Session>>,
    pub(super) next_id: Mutex<u64>,
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

    pub(super) fn send(&self, cmd: Cmd) -> Result<()> {
        let guard = self.session.lock();
        let s = guard
            .as_ref()
            .ok_or_else(|| Error::msg("Not casting".into()))?;
        s.tx.send(cmd)
            .map_err(|_| Error::msg("The cast session has ended".into()))
    }

    pub(super) fn list(&self) -> Devices {
        let mut v: Vec<Device> = self.devices.lock().values().cloned().collect();
        v.sort_by_key(|d| d.name.to_lowercase());
        Devices {
            devices: v,
            searching: self.searching.load(Ordering::Relaxed),
        }
    }

    /// Adds or updates a device; true when the list changed.
    pub(super) fn found(&self, d: Device) -> bool {
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

pub(super) fn emit_devices<R: Runtime>(app: &AppHandle<R>) {
    if let Some(cast) = app.try_state::<Cast>() {
        let _ = app.emit(DEVICES_EVENT, cast.list());
    }
}

pub(super) fn remembered(conn: &rusqlite::Connection) -> Vec<Remembered> {
    serde_json::from_value(settings::get(conn, REMEMBERED)).unwrap_or_default()
}

/// Keeps a TV that worked (or was added by hand) for next time.
pub(super) async fn remember(st: &AppState, d: &Device) {
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
