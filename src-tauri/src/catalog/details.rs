//! Movie and series detail pages: every copy's provider details, the chosen
//! version, episodes and where to resume; plus "up next".

use std::collections::{BTreeMap, HashMap};

use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use serde_json::Value;
use tauri::State;

use crate::db::now;
use crate::error::{Error, Result};
use crate::kind::{ItemKind, ItemRef, WorkKind};
use crate::sources;
use crate::sources::SourceKind;
use crate::state::{AppState, read_blocking, write_blocking};
use crate::util::json::{f64_of, first_str, i64_of, str_of};
use crate::works::versions;
use crate::works::versions::VersionInfo;

use super::{
    MovieItem, SeriesItem, episode_history_filter, movie_by_item, movie_by_key, series_by_item,
    series_by_key,
};

const MOVIE_DETAIL_TTL: i64 = 7 * 86400;
const SERIES_DETAIL_TTL: i64 = 12 * 3600;

// --------------------------------------------------------------- details

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TechInfo {
    pub codec: Option<String>,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub hdr: bool,
    pub channels: Option<i64>,
    pub language: Option<String>,
}

fn tech(v: &Value) -> Option<TechInfo> {
    if !v.is_object() {
        return None;
    }
    let transfer = str_of(&v["color_transfer"]).unwrap_or_default();
    Some(TechInfo {
        codec: str_of(&v["codec_name"]),
        width: i64_of(&v["width"]),
        height: i64_of(&v["height"]),
        hdr: transfer.contains("2084") || transfer.contains("hlg"),
        channels: i64_of(&v["channels"]),
        language: str_of(&v["tags"]["language"]),
    })
}

/// Providers send backdrops as an array, a URL, or a stringified Python list.
fn first_url(v: &Value) -> Option<String> {
    match v {
        Value::Array(a) => a.iter().find_map(first_url),
        Value::String(s) => {
            let s = s.trim();
            if s.starts_with('[') {
                s.split(['\'', '"', ',', '[', ']', ' '])
                    .find(|p| p.starts_with("http"))
                    .map(str::to_owned)
            } else if s.starts_with("http") {
                Some(s.to_owned())
            } else {
                None
            }
        }
        _ => None,
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MovieDetail {
    /// The copy that plays (see `versions`), titled/illustrated as its work.
    #[serde(flatten)]
    pub item: MovieItem,
    pub plot: Option<String>,
    pub cast: Option<String>,
    pub director: Option<String>,
    pub genre: Option<String>,
    pub country: Option<String>,
    pub release_date: Option<String>,
    pub duration: Option<i64>,
    pub backdrop: Option<String>,
    pub trailer: Option<String>,
    pub age_rating: Option<String>,
    pub video: Option<TechInfo>,
    pub audio: Option<TechInfo>,
    pub position: f64,
    /// every provider copy of this movie, the playing one `selected`
    pub versions: Vec<VersionInfo>,
    /// some copies' provider details are still loading — ask again shortly
    pub versions_pending: bool,
    /// from TMDB, when a key is configured
    pub tmdb: Option<crate::tmdb::Facts>,
}

struct CachedDetail {
    json: Value,
    fetched_at: i64,
}

fn cached(
    conn: &Connection,
    source_id: i64,
    kind: WorkKind,
    id: &str,
) -> Result<Option<CachedDetail>> {
    Ok(conn
        .query_row(
            "SELECT json, fetched_at FROM detail_cache WHERE source_id = ?1 AND kind = ?2 AND id = ?3",
            params![source_id, kind, id],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)),
        )
        .optional()?
        .and_then(|(j, t)| serde_json::from_str(&j).ok().map(|json| CachedDetail { json, fetched_at: t })))
}

/// Provider detail JSON with a TTL cache; serves stale data when offline.
async fn provider_detail(
    st: &AppState,
    source_id: i64,
    kind: WorkKind,
    id: &str,
    ttl: i64,
) -> Result<Option<Value>> {
    let (src, cache) = {
        let id = id.to_owned();
        read_blocking(st, move |conn| {
            Ok((
                sources::load(conn, source_id)?,
                cached(conn, source_id, kind, &id)?,
            ))
        })
        .await?
    };
    if src.kind != SourceKind::Xtream {
        return Ok(None);
    }
    if let Some(c) = &cache
        && now() - c.fetched_at < ttl
    {
        return Ok(Some(c.json.clone()));
    }
    let fetched = match sources::xtream_for(&src, st.client(src.user_agent.as_deref())) {
        Ok(x) => match kind {
            WorkKind::Movie => x.vod_info(id).await,
            WorkKind::Series => x.series_info(id).await,
        },
        Err(e) => Err(e),
    };
    match fetched {
        Ok(v) if v.is_object() => {
            let (id, json) = (id.to_owned(), v.to_string());
            write_blocking(st, move |conn| {
                conn.execute(
                    "INSERT OR REPLACE INTO detail_cache (source_id, kind, id, json, fetched_at) VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![source_id, kind, id, json, now()],
                )?;
                Ok(())
            })
            .await?;
            Ok(Some(v))
        }
        Ok(_) => Ok(cache.map(|c| c.json)),
        Err(e) => match cache {
            Some(c) => {
                log::warn!("{kind} {id}: using stale detail ({e})");
                Ok(Some(c.json))
            }
            None => Err(e),
        },
    }
}

/// How long a detail page waits for the provider before showing what it has.
const DETAIL_DEADLINE: std::time::Duration = std::time::Duration::from_millis(2500);

