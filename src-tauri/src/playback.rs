//! `play`: resolves (source, item) to a stream URL — credentials never leave
//! the backend — and hands it to the native player.

use std::sync::Arc;

use rusqlite::{Connection, OptionalExtension, params};
use serde::Deserialize;
use tauri::{Manager, Runtime, State};

use crate::error::{Error, Result};
use crate::kind::PlayKind;
use crate::player::{LoadOptions, MediaRef, Player};
use crate::probe::Probe;
use crate::sources::{self, SourceKind, m3u};
use crate::state::{AppState, read_blocking, write_blocking};
use crate::util::json::i64_of;
use crate::{library, settings};

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlayRequest {
    pub kind: PlayKind,
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
    pub channel: Option<(String, Option<String>)>,
}

/// The stored row of a playable item: its own URL (M3U, or an Xtream item
/// with one) and the request headers of M3U items (none for Xtream).
#[derive(Default)]
struct ItemRow {
    url: Option<String>,
    user_agent: Option<String>,
    referrer: Option<String>,
}

fn item_row(conn: &Connection, sql: &str, req: &PlayRequest, what: &str) -> Result<ItemRow> {
    conn.query_row(sql, params![req.source_id, req.id], |r| {
        Ok(ItemRow {
            url: r.get("url")?,
            user_agent: r.get("user_agent")?,
            referrer: r.get("referrer")?,
        })
    })
    .optional()?
    .ok_or_else(|| Error::NotFound(format!("{what} {}", req.id)))
}

