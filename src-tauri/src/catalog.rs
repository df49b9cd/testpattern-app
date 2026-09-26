//! Read-side commands: categories, channel/movie/series listings, details,
//! programme guide and search.

use std::time::Instant;

use base64::Engine;
use rusqlite::types::Value as Sql;
use rusqlite::{Connection, OptionalExtension, Row, params, params_from_iter};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::State;

use crate::db::now;
use crate::epg::{self, ProgrammeRow};
use crate::error::{Error, Result};
use crate::settings;
use crate::sources::{self, SourceKind};
use crate::state::{AppState, http_client};
use crate::util::json::{f64_of, first_str, i64_of, str_of};

const MOVIE_DETAIL_TTL: i64 = 7 * 86400;
const SERIES_DETAIL_TTL: i64 = 12 * 3600;

// ---------------------------------------------------------------- types

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Page<T> {
    pub total: i64,
    pub items: Vec<T>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CategoryItem {
    pub source_id: i64,
    pub id: String,
    pub title: String,
    pub region: Option<String>,
    pub badges: Vec<String>,
    pub count: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Brief {
    pub title: String,
    pub start: i64,
    pub stop: i64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelItem {
    pub source_id: i64,
    pub id: String,
    pub num: Option<i64>,
    pub title: String,
    pub logo: Option<String>,
    pub epg_id: Option<String>,
    pub category_id: Option<String>,
    pub badges: Vec<String>,
    pub archive: bool,
    pub archive_days: i64,
    pub favorite: bool,
    pub now: Option<Brief>,
    pub next: Option<Brief>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MovieItem {
    pub source_id: i64,
    pub id: String,
    pub title: String,
    pub year: Option<i64>,
    pub poster: Option<String>,
    pub rating: Option<f64>,
    pub added: Option<i64>,
    pub tag: Option<String>,
    pub ext: Option<String>,
    pub favorite: bool,
    /// 0..1 watch progress, when started.
    pub progress: Option<f64>,
    pub watched: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SeriesItem {
    pub source_id: i64,
    pub id: String,
    pub title: String,
    pub year: Option<i64>,
    pub cover: Option<String>,
    pub backdrop: Option<String>,
    pub rating: Option<f64>,
    pub genre: Option<String>,
    pub last_modified: Option<i64>,
    pub tag: Option<String>,
    pub favorite: bool,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelQuery {
    pub source_id: Option<i64>,
    pub category_id: Option<String>,
    #[serde(default)]
    pub favorites: bool,
    /// Only channels that have programme data (TV guide).
    #[serde(default)]
    pub with_epg: bool,
    pub q: Option<String>,
    pub offset: Option<i64>,
    pub limit: Option<i64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaQuery {
    pub source_id: Option<i64>,
    pub category_id: Option<String>,
    #[serde(default)]
    pub favorites: bool,
    pub q: Option<String>,
    /// 'added' (default) | 'title' | 'rating' | 'year'
    pub sort: Option<String>,
    pub offset: Option<i64>,
    pub limit: Option<i64>,
}

/// Badges are stored space separated; multi-word ones ("DOLBY VISION") are
/// re-joined here.
fn split_badges(s: String) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for w in s.split_whitespace() {
        match (out.last_mut(), w) {
            (Some(last), "VISION" | "AUDIO") if last == "DOLBY" => {
                last.push(' ');
                last.push_str(w);
            }
            _ => out.push(w.to_owned()),
        }
    }
    out
}

async fn blocking<T: Send + 'static>(
    st: &AppState,
    f: impl FnOnce(&Connection) -> Result<T> + Send + 'static,
) -> Result<T> {
    let st = st.clone();
    tokio::task::spawn_blocking(move || {
        let conn = st.db.read();
        f(&conn)
    })
    .await
    .map_err(|e| Error::msg(e.to_string()))?
}

fn like_pattern(q: &str) -> String {
    let escaped = q.trim().replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_");
    format!("%{escaped}%")
}

// ------------------------------------------------------------ categories

#[tauri::command]
pub async fn categories(state: State<'_, AppState>, kind: String) -> Result<Vec<CategoryItem>> {
    let table = match kind.as_str() {
        "live" => "channel",
        "movie" => "movie",
        "series" => "series",
        _ => return Err(Error::msg(format!("unknown category kind {kind}"))),
    };
    blocking(state.inner(), move |conn| {
        let show_adult = settings::get_bool(conn, "content.showAdult");
        let extra = if table == "channel" { " AND x.separator = 0" } else { "" };
        let sql = format!(
            "SELECT c.source_id, c.id, c.title, c.region, c.badges,
                    (SELECT COUNT(*) FROM {table} x WHERE x.source_id = c.source_id AND x.category_id = c.id{extra}) AS n
               FROM category c
              WHERE c.kind = ?1 AND (?2 OR c.adult = 0)
              ORDER BY c.source_id, c.position"
        );
        let mut stmt = conn.prepare_cached(&sql)?;
        let rows = stmt
            .query_map(params![kind, show_adult], |r| {
                Ok(CategoryItem {
                    source_id: r.get(0)?,
                    id: r.get(1)?,
                    title: r.get(2)?,
                    region: r.get(3)?,
                    badges: split_badges(r.get(4)?),
                    count: r.get(5)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows.into_iter().filter(|c| c.count > 0).collect())
    })
    .await
}

// -------------------------------------------------------------- channels

const CHANNEL_SELECT: &str = "
    SELECT c.source_id, c.id, c.num, c.title, c.logo, c.epg_id, c.category_id, c.badges,
           c.archive, c.archive_days,
           EXISTS(SELECT 1 FROM favorite f WHERE f.source_id = c.source_id AND f.kind = 'live' AND f.item_id = c.id),
           p.title, p.start, p.stop, q.title, q.start, q.stop
      FROM channel c
      LEFT JOIN programme p ON p.source_id = c.source_id AND p.epg_id = c.epg_id
           AND p.start = (SELECT MAX(start) FROM programme
                           WHERE source_id = c.source_id AND epg_id = c.epg_id AND start <= :now)
           AND p.stop > :now
      LEFT JOIN programme q ON q.source_id = c.source_id AND q.epg_id = c.epg_id
           AND q.start = (SELECT MIN(start) FROM programme
                           WHERE source_id = c.source_id AND epg_id = c.epg_id AND start > :now)";

fn row_to_channel(r: &Row) -> rusqlite::Result<ChannelItem> {
    let brief = |i: usize| -> rusqlite::Result<Option<Brief>> {
        Ok(match r.get::<_, Option<String>>(i)? {
            Some(title) => Some(Brief { title, start: r.get(i + 1)?, stop: r.get(i + 2)? }),
            None => None,
        })
    };
    Ok(ChannelItem {
        source_id: r.get(0)?,
        id: r.get(1)?,
        num: r.get(2)?,
        title: r.get(3)?,
        logo: r.get(4)?,
        epg_id: r.get(5)?,
        category_id: r.get(6)?,
        badges: split_badges(r.get(7)?),
        archive: r.get(8)?,
        archive_days: r.get(9)?,
        favorite: r.get(10)?,
        now: brief(11)?,
        next: brief(14)?,
    })
}

pub fn query_channels(conn: &Connection, q: &ChannelQuery) -> Result<Page<ChannelItem>> {
    let mut filters = vec!["c.separator = 0".to_owned()];
    let mut args: Vec<(String, Sql)> = vec![(":now".into(), Sql::Integer(now()))];
    if !settings::get_bool(conn, "content.showAdult") {
        filters.push("c.adult = 0".into());
    }
    if let Some(s) = q.source_id {
        filters.push("c.source_id = :source".into());
        args.push((":source".into(), Sql::Integer(s)));
    }
    if let Some(cat) = &q.category_id {
        filters.push("c.category_id = :cat".into());
        args.push((":cat".into(), Sql::Text(cat.clone())));
    }
    if q.with_epg {
        filters.push(
            "c.epg_id IS NOT NULL AND EXISTS (SELECT 1 FROM programme pe WHERE pe.source_id = c.source_id AND pe.epg_id = c.epg_id)"
                .into(),
        );
    }
    if let Some(text) = q.q.as_deref().filter(|t| !t.trim().is_empty()) {
        filters.push("c.title LIKE :q ESCAPE '\\'".into());
        args.push((":q".into(), Sql::Text(like_pattern(text))));
    }
    let (join, order) = if q.favorites {
        (
            " JOIN favorite fv ON fv.source_id = c.source_id AND fv.kind = 'live' AND fv.item_id = c.id",
            "fv.added_at",
        )
    } else {
        ("", "c.source_id, c.position")
    };
    let where_sql = filters.join(" AND ");

    let count_sql = format!("SELECT COUNT(*) FROM channel c{join} WHERE {where_sql}");
    let count_args: Vec<(&str, &dyn rusqlite::ToSql)> = args
        .iter()
        .filter(|(k, _)| k != ":now")
        .map(|(k, v)| (k.as_str(), v as &dyn rusqlite::ToSql))
        .collect();
    let total: i64 = conn.prepare_cached(&count_sql)?.query_row(count_args.as_slice(), |r| r.get(0))?;

    let limit = q.limit.unwrap_or(500).clamp(1, 5000);
    let offset = q.offset.unwrap_or(0).max(0);
    let sql = format!("{CHANNEL_SELECT}{join} WHERE {where_sql} ORDER BY {order} LIMIT {limit} OFFSET {offset}");
    let named: Vec<(&str, &dyn rusqlite::ToSql)> =
        args.iter().map(|(k, v)| (k.as_str(), v as &dyn rusqlite::ToSql)).collect();
    let items = conn
        .prepare_cached(&sql)?
        .query_map(named.as_slice(), row_to_channel)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Page { total, items })
}

#[tauri::command]
pub async fn channels(state: State<'_, AppState>, query: ChannelQuery) -> Result<Page<ChannelItem>> {
    blocking(state.inner(), move |conn| {
        let t = Instant::now();
        let page = query_channels(conn, &query)?;
        log::debug!("channels {:?}: {} of {} in {:?}", query.category_id, page.items.len(), page.total, t.elapsed());
        Ok(page)
    })
    .await
}

pub fn channel_by_id(conn: &Connection, source_id: i64, id: &str) -> Result<ChannelItem> {
    let sql = format!("{CHANNEL_SELECT} WHERE c.source_id = :source AND c.id = :id");
    conn.prepare_cached(&sql)?
        .query_row(
            &[(":now", &now() as &dyn rusqlite::ToSql), (":source", &source_id), (":id", &id)],
            row_to_channel,
        )
        .optional()?
        .ok_or_else(|| Error::NotFound(format!("channel {id}")))
}

#[tauri::command]
pub async fn channel(state: State<'_, AppState>, source_id: i64, id: String) -> Result<ChannelItem> {
    blocking(state.inner(), move |conn| channel_by_id(conn, source_id, &id)).await
}

// ---------------------------------------------------------------- movies

const MOVIE_SELECT: &str = "
    SELECT m.source_id, m.id, m.title, m.year, m.poster, m.rating, m.added, m.tag, m.ext,
           EXISTS(SELECT 1 FROM favorite f WHERE f.source_id = m.source_id AND f.kind = 'movie' AND f.item_id = m.id),
           h.position, h.duration, COALESCE(h.watched, 0)
      FROM movie m
      LEFT JOIN history h ON h.source_id = m.source_id AND h.kind = 'movie' AND h.item_id = m.id";

fn row_to_movie(r: &Row) -> rusqlite::Result<MovieItem> {
    let pos: Option<f64> = r.get(10)?;
    let dur: Option<f64> = r.get(11)?;
    Ok(MovieItem {
        source_id: r.get(0)?,
        id: r.get(1)?,
        title: r.get(2)?,
        year: r.get(3)?,
        poster: r.get(4)?,
        rating: r.get(5)?,
        added: r.get(6)?,
        tag: r.get(7)?,
        ext: r.get(8)?,
        favorite: r.get(9)?,
        progress: match (pos, dur) {
            (Some(p), Some(d)) if d > 0.0 && p > 0.0 => Some((p / d).clamp(0.0, 1.0)),
            _ => None,
        },
        watched: r.get(12)?,
    })
}

const SERIES_SELECT: &str = "
    SELECT s.source_id, s.id, s.title, s.year, s.cover, s.backdrop, s.rating, s.genre, s.last_modified, s.tag,
           EXISTS(SELECT 1 FROM favorite f WHERE f.source_id = s.source_id AND f.kind = 'series' AND f.item_id = s.id)
      FROM series s";

fn row_to_series(r: &Row) -> rusqlite::Result<SeriesItem> {
    Ok(SeriesItem {
        source_id: r.get(0)?,
        id: r.get(1)?,
        title: r.get(2)?,
        year: r.get(3)?,
        cover: r.get(4)?,
        backdrop: r.get(5)?,
        rating: r.get(6)?,
        genre: r.get(7)?,
        last_modified: r.get(8)?,
        tag: r.get(9)?,
        favorite: r.get(10)?,
    })
}

/// Shared paging/filter logic for movies ("m") and series ("s").
fn query_media<T>(
    conn: &Connection,
    q: &MediaQuery,
    alias: &str,
    table: &str,
    fav_kind: &str,
    select: &str,
    map: fn(&Row) -> rusqlite::Result<T>,
) -> Result<Page<T>> {
    let mut filters: Vec<String> = vec!["1 = 1".into()];
    let mut args: Vec<Sql> = Vec::new();
    if !settings::get_bool(conn, "content.showAdult") {
        filters.push(format!("{alias}.adult = 0"));
    }
    if let Some(s) = q.source_id {
        args.push(Sql::Integer(s));
        filters.push(format!("{alias}.source_id = ?{}", args.len()));
    }
    if let Some(cat) = &q.category_id {
        args.push(Sql::Text(cat.clone()));
        filters.push(format!("{alias}.category_id = ?{}", args.len()));
    }
    if let Some(text) = q.q.as_deref().filter(|t| !t.trim().is_empty()) {
        args.push(Sql::Text(like_pattern(text)));
        filters.push(format!("{alias}.title LIKE ?{} ESCAPE '\\'", args.len()));
    }
    let join = if q.favorites {
        format!(
            " JOIN favorite fv ON fv.source_id = {alias}.source_id AND fv.kind = '{fav_kind}' AND fv.item_id = {alias}.id"
        )
    } else {
        String::new()
    };
    let added = if table == "series" { "last_modified" } else { "added" };
    let order = match (q.favorites, q.sort.as_deref().unwrap_or("added")) {
        (true, _) => "fv.added_at DESC".to_owned(),
        (_, "title") => format!("{alias}.title COLLATE NOCASE"),
        (_, "rating") => format!("{alias}.rating IS NULL, {alias}.rating DESC, {alias}.title COLLATE NOCASE"),
        (_, "year") => format!("{alias}.year IS NULL, {alias}.year DESC, {alias}.title COLLATE NOCASE"),
        (_, "provider") => format!("{alias}.source_id, {alias}.position"),
        _ => format!("{alias}.{added} IS NULL, {alias}.{added} DESC, {alias}.position"),
    };
    let where_sql = filters.join(" AND ");
    let total: i64 = conn
        .prepare_cached(&format!("SELECT COUNT(*) FROM {table} {alias}{join} WHERE {where_sql}"))?
        .query_row(params_from_iter(args.iter()), |r| r.get(0))?;
    let limit = q.limit.unwrap_or(120).clamp(1, 2000);
    let offset = q.offset.unwrap_or(0).max(0);
    let sql = format!("{select}{join} WHERE {where_sql} ORDER BY {order} LIMIT {limit} OFFSET {offset}");
    let items = conn
        .prepare_cached(&sql)?
        .query_map(params_from_iter(args.iter()), map)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Page { total, items })
}

#[tauri::command]
pub async fn movies(state: State<'_, AppState>, query: MediaQuery) -> Result<Page<MovieItem>> {
    blocking(state.inner(), move |conn| {
        let t = Instant::now();
        let page = query_media(conn, &query, "m", "movie", "movie", MOVIE_SELECT, row_to_movie)?;
        log::debug!("movies {:?}/{:?}: {} of {} in {:?}", query.category_id, query.sort, page.items.len(), page.total, t.elapsed());
        Ok(page)
    })
    .await
}

#[tauri::command]
pub async fn series_list(state: State<'_, AppState>, query: MediaQuery) -> Result<Page<SeriesItem>> {
    blocking(state.inner(), move |conn| {
        query_media(conn, &query, "s", "series", "series", SERIES_SELECT, row_to_series)
    })
    .await
}

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
}

struct CachedDetail {
    json: Value,
    fetched_at: i64,
}

fn cached(conn: &Connection, source_id: i64, kind: &str, id: &str) -> Result<Option<CachedDetail>> {
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
async fn provider_detail(st: &AppState, source_id: i64, kind: &'static str, id: &str, ttl: i64) -> Result<Option<Value>> {
    let (src, cache) = {
        let conn = st.db.read();
        (sources::load(&conn, source_id)?, cached(&conn, source_id, kind, id)?)
    };
    if src.kind != SourceKind::Xtream {
        return Ok(None);
    }
    if let Some(c) = &cache
        && now() - c.fetched_at < ttl {
            return Ok(Some(c.json.clone()));
        }
    let x = sources::xtream_for(&src, http_client(src.user_agent.as_deref()));
    let fetched = match kind {
        "movie" => x.vod_info(id).await,
        _ => x.series_info(id).await,
    };
    match fetched {
        Ok(v) if v.is_object() => {
            let conn = st.db.write();
            conn.execute(
                "INSERT OR REPLACE INTO detail_cache (source_id, kind, id, json, fetched_at) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![source_id, kind, id, v.to_string(), now()],
            )?;
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

#[tauri::command]
pub async fn movie_detail(state: State<'_, AppState>, source_id: i64, id: String) -> Result<MovieDetail> {
    let st = state.inner().clone();
    let item = {
        let id = id.clone();
        blocking(&st, move |conn| {
            conn.prepare_cached(&format!("{MOVIE_SELECT} WHERE m.source_id = ?1 AND m.id = ?2"))?
                .query_row(params![source_id, id], row_to_movie)
                .optional()?
                .ok_or_else(|| Error::NotFound(format!("movie {id}")))
        })
        .await?
    };
    let position: f64 = {
        let conn = st.db.read();
        conn.query_row(
            "SELECT position FROM history WHERE source_id = ?1 AND kind = 'movie' AND item_id = ?2",
            params![source_id, id],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or(0.0)
    };
    let detail = provider_detail(&st, source_id, "movie", &id, MOVIE_DETAIL_TTL).await.unwrap_or_else(|e| {
        log::warn!("movie {id}: no provider detail ({e})");
        None
    });
    let info = detail.as_ref().map(|d| d["info"].clone()).unwrap_or(Value::Null);
    let mut item = item;
    if item.poster.is_none() {
        item.poster = first_str(&info, &["cover_big", "movie_image"]);
    }
    if item.rating.is_none() {
        item.rating = f64_of(&info["rating"]).filter(|r| *r > 0.0);
    }
    Ok(MovieDetail {
        plot: first_str(&info, &["plot", "description"]),
        cast: first_str(&info, &["cast", "actors"]),
        director: str_of(&info["director"]),
        genre: str_of(&info["genre"]),
        country: str_of(&info["country"]),
        release_date: first_str(&info, &["releasedate", "release_date"]),
        duration: i64_of(&info["duration_secs"]).filter(|d| *d > 0),
        backdrop: first_url(&info["backdrop_path"]),
        trailer: first_str(&info, &["youtube_trailer", "trailer"]).or_else(|| {
            let conn = st.db.read();
            conn.query_row("SELECT trailer FROM movie WHERE source_id = ?1 AND id = ?2", params![source_id, id], |r| r.get(0))
                .ok()
                .flatten()
        }),
        age_rating: first_str(&info, &["mpaa_rating", "age"]),
        video: tech(&info["video"]),
        audio: tech(&info["audio"]),
        position,
        item,
    })
}

#[derive(Debug, Serialize)]
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
}

/// Provider `episodes` payload ({"1": [...]} or [[...]]) → seasons in order,
/// episodes sorted, without watch state.
fn parse_episodes(detail: &Value, series_title: &str) -> Vec<(i64, Vec<Episode>)> {
    let mut groups: Vec<(i64, Vec<Value>)> = match &detail["episodes"] {
        Value::Object(map) => map
            .iter()
            .map(|(k, v)| (k.parse().unwrap_or(0), v.as_array().cloned().unwrap_or_default()))
            .collect(),
        Value::Array(arr) => arr
            .iter()
            .enumerate()
            .map(|(i, v)| {
                let eps = v.as_array().cloned().unwrap_or_default();
                let n = eps.first().and_then(|e| i64_of(&e["season"])).unwrap_or(i as i64 + 1);
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
                    let raw_title = first_str(ei, &["name"]).or_else(|| str_of(&e["title"])).unwrap_or_default();
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
    blocking(state.inner(), move |conn| up_next_rows(conn, limit)).await
}

fn up_next_rows(conn: &Connection, limit: usize) -> Result<Vec<UpNext>> {
    // Latest episode per series. Timestamps are whole seconds, so ties (two
    // episodes marked within a second) go to the furthest episode.
    let latest: Vec<(i64, String, String, bool)> = conn
        .prepare_cached(
            "SELECT source_id, series_id, item_id, watched FROM (
                 SELECT source_id, series_id, item_id, watched, updated_at,
                        ROW_NUMBER() OVER (PARTITION BY source_id, series_id
                                           ORDER BY updated_at DESC, season DESC, episode DESC) AS n
                   FROM history WHERE kind = 'episode' AND series_id IS NOT NULL)
              WHERE n = 1 ORDER BY updated_at DESC LIMIT 60",
        )?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
        .collect::<Result<_, _>>()?;
    let mut out = Vec::new();
    for (sid, series_id, last_id, watched) in latest {
        if !watched || out.len() >= limit {
            continue;
        }
        let Some(series) = conn
            .prepare_cached(&format!("{SERIES_SELECT} WHERE s.source_id = ?1 AND s.id = ?2"))?
            .query_row(params![sid, series_id], row_to_series)
            .optional()?
        else {
            continue;
        };
        let Some(detail) = cached(conn, sid, "series", &series_id)? else { continue };
        let flat: Vec<Episode> = parse_episodes(&detail.json, &series.title)
            .into_iter()
            .filter(|(n, _)| *n > 0)
            .flat_map(|(_, eps)| eps)
            .collect();
        let Some(idx) = flat.iter().position(|e| e.id == last_id) else { continue };
        let Some(next) = flat.into_iter().nth(idx + 1) else { continue };
        let seen: bool = conn
            .query_row(
                "SELECT watched FROM history WHERE source_id = ?1 AND kind = 'episode' AND item_id = ?2",
                params![sid, next.id],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(false);
        if !seen {
            out.push(UpNext { series, episode: next });
        }
    }
    Ok(out)
}

/// "Deadly Ties - S01E01" / "S01E01 - Pilot" → "Pilot" or "Episode 1".
fn episode_title(raw: &str, series_title: &str, n: i64) -> String {
    static SXXEXX: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"(?i)\bS\d{1,3}\s*E\d{1,4}\b").unwrap());
    let mut t = raw.trim().to_owned();
    if let Some(m) = SXXEXX.find(&t) {
        let after = t[m.end()..].trim().trim_start_matches(['-', ':', '.', '|']).trim().to_owned();
        let before = t[..m.start()].trim().trim_end_matches(['-', ':', '.', '|']).trim().to_owned();
        t = if !after.is_empty() {
            after
        } else if !before.is_empty() && !before.eq_ignore_ascii_case(series_title) && !series_title.contains(&before) {
            before
        } else {
            String::new()
        };
    }
    if t.is_empty() { format!("Episode {n}") } else { t }
}

/// Long-form series fields that list rows don't carry.
struct SeriesText {
    plot: Option<String>,
    cast: Option<String>,
    director: Option<String>,
    release: Option<String>,
    trailer: Option<String>,
}

/// Watch state of one episode.
struct Watched {
    id: String,
    position: f64,
    watched: bool,
    at: i64,
}

#[tauri::command]
pub async fn series_detail(state: State<'_, AppState>, source_id: i64, id: String) -> Result<SeriesDetail> {
    let st = state.inner().clone();
    let (item, text, history) = {
        let id = id.clone();
        blocking(&st, move |conn| {
            let item = conn
                .prepare_cached(&format!("{SERIES_SELECT} WHERE s.source_id = ?1 AND s.id = ?2"))?
                .query_row(params![source_id, id], row_to_series)
                .optional()?
                .ok_or_else(|| Error::NotFound(format!("series {id}")))?;
            let text = conn.query_row(
                "SELECT plot, cast_list, director, release_date, trailer FROM series WHERE source_id = ?1 AND id = ?2",
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
            )?;
            let mut stmt = conn.prepare_cached(
                "SELECT item_id, position, watched, updated_at FROM history
                  WHERE source_id = ?1 AND kind = 'episode' AND series_id = ?2",
            )?;
            let history: Vec<Watched> = stmt
                .query_map(params![source_id, id], |r| {
                    Ok(Watched { id: r.get(0)?, position: r.get(1)?, watched: r.get(2)?, at: r.get(3)? })
                })?
                .collect::<Result<_, _>>()?;
            Ok((item, text, history))
        })
        .await?
    };

    let detail = provider_detail(&st, source_id, "series", &id, SERIES_DETAIL_TTL).await?.unwrap_or(Value::Null);
    let info = &detail["info"];
    let season_meta: Vec<&Value> = detail["seasons"].as_array().map(|a| a.iter().collect()).unwrap_or_default();

    let hist = |eid: &str| history.iter().find(|h| h.id == eid);
    let mut seasons = Vec::new();
    for (n, mut episodes) in parse_episodes(&detail, &item.title) {
        let meta = season_meta.iter().find(|m| i64_of(&m["season_number"]) == Some(n));
        for e in &mut episodes {
            if let Some(h) = hist(&e.id) {
                e.position = h.position;
                e.watched = h.watched;
            }
        }
        seasons.push(Season {
            season: n,
            name: meta
                .and_then(|m| str_of(&m["name"]))
                .unwrap_or_else(|| if n == 0 { "Specials".into() } else { format!("Season {n}") }),
            cover: meta.and_then(|m| first_str(m, &["cover_big", "cover"])),
            overview: meta.and_then(|m| str_of(&m["overview"])),
            air_date: meta.and_then(|m| str_of(&m["air_date"])),
            episodes,
        });
    }

    // Resume: the most recent unfinished episode, else the one after the
    // most recently finished, else S1E1.
    let flat: Vec<&Episode> = seasons.iter().filter(|s| s.season > 0).flat_map(|s| &s.episodes).collect();
    let latest = history.iter().max_by_key(|h| h.at);
    let resume = match latest {
        Some(h) if !h.watched => flat.iter().find(|e| e.id == h.id).map(|e| Resume {
            episode_id: e.id.clone(),
            season: e.season,
            episode: e.episode,
            position: h.position,
            started: true,
        }),
        Some(h) => {
            let idx = flat.iter().position(|e| e.id == h.id);
            idx.and_then(|i| flat.get(i + 1)).map(|e| Resume {
                episode_id: e.id.clone(),
                season: e.season,
                episode: e.episode,
                position: 0.0,
                started: true,
            })
        }
        None => None,
    }
    .or_else(|| {
        flat.first().map(|e| Resume {
            episode_id: e.id.clone(),
            season: e.season,
            episode: e.episode,
            position: 0.0,
            started: false,
        })
    });

    let mut item = item;
    if item.backdrop.is_none() {
        item.backdrop = first_url(&info["backdrop_path"]);
    }
    Ok(SeriesDetail {
        plot: text.plot.or_else(|| str_of(&info["plot"])),
        cast: text.cast.or_else(|| str_of(&info["cast"])),
        director: text.director.or_else(|| str_of(&info["director"])),
        release_date: text.release.or_else(|| first_str(info, &["releaseDate", "release_date"])),
        trailer: text.trailer.or_else(|| str_of(&info["youtube_trailer"])),
        seasons,
        resume,
        item,
    })
}

// ------------------------------------------------------------------- EPG

#[tauri::command]
pub async fn epg_channel(
    state: State<'_, AppState>,
    source_id: i64,
    channel_id: String,
    from: i64,
    to: i64,
) -> Result<Vec<ProgrammeRow>> {
    let st = state.inner().clone();
    let (epg_id, rows) = {
        let channel_id = channel_id.clone();
        blocking(&st, move |conn| {
            let epg_id: Option<String> = conn
                .query_row(
                    "SELECT epg_id FROM channel WHERE source_id = ?1 AND id = ?2",
                    params![source_id, channel_id],
                    |r| r.get(0),
                )
                .optional()?
                .flatten();
            let rows = match &epg_id {
                Some(e) => epg::programmes(conn, source_id, e, from, to)?,
                None => Vec::new(),
            };
            Ok((epg_id, rows))
        })
        .await?
    };
    if !rows.is_empty() || epg_id.is_none() {
        return Ok(rows);
    }
    // Fall back to the panel's short EPG (base64 encoded fields).
    let src = {
        let conn = st.db.read();
        sources::load(&conn, source_id)?
    };
    if src.kind != SourceKind::Xtream {
        return Ok(rows);
    }
    let x = sources::xtream_for(&src, http_client(src.user_agent.as_deref()));
    let v = match x.short_epg(&channel_id, 24).await {
        Ok(v) => v,
        Err(e) => {
            log::debug!("short epg {channel_id}: {e}");
            return Ok(rows);
        }
    };
    let b64 = |v: &Value| -> Option<String> {
        let s = str_of(v)?;
        base64::engine::general_purpose::STANDARD
            .decode(s.as_bytes())
            .ok()
            .map(|b| String::from_utf8_lossy(&b).trim().to_owned())
            .or(Some(s))
    };
    let list = v["epg_listings"].as_array().cloned().unwrap_or_default();
    Ok(list
        .iter()
        .filter_map(|p| {
            let start = i64_of(&p["start_timestamp"])?;
            let stop = i64_of(&p["stop_timestamp"])?;
            (stop > from && start < to).then(|| ProgrammeRow {
                epg_id: epg_id.clone().unwrap_or_default(),
                start,
                stop,
                title: b64(&p["title"]).unwrap_or_default(),
                subtitle: None,
                description: b64(&p["description"]),
                category: None,
                episode: None,
                icon: None,
            })
        })
        .collect())
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GuideRow {
    pub channel: ChannelItem,
    pub programmes: Vec<ProgrammeRow>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GuideQuery {
    #[serde(flatten)]
    pub channels: ChannelQuery,
    pub from: i64,
    pub to: i64,
}

/// Channels plus their programmes in a time window (TV guide grid).
#[tauri::command]
pub async fn epg_grid(state: State<'_, AppState>, query: GuideQuery) -> Result<Page<GuideRow>> {
    blocking(state.inner(), move |conn| {
        let page = query_channels(conn, &query.channels)?;
        let mut items = Vec::with_capacity(page.items.len());
        for channel in page.items {
            let programmes = match &channel.epg_id {
                Some(e) => epg::programmes(conn, channel.source_id, e, query.from, query.to)?,
                None => Vec::new(),
            };
            items.push(GuideRow { channel, programmes });
        }
        Ok(Page { total: page.total, items })
    })
    .await
}

// ---------------------------------------------------------------- search

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchResults {
    pub channels: Vec<ChannelItem>,
    pub movies: Vec<MovieItem>,
    pub series: Vec<SeriesItem>,
}

fn fts_query(q: &str) -> Option<String> {
    let tokens: Vec<String> = q
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(|t| format!("\"{}\"*", t.replace('"', "")))
        .collect();
    (!tokens.is_empty()).then(|| tokens.join(" "))
}

#[tauri::command]
pub async fn search(state: State<'_, AppState>, q: String, limit: Option<i64>) -> Result<SearchResults> {
    let limit = limit.unwrap_or(30).clamp(1, 200);
    blocking(state.inner(), move |conn| {
        let Some(fts) = fts_query(&q) else {
            return Ok(SearchResults { channels: vec![], movies: vec![], series: vec![] });
        };
        let t = Instant::now();
        let show_adult = settings::get_bool(conn, "content.showAdult");
        let ids = |kind: &str| -> Result<Vec<(i64, String)>> {
            let mut stmt = conn.prepare_cached(
                "SELECT source_id, item_id FROM search WHERE search MATCH ?1 AND kind = ?2 ORDER BY rank LIMIT ?3",
            )?;
            let rows = stmt
                .query_map(params![fts, kind, limit * 2], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        };
        let adult_ok = |table: &str, sid: i64, id: &str| -> bool {
            show_adult
                || conn
                    .query_row(&format!("SELECT adult FROM {table} WHERE source_id = ?1 AND id = ?2"), params![sid, id], |r| {
                        r.get::<_, bool>(0)
                    })
                    .map(|a| !a)
                    .unwrap_or(false)
        };
        let mut channels = Vec::new();
        for (sid, id) in ids("live")? {
            if channels.len() as i64 >= limit {
                break;
            }
            if adult_ok("channel", sid, &id)
                && let Ok(c) = channel_by_id(conn, sid, &id) {
                    channels.push(c);
                }
        }
        let mut movies = Vec::new();
        {
            let mut stmt = conn.prepare_cached(&format!("{MOVIE_SELECT} WHERE m.source_id = ?1 AND m.id = ?2"))?;
            for (sid, id) in ids("movie")? {
                if movies.len() as i64 >= limit {
                    break;
                }
                if adult_ok("movie", sid, &id)
                    && let Some(m) = stmt.query_row(params![sid, id], row_to_movie).optional()? {
                        movies.push(m);
                    }
            }
        }
        let mut series = Vec::new();
        {
            let mut stmt = conn.prepare_cached(&format!("{SERIES_SELECT} WHERE s.source_id = ?1 AND s.id = ?2"))?;
            for (sid, id) in ids("series")? {
                if series.len() as i64 >= limit {
                    break;
                }
                if adult_ok("series", sid, &id)
                    && let Some(s) = stmt.query_row(params![sid, id], row_to_series).optional()? {
                        series.push(s);
                    }
            }
        }
        log::debug!("search {q:?}: {}/{}/{} in {:?}", channels.len(), movies.len(), series.len(), t.elapsed());
        Ok(SearchResults { channels, movies, series })
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn episode_titles() {
        assert_eq!(episode_title("Deadly Ties - S01E01", "Deadly Ties", 1), "Episode 1");
        assert_eq!(episode_title("S01E02 - The Pilot", "X", 2), "The Pilot");
        assert_eq!(episode_title("The Pilot", "X", 2), "The Pilot");
        assert_eq!(episode_title("", "X", 3), "Episode 3");
    }

    #[test]
    fn backdrop_variants() {
        assert_eq!(first_url(&Value::String("['https://a/b.jpg']".into())).as_deref(), Some("https://a/b.jpg"));
        assert_eq!(first_url(&serde_json::json!(["https://c/d.jpg"])).as_deref(), Some("https://c/d.jpg"));
        assert_eq!(first_url(&Value::String("".into())), None);
    }

    #[test]
    fn fts_queries() {
        assert_eq!(fts_query("bbc news").as_deref(), Some("\"bbc\"* \"news\"*"));
        assert_eq!(fts_query("  \"  ").as_deref(), None);
        assert_eq!(fts_query("top-gun").as_deref(), Some("\"top\"* \"gun\"*"));
    }

    #[test]
    fn badge_joining() {
        assert_eq!(split_badges("4K DOLBY VISION HEVC".into()), vec!["4K", "DOLBY VISION", "HEVC"]);
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
        let next = |c: &Connection| -> Vec<String> { up_next_rows(c, 20).unwrap().into_iter().map(|u| u.episode.id).collect() };

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
}