/// Provider details of several copies at once. This provider stalls for ~9 s
/// on single requests now and then, so the page doesn't wait for every copy:
/// what arrives within `DETAIL_DEADLINE` (or from the cache) is used, the rest
/// keeps loading in the background (into the cache) and is reported as
/// pending — the UI asks again. At least one copy is always waited for.
async fn details_of(st: &AppState, kind: WorkKind, copies: &[ItemRef], ttl: i64) -> Details {
    let mut handles: Vec<Option<tokio::task::JoinHandle<Result<Option<Value>>>>> = copies
        .iter()
        .map(|c| {
            let (st, sid, id) = (st.clone(), c.source_id, c.id.clone());
            Some(tokio::spawn(async move {
                match provider_detail(&st, sid, kind, &id, ttl).await {
                    // M3U series: episodes come from the playlist
                    Ok(None) if kind == WorkKind::Series => {
                        read_blocking(&st, move |conn| m3u_series_json(conn, sid, &id)).await
                    }
                    other => other,
                }
            }))
        })
        .collect();
    let deadline = tokio::time::Instant::now() + DETAIL_DEADLINE;
    let mut out: Vec<Option<Value>> = vec![None; copies.len()];
    let mut finished = 0;
    for (i, h) in handles.iter_mut().enumerate() {
        let Some(handle) = h.as_mut() else { continue };
        if let Ok(joined) = tokio::time::timeout_at(deadline, handle).await {
            *h = None;
            finished += 1;
            match joined {
                Ok(Ok(v)) => out[i] = v,
                Ok(Err(e)) => log::warn!("{kind} {}: no provider detail ({e})", copies[i].id),
                Err(e) => log::warn!("{kind} {}: detail task failed ({e})", copies[i].id),
            }
        }
    }
    if finished == 0 {
        // nothing within the deadline: take the first copy that answers
        let pending: Vec<(usize, tokio::task::JoinHandle<Result<Option<Value>>>)> = handles
            .iter_mut()
            .enumerate()
            .filter_map(|(i, h)| h.take().map(|h| (i, h)))
            .collect();
        let (idx, futs): (Vec<usize>, Vec<_>) = pending.into_iter().unzip();
        if !futs.is_empty() {
            let (res, pos, rest) = futures_util::future::select_all(futs).await;
            if let Ok(Ok(v)) = res {
                out[idx[pos]] = v;
            }
            // the others keep filling the cache in the background
            drop(rest);
            return Details {
                per_copy: out,
                pending: idx.len() > 1,
            };
        }
    }
    Details {
        pending: handles.iter().any(Option::is_some),
        per_copy: out,
    }
}

/// What `details_of` got: one provider payload per copy (None: none yet).
struct Details {
    per_copy: Vec<Option<Value>>,
    /// some copies are still loading in the background
    pending: bool,
}

fn copies_of(members: &[versions::Member]) -> Vec<ItemRef> {
    members
        .iter()
        .map(|m| ItemRef::new(m.source_id, m.id.clone()))
        .collect()
}

/// What both detail pages start from: every copy of the work, the copy the
/// user picked, the language preferences and the work's key.
struct WorkContext {
    members: Vec<versions::Member>,
    pref: Option<ItemRef>,
    langs: Vec<&'static str>,
    key: Option<String>,
}

fn work_context(
    conn: &Connection,
    kind: WorkKind,
    source_id: i64,
    id: &str,
) -> Result<WorkContext> {
    let members = versions::members(conn, kind, source_id, id)?;
    if members.is_empty() {
        return Err(Error::NotFound(format!("{kind} {id}")));
    }
    let key = versions::work_key(conn, kind, source_id, id)?;
    let pref = match &key {
        Some(k) => versions::preference(conn, kind, k)?,
        None => None,
    };
    Ok(WorkContext {
        members,
        pref,
        langs: versions::language_prefs(conn),
        key,
    })
}

#[tauri::command]
pub async fn movie_detail(
    state: State<'_, AppState>,
    source_id: i64,
    id: String,
) -> Result<MovieDetail> {
    let st = state.inner().clone();
    let (
        WorkContext {
            members,
            pref,
            langs,
            ..
        },
        work,
        facts,
        tracks,
    ) = {
        let id = id.clone();
        read_blocking(&st, move |conn| {
            let ctx = work_context(conn, WorkKind::Movie, source_id, &id)?;
            let work = match &ctx.key {
                Some(k) => movie_by_key(conn, k)?,
                None => None,
            };
            let facts = match (&ctx.key, &work) {
                (Some(k), Some(w)) => {
                    crate::tmdb::facts_for(conn, WorkKind::Movie, k, &w.title, w.year)?
                }
                _ => None,
            };
            let tracks = ctx
                .members
                .iter()
                .map(|m| versions::tracks(conn, m.source_id, ItemKind::Movie, &m.id))
                .collect::<Result<Vec<_>>>()?;
            Ok((ctx, work, facts, tracks))
        })
        .await?
    };
    // provider details of every copy at once (cached for a week)
    let details = details_of(&st, WorkKind::Movie, &copies_of(&members), MOVIE_DETAIL_TTL).await;
    let infos: Vec<Value> = details
        .per_copy
        .iter()
        .map(|d| d.as_ref().map_or(Value::Null, |d| d["info"].clone()))
        .collect();
    let sel = choose_movie_copy(&members, pref.as_ref(), &langs, &infos, &tracks);
    let chosen = members[sel].clone();
    let info = &infos[sel];
    // missing fields from the other copies' details
    let pick = |keys: &[&str]| -> Option<String> {
        first_str(info, keys).or_else(|| infos.iter().find_map(|i| first_str(i, keys)))
    };

    let mut item = {
        let (sid, mid) = (chosen.source_id, chosen.id.clone());
        read_blocking(&st, move |conn| {
            movie_by_item(conn, sid, &mid)?.ok_or_else(|| Error::NotFound(format!("movie {mid}")))
        })
        .await?
    };
    if let Some(w) = &work {
        item.title = w.title.clone();
        item.year = w.year.or(item.year);
        item.poster = w.poster.clone().or(item.poster);
        item.rating = w.rating.or(item.rating);
        item.work = w.work.clone();
        item.favorite = w.favorite;
    }
    if item.poster.is_none() {
        item.poster = pick(&["cover_big", "movie_image"]);
    }
    if item.rating.is_none() {
        item.rating = f64_of(&info["rating"]).filter(|r| *r > 0.0);
    }
    let multi_source = members.iter().any(|m| m.source_id != members[0].source_id);
    let versions = members
        .iter()
        .zip(&infos)
        .zip(&tracks)
        .enumerate()
        .map(|(i, ((m, inf), tracks))| {
            let mut v = m.info(multi_source, i == sel);
            v.video = tech(&inf["video"]);
            v.audio = tech(&inf["audio"]);
            v.duration = duration_of(inf);
            v.tracks = tracks.clone();
            v
        })
        .collect();
    let trailer = match pick(&["youtube_trailer", "trailer"]) {
        Some(t) => Some(t),
        None => {
            let (sid, mid) = (chosen.source_id, chosen.id.clone());
            read_blocking(&st, move |conn| {
                Ok(conn
                    .query_row(
                        "SELECT trailer FROM movie WHERE source_id = ?1 AND id = ?2",
                        params![sid, mid],
                        |r| r.get(0),
                    )
                    .optional()?
                    .flatten())
            })
            .await?
        }
    };
    Ok(MovieDetail {
        versions_pending: details.pending,
        plot: pick(&["plot", "description"]),
        cast: pick(&["cast", "actors"]),
        director: pick(&["director"]),
        genre: pick(&["genre"]),
        country: pick(&["country"]),
        release_date: pick(&["releasedate", "release_date"]),
        duration: duration_of(info),
        backdrop: first_url(&info["backdrop_path"])
            .or_else(|| infos.iter().find_map(|i| first_url(&i["backdrop_path"]))),
        trailer,
        age_rating: pick(&["mpaa_rating", "age"]),
        video: tech(&info["video"]),
        audio: tech(&info["audio"]),
        position: movie_resume_position(&members, &chosen),
        versions,
        tmdb: facts,
        item,
    })
}

