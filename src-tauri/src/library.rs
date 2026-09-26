//! The user's library: favorites, watch progress, continue watching and
//! recently watched channels.

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use tauri::State;

use crate::catalog::channel_by_id;
use crate::db::now;
use crate::error::{Error, Result};
use crate::state::AppState;

#[tauri::command]
pub async fn favorite_toggle(state: State<'_, AppState>, kind: String, source_id: i64, item_id: String) -> Result<bool> {
    if !matches!(kind.as_str(), "live" | "movie" | "series") {
        return Err(Error::msg(format!("cannot favorite {kind}")));
    }
    let conn = state.db.write();
    let removed = conn.execute(
        "DELETE FROM favorite WHERE source_id = ?1 AND kind = ?2 AND item_id = ?3",
        params![source_id, kind, item_id],
    )?;
    if removed > 0 {
        return Ok(false);
    }
    conn.execute(
        "INSERT INTO favorite (source_id, kind, item_id, added_at) VALUES (?1, ?2, ?3, ?4)",
        params![source_id, kind, item_id, now()],
    )?;
    Ok(true)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryInput {
    pub kind: String,
    pub source_id: i64,
    pub item_id: String,
    pub series_id: Option<String>,
    pub season: Option<i64>,
    pub episode: Option<i64>,
    pub title: String,
    pub subtitle: Option<String>,
    pub image: Option<String>,
    pub backdrop: Option<String>,
    pub ext: Option<String>,
    #[serde(default)]
    pub position: f64,
    #[serde(default)]
    pub duration: f64,
}

/// Finished = within the last 8% or the last 3 minutes (credits).
fn is_finished(position: f64, duration: f64) -> bool {
    duration > 0.0 && (position >= duration * 0.92 || duration - position < 180.0 && duration > 600.0)
}

#[tauri::command]
pub async fn history_update(state: State<'_, AppState>, entry: HistoryInput) -> Result<()> {
    update_history(&state.db.write(), &entry)
}

pub fn update_history(conn: &Connection, entry: &HistoryInput) -> Result<()> {
    // An unknown duration (stream still opening, or already unloaded) must not
    // flip a finished item back to unwatched: judge by the stored duration.
    let duration = if entry.duration > 0.0 {
        entry.duration
    } else {
        conn.query_row(
            "SELECT duration FROM history WHERE source_id = ?1 AND kind = ?2 AND item_id = ?3",
            params![entry.source_id, entry.kind, entry.item_id],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or(0.0)
    };
    let watched = is_finished(entry.position, duration);
    conn.execute(
        "INSERT INTO history (source_id, kind, item_id, series_id, season, episode, title, subtitle, image,
                              backdrop, ext, position, duration, watched, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
         ON CONFLICT(source_id, kind, item_id) DO UPDATE SET
            series_id = excluded.series_id, season = excluded.season, episode = excluded.episode,
            title = excluded.title, subtitle = excluded.subtitle,
            image = COALESCE(excluded.image, history.image),
            backdrop = COALESCE(excluded.backdrop, history.backdrop),
            ext = COALESCE(excluded.ext, history.ext),
            position = excluded.position,
            duration = CASE WHEN excluded.duration > 0 THEN excluded.duration ELSE history.duration END,
            watched = excluded.watched,
            updated_at = excluded.updated_at",
        params![
            entry.source_id,
            entry.kind,
            entry.item_id,
            entry.series_id,
            entry.season,
            entry.episode,
            entry.title,
            entry.subtitle,
            entry.image,
            entry.backdrop,
            entry.ext,
            entry.position,
            entry.duration,
            watched,
            now()
        ],
    )?;
    Ok(())
}

/// What `mark_watched` records when the item has no history row yet (e.g. an
/// episode that was never played). Same conventions as `history_update`:
/// episodes carry the series title in `title`, the episode title in `subtitle`.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WatchedMeta {
    pub series_id: Option<String>,
    pub season: Option<i64>,
    pub episode: Option<i64>,
    pub title: Option<String>,
    pub subtitle: Option<String>,
    pub image: Option<String>,
    pub backdrop: Option<String>,
    pub ext: Option<String>,
    pub duration: Option<f64>,
}

#[tauri::command]
pub async fn mark_watched(
    state: State<'_, AppState>,
    kind: String,
    source_id: i64,
    item_id: String,
    watched: bool,
    meta: Option<WatchedMeta>,
) -> Result<()> {
    set_watched(&state.db.write(), &kind, source_id, &item_id, watched, meta.unwrap_or_default())
}

pub fn set_watched(
    conn: &Connection,
    kind: &str,
    source_id: i64,
    item_id: &str,
    watched: bool,
    meta: WatchedMeta,
) -> Result<()> {
    if !matches!(kind, "movie" | "episode") {
        return Err(Error::msg(format!("cannot mark {kind} as watched")));
    }
    let n = conn.execute(
        "UPDATE history SET watched = ?4, position = CASE WHEN ?4 THEN position ELSE 0 END, updated_at = ?5
          WHERE source_id = ?1 AND kind = ?2 AND item_id = ?3",
        params![source_id, kind, item_id, watched, now()],
    )?;
    if n > 0 || !watched {
        return Ok(());
    }
    let title = match meta.title {
        Some(t) => t,
        None if kind == "movie" => conn
            .query_row("SELECT title FROM movie WHERE source_id = ?1 AND id = ?2", params![source_id, item_id], |r| {
                r.get(0)
            })
            .optional()?
            .unwrap_or_default(),
        None => String::new(),
    };
    conn.execute(
        "INSERT INTO history (source_id, kind, item_id, series_id, season, episode, title, subtitle, image,
                              backdrop, ext, position, duration, watched, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 0, ?12, 1, ?13)",
        params![
            source_id,
            kind,
            item_id,
            meta.series_id,
            meta.season,
            meta.episode,
            title,
            meta.subtitle,
            meta.image,
            meta.backdrop,
            meta.ext,
            meta.duration.unwrap_or(0.0),
            now()
        ],
    )?;
    Ok(())
}

