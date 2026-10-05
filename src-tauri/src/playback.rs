//! `play`: resolves (source, item) to a stream URL — credentials never leave
//! the backend — and hands it to the native player.

use rusqlite::{Connection, OptionalExtension, params};
use serde::Deserialize;
use tauri::{Runtime, State};

use crate::error::{Error, Result};
use crate::player::{LoadOptions, MediaRef, Player};
use crate::sources::{self, SourceKind, m3u};
use crate::state::{AppState, http_client};
use crate::util::json::i64_of;
use crate::{library, settings};

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlayRequest {
    /// 'live' | 'movie' | 'episode' | 'catchup'
    pub kind: String,
    pub source_id: i64,
    pub id: String,
    /// Container extension for episodes (from series_detail).
    pub ext: Option<String>,
    /// Resume position in seconds.
    pub start: Option<f64>,
    pub title: Option<String>,
    /// Catch-up: programme start (unix seconds, UTC) and length in minutes.
    pub catchup_start: Option<i64>,
    pub catchup_minutes: Option<i64>,
    #[serde(default)]
    pub paused: bool,
}

/// A playable stream. The URL carries the account's credentials: it goes to
/// mpv, never to the UI.
pub struct Stream {
    pub url: String,
    /// the same stream on the provider's mirror servers
    pub alternates: Vec<String>,
    pub live: bool,
    pub user_agent: Option<String>,
    pub referrer: Option<String>,
    /// live channels: title and logo for "recently watched"
    channel: Option<(String, Option<String>)>,
}

/// Resolves (source, item) to its stream.
pub fn resolve(conn: &Connection, req: &PlayRequest) -> Result<Stream> {
    let (url, alternates, live, user_agent, referrer, channel) = {
        let src = sources::load(conn, req.source_id)?;
        let live_format = settings::get_str(conn, "player.liveFormat");
        let fmt = if live_format == "m3u8" { "m3u8" } else { "ts" };
        let x = match src.kind {
            SourceKind::Xtream => Some(sources::xtream_for(&src, http_client(None))?),
            SourceKind::M3u => None,
        };

        // request headers of M3U items (user agent, referrer); none for Xtream
        let mut headers: (Option<String>, Option<String>) = (None, None);
        let mut channel = None;
        let (url, live) = match req.kind.as_str() {
            "live" => {
                let (title, logo, direct, ua, referrer): (String, Option<String>, Option<String>, Option<String>, Option<String>) = conn
                    .query_row(
                        "SELECT title, logo, url, user_agent, referrer FROM channel WHERE source_id = ?1 AND id = ?2",
                        params![req.source_id, req.id],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
                    )
                    .optional()?
                    .ok_or_else(|| Error::NotFound(format!("channel {}", req.id)))?;
                headers = (ua, referrer);
                channel = Some((title, logo));
                let url = match (&x, direct) {
                    (_, Some(u)) => u,
                    (Some(x), None) => x.live_url(&req.id, fmt),
                    (None, None) => return Err(Error::msg("channel has no stream url")),
                };
                (url, true)
            }
            "movie" => {
                let (ext, direct, ua, referrer): (Option<String>, Option<String>, Option<String>, Option<String>) = conn
                    .query_row(
                        "SELECT ext, url, user_agent, referrer FROM movie WHERE source_id = ?1 AND id = ?2",
                        params![req.source_id, req.id],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                    )
                    .optional()?
                    .ok_or_else(|| Error::NotFound(format!("movie {}", req.id)))?;
                headers = (ua, referrer);
                let url = match (&x, direct) {
                    (_, Some(u)) => u,
                    (Some(x), None) => x.movie_url(&req.id, ext.as_deref().unwrap_or("mp4")),
                    (None, None) => return Err(Error::msg("movie has no stream url")),
                };
                (url, false)
            }
            "episode" => match &x {
                Some(x) => (
                    x.episode_url(&req.id, req.ext.as_deref().unwrap_or("mp4")),
                    false,
                ),
                // M3U series (sources::write_m3u): the playlist's own URL
                None => {
                    let (url, ua, referrer): (String, Option<String>, Option<String>) = conn
                        .query_row(
                            "SELECT url, user_agent, referrer FROM episode WHERE source_id = ?1 AND id = ?2",
                            params![req.source_id, req.id],
                            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                        )
                        .optional()?
                        .ok_or_else(|| Error::NotFound(format!("episode {}", req.id)))?;
                    headers = (ua, referrer);
                    (url, false)
                }
            },
            "catchup" => {
                let start = req
                    .catchup_start
                    .ok_or_else(|| Error::msg("missing catch-up start"))?;
                let minutes = req.catchup_minutes.unwrap_or(60).max(1);
                let url = match &x {
                    Some(x) => {
                        let offset = src
                            .account
                            .as_ref()
                            .and_then(|a| i64_of(&a["serverUtcOffset"]))
                            .unwrap_or(0);
                        let local = chrono::DateTime::from_timestamp(
                            start + offset + src.catchup_shift_minutes * 60,
                            0,
                        )
                        .ok_or_else(|| Error::msg("bad catch-up time"))?
                        .naive_utc();
                        x.timeshift_url(&req.id, local, minutes)
                    }
                    // M3U: the playlist's catch-up scheme (sources::m3u::catchup_url)
                    None => {
                        type Row = (
                            Option<String>,
                            Option<String>,
                            Option<String>,
                            Option<String>,
                            Option<String>,
                        );
                        let (stream, mode, template, ua, referrer): Row = conn
                            .query_row(
                                "SELECT url, catchup_mode, catchup_source, user_agent, referrer
                                   FROM channel WHERE source_id = ?1 AND id = ?2",
                                params![req.source_id, req.id],
                                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
                            )
                            .optional()?
                            .ok_or_else(|| Error::NotFound(format!("channel {}", req.id)))?;
                        headers = (ua, referrer);
                        stream
                            .zip(mode)
                            .and_then(|(stream, mode)| {
                                let shift = src.catchup_shift_minutes * 60;
                                m3u::catchup_url(
                                    &mode,
                                    template.as_deref(),
                                    &stream,
                                    start,
                                    minutes * 60,
                                    crate::db::now(),
                                    shift,
                                )
                            })
                            .ok_or_else(|| Error::msg("This channel does not offer catch-up"))?
                    }
                };
                (url, false)
            }
            other => return Err(Error::msg(format!("cannot play {other}"))),
        };
        let alternates = x
            .as_ref()
            .map(|x| x.alternates_for(&url))
            .unwrap_or_default();
        (
            url,
            alternates,
            live,
            headers.0.or(src.user_agent),
            headers.1,
            channel,
        )
    };
    Ok(Stream {
        url,
        alternates,
        live,
        user_agent,
        referrer,
        channel,
    })
}