/// A copy's running time from its provider `info`, when it has one.
fn duration_of(info: &Value) -> Option<i64> {
    i64_of(&info["duration_secs"]).filter(|d| *d > 0)
}

/// Which copy of a movie plays: the user's pick, else the best fit. A copy
/// much shorter than the others is a trailer or cut off, and one that failed
/// to play is ruled out.
fn choose_movie_copy(
    members: &[versions::Member],
    pref: Option<&ItemRef>,
    langs: &[&'static str],
    infos: &[Value],
    tracks: &[Option<Value>],
) -> usize {
    let durations: Vec<Option<i64>> = infos.iter().map(duration_of).collect();
    let longest = durations.iter().flatten().copied().max().unwrap_or(0);
    versions::choose(members, pref, langs, |i| match durations[i] {
        _ if versions::unavailable(tracks[i].as_ref()) => versions::BROKEN,
        Some(d) if longest > 0 => d as f64 / longest as f64,
        _ => 1.0,
    })
}

/// Where a movie resumes: the chosen copy's own position, else where any
/// copy was left off unfinished (versions are the same film).
fn movie_resume_position(members: &[versions::Member], chosen: &versions::Member) -> f64 {
    if chosen.position > 0.0 {
        return chosen.position;
    }
    members
        .iter()
        .filter(|m| m.played_at > 0)
        .max_by_key(|m| m.played_at)
        .filter(|m| !m.watched)
        .map_or(0.0, |m| m.position)
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Episode {
    pub id: String,
    pub season: i64,
    pub episode: i64,
    pub title: String,
    pub plot: Option<String>,
    pub duration: Option<i64>,
    pub image: Option<String>,
    pub ext: Option<String>,
    pub rating: Option<f64>,
    pub air_date: Option<String>,
    pub video: Option<TechInfo>,
    pub position: f64,
    pub watched: bool,
    /// the copy (series row) this episode plays from
    pub source_id: i64,
    pub series_id: String,
    /// label of that copy when it isn't the selected version
    pub version: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Season {
    pub season: i64,
    pub name: String,
    pub cover: Option<String>,
    pub overview: Option<String>,
    pub air_date: Option<String>,
    pub episodes: Vec<Episode>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Resume {
    pub episode_id: String,
    pub season: i64,
    pub episode: i64,
    pub position: f64,
    pub started: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SeriesDetail {
    #[serde(flatten)]
    pub item: SeriesItem,
    pub plot: Option<String>,
    pub cast: Option<String>,
    pub director: Option<String>,
    pub release_date: Option<String>,
    pub trailer: Option<String>,
    pub seasons: Vec<Season>,
    pub resume: Option<Resume>,
    /// every provider copy; seasons/episodes above are their union, each
    /// episode from the selected copy when it has it
    pub versions: Vec<VersionInfo>,
    /// some copies' episode lists are still loading — ask again shortly
    pub versions_pending: bool,
    /// from TMDB, when a key is configured
    pub tmdb: Option<crate::tmdb::Facts>,
}

/// Episodes of an M3U series (`episode` table) in the shape of Xtream's
/// `get_series_info` (`{"episodes": {"1": [...]}}`), so series pages, resume
/// and Up next treat both source kinds alike.
pub fn m3u_series_json(
    conn: &Connection,
    source_id: i64,
    series_id: &str,
) -> Result<Option<Value>> {
    let mut stmt = conn.prepare_cached(
        "SELECT id, season, episode, title, image, ext FROM episode
          WHERE source_id = ?1 AND series_id = ?2 ORDER BY season, episode, position",
    )?;
    type Row = (String, i64, i64, String, Option<String>, Option<String>);
    let rows: Vec<Row> = stmt
        .query_map(params![source_id, series_id], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
            ))
        })?
        .collect::<Result<_, _>>()?;
    if rows.is_empty() {
        return Ok(None);
    }
    let mut seasons = serde_json::Map::new();
    for (id, season, episode, title, image, ext) in rows {
        if let Value::Array(list) = seasons
            .entry(season.to_string())
            .or_insert_with(|| Value::Array(Vec::new()))
        {
            list.push(serde_json::json!({
                "id": id, "episode_num": episode, "title": title, "container_extension": ext,
                "info": { "movie_image": image },
            }));
        }
    }
    Ok(Some(serde_json::json!({ "episodes": seasons })))
}

/// Provider `episodes` payload ({"1": [...]} or [[...]]) → seasons in order,
/// episodes sorted, without watch state. `source_id`/`series_id` name the
/// copy the payload belongs to.
fn parse_episodes(
    detail: &Value,
    series_title: &str,
    source_id: i64,
    series_id: &str,
) -> Vec<(i64, Vec<Episode>)> {
    let mut groups: Vec<(i64, Vec<Value>)> = match &detail["episodes"] {
        Value::Object(map) => map
            .iter()
            .map(|(k, v)| {
                (
                    k.parse().unwrap_or(0),
                    v.as_array().cloned().unwrap_or_default(),
                )
            })
            .collect(),
        Value::Array(arr) => arr
            .iter()
            .enumerate()
            .map(|(i, v)| {
                let eps = v.as_array().cloned().unwrap_or_default();
                let n = eps
                    .first()
                    .and_then(|e| i64_of(&e["season"]))
                    .unwrap_or(i as i64 + 1);
                (n, eps)
            })
            .collect(),
        _ => Vec::new(),
    };
    groups.sort_by_key(|(n, _)| *n);
    groups
        .into_iter()
        .map(|(n, eps)| {
            let mut episodes: Vec<Episode> = eps
                .iter()
                .enumerate()
                .filter_map(|(i, e)| {
                    let eid = str_of(&e["id"])?;
                    let ei = &e["info"];
                    let num = i64_of(&e["episode_num"]).unwrap_or(i as i64 + 1);
                    let raw_title = first_str(ei, &["name"])
                        .or_else(|| str_of(&e["title"]))
                        .unwrap_or_default();
                    Some(Episode {
                        title: episode_title(&raw_title, series_title, num),
                        season: n,
                        episode: num,
                        plot: first_str(ei, &["plot", "overview"]),
                        duration: i64_of(&ei["duration_secs"]).filter(|d| *d > 0),
                        image: first_str(ei, &["movie_image", "cover_big"]),
                        ext: str_of(&e["container_extension"]),
                        rating: f64_of(&ei["rating"]).filter(|r| *r > 0.0),
                        air_date: first_str(ei, &["releasedate", "air_date"]),
                        video: tech(&ei["video"]),
                        position: 0.0,
                        watched: false,
                        source_id,
                        series_id: series_id.to_owned(),
                        version: None,
                        id: eid,
                    })
                })
                .collect();
            episodes.sort_by_key(|e| e.episode);
            (n, episodes)
        })
        .collect()
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpNext {
    pub series: SeriesItem,
    pub episode: Episode,
}

/// Next episode for series whose most recently watched episode was finished
/// (partially watched ones already show in "Continue watching").
#[tauri::command]
pub async fn up_next(state: State<'_, AppState>, limit: Option<i64>) -> Result<Vec<UpNext>> {
    let limit = limit.unwrap_or(20).clamp(1, 50) as usize;
    read_blocking(state.inner(), move |conn| up_next_rows(conn, limit)).await
}

/// Episodes of one series copy from what is stored locally (the provider
/// detail cache, or the playlist for M3U series) — no network.
pub fn stored_episodes(
    conn: &Connection,
    source_id: i64,
    series_id: &str,
    title: &str,
) -> Result<Vec<(i64, Vec<Episode>)>> {
    let json = match cached(conn, source_id, WorkKind::Series, series_id)? {
        Some(d) => d.json,
        None => match m3u_series_json(conn, source_id, series_id)? {
            Some(v) => v,
            None => return Ok(Vec::new()),
        },
    };
    Ok(parse_episodes(&json, title, source_id, series_id))
}

/// Union of all copies' episodes keyed by (season, episode), each taken from
/// the first copy in `order` that has it.
fn union_episodes(
    per_copy: &[Vec<(i64, Vec<Episode>)>],
    order: &[usize],
) -> BTreeMap<(i64, i64), (usize, Episode)> {
    let mut out = BTreeMap::new();
    for &i in order {
        for (_, eps) in &per_copy[i] {
            for e in eps {
                out.entry((e.season, e.episode))
                    .or_insert_with(|| (i, e.clone()));
            }
        }
    }
    out
}

fn up_next_rows(conn: &Connection, limit: usize) -> Result<Vec<UpNext>> {
    // Latest episode per show — all copies of a show count as one. Timestamps
    // are whole seconds, so ties (two episodes marked within a second) go to
    // the furthest episode.
    type Latest = (i64, String, String, bool, Option<i64>, Option<i64>);
    let latest: Vec<Latest> = conn
        .prepare_cached(
            "SELECT source_id, series_id, item_id, watched, season, episode FROM (
                 SELECT h.source_id, h.series_id, h.item_id, h.watched, h.season, h.episode, h.updated_at,
                        ROW_NUMBER() OVER (
                            PARTITION BY COALESCE(s.work_key, 'item:' || h.source_id || ':' || h.series_id)
                            ORDER BY h.updated_at DESC, h.season DESC, h.episode DESC) AS n
                   FROM history h
                   LEFT JOIN series s ON s.source_id = h.source_id AND s.id = h.series_id
                  WHERE h.kind = 'episode' AND h.series_id IS NOT NULL)
              WHERE n = 1 ORDER BY updated_at DESC LIMIT 60",
        )?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)))?
        .collect::<Result<_, _>>()?;
    let mut out = Vec::new();
    for (sid, series_id, last_id, watched, season, episode) in latest {
        if !watched || out.len() >= limit {
            continue;
        }
        let members = versions::members(conn, WorkKind::Series, sid, &series_id)?;
        if members.is_empty() {
            continue;
        }
        let key = versions::work_key(conn, WorkKind::Series, sid, &series_id)?;
        let series = match &key {
            Some(k) => series_by_key(conn, k)?,
            None => series_by_item(conn, sid, &series_id)?,
        };
        let Some(series) = series else { continue };
        let per_copy: Vec<_> = members
            .iter()
            .map(|m| stored_episodes(conn, m.source_id, &m.id, &series.title))
            .collect::<Result<_>>()?;
        // continue in the copy that was being watched, then the chosen one
        let watching = members
            .iter()
            .position(|m| m.source_id == sid && m.id == series_id)
            .unwrap_or(0);
        let pref = match &key {
            Some(k) => versions::preference(conn, WorkKind::Series, k)?,
            None => None,
        };
        let chosen = versions::choose(
            &members,
            pref.as_ref(),
            &versions::language_prefs(conn),
            |_| 1.0,
        );
        let mut order = vec![watching, chosen];
        order.extend(0..members.len());
        order.dedup();
        let union = union_episodes(&per_copy, &order);
        // where we are: the history row's numbers, else find its episode id
        let here = match (season, episode) {
            (Some(s), Some(e)) => Some((s, e)),
            _ => union
                .iter()
                .find(|(_, (_, e))| e.id == last_id)
                .map(|(k, _)| *k),
        };
        let Some(here) = here else { continue };
        let Some((_, (copy, next))) = union
            .range((here.0, here.1 + 1)..)
            .find(|((s, _), _)| *s > 0)
        else {
            continue;
        };
        let mut next = next.clone();
        if *copy != chosen {
            next.version = Some(members[*copy].variant.label.clone());
        }
        let seen: bool = conn
            .query_row(
                &format!(
                    "SELECT MAX(h.watched) FROM history h
                       LEFT JOIN series s ON s.source_id = h.source_id AND s.id = h.series_id
                      WHERE h.kind = 'episode' AND h.season = ?1 AND h.episode = ?2 AND {}",
                    episode_history_filter(3, 4, 5)
                ),
                params![next.season, next.episode, key, sid, series_id],
                |r| r.get::<_, Option<bool>>(0),
            )?
            .unwrap_or(false);
        if !seen {
            out.push(UpNext {
                series,
                episode: next,
            });
        }
    }
    Ok(out)
}

