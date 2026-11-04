//! Syncing a source: download its lists (or playlist) into the catalog,
//! then its guide; and the periodic refresh of stale sources.

use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncScope {
    Full,
    Epg,
}

/// Starts a background sync unless one is already running for the source.
pub fn spawn_sync<R: Runtime>(app: AppHandle<R>, st: AppState, id: i64, scope: SyncScope) {
    if !st.syncing.lock().insert(id) {
        return;
    }
    tauri::async_runtime::spawn(async move {
        let _ = app.emit(SYNC_EVENT, SyncProgress::Started { source_id: id });
        let result = run_sync(&app, &st, id, scope).await;
        st.syncing.lock().remove(&id);
        match result {
            Ok(counts) => {
                let _ = app.emit(
                    SYNC_EVENT,
                    SyncProgress::Done {
                        source_id: id,
                        counts,
                    },
                );
                // new titles: their TMDB details (when a key is set)
                if scope == SyncScope::Full {
                    crate::tmdb::spawn(app.clone(), st.clone());
                }
            }
            Err(e) => {
                log::warn!("sync of source {id} failed: {e}");
                if scope == SyncScope::Full {
                    let message = e.to_string();
                    let saved = write_blocking(&st, move |conn| {
                        conn.execute(
                            "UPDATE source SET sync_error = ?2 WHERE id = ?1",
                            params![id, message],
                        )?;
                        Ok(())
                    })
                    .await;
                    if let Err(e) = saved {
                        log::warn!("source {id}: cannot store the sync error: {e}");
                    }
                }
                let _ = app.emit(
                    SYNC_EVENT,
                    SyncProgress::Failed {
                        source_id: id,
                        error: e.to_string(),
                    },
                );
            }
        }
    });
}

/// Syncs every source whose catalog/EPG is older than the given ages.
pub async fn sync_stale<R: Runtime>(
    app: &AppHandle<R>,
    st: &AppState,
    catalog_max_age: i64,
    epg_max_age: i64,
) {
    let sources = read_blocking(st, load_all).await.unwrap_or_default();
    let t = now();
    for s in sources {
        if s.last_sync.is_none_or(|l| t - l > catalog_max_age) {
            spawn_sync(app.clone(), st.clone(), s.id, SyncScope::Full);
        } else if s.last_epg_sync.is_none_or(|l| t - l > epg_max_age) {
            spawn_sync(app.clone(), st.clone(), s.id, SyncScope::Epg);
        }
    }
}

fn step<R: Runtime>(app: &AppHandle<R>, id: i64, msg: &str) {
    let _ = app.emit(
        SYNC_EVENT,
        SyncProgress::Step {
            source_id: id,
            message: msg.to_owned(),
        },
    );
}

async fn run_sync<R: Runtime>(
    app: &AppHandle<R>,
    st: &AppState,
    id: i64,
    scope: SyncScope,
) -> Result<Counts> {
    let src = read_blocking(st, move |conn| load(conn, id)).await?;
    let client = http_client(src.user_agent.as_deref());
    let mut epg_urls: Vec<String> = src.epg_url.iter().cloned().collect();

    if scope == SyncScope::Full {
        match src.kind {
            SourceKind::Xtream => {
                let x = xtream_for(&src, client.clone())?;
                step(app, id, "Signing in…");
                let account = x.account().await?;
                if !account.status.eq_ignore_ascii_case("active") {
                    return Err(Error::msg(format!(
                        "Account is {}",
                        account.status.to_lowercase()
                    )));
                }
                let account_json = serde_json::to_string(&account)?;
                write_blocking(st, move |conn| {
                    conn.execute(
                        "UPDATE source SET account_json = ?2 WHERE id = ?1",
                        params![id, account_json],
                    )?;
                    Ok(())
                })
                .await?;
                step(app, id, "Downloading channels, movies and series…");
                let (live_categories, movie_categories, series_categories, live, movies, series) =
                    tokio::try_join!(
                        x.list("get_live_categories"),
                        x.list("get_vod_categories"),
                        x.list("get_series_categories"),
                        x.list("get_live_streams"),
                        x.list("get_vod_streams"),
                        x.list("get_series"),
                    )?;
                step(
                    app,
                    id,
                    &format!(
                        "Saving {} channels, {} movies, {} series…",
                        live.len(),
                        movies.len(),
                        series.len()
                    ),
                );
                let lists = XtreamLists {
                    live_categories,
                    movie_categories,
                    series_categories,
                    live,
                    movies,
                    series,
                };
                write_blocking(st, move |conn| write_xtream(conn, id, lists)).await?;
                if epg_urls.is_empty() {
                    epg_urls.push(x.xmltv_url());
                }
            }
            SourceKind::M3u => {
                step(app, id, "Downloading playlist…");
                let text = download_text(&client, &src.url).await?;
                let pl = m3u::parse(&text);
                if pl.entries.is_empty() {
                    return Err(Error::msg("The playlist is empty or not an M3U file"));
                }
                step(app, id, &format!("Saving {} entries…", pl.entries.len()));
                if epg_urls.is_empty() {
                    epg_urls.extend(pl.epg_urls.iter().cloned());
                }
                write_blocking(st, move |conn| write_m3u(conn, id, &pl)).await?;
            }
        }
        write_blocking(st, move |conn| {
            conn.execute(
                "UPDATE source SET last_sync = ?2, sync_error = NULL WHERE id = ?1",
                params![id, now()],
            )?;
            Ok(())
        })
        .await?;
    } else if epg_urls.is_empty() {
        match src.kind {
            SourceKind::Xtream => epg_urls.push(xtream_for(&src, client.clone())?.xmltv_url()),
            SourceKind::M3u => {
                // the playlist header may name a guide
                if let Ok(text) = download_text(&client, &src.url).await {
                    epg_urls.extend(m3u::parse(&text).epg_urls);
                }
            }
        }
    }

    // EPG is best effort: a broken guide must not fail the catalog sync.
    if !epg_urls.is_empty() {
        step(app, id, "Updating TV guide…");
        match sync_epg(st, id, &client, &epg_urls).await {
            Ok(n) => log::info!("source {id}: imported {n} programmes"),
            Err(e) => {
                log::warn!("source {id}: EPG import failed: {e}");
                if scope == SyncScope::Epg {
                    return Err(e);
                }
            }
        }
    }

    read_blocking(st, move |conn| counts(conn, id)).await
}

