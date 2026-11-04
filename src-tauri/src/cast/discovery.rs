//! Finding TVs: mDNS browsing, the remembered ones, and a sweep of the
//! local networks by address.

use super::*;

fn device_from(s: &mdns_sd::ResolvedService) -> Option<Device> {
    // capabilities bit 0: video out (speakers and speaker groups lack it)
    let ca: u32 = s
        .get_property_val_str("ca")
        .and_then(|c| c.parse().ok())
        .unwrap_or(1);
    if ca & 1 == 0 {
        return None;
    }
    let ip = s
        .addresses
        .iter()
        .map(|a| a.to_ip_addr())
        .find(|ip| ip.is_ipv4() && !ip.is_loopback())?;
    let id = probe::normalize_id(s.get_property_val_str("id").unwrap_or(&s.fullname));
    let name = s
        .get_property_val_str("fn")
        .unwrap_or("Chromecast")
        .to_owned();
    let model = s.get_property_val_str("md").unwrap_or("").to_owned();
    Some(Device {
        id,
        name,
        model,
        addr: SocketAddr::new(ip, s.port),
        fullname: s.fullname.clone(),
    })
}

pub(super) fn start_browsing<R: Runtime>(app: &AppHandle<R>, cast: &Cast) -> Result<()> {
    let mut browser = cast.browser.lock();
    if browser.is_some() {
        return Ok(());
    }
    let daemon = mdns_sd::ServiceDaemon::new()
        .map_err(|e| Error::msg(format!("cannot search the network: {e}")))?;
    let events = daemon
        .browse(SERVICE)
        .map_err(|e| Error::msg(format!("cannot search the network: {e}")))?;
    let devices = cast.devices.clone();
    let app = app.clone();
    std::thread::Builder::new()
        .name("cast-discovery".into())
        .spawn(move || {
            while let Ok(event) = events.recv() {
                let changed = match event {
                    mdns_sd::ServiceEvent::ServiceResolved(s) => {
                        match (device_from(&s), app.try_state::<Cast>()) {
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
                        }
                    }
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
pub(super) fn search<R: Runtime>(app: &AppHandle<R>, cast: &Cast) {
    if cast.searching.swap(true, Ordering::Relaxed) {
        return;
    }
    let sweep = cast
        .last_sweep
        .lock()
        .is_none_or(|t| t.elapsed() > SWEEP_EVERY);
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let Some(st) = app.try_state::<AppState>().map(|s| s.inner().clone()) else {
            return;
        };
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
        let known = read_blocking(&st, |conn| Ok(remembered(conn)))
            .await
            .unwrap_or_default();
        probe_all(known.into_iter().map(|r| r.ip).collect()).await;
        if sweep {
            let networks = probe::local_networks();
            log::info!(
                "cast: checking {} local network(s) for Cast devices",
                networks.len()
            );
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