/// "Deadly Ties - S01E01" / "S01E01 - Pilot" → "Pilot" or "Episode 1".
fn episode_title(raw: &str, series_title: &str, n: i64) -> String {
    static SXXEXX: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"(?i)\bS\d{1,3}\s*E\d{1,4}\b").expect("valid regex")
    });
    let mut t = raw.trim().to_owned();
    if let Some(m) = SXXEXX.find(&t) {
        let after = t[m.end()..]
            .trim()
            .trim_start_matches(['-', ':', '.', '|'])
            .trim()
            .to_owned();
        let before = t[..m.start()]
            .trim()
            .trim_end_matches(['-', ':', '.', '|'])
            .trim()
            .to_owned();
        t = if !after.is_empty() {
            after
        } else if !before.is_empty()
            && !before.eq_ignore_ascii_case(series_title)
            && !series_title.contains(&before)
        {
            before
        } else {
            String::new()
        };
    }
    if t.is_empty() {
        format!("Episode {n}")
    } else {
        t
    }
}

/// Long-form series fields that list rows don't carry.
struct SeriesText {
    plot: Option<String>,
    cast: Option<String>,
    director: Option<String>,
    release: Option<String>,
    trailer: Option<String>,
}

/// Watch state of one episode (of any copy of the show).
struct Watched {
    source_id: i64,
    id: String,
    season: Option<i64>,
    episode: Option<i64>,
    position: f64,
    watched: bool,
    at: i64,
}