#[tauri::command]
pub async fn history_remove(state: State<'_, AppState>, kind: String, source_id: i64, item_id: String) -> Result<()> {
    let conn = state.db.write();
    conn.execute(
        "DELETE FROM history WHERE source_id = ?1 AND kind = ?2 AND item_id = ?3",
        params![source_id, kind, item_id],
    )?;
    Ok(())
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryItem {
    pub kind: String,
    pub source_id: i64,
    pub item_id: String,
    pub series_id: Option<String>,
    pub season: Option<i64>,
    pub episode: Option<i64>,
    pub title: String,
    pub subtitle: Option<String>,
    pub image: Option<String>,
    pub backdrop: Option<String>,
    pub ext: Option<String>,
    pub position: f64,
    pub duration: f64,
    pub updated_at: i64,
}

#[tauri::command]
pub async fn continue_watching(state: State<'_, AppState>, limit: Option<i64>) -> Result<Vec<HistoryItem>> {
    let conn = state.db.read();
    let mut stmt = conn.prepare_cached(
        "SELECT kind, source_id, item_id, series_id, season, episode, title, subtitle, image, backdrop, ext,
                position, duration, updated_at
           FROM history
          WHERE kind IN ('movie', 'episode') AND watched = 0 AND position > 30
          ORDER BY updated_at DESC LIMIT ?1",
    )?;
    let rows = stmt
        .query_map([limit.unwrap_or(20)], |r| {
            Ok(HistoryItem {
                kind: r.get(0)?,
                source_id: r.get(1)?,
                item_id: r.get(2)?,
                series_id: r.get(3)?,
                season: r.get(4)?,
                episode: r.get(5)?,
                title: r.get(6)?,
                subtitle: r.get(7)?,
                image: r.get(8)?,
                backdrop: r.get(9)?,
                ext: r.get(10)?,
                position: r.get(11)?,
                duration: r.get(12)?,
                updated_at: r.get(13)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecentChannel {
    #[serde(flatten)]
    pub channel: crate::catalog::ChannelItem,
    pub watched_at: i64,
}

#[tauri::command]
pub async fn recent_channels(state: State<'_, AppState>, limit: Option<i64>) -> Result<Vec<RecentChannel>> {
    let conn = state.db.read();
    let recent: Vec<(i64, String, i64)> = conn
        .prepare_cached(
            "SELECT source_id, item_id, updated_at FROM history WHERE kind = 'live' ORDER BY updated_at DESC LIMIT ?1",
        )?
        .query_map([limit.unwrap_or(20)], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<Result<_, _>>()?;
    Ok(recent
        .into_iter()
        .filter_map(|(sid, id, at)| {
            channel_by_id(&conn, sid, &id).ok().map(|channel| RecentChannel { channel, watched_at: at })
        })
        .collect())
}

/// Records that a live channel was tuned (for "recently watched").
pub fn touch_channel(conn: &rusqlite::Connection, source_id: i64, id: &str, title: &str, logo: Option<&str>) -> Result<()> {
    conn.execute(
        "INSERT INTO history (source_id, kind, item_id, title, image, updated_at) VALUES (?1, 'live', ?2, ?3, ?4, ?5)
         ON CONFLICT(source_id, kind, item_id) DO UPDATE SET updated_at = excluded.updated_at",
        params![source_id, id, title, logo, now()],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_conn;

    fn progress(position: f64, duration: f64) -> HistoryInput {
        HistoryInput {
            kind: "episode".into(),
            source_id: 1,
            item_id: "e1".into(),
            series_id: Some("s1".into()),
            season: Some(1),
            episode: Some(1),
            title: "Show".into(),
            subtitle: Some("Pilot".into()),
            image: None,
            backdrop: None,
            ext: Some("mkv".into()),
            position,
            duration,
        }
    }

    fn watched(c: &Connection, id: &str) -> bool {
        c.query_row("SELECT watched FROM history WHERE item_id = ?1", [id], |r| r.get(0)).unwrap()
    }

    #[test]
    fn finished_means_credits_or_last_eight_percent() {
        assert!(is_finished(920.0, 1000.0));
        assert!(is_finished(3500.0, 3600.0)); // last 3 minutes of a long item
        assert!(!is_finished(500.0, 1000.0));
        assert!(!is_finished(100.0, 250.0)); // short clip: only the 92% rule
        assert!(!is_finished(100.0, 0.0));
    }

    #[test]
    fn unknown_duration_keeps_a_finished_item_watched() {
        let c = test_conn();
        update_history(&c, &progress(8324.0, 8331.0)).unwrap();
        assert!(watched(&c, "e1"));
        // a periodic save after the stream was unloaded reports duration 0
        update_history(&c, &progress(8330.0, 0.0)).unwrap();
        assert!(watched(&c, "e1"));
        let d: f64 = c.query_row("SELECT duration FROM history", [], |r| r.get(0)).unwrap();
        assert_eq!(d, 8331.0);
        // starting over resets it
        update_history(&c, &progress(60.0, 8331.0)).unwrap();
        assert!(!watched(&c, "e1"));
    }

    #[test]
    fn marks_a_never_played_episode_watched() {
        let c = test_conn();
        let meta = WatchedMeta {
            series_id: Some("s1".into()),
            season: Some(1),
            episode: Some(3),
            title: Some("Show".into()),
            subtitle: Some("Episode 3".into()),
            duration: Some(2700.0),
            ..Default::default()
        };
        set_watched(&c, "episode", 1, "e3", true, meta).unwrap();
        let row: (String, i64, String, bool) = c
            .query_row("SELECT series_id, episode, title, watched FROM history WHERE item_id = 'e3'", [], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })
            .unwrap();
        assert_eq!(row, ("s1".into(), 3, "Show".into(), true));
        set_watched(&c, "episode", 1, "e3", false, WatchedMeta::default()).unwrap();
        assert!(!watched(&c, "e3"));
        // unmarking something never played is a no-op, live TV can't be marked
        set_watched(&c, "movie", 1, "m9", false, WatchedMeta::default()).unwrap();
        assert_eq!(c.query_row("SELECT COUNT(*) FROM history WHERE item_id = 'm9'", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
        assert!(set_watched(&c, "live", 1, "c1", true, WatchedMeta::default()).is_err());
    }
}
