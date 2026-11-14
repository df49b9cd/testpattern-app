//! One cast session: the task that owns the device connection, loads the
//! item, follows the TV's media status and carries out the UI's commands.

use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde_json::{Value, json};
use tauri::{AppHandle, Emitter, Manager, Runtime};
use tokio::sync::mpsc;

use crate::state::AppState;

use super::castv2::{Message, NS_CONNECTION, NS_HEARTBEAT, NS_MEDIA, NS_RECEIVER, RECEIVER};
use super::{
    Cast, CastState, Cmd, DEFAULT_RECEIVER, Device, Item, LIVE_PAUSE_LIMIT, Meta, STATUS_EVENT,
    Status, UNREACHABLE_AFTER, castv2, relay, remember,
};

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

    async fn send(
        &mut self,
        destination: &str,
        namespace: &str,
        payload: Value,
    ) -> std::io::Result<()> {
        self.w
            .send(&Message::new(destination, namespace, payload.to_string()))
            .await
    }

    async fn request(
        &mut self,
        destination: &str,
        namespace: &str,
        mut payload: Value,
    ) -> std::io::Result<()> {
        payload["requestId"] = json!(self.next_id());
        self.send(destination, namespace, payload).await
    }

    async fn media(&mut self, kind: &str) -> std::io::Result<()> {
        let (Some(transport), Some(media)) = (self.transport.clone(), self.media_session) else {
            return Ok(());
        };
        self.request(
            &transport,
            NS_MEDIA,
            json!({"type": kind, "mediaSessionId": media}),
        )
        .await
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
        let Some(cast) = app.try_state::<Cast>() else {
            return Ok(());
        };
        let Some(relay) = cast.relay.as_ref() else {
            return Ok(());
        };
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
        let Some(transport) = self.transport.clone() else {
            return Ok(());
        };
        let app = self.app.clone();
        let Some(cast) = app.try_state::<Cast>() else {
            return Ok(());
        };
        let Some(relay) = cast.relay.as_ref() else {
            return Ok(());
        };
        let Some(cur) = self.current.as_mut() else {
            return Ok(());
        };
        cur.n += 1;
        let start = if cur.live { 0.0 } else { start.max(0.0) };
        relay.position(&cur.token, cur.n, start);
        let url = relay.url(self.local, &cur.token, cur.n);
        cur.offset = start;
        cur.loaded_at = Instant::now();
        cur.started = false;
        cur.paused_at = None;
        let mut metadata =
            json!({"metadataType": 0, "title": cur.meta.title.clone().unwrap_or_default()});
        if let Some(sub) = cur.meta.subtitle.clone().filter(|s| !s.is_empty()) {
            metadata["subtitle"] = json!(sub);
        }
        if let Some(img) = cur
            .meta
            .image
            .clone()
            .filter(|u| u.starts_with("http://") || u.starts_with("https://"))
        {
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
        self.media_time = 0.0;
        self.update(|s| {
            s.state = CastState::Loading;
            s.position = start;
        });
        self.request(&transport, NS_MEDIA, payload).await
    }

    fn position(&self) -> f64 {
        let Some(cur) = &self.current else { return 0.0 };
        let p = cur.offset + self.media_time;
        if cur.duration > 0.0 {
            p.min(cur.duration)
        } else {
            p
        }
    }

    fn relay_error(&self) -> Option<String> {
        let cur = self.current.as_ref()?;
        self.app
            .try_state::<Cast>()?
            .relay
            .as_ref()?
            .error(&cur.token)
    }

    async fn on_message(&mut self, m: Message) -> std::io::Result<Flow> {
        let p: Value = serde_json::from_str(&m.payload).unwrap_or(Value::Null);
        let kind = p["type"].as_str().unwrap_or("");
        match (m.namespace.as_str(), kind) {
            (NS_HEARTBEAT, "PING") => {
                self.send(&m.source, NS_HEARTBEAT, json!({"type": "PONG"}))
                    .await?;
            }
            (NS_CONNECTION, "CLOSE") if Some(&m.source) == self.transport.as_ref() => {
                self.update(|s| {
                    s.state = CastState::Stopped;
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
                    apps.iter()
                        .find(|a| a["appId"].as_str() == Some(DEFAULT_RECEIVER))
                        .map(|a| {
                            (
                                a["transportId"].as_str().unwrap_or("").to_owned(),
                                a["sessionId"].as_str().unwrap_or("").to_owned(),
                            )
                        })
                });
                match (ours, &self.transport) {
                    (Some((transport, session)), None) if !transport.is_empty() => {
                        self.send(&transport, NS_CONNECTION, json!({"type": "CONNECT"}))
                            .await?;
                        self.transport = Some(transport);
                        self.app_session = Some(session);
                        if let Some(item) = self.queued.take() {
                            self.load_item(item).await?;
                        }
                    }
                    // another app took over the TV, or the remote closed ours
                    (None, Some(_)) => {
                        self.update(|s| {
                            s.state = CastState::Stopped;
                            s.error = None;
                        });
                        return Ok(Flow::End);
                    }
                    _ => {}
                }
            }
            (NS_RECEIVER, "LAUNCH_ERROR") => {
                self.update(|s| {
                    s.state = CastState::Stopped;
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
                let error = self
                    .relay_error()
                    .unwrap_or_else(|| "The TV could not play this stream".into());
                self.update(|s| {
                    s.state = CastState::Error;
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
        let position = self.position();
        let (live, reloads) = self
            .current
            .as_ref()
            .map_or((false, 0), |c| (c.live, c.reloads));
        let player_state = ms["playerState"].as_str().unwrap_or("");
        let reason = ms["idleReason"].as_str().unwrap_or("");
        match media_transition(player_state, reason, live, reloads) {
            Transition::None => {}
            Transition::Playing => {
                if let Some(c) = self.current.as_mut() {
                    c.paused_at = None;
                }
                self.update(|s| {
                    s.state = CastState::Playing;
                    s.position = position;
                    s.error = None;
                });
            }
            Transition::Paused => {
                if let Some(c) = self.current.as_mut() {
                    c.paused_at.get_or_insert_with(Instant::now);
                }
                self.update(|s| {
                    s.state = CastState::Paused;
                    s.position = position;
                });
            }
            Transition::Buffering => self.update(|s| {
                s.state = CastState::Buffering;
                s.position = position;
            }),
            Transition::ReloadLive => {
                if let Some(c) = self.current.as_mut() {
                    c.reloads += 1;
                }
                self.load_at(0.0).await?;
            }
            Transition::LiveGaveUp => {
                if let Some(c) = self.current.as_mut() {
                    c.reloads += 1;
                }
                // a stream that ended without an error of its own (e.g. a
                // file, or a receiver that closed early) is still "playing"
                let error = self.relay_error();
                self.update(|s| match error {
                    Some(e) => {
                        s.state = CastState::Error;
                        s.error = Some(e);
                    }
                    None => {
                        s.state = CastState::Playing;
                        s.error = None;
                    }
                });
            }
            Transition::Ended => self.update(|s| {
                s.state = CastState::Ended;
                s.position = s.duration.max(position);
            }),
            Transition::Failed => {
                let error = self
                    .relay_error()
                    .unwrap_or_else(|| "The TV could not play this stream".into());
                self.update(|s| {
                    s.state = CastState::Error;
                    s.error = Some(error);
                });
            }
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
                let stale = self.current.as_ref().is_some_and(|c| {
                    c.live && c.paused_at.is_some_and(|t| t.elapsed() > LIVE_PAUSE_LIMIT)
                });
                let ended = self.status.lock().state == CastState::Ended;
                if stale || ended {
                    // live: back to "now"; VOD at the end: from the start
                    self.load_at(0.0).await?;
                } else {
                    self.media("PLAY").await?;
                }
            }
            Cmd::Pause => self.media("PAUSE").await?,
            Cmd::Seek(t) => {
                let Some(cur) = &self.current else {
                    return Ok(Flow::Continue);
                };
                if !cur.live {
                    let end = if cur.duration > 0.0 {
                        (cur.duration - 1.0).max(0.0)
                    } else {
                        f64::MAX
                    };
                    self.load_at(t.clamp(0.0, end)).await?;
                }
            }
            Cmd::Volume(level) => {
                self.request(
                    RECEIVER,
                    NS_RECEIVER,
                    json!({"type": "SET_VOLUME", "volume": {"level": level.clamp(0.0, 1.0)}}),
                )
                .await?;
                self.update(|s| s.volume = level.clamp(0.0, 1.0));
            }
            Cmd::Mute(muted) => {
                self.request(
                    RECEIVER,
                    NS_RECEIVER,
                    json!({"type": "SET_VOLUME", "volume": {"muted": muted}}),
                )
                .await?;
                self.update(|s| s.muted = muted);
            }
            Cmd::Stop => {
                if let Some(session) = self.app_session.clone() {
                    let _ = self
                        .request(
                            RECEIVER,
                            NS_RECEIVER,
                            json!({"type": "STOP", "sessionId": session}),
                        )
                        .await;
                }
                self.update(|s| {
                    s.state = CastState::Stopped;
                    s.error = None;
                });
                return Ok(Flow::End);
            }
            Cmd::Started(generation, n, started) => {
                if let Some(cur) = self
                    .current
                    .as_mut()
                    .filter(|c| c.generation == generation && c.n == n)
                {
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
        if matches!(state, CastState::Playing | CastState::Buffering) {
            self.media("GET_STATUS").await?;
        }
        let unreachable = self.current.as_ref().is_some_and(|c| {
            !c.started && state == CastState::Loading && c.loaded_at.elapsed() > UNREACHABLE_AFTER
        });
        if unreachable {
            let error = self.relay_error().unwrap_or_else(|| {
                "The TV can't reach this computer. Make sure both are on the same network and no firewall blocks \
                 incoming connections."
                    .into()
            });
            self.update(|s| {
                s.state = CastState::Error;
                s.error = Some(error);
            });
            if let Some(c) = self.current.as_mut() {
                c.started = true; // report once
            }
        }
        Ok(Flow::Continue)
    }
}

pub(super) async fn run<R: Runtime>(
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
            log::warn!(
                "cast: cannot connect to {} at {}: {e}",
                device.name,
                device.addr
            );
            let mut s = status.lock();
            s.state = CastState::Stopped;
            s.error = Some(format!("Can't connect to {}", device.name));
            emit(&s);
            return;
        }
    };
    log::info!("cast: connected to {} at {}", device.name, device.addr);
    if let Some(st) = app.try_state::<AppState>() {
        remember(st.inner(), &device).await;
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
        c.send(RECEIVER, NS_CONNECTION, json!({"type": "CONNECT"}))
            .await?;
        c.request(
            RECEIVER,
            NS_RECEIVER,
            json!({"type": "LAUNCH", "appId": DEFAULT_RECEIVER}),
        )
        .await
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
                s.state = CastState::Stopped;
                s.error = Some(format!("Lost the connection to {}", c.device.name));
            }
        });
    }
    if let Some(cur) = c.current.take()
        && let Some(relay) = app
            .try_state::<Cast>()
            .as_ref()
            .and_then(|cast| cast.relay.as_ref())
    {
        relay.unpublish(&cur.token);
    }
    if let Some(transport) = c.transport.clone() {
        let _ = c
            .send(&transport, NS_CONNECTION, json!({"type": "CLOSE"}))
            .await;
    }
    let _ = c
        .send(RECEIVER, NS_CONNECTION, json!({"type": "CLOSE"}))
        .await;
    log::info!("cast: session with {} ended", c.device.name);
}

/// Times a live channel that dropped is tuned in again before giving up.
const LIVE_RELOADS: u32 = 3;

/// What a receiver `MEDIA_STATUS` means for the session.
#[derive(Debug, PartialEq, Eq)]
enum Transition {
    None,
    Playing,
    Paused,
    Buffering,
    /// the live stream dropped: tune in again
    ReloadLive,
    /// the live stream dropped once too often
    LiveGaveUp,
    Ended,
    Failed,
}

/// `playerState`/`idleReason` of a media status → what to do. `reloads` is
/// how often the current live item was tuned in again already.
fn media_transition(player_state: &str, idle_reason: &str, live: bool, reloads: u32) -> Transition {
    match (player_state, idle_reason) {
        ("PLAYING", _) => Transition::Playing,
        ("PAUSED", _) => Transition::Paused,
        ("BUFFERING", _) => Transition::Buffering,
        ("IDLE", "FINISHED" | "CANCELLED") if live => {
            if reloads < LIVE_RELOADS {
                Transition::ReloadLive
            } else {
                Transition::LiveGaveUp
            }
        }
        ("IDLE", "FINISHED") => Transition::Ended,
        ("IDLE", "ERROR") => Transition::Failed,
        _ => Transition::None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_status_transitions() {
        assert_eq!(
            media_transition("PLAYING", "", false, 0),
            Transition::Playing
        );
        assert_eq!(media_transition("PAUSED", "", true, 0), Transition::Paused);
        assert_eq!(
            media_transition("BUFFERING", "", false, 0),
            Transition::Buffering
        );
        assert_eq!(
            media_transition("IDLE", "FINISHED", false, 0),
            Transition::Ended
        );
        assert_eq!(
            media_transition("IDLE", "CANCELLED", false, 0),
            Transition::None
        );
        assert_eq!(
            media_transition("IDLE", "ERROR", false, 0),
            Transition::Failed
        );
        assert_eq!(
            media_transition("IDLE", "ERROR", true, 0),
            Transition::Failed
        );
        assert_eq!(media_transition("", "", false, 0), Transition::None);
    }

    #[test]
    fn a_dropped_live_stream_is_reloaded_three_times() {
        let attempts: Vec<Transition> = (0..=LIVE_RELOADS)
            .map(|reloads| media_transition("IDLE", "FINISHED", true, reloads))
            .collect();
        assert_eq!(
            attempts,
            vec![
                Transition::ReloadLive,
                Transition::ReloadLive,
                Transition::ReloadLive,
                Transition::LiveGaveUp
            ]
        );
        assert_eq!(
            media_transition("IDLE", "CANCELLED", true, 0),
            Transition::ReloadLive,
            "a receiver that cancels a live stream is retried too"
        );
    }
}