#[tauri::command]
pub async fn series_detail(
    state: State<'_, AppState>,
    source_id: i64,
    id: String,
) -> Result<SeriesDetail> {
    let st = state.inner().clone();
    let (
        WorkContext {
            members,
            pref,
            langs,
            ..
        },
        work,
        text,
        history,
        facts,
    ) = {
        let id = id.clone();
        read_blocking(&st, move |conn| {
            let ctx = work_context(conn, WorkKind::Series, source_id, &id)?;
            let key = ctx.key.clone();
            let work = match &key {
                Some(k) => series_by_key(conn, k)?,
                None => None,
            };
            let work = match work {
                Some(w) => w,
                None => series_by_item(conn, source_id, &id)?
                    .ok_or_else(|| Error::NotFound(format!("series {id}")))?,
            };
            let text = series_text(conn, source_id, &id)?;
            let history = watch_history(conn, key.as_deref(), source_id, &id)?;
            let facts = match &key {
                Some(k) => {
                    crate::tmdb::facts_for(conn, WorkKind::Series, k, &work.title, work.year)?
                }
                None => None,
            };
            Ok((ctx, work, text, history, facts))
        })
        .await?
    };

    // every copy's episodes at once (cached for 12 h)
    let fetched = details_of(
        &st,
        WorkKind::Series,
        &copies_of(&members),
        SERIES_DETAIL_TTL,
    )
    .await;
    let details: Vec<Value> = fetched
        .per_copy
        .into_iter()
        .map(|d| d.unwrap_or(Value::Null))
        .collect();
    let per_copy: Vec<Vec<(i64, Vec<Episode>)>> = members
        .iter()
        .zip(&details)
        .map(|(m, d)| parse_episodes(d, &work.title, m.source_id, &m.id))
        .collect();
    let counts: Vec<usize> = per_copy
        .iter()
        .map(|seasons| episode_count(seasons))
        .collect();
    let tracks = copy_tracks(&st, &members, &per_copy).await?;
    let most = counts.iter().copied().max().unwrap_or(0).max(1);
    let sel = versions::choose(&members, pref.as_ref(), &langs, |i| {
        if versions::unavailable(tracks[i].as_ref()) {
            versions::BROKEN
        } else {
            counts[i] as f64 / most as f64
        }
    });

    // selected copy first, then the others by their own fit
    let mut order: Vec<usize> = (0..members.len()).collect();
    order.sort_by_key(|&i| {
        let m = &members[i];
        (
            i != sel,
            -(crate::works::variant::affinity(&m.variant, &langs)
                + crate::works::variant::score(&m.variant)),
        )
    });
    let union = union_episodes(&per_copy, &order);
    let state_of = latest_watch_state(&history, &per_copy);
    let seasons = build_seasons(&union, &state_of, &members, sel, &order, &details);
    let resume = resume_point(&seasons, &history, &per_copy);

    let multi_source = members.iter().any(|m| m.source_id != members[0].source_id);
    let versions: Vec<VersionInfo> = members
        .iter()
        .enumerate()
        .map(|(i, m)| {
            let mut v = m.info(multi_source, i == sel);
            v.seasons = per_copy[i]
                .iter()
                .filter(|(n, e)| *n > 0 && !e.is_empty())
                .map(|(n, _)| *n)
                .collect();
            v.episodes = counts[i] as i64;
            let first = per_copy[i].iter().flat_map(|(_, e)| e).next();
            v.video = first.and_then(|e| e.video.clone());
            v.duration = first.and_then(|e| e.duration);
            v.tracks = tracks[i].clone();
            v
        })
        .collect();

    let mut item = work;
    if item.backdrop.is_none() {
        item.backdrop = order
            .iter()
            .find_map(|&i| first_url(&details[i]["info"]["backdrop_path"]));
    }
    let info = &details[sel]["info"];
    Ok(SeriesDetail {
        versions_pending: fetched.pending,
        plot: text.plot.or_else(|| str_of(&info["plot"])),
        cast: text.cast.or_else(|| str_of(&info["cast"])),
        director: text.director.or_else(|| str_of(&info["director"])),
        release_date: text
            .release
            .or_else(|| first_str(info, &["releaseDate", "release_date"])),
        trailer: text.trailer.or_else(|| str_of(&info["youtube_trailer"])),
        seasons,
        resume,
        versions,
        tmdb: facts,
        item,
    })
}

