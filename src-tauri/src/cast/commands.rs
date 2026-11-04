//! The `cast_*` commands the UI drives casting with.

use super::*;

/// The cast devices found so far; the first call starts looking
/// (updates follow as `cast://devices`).
#[tauri::command]
pub fn cast_devices<R: Runtime>(app: AppHandle<R>, cast: State<'_, Cast>) -> Result<Devices> {
    if cast.relay.is_none() {
        return Err(Error::msg(
            "Casting is unavailable: no network port could be opened",
        ));
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
    let device = probe::probe(&state.http, ip).await.ok_or_else(|| {
        Error::msg(format!(
            "No Chromecast answered at {ip}. Check the address in the Google Home app."
        ))
    })?;
    remember(state.inner(), &device).await;
    if cast.found(device.clone()) {
        emit_devices(&app);
    }
    Ok(device)
}

/// Plays `req` on a TV: on `device_id` (starting a session there), or on the
/// TV already being cast to. Local playback stops — the account streams to
/// one place.
#[tauri::command]
#[allow(clippy::too_many_arguments)] // Tauri injects each managed `State` as an argument
pub async fn cast_play<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
    player: State<'_, Player>,
    probe: State<'_, Arc<crate::probe::Probe>>,
    cast: State<'_, Cast>,
    device_id: Option<String>,
    req: PlayRequest,
    meta: Option<Meta>,
) -> Result<()> {
    let st = state.inner().clone();
    let stream = playback::resolve_and_touch(&st, &req).await?;
    let audio_lang =
        read_blocking(&st, |conn| Ok(settings::get_str(conn, "player.audioLang"))).await?;
    probe.cancel_running().await?;
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
        log::info!(
            "cast: {} {}:{} to {}",
            req.kind,
            req.source_id,
            req.id,
            s.device.name
        );
        return s
            .tx
            .send(Cmd::Load(item))
            .map_err(|_| Error::msg("The cast session has ended"));
    }

    let id = device_id.unwrap_or_default();
    let device = cast
        .devices
        .lock()
        .get(&id)
        .cloned()
        .ok_or_else(|| Error::msg("That TV is no longer on the network"))?;
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
    log::info!(
        "cast: {} {}:{} to {}",
        req.kind,
        req.source_id,
        req.id,
        device.name
    );
    let _ = tx.send(Cmd::Load(item));
    let sid = {
        let mut n = cast.next_id.lock();
        *n += 1;
        *n
    };
    *session = Some(Session {
        id: sid,
        device: device.clone(),
        tx: tx.clone(),
        status: status.clone(),
    });
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
    cast.session
        .lock()
        .as_ref()
        .map(|s| s.status.lock().clone())
        .filter(|s| !s.finished())
}