#[tauri::command]
pub async fn play<R: Runtime>(
    _app: tauri::AppHandle<R>,
    state: State<'_, AppState>,
    player: State<'_, Player>,
    req: PlayRequest,
) -> Result<()> {
    let st = state.inner().clone();
    let Stream {
        url,
        alternates,
        live,
        user_agent,
        referrer,
        channel,
    } = resolve(&st.db.read(), &req)?;
    if let Some((title, logo)) = channel {
        let w = st.db.write();
        library::touch_channel(&w, req.source_id, &req.id, &title, logo.as_deref())?;
    }
    // one stream per account: a running version check closes its stream first
    if crate::probe::running() {
        tokio::task::spawn_blocking(crate::probe::cancel)
            .await
            .map_err(|e| Error::msg(e.to_string()))?;
    }

    log::info!(
        "play {} {}:{} (live={live}, mirrors={})",
        req.kind,
        req.source_id,
        req.id,
        alternates.len()
    );
    let media = match req.kind.as_str() {
        "movie" => Some("movie"),
        "episode" => Some("episode"),
        _ => None,
    }
    .map(|kind| MediaRef {
        kind,
        source_id: req.source_id,
        id: req.id.clone(),
    });
    player
        .load_with_fallbacks(
            &url,
            alternates,
            LoadOptions {
                start: req.start,
                live,
                title: req.title,
                user_agent,
                referrer,
                paused: req.paused,
                media,
            },
        )
        .map_err(|e| Error::msg(e.to_string()))
}

/// Starts (`on`) or stops recording the playing live channel into the
/// recordings folder (setting `recording.dir`, else ~/Videos/testpattern).
/// Returns the file being / last written.
#[tauri::command]
pub async fn player_record<R: Runtime>(
    app: tauri::AppHandle<R>,
    state: State<'_, AppState>,
    player: State<'_, Player>,
    on: bool,
) -> Result<Option<String>> {
    if !on {
        return Ok(player.stop_recording().map(|p| p.display().to_string()));
    }
    let dir = recording_dir(&app, state.inner())?;
    tokio::fs::create_dir_all(&dir).await?;
    let file = player.start_recording(&dir).map_err(Error::msg)?;
    log::info!("recording to {}", file.display());
    Ok(Some(file.display().to_string()))
}

fn recording_dir<R: Runtime>(
    app: &tauri::AppHandle<R>,
    st: &AppState,
) -> Result<std::path::PathBuf> {
    use tauri::Manager;
    let configured = settings::get_str(&st.db.read(), "recording.dir");
    if !configured.trim().is_empty() {
        return Ok(configured.trim().into());
    }
    let videos = app
        .path()
        .video_dir()
        .or_else(|_| app.path().home_dir().map(|h| h.join("Videos")))?;
    Ok(videos.join("testpattern"))
}
