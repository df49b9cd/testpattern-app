//! `play`: resolves (source, item) to a stream URL — credentials never leave
//! the backend — and hands it to the native player.

use rusqlite::{OptionalExtension, params};
use serde::Deserialize;
use tauri::{Runtime, State};

use crate::error::{Error, Result};
use crate::player::{LoadOptions, Player};
use crate::sources::{self, SourceKind};
use crate::state::{AppState, http_client};
use crate::util::json::i64_of;
use crate::{library, settings};

#[derive(Debug, Deserialize)]
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

#[tauri::command]
pub async fn play<R: Runtime>(
    _app: tauri::AppHandle<R>,
    state: State<'_, AppState>,
    player: State<'_, Player>,
    req: PlayRequest,
) -> Result<()> {
    let st = state.inner().clone();
    let (url, alternates, live, user_agent) = {
        let conn = st.db.read();
        let src = sources::load(&conn, req.source_id)?;
        let live_format = settings::get_str(&conn, "player.liveFormat");
        let fmt = if live_format == "m3u8" { "m3u8" } else { "ts" };
        let x = (src.kind == SourceKind::Xtream).then(|| sources::xtream_for(&src, http_client(None)));

        let (url, live) = match req.kind.as_str() {
            "live" => {
                let (title, logo, direct): (String, Option<String>, Option<String>) = conn
                    .query_row(
                        "SELECT title, logo, url FROM channel WHERE source_id = ?1 AND id = ?2",
                        params![req.source_id, req.id],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                    )
                    .optional()?
                    .ok_or_else(|| Error::NotFound(format!("channel {}", req.id)))?;
                drop(conn);
                {
                    let w = st.db.write();
                    library::touch_channel(&w, req.source_id, &req.id, &title, logo.as_deref())?;
                }
                let url = match (&x, direct) {
                    (_, Some(u)) => u,
                    (Some(x), None) => x.live_url(&req.id, fmt),
                    (None, None) => return Err(Error::msg("channel has no stream url")),
                };
                (url, true)
            }
            "movie" => {
                let (ext, direct): (Option<String>, Option<String>) = conn
                    .query_row(
                        "SELECT ext, url FROM movie WHERE source_id = ?1 AND id = ?2",
                        params![req.source_id, req.id],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .optional()?
                    .ok_or_else(|| Error::NotFound(format!("movie {}", req.id)))?;
                let url = match (&x, direct) {
                    (_, Some(u)) => u,
                    (Some(x), None) => x.movie_url(&req.id, ext.as_deref().unwrap_or("mp4")),
                    (None, None) => return Err(Error::msg("movie has no stream url")),
                };
                (url, false)
            }
            "episode" => {
                let x = x.as_ref().ok_or_else(|| Error::msg("episodes need an Xtream source"))?;
                (x.episode_url(&req.id, req.ext.as_deref().unwrap_or("mp4")), false)
            }
            "catchup" => {
                let x = x.as_ref().ok_or_else(|| Error::msg("catch-up needs an Xtream source"))?;
                let start = req.catchup_start.ok_or_else(|| Error::msg("missing catch-up start"))?;
                let minutes = req.catchup_minutes.unwrap_or(60).max(1);
                let offset = src.account.as_ref().and_then(|a| i64_of(&a["serverUtcOffset"])).unwrap_or(0);
                let local = chrono::DateTime::from_timestamp(start + offset, 0)
                    .ok_or_else(|| Error::msg("bad catch-up time"))?
                    .naive_utc();
                (x.timeshift_url(&req.id, local, minutes), false)
            }
            other => return Err(Error::msg(format!("cannot play {other}"))),
        };
        let alternates = x.as_ref().map(|x| x.alternates_for(&url)).unwrap_or_default();
        (url, alternates, live, src.user_agent)
    };

    log::info!("play {} {}:{} (live={live}, mirrors={})", req.kind, req.source_id, req.id, alternates.len());
    player
        .load_with_fallbacks(
            &url,
            alternates,
            LoadOptions { start: req.start, live, title: req.title, user_agent, paused: req.paused },
        )
        .map_err(|e| Error::msg(e.to_string()))
}