/// Resolves (source, item) to its stream.
pub fn resolve(conn: &Connection, req: &PlayRequest) -> Result<Stream> {
    let src = sources::load(conn, req.source_id)?;
    let live_format = settings::get_str(conn, "player.liveFormat");
    let fmt = if live_format == "m3u8" { "m3u8" } else { "ts" };
    let x = match src.kind {
        SourceKind::Xtream => Some(sources::xtream_url_builder(&src)?),
        SourceKind::M3u => None,
    };

    let mut channel = None;
    let (url, row) = match req.kind {
        PlayKind::Live => {
            let (title, logo): (String, Option<String>) = conn
                .query_row(
                    "SELECT title, logo FROM channel WHERE source_id = ?1 AND id = ?2",
                    params![req.source_id, req.id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?
                .ok_or_else(|| Error::NotFound(format!("channel {}", req.id)))?;
            channel = Some((title, logo));
            let row = item_row(
                conn,
                "SELECT url, user_agent, referrer FROM channel WHERE source_id = ?1 AND id = ?2",
                req,
                "channel",
            )?;
            let url = match (&x, row.url.clone()) {
                (_, Some(u)) => u,
                (Some(x), None) => x.live_url(&req.id, fmt),
                (None, None) => return Err(Error::msg("channel has no stream url".into())),
            };
            (url, row)
        }
        PlayKind::Movie => {
            let ext: Option<String> = conn
                .query_row(
                    "SELECT ext FROM movie WHERE source_id = ?1 AND id = ?2",
                    params![req.source_id, req.id],
                    |r| r.get(0),
                )
                .optional()?
                .ok_or_else(|| Error::NotFound(format!("movie {}", req.id)))?;
            let row = item_row(
                conn,
                "SELECT url, user_agent, referrer FROM movie WHERE source_id = ?1 AND id = ?2",
                req,
                "movie",
            )?;
            let url = match (&x, row.url.clone()) {
                (_, Some(u)) => u,
                (Some(x), None) => x.movie_url(&req.id, ext.as_deref().unwrap_or("mp4")),
                (None, None) => return Err(Error::msg("movie has no stream url".into())),
            };
            (url, row)
        }
        PlayKind::Episode => match &x {
            Some(x) => (
                x.episode_url(&req.id, req.ext.as_deref().unwrap_or("mp4")),
                ItemRow::default(),
            ),
            // M3U series (sources::write_m3u): the playlist's own URL
            None => {
                let row = item_row(
                    conn,
                    "SELECT url, user_agent, referrer FROM episode WHERE source_id = ?1 AND id = ?2",
                    req,
                    "episode",
                )?;
                let url = row
                    .url
                    .clone()
                    .ok_or_else(|| Error::msg("episode has no stream url".into()))?;
                (url, row)
            }
        },
        PlayKind::Catchup => {
            let start = req
                .catchup_start
                .ok_or_else(|| Error::msg("missing catch-up start".into()))?;
            let minutes = req.catchup_minutes.unwrap_or(60).max(1);
            match &x {
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
                    .ok_or_else(|| Error::msg("bad catch-up time".into()))?
                    .naive_utc();
                    (x.timeshift_url(&req.id, local, minutes), ItemRow::default())
                }
                // M3U: the playlist's catch-up scheme (sources::m3u::catchup_url)
                None => {
                    let (mode, template): (Option<String>, Option<String>) = conn
                        .query_row(
                            "SELECT catchup_mode, catchup_source FROM channel WHERE source_id = ?1 AND id = ?2",
                            params![req.source_id, req.id],
                            |r| Ok((r.get(0)?, r.get(1)?)),
                        )
                        .optional()?
                        .ok_or_else(|| Error::NotFound(format!("channel {}", req.id)))?;
                    let row = item_row(
                        conn,
                        "SELECT url, user_agent, referrer FROM channel WHERE source_id = ?1 AND id = ?2",
                        req,
                        "channel",
                    )?;
                    let url = row
                        .url
                        .clone()
                        .zip(mode)
                        .and_then(|(stream, mode)| {
                            m3u::catchup_url(
                                &mode,
                                template.as_deref(),
                                &stream,
                                start,
                                minutes * 60,
                                crate::db::now(),
                                src.catchup_shift_minutes * 60,
                            )
                        })
                        .ok_or_else(|| Error::msg("This channel does not offer catch-up".into()))?;
                    (url, row)
                }
            }
        }
    };
    let alternates = x
        .as_ref()
        .map(|x| x.alternates_for(&url))
        .unwrap_or_default();
    Ok(Stream {
        url,
        alternates,
        live: matches!(req.kind, PlayKind::Live),
        user_agent: row.user_agent.or(src.user_agent),
        referrer: row.referrer,
        channel,
    })
}

/// `resolve` off the async runtime, recording a tuned live channel for
/// "recently watched" (`play` and `cast_play`).
pub async fn resolve_and_touch(st: &AppState, req: &PlayRequest) -> Result<Stream> {
    let req = req.clone();
    write_blocking(st, move |conn| {
        let stream = resolve(conn, &req)?;
        if let Some((title, logo)) = &stream.channel {
            library::touch_channel(conn, req.source_id, &req.id, title, logo.as_deref())?;
        }
        Ok(stream)
    })
    .await
}

#[tauri::command]
pub async fn play<R: Runtime>(
    app: tauri::AppHandle<R>,
    state: State<'_, AppState>,
    player: State<'_, Player>,
    probe: State<'_, Arc<Probe>>,
    req: PlayRequest,
) -> Result<()> {
    let st = state.inner().clone();
    let Stream {
        url,
        alternates,
        live,
        user_agent,
        referrer,
        ..
    } = resolve_and_touch(&st, &req).await?;
    // one stream per account: casting and a running version check close
    // their stream first
    if let Some(cast) = app.try_state::<crate::cast::Cast>().filter(|c| c.busy()) {
        log::info!("play: ending the cast session first");
        cast.stop();
    }
    probe.cancel_running().await?;

    log::info!(
        "play {} {}:{} (live={live}, mirrors={})",
        req.kind,
        req.source_id,
        req.id,
        alternates.len()
    );
    let media = req.kind.file_item().map(|kind| MediaRef {
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
    let configured = read_blocking(state.inner(), |conn| {
        Ok(settings::get_str(conn, "recording.dir"))
    })
    .await?;
    let dir = recording_dir(&app, &configured)?;
    tokio::fs::create_dir_all(&dir).await?;
    let file = player.start_recording(&dir).map_err(Error::msg)?;
    log::info!("recording to {}", file.display());
    Ok(Some(file.display().to_string()))
}

fn recording_dir<R: Runtime>(
    app: &tauri::AppHandle<R>,
    configured: &str,
) -> Result<std::path::PathBuf> {
    if !configured.trim().is_empty() {
        return Ok(configured.trim().into());
    }
    let videos = app
        .path()
        .video_dir()
        .or_else(|_| app.path().home_dir().map(|h| h.join("Videos")))?;
    Ok(videos.join("testpattern"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_conn;

    /// Source 1 (Xtream, from `test_conn`) with credentials, plus an M3U
    /// source 2, each with one channel, movie and episode.
    fn catalog() -> Connection {
        let c = test_conn();
        c.execute_batch(
            "UPDATE source SET username = 'u', password = 'p',
                    account_json = '{\"serverUtcOffset\": 3600}' WHERE id = 1;
             INSERT INTO source (id, kind, name, url, created_at) VALUES (2, 'm3u', 'list', 'http://l.tv/a.m3u', 0);
             INSERT INTO channel (source_id, id, name, title, position) VALUES (1, '7', 'DR1', 'DR1', 0);
             INSERT INTO movie (source_id, id, name, title, ext, position) VALUES (1, '9', 'm', 'Film', 'mkv', 0);
             INSERT INTO channel (source_id, id, name, title, position, url, user_agent, referrer,
                                  catchup_mode, catchup_source)
                  VALUES (2, 'c', 'TV2', 'TV2', 0, 'http://s.tv/tv2.m3u8', 'UA/1', 'http://ref', 'shift', NULL);
             INSERT INTO movie (source_id, id, name, title, position, url) VALUES (2, 'm', 'm', 'Film', 0, 'http://s.tv/m.mp4');
             INSERT INTO series (source_id, id, name, title, position) VALUES (2, 's', 's', 'Show', 0);
             INSERT INTO episode (source_id, series_id, id, season, episode, title, url, position)
                  VALUES (2, 's', 'e', 1, 1, 'Pilot', 'http://s.tv/e.mkv', 0);",
        )
        .unwrap();
        c
    }

    fn req(source_id: i64, kind: PlayKind, id: &str) -> PlayRequest {
        PlayRequest {
            kind,
            source_id,
            id: id.into(),
            ..Default::default()
        }
    }

    #[test]
    fn xtream_items_build_panel_urls() {
        let c = catalog();
        let live = resolve(&c, &req(1, PlayKind::Live, "7")).unwrap();
        assert_eq!(live.url, "http://h.tv/live/u/p/7.ts");
        assert!(live.live);
        assert_eq!(live.channel, Some(("DR1".into(), None)));
        let movie = resolve(&c, &req(1, PlayKind::Movie, "9")).unwrap();
        assert_eq!(movie.url, "http://h.tv/movie/u/p/9.mkv");
        assert!(!movie.live && movie.channel.is_none());
        let ep = resolve(
            &c,
            &PlayRequest {
                ext: Some("mp4".into()),
                ..req(1, PlayKind::Episode, "e5")
            },
        )
        .unwrap();
        assert_eq!(ep.url, "http://h.tv/series/u/p/e5.mp4");
    }

    #[test]
    fn xtream_catch_up_is_in_server_time() {
        let c = catalog();
        // 2024-01-01 20:00 UTC, the server one hour ahead
        let r = PlayRequest {
            catchup_start: Some(1_704_139_200),
            catchup_minutes: Some(90),
            ..req(1, PlayKind::Catchup, "7")
        };
        let s = resolve(&c, &r).unwrap();
        assert_eq!(s.url, "http://h.tv/timeshift/u/p/90/2024-01-01:21-00/7.ts");
        assert!(!s.live, "catch-up is seekable, not live");
        assert!(
            resolve(&c, &req(1, PlayKind::Catchup, "7")).is_err(),
            "needs a start"
        );
    }

    #[test]
    fn m3u_items_play_their_own_urls_with_headers() {
        let c = catalog();
        let live = resolve(&c, &req(2, PlayKind::Live, "c")).unwrap();
        assert_eq!(live.url, "http://s.tv/tv2.m3u8");
        assert_eq!(live.user_agent.as_deref(), Some("UA/1"));
        assert_eq!(live.referrer.as_deref(), Some("http://ref"));
        assert!(live.alternates.is_empty());
        assert_eq!(
            resolve(&c, &req(2, PlayKind::Movie, "m")).unwrap().url,
            "http://s.tv/m.mp4"
        );
        assert_eq!(
            resolve(&c, &req(2, PlayKind::Episode, "e")).unwrap().url,
            "http://s.tv/e.mkv"
        );
        let catchup = PlayRequest {
            catchup_start: Some(100),
            ..req(2, PlayKind::Catchup, "c")
        };
        assert!(
            resolve(&c, &catchup)
                .unwrap()
                .url
                .starts_with("http://s.tv/tv2.m3u8?utc=100&lutc=")
        );
    }

    #[test]
    fn unknown_items_are_not_found() {
        let c = catalog();
        assert!(matches!(
            resolve(&c, &req(1, PlayKind::Movie, "nope")),
            Err(Error::NotFound(_))
        ));
        assert!(matches!(
            resolve(&c, &req(2, PlayKind::Live, "nope")),
            Err(Error::NotFound(_))
        ));
        assert!(matches!(
            resolve(&c, &req(9, PlayKind::Live, "7")),
            Err(Error::NotFound(_))
        ));
    }
}