/// Long texts of a series: the requested copy, else any copy that has them.
fn series_text(conn: &Connection, source_id: i64, id: &str) -> Result<SeriesText> {
    Ok(conn.query_row(
        "SELECT (SELECT plot FROM series x WHERE x.work_key IS s.work_key AND x.plot IS NOT NULL
                  ORDER BY x.source_id = s.source_id AND x.id = s.id DESC LIMIT 1),
                COALESCE(s.cast_list, (SELECT cast_list FROM series x WHERE x.work_key = s.work_key AND x.cast_list IS NOT NULL LIMIT 1)),
                COALESCE(s.director, (SELECT director FROM series x WHERE x.work_key = s.work_key AND x.director IS NOT NULL LIMIT 1)),
                COALESCE(s.release_date, (SELECT release_date FROM series x WHERE x.work_key = s.work_key AND x.release_date IS NOT NULL LIMIT 1)),
                COALESCE(s.trailer, (SELECT trailer FROM series x WHERE x.work_key = s.work_key AND x.trailer IS NOT NULL LIMIT 1))
           FROM series s WHERE s.source_id = ?1 AND s.id = ?2",
        params![source_id, id],
        |r| {
            Ok(SeriesText {
                plot: r.get(0)?,
                cast: r.get(1)?,
                director: r.get(2)?,
                release: r.get(3)?,
                trailer: r.get(4)?,
            })
        },
    )?)
}

/// Watch state of every episode of a show, across all its copies.
fn watch_history(
    conn: &Connection,
    key: Option<&str>,
    source_id: i64,
    id: &str,
) -> Result<Vec<Watched>> {
    Ok(conn
        .prepare_cached(&format!(
            "SELECT h.source_id, h.item_id, h.season, h.episode, h.position, h.watched, h.updated_at
               FROM history h
               LEFT JOIN series s ON s.source_id = h.source_id AND s.id = h.series_id
              WHERE h.kind = 'episode' AND {}",
            episode_history_filter(1, 2, 3)
        ))?
        .query_map(params![key, source_id, id], |r| {
            Ok(Watched {
                source_id: r.get(0)?,
                id: r.get(1)?,
                season: r.get(2)?,
                episode: r.get(3)?,
                position: r.get(4)?,
                watched: r.get(5)?,
                at: r.get(6)?,
            })
        })?
        .collect::<Result<_, _>>()?)
}

/// Regular episodes of one copy (specials, season 0, don't count).
fn episode_count(seasons: &[(i64, Vec<Episode>)]) -> usize {
    seasons
        .iter()
        .filter(|(n, _)| *n > 0)
        .map(|(_, e)| e.len())
        .sum()
}

/// Episodes probed per copy when looking for its stored tracks.
const TRACK_LOOKUP_EPISODES: usize = 40;

/// Tracks seen in each copy (the first of its episodes with any).
async fn copy_tracks(
    st: &AppState,
    members: &[versions::Member],
    per_copy: &[Vec<(i64, Vec<Episode>)>],
) -> Result<Vec<Option<Value>>> {
    let ids: Vec<(i64, Vec<String>)> = per_copy
        .iter()
        .zip(members)
        .map(|(seasons, m)| {
            (
                m.source_id,
                seasons
                    .iter()
                    .flat_map(|(_, e)| e.iter().map(|e| e.id.clone()))
                    .take(TRACK_LOOKUP_EPISODES)
                    .collect(),
            )
        })
        .collect();
    read_blocking(st, move |conn| {
        ids.iter()
            .map(|(sid, eps)| {
                for e in eps {
                    if let Some(t) = versions::tracks(conn, *sid, ItemKind::Episode, e)? {
                        return Ok(Some(t));
                    }
                }
                Ok(None)
            })
            .collect::<Result<Vec<_>>>()
    })
    .await
}

/// A history row's (season, episode): by the episode id in its copy, else
/// the numbers stored with it.
fn number_of(w: &Watched, per_copy: &[Vec<(i64, Vec<Episode>)>]) -> Option<(i64, i64)> {
    per_copy
        .iter()
        .flatten()
        .flat_map(|(_, eps)| eps)
        .find(|e| e.source_id == w.source_id && e.id == w.id)
        .map(|e| (e.season, e.episode))
        .or(match (w.season, w.episode) {
            (Some(s), Some(e)) => Some((s, e)),
            _ => None,
        })
}

/// The newest history row per (season, episode).
fn latest_watch_state<'h>(
    history: &'h [Watched],
    per_copy: &[Vec<(i64, Vec<Episode>)>],
) -> HashMap<(i64, i64), &'h Watched> {
    let mut state_of: HashMap<(i64, i64), &Watched> = HashMap::new();
    for w in history {
        if let Some(k) = number_of(w, per_copy)
            && state_of.get(&k).is_none_or(|prev| w.at > prev.at)
        {
            state_of.insert(k, w);
        }
    }
    state_of
}

/// The episode union as seasons, with watch state and, for episodes that
/// play from another copy, that copy's label. Season names and art come
/// from the selected copy, else any copy (in `order`).
fn build_seasons(
    union: &BTreeMap<(i64, i64), (usize, Episode)>,
    state_of: &HashMap<(i64, i64), &Watched>,
    members: &[versions::Member],
    sel: usize,
    order: &[usize],
    details: &[Value],
) -> Vec<Season> {
    let mut seasons: Vec<Season> = Vec::new();
    for ((s_no, _), (copy, e)) in union {
        let mut e = e.clone();
        if let Some(w) = state_of.get(&(e.season, e.episode)) {
            e.position = w.position;
            e.watched = w.watched;
        }
        if *copy != sel {
            e.version = Some(members[*copy].variant.label.clone());
        }
        match seasons.last_mut() {
            Some(last) if last.season == *s_no => last.episodes.push(e),
            _ => {
                let meta = order.iter().find_map(|&i| {
                    details[i]["seasons"].as_array().and_then(|a| {
                        a.iter()
                            .find(|m| i64_of(&m["season_number"]) == Some(*s_no))
                    })
                });
                seasons.push(Season {
                    season: *s_no,
                    name: meta.and_then(|m| str_of(&m["name"])).unwrap_or_else(|| {
                        if *s_no == 0 {
                            "Specials".into()
                        } else {
                            format!("Season {s_no}")
                        }
                    }),
                    cover: meta.and_then(|m| first_str(m, &["cover_big", "cover"])),
                    overview: meta.and_then(|m| str_of(&m["overview"])),
                    air_date: meta.and_then(|m| str_of(&m["air_date"])),
                    episodes: vec![e],
                });
            }
        }
    }
    seasons
}

