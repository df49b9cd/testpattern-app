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
    toggle_favorite(&conn, &kind, source_id, &item_id)
}

/// Movies and series are favorites as a whole: a work counts as favorite when
/// any of its copies is, and un-favoriting clears every copy.
pub fn toggle_favorite(conn: &Connection, kind: &str, source_id: i64, item_id: &str) -> Result<bool> {
    let removed = if kind == "live" {
        conn.execute(
            "DELETE FROM favorite WHERE source_id = ?1 AND kind = ?2 AND item_id = ?3",
            params![source_id, kind, item_id],
        )?
    } else {
        let table = if kind == "movie" { "movie" } else { "series" };
        conn.execute(
            &format!(
                "DELETE FROM favorite WHERE kind = ?2 AND (
                     (source_id = ?1 AND item_id = ?3)
                     OR (source_id, item_id) IN (
                         SELECT x.source_id, x.id FROM {table} x
                          WHERE x.work_key = (SELECT work_key FROM {table} WHERE source_id = ?1 AND id = ?3)))"
            ),
            params![source_id, kind, item_id],
        )?
    };
    if removed > 0 {
        return Ok(false);
    }
    conn.execute(
        "INSERT INTO favorite (source_id, kind, item_id, added_at) VALUES (?1, ?2, ?3, ?4)",
        params![source_id, kind, item_id, now()],
    )?;
    Ok(true)
}

/// Makes `(source_id, id)` the feed that plays for its channel group.
pub fn prefer_channel(conn: &Connection, key: &str, source_id: i64, id: &str) -> Result<()> {
    let member: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM channel WHERE source_id = ?1 AND id = ?2 AND group_key = ?3)",
        params![source_id, id, key],
        |r| r.get(0),
    )?;
    if !member {
        return Err(Error::NotFound(format!("channel {id} in {key}")));
    }
    conn.execute(
        "INSERT INTO channel_pref (key, source_id, item_id, updated_at) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(key) DO UPDATE SET source_id = excluded.source_id, item_id = excluded.item_id,
                                        updated_at = excluded.updated_at",
        params![key, source_id, id, now()],
    )?;
    conn.execute("UPDATE channel_group SET source_id = ?2, item_id = ?3 WHERE key = ?1", params![key, source_id, id])?;
    Ok(())
}

#[tauri::command]
pub async fn channel_prefer(state: State<'_, AppState>, key: String, source_id: i64, id: String) -> Result<()> {
    prefer_channel(&state.db.write(), &key, source_id, &id)
}