pub fn xtream_for(src: &SourceRow, client: reqwest::Client) -> Result<Xtream> {
    if src.password_in_keyring && src.password.is_none() {
        return Err(Error::msg(crate::secrets::UNAVAILABLE));
    }
    // prefer the mirror that answered last time
    let preferred = src.account.as_ref().and_then(|a| str_of(&a["baseUrl"]));
    let mut alts = src.alt_urls.clone();
    let mut primary = src.url.clone();
    if let Some(p) = preferred.filter(|p| *p != src.url && alts.contains(p)) {
        alts.retain(|a| *a != p);
        alts.insert(0, primary);
        primary = p;
    }
    Ok(Xtream::new(
        &primary,
        &alts,
        src.username.as_deref().unwrap_or(""),
        src.password.as_deref().unwrap_or(""),
        client,
    ))
}

/// Whole-body limits: playlists and guides of big providers are tens of MB.
const PLAYLIST_TIMEOUT: Duration = Duration::from_secs(300);
const GUIDE_TIMEOUT: Duration = Duration::from_secs(600);

pub(super) async fn download_text(client: &reqwest::Client, url: &str) -> Result<String> {
    let r = client.get(url).timeout(PLAYLIST_TIMEOUT).send().await?;
    if !r.status().is_success() {
        return Err(Error::msg(format!("server answered {}", r.status())));
    }
    let bytes = crate::epg::maybe_gunzip(r.bytes().await?.to_vec())?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

async fn sync_epg(
    st: &AppState,
    id: i64,
    client: &reqwest::Client,
    urls: &[String],
) -> Result<usize> {
    let mut last_err = None;
    // guides reach 70+ MB: stream them to disk and parse from there
    let file = st.cache_dir.join(format!("guide-{id}.part"));
    for url in urls {
        let res = async {
            download_to(client, url, &file, GUIDE_TIMEOUT).await?;
            let st2 = st.clone();
            let file2 = file.clone();
            run_blocking(move || -> Result<usize> {
                let (wanted, past_days) = {
                    let conn = st2.db.read();
                    let mut stmt = conn.prepare(
                        "SELECT DISTINCT epg_id FROM channel WHERE source_id = ?1 AND epg_id IS NOT NULL",
                    )?;
                    let wanted: HashSet<String> = stmt.query_map([id], |r| r.get(0))?.collect::<Result<_, _>>()?;
                    (wanted, crate::epg::history_days(&conn, id)?)
                };
                let guide = crate::epg::open_guide(&file2)?;
                let mut conn = st2.db.write();
                let imported = crate::epg::import(&mut conn, id, guide, &wanted, past_days)?;
                if let Some(e) = &imported.incomplete {
                    log::warn!("source {id}: guide document incomplete ({e}); kept {} programmes", imported.programmes);
                }
                conn.execute("UPDATE source SET last_epg_sync = ?2 WHERE id = ?1", params![id, now()])?;
                Ok(imported.programmes)
            })
            .await?
        }
        .await;
        let _ = tokio::fs::remove_file(&file).await;
        match res {
            Ok(n) => return Ok(n),
            Err(e) => last_err = Some(e),
        }
    }
    Err(last_err.unwrap_or_else(|| Error::msg("no guide url")))
}

/// Streams a download into `path` (constant memory).
async fn download_to(
    client: &reqwest::Client,
    url: &str,
    path: &std::path::Path,
    timeout: Duration,
) -> Result<u64> {
    use tokio::io::AsyncWriteExt;
    let mut r = client.get(url).timeout(timeout).send().await?;
    if !r.status().is_success() {
        return Err(Error::msg(format!("guide server answered {}", r.status())));
    }
    if let Some(dir) = path.parent() {
        tokio::fs::create_dir_all(dir).await?;
    }
    let mut f = tokio::fs::File::create(path).await?;
    let mut size = 0u64;
    while let Some(chunk) = r.chunk().await? {
        f.write_all(&chunk).await?;
        size += chunk.len() as u64;
    }
    f.flush().await?;
    Ok(size)
}