/// Resume: the most recent unfinished episode, else the one after the most
/// recently finished, else S1E1 — across all copies.
fn resume_point(
    seasons: &[Season],
    history: &[Watched],
    per_copy: &[Vec<(i64, Vec<Episode>)>],
) -> Option<Resume> {
    let flat: Vec<&Episode> = seasons
        .iter()
        .filter(|s| s.season > 0)
        .flat_map(|s| &s.episodes)
        .collect();
    let at = |e: &Episode, position: f64, started: bool| Resume {
        episode_id: e.id.clone(),
        season: e.season,
        episode: e.episode,
        position,
        started,
    };
    let latest = history
        .iter()
        .filter_map(|w| number_of(w, per_copy).map(|k| (k, w)))
        .max_by_key(|(_, w)| w.at);
    match latest {
        Some((k, w)) if !w.watched => flat
            .iter()
            .find(|e| (e.season, e.episode) == k)
            .map(|e| at(e, w.position, true)),
        Some((k, _)) => flat
            .iter()
            .position(|e| (e.season, e.episode) == k)
            .and_then(|i| flat.get(i + 1))
            .map(|e| at(e, 0.0, true)),
        None => None,
    }
    .or_else(|| flat.first().map(|e| at(e, 0.0, false)))
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    #[test]
    fn episode_titles() {
        assert_eq!(
            episode_title("Deadly Ties - S01E01", "Deadly Ties", 1),
            "Episode 1"
        );
        assert_eq!(episode_title("S01E02 - The Pilot", "X", 2), "The Pilot");
        assert_eq!(episode_title("The Pilot", "X", 2), "The Pilot");
        assert_eq!(episode_title("", "X", 3), "Episode 3");
    }

    #[test]
    fn backdrop_variants() {
        assert_eq!(
            first_url(&Value::String("['https://a/b.jpg']".into())).as_deref(),
            Some("https://a/b.jpg")
        );
        assert_eq!(
            first_url(&serde_json::json!(["https://c/d.jpg"])).as_deref(),
            Some("https://c/d.jpg")
        );
        assert_eq!(first_url(&Value::String("".into())), None);
    }

    #[test]
    fn m3u_series_read_like_xtream_ones() {
        let c = crate::db::test_conn();
        c.execute("INSERT INTO series (source_id, id, name, title, position) VALUES (1, 'sh', 'Show', 'Show', 0)", [])
            .unwrap();
        for (id, season, ep, title) in [
            ("a", 1, 2, "Second"),
            ("b", 1, 1, ""),
            ("c", 2, 1, "Premiere"),
        ] {
            c.execute(
                "INSERT INTO episode (source_id, series_id, id, season, episode, title, url, ext, position)
                 VALUES (1, 'sh', ?1, ?2, ?3, ?4, 'http://h/x.mkv', 'mkv', 0)",
                params![id, season, ep, title],
            )
            .unwrap();
        }
        let json = m3u_series_json(&c, 1, "sh").unwrap().unwrap();
        let seasons = parse_episodes(&json, "Show", 1, "sh");
        type Shape = Vec<(i64, Vec<(String, i64, String)>)>;
        let shape: Shape = seasons
            .into_iter()
            .map(|(n, eps)| {
                (
                    n,
                    eps.into_iter()
                        .map(|e| (e.id, e.episode, e.title))
                        .collect(),
                )
            })
            .collect();
        assert_eq!(
            shape,
            vec![
                (
                    1,
                    vec![
                        ("b".into(), 1, "Episode 1".into()),
                        ("a".into(), 2, "Second".into())
                    ]
                ),
                (2, vec![("c".into(), 1, "Premiere".into())]),
            ]
        );
        assert!(m3u_series_json(&c, 1, "missing").unwrap().is_none());
        // Up next works without a provider detail cache
        c.execute(
            "INSERT INTO history (source_id, kind, item_id, series_id, season, episode, title, watched, updated_at)
             VALUES (1, 'episode', 'a', 'sh', 1, 2, 'Show', 1, 100)",
            [],
        )
        .unwrap();
        let next: Vec<String> = up_next_rows(&c, 20)
            .unwrap()
            .into_iter()
            .map(|u| u.episode.id)
            .collect();
        assert_eq!(next, vec!["c"]);
    }

    /// Two copies of one show: the Nordic one only has season 5.
    pub(in crate::catalog) fn two_copy_show(c: &Connection) {
        for (cat, name) in [("ap", "APPLE+ SERIES"), ("sc", "NORDIC SERIES")] {
            c.execute("INSERT INTO category (source_id, kind, id, name, title, position) VALUES (1, 'series', ?1, ?2, ?2, 0)", params![cat, name])
                .unwrap();
        }
        for (id, tag, cat) in [("full", "A+", "ap"), ("s5", "SC", "sc")] {
            c.execute(
                "INSERT INTO series (source_id, id, name, title, tag, year, tmdb, category_id, position)
                 VALUES (1, ?1, 'x', 'Show', ?2, 2019, '777', ?3, 0)",
                params![id, tag, cat],
            )
            .unwrap();
        }
        let eps = |prefix: &str, seasons: std::ops::RangeInclusive<i64>| {
            let mut map = serde_json::Map::new();
            for season in seasons {
                let list: Vec<Value> = (1..=3)
                    .map(|n| serde_json::json!({"id": format!("{prefix}{season}{n}"), "episode_num": n, "title": format!("E{n}")}))
                    .collect();
                map.insert(season.to_string(), Value::Array(list));
            }
            serde_json::json!({ "episodes": map }).to_string()
        };
        for (id, json) in [("full", eps("f", 1..=5)), ("s5", eps("n", 5..=5))] {
            c.execute("INSERT INTO detail_cache (source_id, kind, id, json, fetched_at) VALUES (1, 'series', ?1, ?2, 0)", params![id, json])
                .unwrap();
        }
        crate::works::rebuild(c).unwrap();
    }

    fn finish(c: &Connection, series: &str, ep_id: &str, season: i64, episode: i64, at: i64) {
        c.execute(
            "INSERT OR REPLACE INTO history (source_id, kind, item_id, series_id, season, episode, title, watched, updated_at)
             VALUES (1, 'episode', ?1, ?2, ?3, ?4, 'Show', 1, ?5)",
            params![ep_id, series, season, episode, at],
        )
        .unwrap();
    }

    #[test]
    fn up_next_follows_a_show_across_its_copies() {
        let c = crate::db::test_conn();
        two_copy_show(&c);
        let next = |c: &Connection| -> Vec<(String, String, i64, i64, Option<String>)> {
            up_next_rows(c, 20)
                .unwrap()
                .into_iter()
                .map(|u| {
                    (
                        u.episode.series_id,
                        u.episode.id,
                        u.episode.season,
                        u.episode.episode,
                        u.episode.version,
                    )
                })
                .collect()
        };
        // one entry per show, continuing in the copy being watched
        finish(&c, "full", "f11", 1, 1, 100);
        assert_eq!(next(&c), vec![("full".into(), "f12".into(), 1, 2, None)]);
        // switched to the Nordic copy for season 5: continue there
        finish(&c, "s5", "n51", 5, 1, 200);
        assert_eq!(next(&c), vec![("s5".into(), "n52".into(), 5, 2, None)]);
        // the next one was already seen in the other copy: nothing to offer
        finish(&c, "full", "f52", 5, 2, 150);
        assert!(next(&c).is_empty());
        // after the Nordic copy's last episode there is nothing more anywhere
        finish(&c, "s5", "n53", 5, 3, 300);
        assert!(next(&c).is_empty());
    }

    #[test]
    fn up_next_takes_missing_episodes_from_another_copy() {
        let c = crate::db::test_conn();
        two_copy_show(&c);
        // chose the Nordic copy (season 5 only) but watched S4E3 in the full one
        c.execute("INSERT INTO work_pref (kind, key, source_id, item_id, updated_at) VALUES ('series', 'tmdb:777', 1, 's5', 0)", [])
            .unwrap();
        finish(&c, "full", "f43", 4, 3, 100);
        let u = up_next_rows(&c, 20).unwrap();
        assert_eq!(u.len(), 1);
        // S5E1 continues in the copy that was being watched
        assert_eq!(
            (
                u[0].episode.id.as_str(),
                u[0].episode.season,
                u[0].episode.episode
            ),
            ("f51", 5, 1)
        );
        assert_eq!(u[0].series.title, "Show");
        assert_eq!(u[0].series.work.version_count, 2);
    }

    #[test]
    fn up_next_follows_the_latest_finished_episode() {
        let c = crate::db::test_conn();
        c.execute("INSERT INTO series (source_id, id, name, title, position) VALUES (1, 's1', 'Show', 'Show', 0)", [])
            .unwrap();
        let episodes: Vec<Value> = (1..=5)
            .map(|n| serde_json::json!({"id": format!("e{n}"), "episode_num": n, "title": format!("Show - S01E0{n}")}))
            .collect();
        let detail = serde_json::json!({ "episodes": { "1": episodes } }).to_string();
        c.execute("INSERT INTO detail_cache (source_id, kind, id, json, fetched_at) VALUES (1, 'series', 's1', ?1, 0)", [detail])
            .unwrap();
        let finish = |ep: i64, at: i64| {
            c.execute(
                "INSERT OR REPLACE INTO history (source_id, kind, item_id, series_id, season, episode, title, watched, updated_at)
                 VALUES (1, 'episode', ?1, 's1', 1, ?2, 'Show', 1, ?3)",
                params![format!("e{ep}"), ep, at],
            )
            .unwrap();
        };
        let next = |c: &Connection| -> Vec<String> {
            up_next_rows(c, 20)
                .unwrap()
                .into_iter()
                .map(|u| u.episode.id)
                .collect()
        };

        finish(1, 100);
        assert_eq!(next(&c), vec!["e2"]);
        // two episodes finished within the same second: one entry, after the furthest
        finish(4, 200);
        finish(2, 200);
        assert_eq!(next(&c), vec!["e5"]);
        // nothing after the last episode
        finish(5, 300);
        assert!(next(&c).is_empty());
    }

    fn ep(id: &str, season: i64, episode: i64) -> Episode {
        Episode {
            id: id.into(),
            season,
            episode,
            title: String::new(),
            plot: None,
            duration: None,
            image: None,
            ext: None,
            rating: None,
            air_date: None,
            video: None,
            position: 0.0,
            watched: false,
            source_id: 1,
            series_id: "s".into(),
            version: None,
        }
    }

    fn watched(id: &str, season: i64, episode: i64, position: f64, done: bool, at: i64) -> Watched {
        Watched {
            source_id: 1,
            id: id.into(),
            season: Some(season),
            episode: Some(episode),
            position,
            watched: done,
            at,
        }
    }

    #[test]
    fn resume_point_picks_unfinished_then_next_then_first() {
        let season = |n: i64, eps: Vec<Episode>| Season {
            season: n,
            name: String::new(),
            cover: None,
            overview: None,
            air_date: None,
            episodes: eps,
        };
        // specials never count as "the first episode"
        let seasons = vec![
            season(0, vec![ep("sp", 0, 1)]),
            season(1, vec![ep("a", 1, 1), ep("b", 1, 2), ep("c", 1, 3)]),
        ];
        let resume = |history: &[Watched]| {
            resume_point(&seasons, history, &[]).map(|r| (r.episode_id, r.position, r.started))
        };
        assert_eq!(
            resume(&[]),
            Some(("a".into(), 0.0, false)),
            "nothing watched: S1E1"
        );
        assert_eq!(
            resume(&[watched("b", 1, 2, 42.0, false, 10)]),
            Some(("b".into(), 42.0, true)),
            "the latest episode is unfinished: resume it"
        );
        assert_eq!(
            resume(&[
                watched("b", 1, 2, 0.0, true, 10),
                watched("a", 1, 1, 5.0, false, 5)
            ]),
            Some(("c".into(), 0.0, true)),
            "the latest episode was finished: the one after it"
        );
        assert_eq!(
            resume(&[watched("c", 1, 3, 0.0, true, 10)]),
            Some(("a".into(), 0.0, false)),
            "the last episode was finished: back to the start"
        );
    }
}