/// Heart on a channel row (all feeds of the channel): clears the favorite
/// of every feed, or makes the playing feed a favorite.
pub fn toggle_group_favorite(conn: &Connection, key: &str) -> Result<bool> {
    let removed = conn.execute(
        "DELETE FROM favorite WHERE kind = 'live'
            AND (source_id, item_id) IN (SELECT source_id, id FROM channel WHERE group_key = ?1)",
        [key],
    )?;
    if removed > 0 {
        return Ok(false);
    }
    let (source_id, id): (i64, String) = conn
        .query_row("SELECT source_id, item_id FROM channel_group WHERE key = ?1", [key], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()?
        .ok_or_else(|| Error::NotFound(format!("channel {key}")))?;
    conn.execute(
        "INSERT INTO favorite (source_id, kind, item_id, added_at) VALUES (?1, 'live', ?2, ?3)",
        params![source_id, id, now()],
    )?;
    Ok(true)
}

#[tauri::command]
pub async fn channel_group_favorite(state: State<'_, AppState>, key: String) -> Result<bool> {
    toggle_group_favorite(&state.db.write(), &key)
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
    continue_watching_rows(&state.db.read(), limit.unwrap_or(20))
}

pub fn continue_watching_rows(conn: &Connection, limit: i64) -> Result<Vec<HistoryItem>> {
    // One entry per title: the latest play of any copy (episode of any copy of
    // a show). A title whose latest play was finished is not "in progress".
    let mut stmt = conn.prepare_cached(
        "SELECT kind, source_id, item_id, series_id, season, episode, title, subtitle, image, backdrop, ext,
                position, duration, updated_at
           FROM (SELECT h.*, ROW_NUMBER() OVER (
                        PARTITION BY COALESCE(m.work_key, s.work_key,
                                              h.kind || ':' || h.source_id || ':' || COALESCE(h.series_id, h.item_id))
                        ORDER BY h.updated_at DESC) AS n
                   FROM history h
                   LEFT JOIN movie m ON h.kind = 'movie' AND m.source_id = h.source_id AND m.id = h.item_id
                   LEFT JOIN series s ON h.kind = 'episode' AND s.source_id = h.source_id AND s.id = h.series_id
                  WHERE h.kind IN ('movie', 'episode'))
          WHERE n = 1 AND watched = 0 AND position > 30
          ORDER BY updated_at DESC LIMIT ?1",
    )?;
    let rows = stmt
        .query_map([limit], |r| {
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
    // one entry per channel: the feed watched last
    let recent: Vec<(i64, String, i64)> = conn
        .prepare_cached(
            "SELECT source_id, item_id, updated_at
               FROM (SELECT h.source_id, h.item_id, h.updated_at,
                            ROW_NUMBER() OVER (PARTITION BY COALESCE(c.group_key, h.source_id || ':' || h.item_id)
                                               ORDER BY h.updated_at DESC) AS n
                       FROM history h
                       LEFT JOIN channel c ON c.source_id = h.source_id AND c.id = h.item_id
                      WHERE h.kind = 'live')
              WHERE n = 1 ORDER BY updated_at DESC LIMIT ?1",
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

    fn two_copies(c: &Connection) {
        for id in ["m1", "m2"] {
            c.execute(
                "INSERT INTO movie (source_id, id, name, title, year, tmdb, position) VALUES (1, ?1, 'Film', 'Film', 2020, '5', 0)",
                [id],
            )
            .unwrap();
        }
        crate::works::rebuild(c).unwrap();
    }

    fn sky_feeds(c: &Connection) {
        c.execute("INSERT INTO category (source_id, kind, id, name, title, region, position) VALUES (1, 'live', 'sp', 'SPORT', 'SPORT', 'UK', 0)", [])
            .unwrap();
        for (id, badges) in [("hd", "HD"), ("sd", "SD"), ("raw", "RAW")] {
            c.execute(
                "INSERT INTO channel (source_id, id, name, title, badges, category_id, position) VALUES (1, ?1, 'SKY', 'SKY SPORTS F1', ?2, 'sp', 0)",
                params![id, badges],
            )
            .unwrap();
        }
        crate::works::rebuild(c).unwrap();
    }

    #[test]
    fn channel_feeds_play_as_one_channel() {
        let c = crate::db::test_conn();
        sky_feeds(&c);
        let key = "UK|skysportsf1";
        let playing = |c: &Connection| crate::catalog::channel_group_by_key(c, key).unwrap().id;
        assert_eq!(playing(&c), "hd");
        prefer_channel(&c, key, 1, "raw").unwrap();
        assert_eq!(playing(&c), "raw");
        assert!(prefer_channel(&c, key, 1, "nope").is_err());
        // the pick survives regrouping (a sync)
        crate::works::rebuild(&c).unwrap();
        assert_eq!(playing(&c), "raw");
        // the heart covers every feed
        assert!(toggle_group_favorite(&c, key).unwrap());
        c.execute("INSERT INTO favorite (source_id, kind, item_id, added_at) VALUES (1, 'live', 'sd', 9)", []).unwrap();
        assert!(!toggle_group_favorite(&c, key).unwrap());
        let n: i64 = c.query_row("SELECT COUNT(*) FROM favorite", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
        // grouped lists: one row, variant chips list all three
        let q = crate::catalog::ChannelQuery { grouped: true, country: Some("UK".into()), ..Default::default() };
        let page = crate::catalog::query_channels(&c, &q).unwrap();
        assert_eq!((page.total, page.items[0].group.as_ref().unwrap().variants), (1, 3));
        let v = crate::catalog::variants_of(&c, key).unwrap();
        assert_eq!(v.iter().map(|v| v.label.as_str()).collect::<Vec<_>>(), vec!["HD", "RAW", "SD"]);
        assert!(v[1].selected);
        let nav = crate::catalog::live_nav_for(&c).unwrap();
        assert_eq!((nav.countries[0].name.as_str(), nav.cells[0].genre.as_str(), nav.cells[0].count), ("United Kingdom", "Sports", 1));
    }

    #[test]
    fn favorites_belong_to_the_whole_work() {
        let c = crate::db::test_conn();
        two_copies(&c);
        assert!(toggle_favorite(&c, "movie", 1, "m1").unwrap());
        // un-favoriting through the other copy clears the work
        assert!(!toggle_favorite(&c, "movie", 1, "m2").unwrap());
        let n: i64 = c.query_row("SELECT COUNT(*) FROM favorite", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn continue_watching_lists_a_title_once() {
        let c = crate::db::test_conn();
        two_copies(&c);
        let play = |kind: &str, id: &str, series: Option<&str>, pos: f64, watched: bool, at: i64| {
            c.execute(
                "INSERT OR REPLACE INTO history (source_id, kind, item_id, series_id, title, position, duration, watched, updated_at)
                 VALUES (1, ?1, ?2, ?3, 'Title', ?4, 3000, ?5, ?6)",
                params![kind, id, series, pos, watched, at],
            )
            .unwrap();
        };
        let listed = |c: &Connection| -> Vec<String> {
            continue_watching_rows(c, 20).unwrap().into_iter().map(|h| h.item_id).collect()
        };
        play("movie", "m1", None, 600.0, false, 100);
        play("movie", "m2", None, 900.0, false, 200);
        assert_eq!(listed(&c), vec!["m2"]);
        // finished in the other copy: no longer in progress
        play("movie", "m1", None, 2990.0, true, 300);
        assert!(listed(&c).is_empty());

        // a show: episodes of all its copies are one title, the latest counts
        for id in ["s1", "s2"] {
            c.execute(
                "INSERT INTO series (source_id, id, name, title, year, tmdb, position) VALUES (1, ?1, 'Show', 'Show', 2020, '9', 0)",
                [id],
            )
            .unwrap();
        }
        crate::works::rebuild(&c).unwrap();
        play("episode", "e1", Some("s1"), 400.0, false, 400);
        play("episode", "e7", Some("s2"), 700.0, false, 500);
        play("episode", "x1", Some("elsewhere"), 20.0, false, 550); // barely started
        assert_eq!(listed(&c), vec!["e7"]);
        play("episode", "e8", Some("s1"), 2990.0, true, 600);
        assert!(listed(&c).is_empty());
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
