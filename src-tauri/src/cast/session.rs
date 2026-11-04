//! One cast session: the task that owns the device connection, loads the
//! item, follows the TV's media status and carries out the UI's commands.

use super::*;

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
                let error = self
                    .relay_error()
                    .unwrap_or_else(|| "The TV could not play this stream".into());
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
                let error = self
                    .relay_error()
                    .unwrap_or_else(|| "The TV could not play this stream".into());
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
                let stale = self.current.as_ref().is_some_and(|c| {
                    c.live && c.paused_at.is_some_and(|t| t.elapsed() > LIVE_PAUSE_LIMIT)
                });
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
                    s.state = "stopped";
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
            s.state = "stopped";
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
                s.state = "stopped";
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
