//! Read-side commands: categories, channel/movie/series listings, details,
//! programme guide and search.

use std::collections::{BTreeMap, HashMap};
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
use crate::works::versions::{self, VersionInfo};

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
    /// the channel this feed belongs to (all its quality variants)
    pub group: Option<GroupInfo>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupInfo {
    pub key: String,
    /// number of feeds (quality variants) of the channel
    pub variants: i64,
    pub country: Option<String>,
    pub genre: String,
}

/// Grouping info shared by movie and series items (see `works`): the work
/// key and what its versions offer.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkInfo {
    pub key: Option<String>,
    /// number of provider copies of this title (details list them in `versions`)
    pub version_count: i64,
    /// "4K", "Dolby Vision", "Dolby Audio", "HEVC", "Blu-ray"
    pub quality: Vec<String>,
    pub services: Vec<String>,
}

fn work_info(key: Option<String>, versions: Option<i64>, badges: Option<String>, services: Option<String>) -> WorkInfo {
    let split = |s: Option<String>| -> Vec<String> {
        s.unwrap_or_default().split('|').filter(|x| !x.is_empty()).map(str::to_owned).collect()
    };
    WorkInfo { version_count: versions.unwrap_or(1), quality: split(badges), services: split(services), key }
}

/// A movie as listed: in browse lists one row per work (`work` table) whose
/// `source_id`/`id` name a representative copy.
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
    #[serde(flatten)]
    pub work: WorkInfo,
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
    #[serde(flatten)]
    pub work: WorkInfo,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelQuery {
    pub source_id: Option<i64>,
    pub category_id: Option<String>,
    /// one row per channel (`channel_group`) playing its chosen variant,
    /// instead of every provider feed
    #[serde(default)]
    pub grouped: bool,
    /// channel groups of one country ("DK") / live genre ("Sports")
    pub country: Option<String>,
    pub genre: Option<String>,
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
    /// with `category_id`: a provider category ("collection")
    pub source_id: Option<i64>,
    pub category_id: Option<String>,
    #[serde(default)]
    pub favorites: bool,
    pub q: Option<String>,
    /// 'added' (default) | 'title' | 'rating' | 'year'
    pub sort: Option<String>,
    /// browse facets that must all match (`work_facet`)
    #[serde(default)]
    pub facets: Vec<FacetFilter>,
    pub offset: Option<i64>,
    pub limit: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FacetFilter {
    pub facet: String,
    pub value: String,
}

const FACETS: &[&str] = &["service", "language", "quality", "genre", "decade", "collection", "original", "franchise", "network"];

/// Badges are stored space separated; multi-word ones ("DOLBY VISION") are
/// re-joined here.
pub(crate) fn split_badges(s: String) -> Vec<String> {
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

/// Channel rows with now/next. `grouped`: one row per channel group,
/// titled as the group and playing its chosen variant (`g.source_id`/
/// `g.item_id`), a favorite when any variant is. A feed without its own EPG
/// id uses its group's.
fn channel_select(grouped: bool) -> String {
    let (title, favorite, from) = if grouped {
        (
            "g.title",
            "EXISTS(SELECT 1 FROM channel x JOIN favorite f ON f.source_id = x.source_id AND f.kind = 'live' AND f.item_id = x.id
                     WHERE x.group_key = g.key)",
            "channel_group g JOIN channel c ON c.source_id = g.source_id AND c.id = g.item_id",
        )
    } else {
        (
            "c.title",
            "EXISTS(SELECT 1 FROM favorite f WHERE f.source_id = c.source_id AND f.kind = 'live' AND f.item_id = c.id)",
            "channel c LEFT JOIN channel_group g ON g.key = c.group_key",
        )
    };
    format!(
        "SELECT c.source_id, c.id, c.num, {title}, COALESCE(c.logo, g.logo), COALESCE(c.epg_id, g.epg_id), c.category_id,
                c.badges, c.archive, c.archive_days, {favorite},
                p.title, p.start, p.stop, q.title, q.start, q.stop, g.key, g.variants, g.country, g.genre
           FROM {from}
           LEFT JOIN programme p ON p.source_id = c.source_id AND p.epg_id = COALESCE(c.epg_id, g.epg_id)
                AND p.start = (SELECT MAX(start) FROM programme
                                WHERE source_id = c.source_id AND epg_id = COALESCE(c.epg_id, g.epg_id) AND start <= :now)
                AND p.stop > :now
           LEFT JOIN programme q ON q.source_id = c.source_id AND q.epg_id = COALESCE(c.epg_id, g.epg_id)
                AND q.start = (SELECT MIN(start) FROM programme
                                WHERE source_id = c.source_id AND epg_id = COALESCE(c.epg_id, g.epg_id) AND start > :now)"
    )
}

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
        group: match r.get::<_, Option<String>>(17)? {
            Some(key) => Some(GroupInfo { key, variants: r.get(18)?, country: r.get(19)?, genre: r.get(20)? }),
            None => None,
        },
    })
}

pub fn query_channels(conn: &Connection, q: &ChannelQuery) -> Result<Page<ChannelItem>> {
    let g = q.grouped;
    let mut filters = vec!["c.separator = 0".to_owned()];
    let mut args: Vec<(String, Sql)> = vec![(":now".into(), Sql::Integer(now()))];
    if !settings::get_bool(conn, "content.showAdult") {
        filters.push(if g { "g.adult = 0" } else { "c.adult = 0" }.into());
    }
    match (q.source_id, &q.category_id, g) {
        (Some(s), Some(cat), true) => {
            filters.push("g.key IN (SELECT group_key FROM channel WHERE source_id = :source AND category_id = :cat)".into());
            args.push((":source".into(), Sql::Integer(s)));
            args.push((":cat".into(), Sql::Text(cat.clone())));
        }
        (Some(s), None, true) => {
            filters.push("g.key IN (SELECT group_key FROM channel WHERE source_id = :source)".into());
            args.push((":source".into(), Sql::Integer(s)));
        }
        (s, cat, false) => {
            if let Some(s) = s {
                filters.push("c.source_id = :source".into());
                args.push((":source".into(), Sql::Integer(s)));
            }
            if let Some(cat) = cat {
                filters.push("c.category_id = :cat".into());
                args.push((":cat".into(), Sql::Text(cat.clone())));
            }
        }
        _ => {}
    }
    match q.country.as_deref() {
        // channels whose category has no region
        Some("") => filters.push("g.key IS NOT NULL AND g.country IS NULL".into()),
        Some(country) => {
            filters.push("g.country = :country".into());
            args.push((":country".into(), Sql::Text(country.to_owned())));
        }
        None => {}
    }
    if let Some(genre) = &q.genre {
        filters.push("g.genre = :genre".into());
        args.push((":genre".into(), Sql::Text(genre.clone())));
    }
    if q.with_epg {
        filters.push(
            "COALESCE(c.epg_id, g.epg_id) IS NOT NULL
             AND EXISTS (SELECT 1 FROM programme pe WHERE pe.source_id = c.source_id AND pe.epg_id = COALESCE(c.epg_id, g.epg_id))"
                .into(),
        );
    }
    if let Some(text) = q.q.as_deref().filter(|t| !t.trim().is_empty()) {
        filters.push(if g { "g.title LIKE :q ESCAPE '\\'" } else { "c.title LIKE :q ESCAPE '\\'" }.into());
        args.push((":q".into(), Sql::Text(like_pattern(text))));
    }
    let (join, order) = match (q.favorites, g) {
        // a channel is a favorite since its first favorited variant
        (true, true) => (
            " JOIN (SELECT x.group_key AS key, MIN(f.added_at) AS added_at FROM favorite f
                      JOIN channel x ON x.source_id = f.source_id AND x.id = f.item_id
                     WHERE f.kind = 'live' AND x.group_key IS NOT NULL GROUP BY x.group_key) fv ON fv.key = g.key",
            "fv.added_at".to_owned(),
        ),
        (true, false) => (
            " JOIN favorite fv ON fv.source_id = c.source_id AND fv.kind = 'live' AND fv.item_id = c.id",
            "fv.added_at".to_owned(),
        ),
        // a country: its regular channels first, event feeds last; a genre:
        // the viewer's countries first
        (false, true) if q.country.is_some() => ("", genre_order()),
        (false, true) if q.genre.is_some() => ("", country_order_sql(conn)?),
        (false, true) => ("", "g.position".to_owned()),
        (false, false) => ("", "c.source_id, c.position".to_owned()),
    };
    let where_sql = filters.join(" AND ");
    let from = if g {
        "channel_group g JOIN channel c ON c.source_id = g.source_id AND c.id = g.item_id"
    } else {
        "channel c LEFT JOIN channel_group g ON g.key = c.group_key"
    };

    let count_sql = format!("SELECT COUNT(*) FROM {from}{join} WHERE {where_sql}");
    let count_args: Vec<(&str, &dyn rusqlite::ToSql)> = args
        .iter()
        .filter(|(k, _)| k != ":now")
        .map(|(k, v)| (k.as_str(), v as &dyn rusqlite::ToSql))
        .collect();
    let total: i64 = conn.prepare_cached(&count_sql)?.query_row(count_args.as_slice(), |r| r.get(0))?;

    let limit = q.limit.unwrap_or(500).clamp(1, 5000);
    let offset = q.offset.unwrap_or(0).max(0);
    let sql = format!("{}{join} WHERE {where_sql} ORDER BY {order} LIMIT {limit} OFFSET {offset}", channel_select(g));
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
    let sql = format!("{} WHERE c.source_id = :source AND c.id = :id", channel_select(false));
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

/// A channel group as a row: its title, playing its chosen variant.
pub fn channel_group_by_key(conn: &Connection, key: &str) -> Result<ChannelItem> {
    let sql = format!("{} WHERE g.key = :key", channel_select(true));
    conn.prepare_cached(&sql)?
        .query_row(&[(":now", &now() as &dyn rusqlite::ToSql), (":key", &key)], row_to_channel)
        .optional()?
        .ok_or_else(|| Error::NotFound(format!("channel {key}")))
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveCountry {
    /// region code from the provider's categories ("DK"); None = no region
    pub code: Option<String>,
    pub name: String,
    pub count: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveCell {
    pub country: Option<String>,
    pub genre: String,
    pub count: i64,
}

/// Live TV navigation: channel groups per country and genre.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveNav {
    /// the viewer's countries first (player languages), then by size
    pub countries: Vec<LiveCountry>,
    /// genres in display order
    pub genres: Vec<String>,
    pub cells: Vec<LiveCell>,
}

/// `ORDER BY` for channel groups: genres in `LIVE_GENRES` order; within one,
/// channels with guide data first — the linear channels (BBC One, ITV, …)
/// ahead of a provider's 24/7 and FAST feeds, which rarely have any.
fn genre_order() -> String {
    let cases: String = crate::works::genre::LIVE_GENRES
        .iter()
        .enumerate()
        .map(|(i, g)| format!(" WHEN '{}' THEN {i}", g.replace('\'', "''")))
        .collect();
    format!("CASE g.genre{cases} ELSE 99 END, g.epg_id IS NULL, g.position")
}

/// `ORDER BY` for channel groups: countries as `live_nav` lists them, each
/// with its guide channels first.
fn country_order_sql(conn: &Connection) -> Result<String> {
    let cases: String = live_nav_for(conn)?
        .countries
        .iter()
        .enumerate()
        .filter_map(|(i, c)| c.code.as_ref().map(|code| format!(" WHEN '{}' THEN {i}", code.replace('\'', "''"))))
        .collect();
    Ok(format!("CASE g.country{cases} ELSE 9999 END, g.epg_id IS NULL, g.position"))
}

pub fn live_nav_for(conn: &Connection) -> Result<LiveNav> {
    let show_adult = settings::get_bool(conn, "content.showAdult");
    let cells: Vec<LiveCell> = conn
        .prepare_cached(
            "SELECT country, genre, COUNT(*) FROM channel_group WHERE ?1 OR adult = 0 GROUP BY country, genre",
        )?
        .query_map([show_adult], |r| Ok(LiveCell { country: r.get(0)?, genre: r.get(1)?, count: r.get(2)? }))?
        .collect::<Result<_, _>>()?;
    let mut countries: Vec<LiveCountry> = Vec::new();
    for c in &cells {
        match countries.iter_mut().find(|x| x.code == c.country) {
            Some(x) => x.count += c.count,
            None => countries.push(LiveCountry {
                code: c.country.clone(),
                name: c.country.as_deref().map(crate::works::genre::country_name).unwrap_or_else(|| "Other".into()),
                count: c.count,
            }),
        }
    }
    let home: Vec<&str> = versions::language_prefs(conn)
        .into_iter()
        .flat_map(|l| crate::works::genre::countries_for_language(l).iter().copied())
        .collect();
    let rank = |c: &LiveCountry| c.code.as_deref().and_then(|code| home.iter().position(|h| *h == code)).unwrap_or(usize::MAX);
    countries.sort_by(|a, b| {
        rank(a).cmp(&rank(b)).then(a.code.is_none().cmp(&b.code.is_none())).then(b.count.cmp(&a.count)).then(a.name.cmp(&b.name))
    });
    let genres = crate::works::genre::LIVE_GENRES
        .iter()
        .filter(|g| cells.iter().any(|c| c.genre == **g))
        .map(|g| g.to_string())
        .collect();
    Ok(LiveNav { countries, genres, cells })
}

#[tauri::command]
pub async fn live_nav(state: State<'_, AppState>) -> Result<LiveNav> {
    blocking(state.inner(), live_nav_for).await
}

/// One provider feed of a channel, for the variant chips.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelVariant {
    #[serde(flatten)]
    pub channel: ChannelItem,
    /// "RAW · HEVC", "HD · VIP", "4K"
    pub label: String,
    /// provider category, e.g. "Now TV Sport · HD RAW"
    pub category: Option<String>,
    /// the one that plays for this channel
    pub selected: bool,
}

pub fn variants_of(conn: &Connection, key: &str) -> Result<Vec<ChannelVariant>> {
    let chosen: Option<(i64, String)> = conn
        .query_row("SELECT source_id, item_id FROM channel_group WHERE key = ?1", [key], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()?;
    type Row = (i64, String, String, Option<String>, Option<String>, i64);
    let rows: Vec<Row> = conn
        .prepare_cached(
            "SELECT c.source_id, c.id, c.badges, k.badges, k.name, COALESCE(k.position, 0) * 100000 + c.position
               FROM channel c
               LEFT JOIN category k ON k.source_id = c.source_id AND k.kind = 'live' AND k.id = c.category_id
              WHERE c.group_key = ?1",
        )?
        .query_map([key], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)))?
        .collect::<Result<_, _>>()?;
    let mut out: Vec<(i32, i64, ChannelVariant)> = Vec::new();
    for (sid, id, badges, cat_badges, cat_name, order) in rows {
        let channel = channel_by_id(conn, sid, &id)?;
        let selected = chosen.as_ref().is_some_and(|(s, i)| *s == sid && *i == id);
        let label = crate::works::channel_variant_label(&badges, cat_badges.as_deref().unwrap_or(""));
        let category = cat_name.as_deref().map(crate::names::display_category);
        out.push((crate::works::channel_rank(&badges), order, ChannelVariant { channel, label, category, selected }));
    }
    out.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    // identical feeds (backups in the same category) get a number
    let mut seen: HashMap<String, usize> = HashMap::new();
    let mut variants: Vec<ChannelVariant> = out.into_iter().map(|(_, _, v)| v).collect();
    for v in &mut variants {
        let n = seen.entry(v.label.clone()).or_insert(0);
        *n += 1;
        if *n > 1 {
            v.label = format!("{} ({n})", v.label);
        }
    }
    Ok(variants)
}

#[tauri::command]
pub async fn channel_variants(state: State<'_, AppState>, key: String) -> Result<Vec<ChannelVariant>> {
    blocking(state.inner(), move |conn| variants_of(conn, &key)).await
}

// ---------------------------------------------------------------- movies

/// One movie copy with its work's grouping info.
const MOVIE_SELECT: &str = "
    SELECT m.source_id, m.id, m.title, m.year, m.poster, m.rating, m.added, m.tag, m.ext,
           EXISTS(SELECT 1 FROM favorite f WHERE f.source_id = m.source_id AND f.kind = 'movie' AND f.item_id = m.id),
           h.position, h.duration, COALESCE(h.watched, 0),
           m.work_key, w.versions, w.badges, w.services
      FROM movie m
      LEFT JOIN history h ON h.source_id = m.source_id AND h.kind = 'movie' AND h.item_id = m.id
      LEFT JOIN work w ON w.kind = 'movie' AND w.key = m.work_key";

/// Watch state of a movie work: the most recently played copy.
fn work_history(col: &str) -> String {
    format!(
        "(SELECT h.{col} FROM movie hm JOIN history h ON h.source_id = hm.source_id AND h.kind = 'movie' AND h.item_id = hm.id
           WHERE hm.work_key = w.key ORDER BY h.updated_at DESC LIMIT 1)"
    )
}

/// One movie work (browse lists), same columns as `MOVIE_SELECT`.
fn movie_work_select() -> String {
    format!(
        "SELECT w.source_id, w.item_id, w.title, w.year, w.poster, w.rating, w.added, m.tag, m.ext,
                EXISTS(SELECT 1 FROM favorite f JOIN movie fm ON fm.source_id = f.source_id AND fm.id = f.item_id
                        WHERE f.kind = 'movie' AND fm.work_key = w.key),
                {}, {}, COALESCE({}, 0),
                w.key, w.versions, w.badges, w.services
           FROM work w
           LEFT JOIN movie m ON m.source_id = w.source_id AND m.id = w.item_id",
        work_history("position"),
        work_history("duration"),
        work_history("watched")
    )
}

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
        work: work_info(r.get(13)?, r.get(14)?, r.get(15)?, r.get(16)?),
    })
}

/// One series copy with its work's grouping info.
const SERIES_SELECT: &str = "
    SELECT s.source_id, s.id, s.title, s.year, s.cover, s.backdrop, s.rating, s.genre, s.last_modified, s.tag,
           EXISTS(SELECT 1 FROM favorite f WHERE f.source_id = s.source_id AND f.kind = 'series' AND f.item_id = s.id),
           s.work_key, w.versions, w.badges, w.services
      FROM series s
      LEFT JOIN work w ON w.kind = 'series' AND w.key = s.work_key";

/// One series work (browse lists), same columns as `SERIES_SELECT`.
const SERIES_WORK_SELECT: &str = "
    SELECT w.source_id, w.item_id, w.title, w.year, w.poster, w.backdrop, w.rating, w.genre, w.added, s.tag,
           EXISTS(SELECT 1 FROM favorite f JOIN series fs ON fs.source_id = f.source_id AND fs.id = f.item_id
                   WHERE f.kind = 'series' AND fs.work_key = w.key),
           w.key, w.versions, w.badges, w.services
      FROM work w
      LEFT JOIN series s ON s.source_id = w.source_id AND s.id = w.item_id";

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
        work: work_info(r.get(11)?, r.get(12)?, r.get(13)?, r.get(14)?),
    })
}

/// Browse lists: one row per work. Filters match through the members, so a
/// work shows up in every provider category and facet any copy belongs to.
/// WHERE clause over `work w` for a browse query (`?1` = kind). Facet
/// `skip` is left out — facet counts show what choosing another value of
/// that facet would give.
fn work_filters(conn: &Connection, q: &MediaQuery, kind: &str, skip: Option<&str>) -> Result<(String, Vec<Sql>)> {
    let table = if kind == "movie" { "movie" } else { "series" };
    let mut args: Vec<Sql> = vec![Sql::Text(kind.to_owned())];
    let mut filters: Vec<String> = vec!["w.kind = ?1".into()];
    if !settings::get_bool(conn, "content.showAdult") {
        filters.push("w.adult = 0".into());
    }
    match (q.source_id, &q.category_id) {
        (Some(s), Some(cat)) => {
            args.push(Sql::Integer(s));
            args.push(Sql::Text(cat.clone()));
            filters.push(format!(
                "w.key IN (SELECT work_key FROM {table} WHERE source_id = ?{} AND category_id = ?{})",
                args.len() - 1,
                args.len()
            ));
        }
        (Some(s), None) => {
            args.push(Sql::Integer(s));
            filters.push(format!("w.key IN (SELECT work_key FROM {table} WHERE source_id = ?{})", args.len()));
        }
        _ => {}
    }
    for f in &q.facets {
        if !FACETS.contains(&f.facet.as_str()) {
            return Err(Error::msg(format!("unknown facet {}", f.facet)));
        }
        if skip == Some(f.facet.as_str()) {
            continue;
        }
        args.push(Sql::Text(f.facet.clone()));
        args.push(Sql::Text(f.value.clone()));
        filters.push(format!(
            "w.key IN (SELECT key FROM work_facet WHERE kind = ?1 AND facet = ?{} AND value = ?{})",
            args.len() - 1,
            args.len()
        ));
    }
    if q.favorites {
        filters.push(format!(
            "w.key IN (SELECT x.work_key FROM favorite f JOIN {table} x ON x.source_id = f.source_id AND x.id = f.item_id
                        WHERE f.kind = ?1)"
        ));
    }
    if let Some(text) = q.q.as_deref().filter(|t| !t.trim().is_empty()) {
        // any copy's title: "Kastanjemanden" finds "The Chestnut Man"
        args.push(Sql::Text(like_pattern(text)));
        filters.push(format!("w.key IN (SELECT work_key FROM {table} WHERE title LIKE ?{} ESCAPE '\\')", args.len()));
    }
    Ok((filters.join(" AND "), args))
}

fn query_works<T>(
    conn: &Connection,
    q: &MediaQuery,
    kind: &str,
    select: &str,
    map: fn(&Row) -> rusqlite::Result<T>,
) -> Result<Page<T>> {
    let table = if kind == "movie" { "movie" } else { "series" };
    let (where_sql, args) = work_filters(conn, q, kind, None)?;
    let order = match (q.favorites, q.sort.as_deref().unwrap_or("added")) {
        (true, _) => format!(
            "(SELECT MAX(f.added_at) FROM favorite f JOIN {table} x ON x.source_id = f.source_id AND x.id = f.item_id
               WHERE f.kind = ?1 AND x.work_key = w.key) DESC"
        ),
        (_, "title") => "w.title COLLATE NOCASE".to_owned(),
        (_, "rating") => "w.rating IS NULL, w.rating DESC, w.title COLLATE NOCASE".to_owned(),
        (_, "year") => "w.year IS NULL, w.year DESC, w.title COLLATE NOCASE".to_owned(),
        (_, "provider") => format!(
            "w.source_id, (SELECT position FROM {table} x WHERE x.source_id = w.source_id AND x.id = w.item_id)"
        ),
        _ => "w.added IS NULL, w.added DESC, w.title COLLATE NOCASE".to_owned(),
    };
    let total: i64 = conn
        .prepare_cached(&format!("SELECT COUNT(*) FROM work w WHERE {where_sql}"))?
        .query_row(params_from_iter(args.iter()), |r| r.get(0))?;
    let limit = q.limit.unwrap_or(120).clamp(1, 2000);
    let offset = q.offset.unwrap_or(0).max(0);
    let sql = format!("{select} WHERE {where_sql} ORDER BY {order} LIMIT {limit} OFFSET {offset}");
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
        let page = query_works(conn, &query, "movie", &movie_work_select(), row_to_movie)?;
        log::debug!("movies {:?}/{:?}: {} of {} in {:?}", query.category_id, query.sort, page.items.len(), page.total, t.elapsed());
        Ok(page)
    })
    .await
}

#[tauri::command]
pub async fn series_list(state: State<'_, AppState>, query: MediaQuery) -> Result<Page<SeriesItem>> {
    blocking(state.inner(), move |conn| query_works(conn, &query, "series", SERIES_WORK_SELECT, row_to_series)).await
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FacetValue {
    pub value: String,
    /// display name (collections: the provider category)
    pub label: String,
    /// works, not copies
    pub count: i64,
    /// collections: service/language group they are shown under
    pub group: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Facets {
    pub total: i64,
    pub service: Vec<FacetValue>,
    pub genre: Vec<FacetValue>,
    pub language: Vec<FacetValue>,
    pub quality: Vec<FacetValue>,
    pub decade: Vec<FacetValue>,
    pub collection: Vec<FacetValue>,
    /// from TMDB: original language, movie collections, TV networks
    pub original: Vec<FacetValue>,
    pub franchise: Vec<FacetValue>,
    pub network: Vec<FacetValue>,
}

/// Everything the Movies / Series browse panel offers, with work counts.
/// Facet values with the number of works each would show, given the rest
/// of `query` (its other facets, favorites, text filter).
#[tauri::command]
pub async fn work_facets(state: State<'_, AppState>, kind: String, query: Option<MediaQuery>) -> Result<Facets> {
    if kind != "movie" && kind != "series" {
        return Err(Error::msg(format!("no facets for {kind}")));
    }
    blocking(state.inner(), move |conn| {
        let t = Instant::now();
        let f = facets_for(conn, &kind, &query.unwrap_or_default())?;
        log::debug!("{kind} facets in {:?}", t.elapsed());
        Ok(f)
    })
    .await
}

pub fn facets_for(conn: &Connection, kind: &str, q: &MediaQuery) -> Result<Facets> {
    let show_adult = settings::get_bool(conn, "content.showAdult");
    let (where_sql, args) = work_filters(conn, q, kind, None)?;
    let total: i64 = conn
        .prepare_cached(&format!("SELECT COUNT(*) FROM work w WHERE {where_sql}"))?
        .query_row(params_from_iter(args.iter()), |r| r.get(0))?;
    let count = |facet: Option<&str>| -> Result<Vec<(String, String, i64)>> {
        let (where_sql, args) = work_filters(conn, q, kind, facet)?;
        let only = facet.map(|f| format!("AND f.facet = '{f}'")).unwrap_or_default();
        Ok(conn
            .prepare_cached(&format!(
                "SELECT f.facet, f.value, COUNT(*) FROM work_facet f
                   JOIN work w ON w.kind = f.kind AND w.key = f.key
                  WHERE f.kind = ?1 {only} AND {where_sql}
                  GROUP BY f.facet, f.value"
            ))?
            .query_map(params_from_iter(args.iter()), |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<Result<_, _>>()?)
    };
    let narrowed = q.favorites
        || q.source_id.is_some()
        || !q.facets.is_empty()
        || q.q.as_deref().is_some_and(|t| !t.trim().is_empty());
    let rows = if narrowed {
        // each facet counted without its own selection (FACETS are fixed names)
        let mut rows = Vec::new();
        for f in FACETS {
            rows.extend(count(Some(f))?);
        }
        rows
    } else {
        count(None)?
    };
    let mut out = Facets {
        total,
        service: vec![],
        genre: vec![],
        language: vec![],
        quality: vec![],
        decade: vec![],
        collection: vec![],
        original: vec![],
        franchise: vec![],
        network: vec![],
    };
    // provider categories behind the collection values
    let cats: std::collections::HashMap<String, (String, i64, i64, bool)> = conn
        .prepare_cached("SELECT source_id, id, name, position, adult FROM category WHERE kind = ?1")?
        .query_map([kind], |r| {
            let sid: i64 = r.get(0)?;
            let id: String = r.get(1)?;
            Ok((format!("{sid}:{id}"), (r.get(2)?, sid, r.get(3)?, r.get(4)?)))
        })?
        .collect::<Result<_, _>>()?;
    let mut collections: Vec<(i64, i64, FacetValue)> = Vec::new();
    for (facet, value, count) in rows {
        let label = value.clone();
        let fv = FacetValue { value, label, count, group: None };
        match facet.as_str() {
            "service" => out.service.push(fv),
            "genre" => out.genre.push(fv),
            "language" => out.language.push(fv),
            "quality" => out.quality.push(fv),
            "decade" => out.decade.push(fv),
            "original" => out.original.push(fv),
            "franchise" => out.franchise.push(fv),
            "network" => out.network.push(fv),
            "collection" => {
                if let Some((name, sid, pos, adult)) = cats.get(&fv.value)
                    && (show_adult || !adult)
                {
                    let v = crate::works::variant::parse(None, Some(name));
                    let group = v.service.or(v.language).unwrap_or("Other").to_owned();
                    let label = crate::names::display_category(name);
                    collections.push((*sid, *pos, FacetValue { label, group: Some(group), ..fv }));
                }
            }
            _ => {}
        }
    }
    let by_count = |v: &mut Vec<FacetValue>| v.sort_by(|a, b| b.count.cmp(&a.count).then(a.label.cmp(&b.label)));
    by_count(&mut out.service);
    by_count(&mut out.language);
    by_count(&mut out.original);
    by_count(&mut out.franchise);
    by_count(&mut out.network);
    out.genre.sort_by_key(|g| crate::works::genre::GENRES.iter().position(|x| *x == g.value).unwrap_or(usize::MAX));
    const QUALITY: &[&str] = &["4K", "Dolby Vision", "Dolby Audio", "HEVC", "Blu-ray"];
    out.quality.sort_by_key(|q| QUALITY.iter().position(|x| *x == q.value).unwrap_or(usize::MAX));
    // newest decade first, "Older" last
    out.decade.sort_by(|a, b| {
        let key = |v: &str| if v == "Older" { 0 } else { v.trim_end_matches('s').parse::<i64>().unwrap_or(0) };
        key(&b.value).cmp(&key(&a.value))
    });
    collections.sort_by_key(|(sid, pos, _)| (*sid, *pos));
    out.collection = collections.into_iter().map(|(_, _, f)| f).collect();
    Ok(out)
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
    let fetched = match sources::xtream_for(&src, http_client(src.user_agent.as_deref())) {
        Ok(x) if kind == "movie" => x.vod_info(id).await,
        Ok(x) => x.series_info(id).await,
        Err(e) => Err(e),
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

/// How long a detail page waits for the provider before showing what it has.
const DETAIL_DEADLINE: std::time::Duration = std::time::Duration::from_millis(2500);

/// Provider details of several copies at once. This provider stalls for ~9 s
/// on single requests now and then, so the page doesn't wait for every copy:
/// what arrives within `DETAIL_DEADLINE` (or from the cache) is used, the rest
/// keeps loading in the background (into the cache) and is reported as
/// pending — the UI asks again. At least one copy is always waited for.
async fn details_of(st: &AppState, kind: &'static str, copies: &[(i64, String)], ttl: i64) -> (Vec<Option<Value>>, bool) {
    let mut handles: Vec<Option<tokio::task::JoinHandle<Result<Option<Value>>>>> = copies
        .iter()
        .map(|(sid, id)| {
            let (st, sid, id) = (st.clone(), *sid, id.clone());
            Some(tokio::spawn(async move {
                match provider_detail(&st, sid, kind, &id, ttl).await {
                    // M3U series: episodes come from the playlist
                    Ok(None) if kind == "series" => blocking(&st, move |conn| m3u_series_json(conn, sid, &id)).await,
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
                Ok(Err(e)) => log::warn!("{kind} {}: no provider detail ({e})", copies[i].1),
                Err(e) => log::warn!("{kind} {}: detail task failed ({e})", copies[i].1),
            }
        }
    }
    if finished == 0 {
        // nothing within the deadline: take the first copy that answers
        let pending: Vec<(usize, tokio::task::JoinHandle<Result<Option<Value>>>)> =
            handles.iter_mut().enumerate().filter_map(|(i, h)| h.take().map(|h| (i, h))).collect();
        let (idx, futs): (Vec<usize>, Vec<_>) = pending.into_iter().unzip();
        if !futs.is_empty() {
            let (res, pos, rest) = futures_util::future::select_all(futs).await;
            if let Ok(Ok(v)) = res {
                out[idx[pos]] = v;
            }
            // the others keep filling the cache in the background
            drop(rest);
            return (out, idx.len() > 1);
        }
    }
    let pending = handles.iter().any(Option::is_some);
    (out, pending)
}

#[tauri::command]
pub async fn movie_detail(state: State<'_, AppState>, source_id: i64, id: String) -> Result<MovieDetail> {
    let st = state.inner().clone();
    let (members, pref, langs, work, facts, tracks) = {
        let id = id.clone();
        blocking(&st, move |conn| {
            let members = versions::members(conn, "movie", source_id, &id)?;
            if members.is_empty() {
                return Err(Error::NotFound(format!("movie {id}")));
            }
            let key = versions::work_key(conn, "movie", source_id, &id)?;
            let pref = match &key {
                Some(k) => versions::preference(conn, "movie", k)?,
                None => None,
            };
            let work = match &key {
                Some(k) => conn
                    .prepare_cached(&format!("{} WHERE w.kind = 'movie' AND w.key = ?1", movie_work_select()))?
                    .query_row([k], row_to_movie)
                    .optional()?,
                None => None,
            };
            let facts = match (&key, &work) {
                (Some(k), Some(w)) => crate::tmdb::facts_for(conn, "movie", k, &w.title, w.year)?,
                _ => None,
            };
            let tracks =
                members.iter().map(|m| versions::tracks(conn, m.source_id, "movie", &m.id)).collect::<Result<Vec<_>>>()?;
            Ok((members, pref, versions::language_prefs(conn), work, facts, tracks))
        })
        .await?
    };
    // provider details of every copy at once (cached for a week)
    let copies: Vec<(i64, String)> = members.iter().map(|m| (m.source_id, m.id.clone())).collect();
    let (details, pending) = details_of(&st, "movie", &copies, MOVIE_DETAIL_TTL).await;
    let info_of = |i: usize| details[i].as_ref().map(|d| d["info"].clone()).unwrap_or(Value::Null);
    // a copy much shorter than the others is a trailer or cut off
    let durations: Vec<Option<i64>> = (0..members.len()).map(|i| i64_of(&info_of(i)["duration_secs"]).filter(|d| *d > 0)).collect();
    let longest = durations.iter().flatten().copied().max().unwrap_or(0);
    let sel = versions::choose(&members, pref.as_ref(), &langs, |i| match durations[i] {
        _ if versions::unavailable(&tracks[i]) => versions::BROKEN,
        Some(d) if longest > 0 => d as f64 / longest as f64,
        _ => 1.0,
    });
    let chosen = members[sel].clone();
    let info = info_of(sel);
    // missing fields from the other copies' details
    let pick = |keys: &[&str]| -> Option<String> {
        first_str(&info, keys).or_else(|| (0..members.len()).find_map(|i| first_str(&info_of(i), keys)))
    };

    let mut item = {
        let (sid, mid) = (chosen.source_id, chosen.id.clone());
        blocking(&st, move |conn| {
            conn.prepare_cached(&format!("{MOVIE_SELECT} WHERE m.source_id = ?1 AND m.id = ?2"))?
                .query_row(params![sid, mid], row_to_movie)
                .optional()?
                .ok_or_else(|| Error::NotFound(format!("movie {mid}")))
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
    // resume where any copy was left off (versions are the same film)
    let latest = members.iter().filter(|m| m.played_at > 0).max_by_key(|m| m.played_at);
    let position = if chosen.position > 0.0 {
        chosen.position
    } else {
        latest.filter(|m| !m.watched).map(|m| m.position).unwrap_or(0.0)
    };
    let multi_source = members.iter().any(|m| m.source_id != members[0].source_id);
    let versions = members
        .iter()
        .enumerate()
        .map(|(i, m)| {
            let mut v = m.info(multi_source, i == sel);
            let inf = info_of(i);
            v.video = tech(&inf["video"]);
            v.audio = tech(&inf["audio"]);
            v.duration = i64_of(&inf["duration_secs"]).filter(|d| *d > 0);
            v.tracks = tracks[i].clone();
            v
        })
        .collect();
    let trailer = pick(&["youtube_trailer", "trailer"]).or_else(|| {
        let conn = st.db.read();
        conn.query_row("SELECT trailer FROM movie WHERE source_id = ?1 AND id = ?2", params![chosen.source_id, chosen.id], |r| {
            r.get(0)
        })
        .ok()
        .flatten()
    });
    Ok(MovieDetail {
        versions_pending: pending,
        plot: pick(&["plot", "description"]),
        cast: pick(&["cast", "actors"]),
        director: pick(&["director"]),
        genre: pick(&["genre"]),
        country: pick(&["country"]),
        release_date: pick(&["releasedate", "release_date"]),
        duration: i64_of(&info["duration_secs"]).filter(|d| *d > 0),
        backdrop: first_url(&info["backdrop_path"])
            .or_else(|| (0..members.len()).find_map(|i| first_url(&info_of(i)["backdrop_path"]))),
        trailer,
        age_rating: pick(&["mpaa_rating", "age"]),
        video: tech(&info["video"]),
        audio: tech(&info["audio"]),
        position,
        versions,
        tmdb: facts,
        item,
    })
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
pub fn m3u_series_json(conn: &Connection, source_id: i64, series_id: &str) -> Result<Option<Value>> {
    let mut stmt = conn.prepare_cached(
        "SELECT id, season, episode, title, image, ext FROM episode
          WHERE source_id = ?1 AND series_id = ?2 ORDER BY season, episode, position",
    )?;
    type Row = (String, i64, i64, String, Option<String>, Option<String>);
    let rows: Vec<Row> = stmt
        .query_map(params![source_id, series_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)))?
        .collect::<Result<_, _>>()?;
    if rows.is_empty() {
        return Ok(None);
    }
    let mut seasons = serde_json::Map::new();
    for (id, season, episode, title, image, ext) in rows {
        if let Value::Array(list) = seasons.entry(season.to_string()).or_insert_with(|| Value::Array(Vec::new())) {
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
fn parse_episodes(detail: &Value, series_title: &str, source_id: i64, series_id: &str) -> Vec<(i64, Vec<Episode>)> {
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
    blocking(state.inner(), move |conn| up_next_rows(conn, limit)).await
}

/// Episodes of one series copy from what is stored locally (the provider
/// detail cache, or the playlist for M3U series) — no network.
pub fn stored_episodes(conn: &Connection, source_id: i64, series_id: &str, title: &str) -> Result<Vec<(i64, Vec<Episode>)>> {
    let json = match cached(conn, source_id, "series", series_id)? {
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
fn union_episodes(per_copy: &[Vec<(i64, Vec<Episode>)>], order: &[usize]) -> BTreeMap<(i64, i64), (usize, Episode)> {
    let mut out = BTreeMap::new();
    for &i in order {
        for (_, eps) in &per_copy[i] {
            for e in eps {
                out.entry((e.season, e.episode)).or_insert_with(|| (i, e.clone()));
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
        let members = versions::members(conn, "series", sid, &series_id)?;
        if members.is_empty() {
            continue;
        }
        let key = versions::work_key(conn, "series", sid, &series_id)?;
        let series = match &key {
            Some(k) => conn
                .prepare_cached(&format!("{SERIES_WORK_SELECT} WHERE w.kind = 'series' AND w.key = ?1"))?
                .query_row([k], row_to_series)
                .optional()?,
            None => conn
                .prepare_cached(&format!("{SERIES_SELECT} WHERE s.source_id = ?1 AND s.id = ?2"))?
                .query_row(params![sid, series_id], row_to_series)
                .optional()?,
        };
        let Some(series) = series else { continue };
        let per_copy: Vec<_> = members
            .iter()
            .map(|m| stored_episodes(conn, m.source_id, &m.id, &series.title))
            .collect::<Result<_>>()?;
        // continue in the copy that was being watched, then the chosen one
        let watching = members.iter().position(|m| m.source_id == sid && m.id == series_id).unwrap_or(0);
        let pref = match &key {
            Some(k) => versions::preference(conn, "series", k)?,
            None => None,
        };
        let chosen = versions::choose(&members, pref.as_ref(), &versions::language_prefs(conn), |_| 1.0);
        let mut order = vec![watching, chosen];
        order.extend(0..members.len());
        order.dedup();
        let union = union_episodes(&per_copy, &order);
        // where we are: the history row's numbers, else find its episode id
        let here = match (season, episode) {
            (Some(s), Some(e)) => Some((s, e)),
            _ => union.iter().find(|(_, (_, e))| e.id == last_id).map(|(k, _)| *k),
        };
        let Some(here) = here else { continue };
        let Some((_, (copy, next))) = union.range((here.0, here.1 + 1)..).find(|((s, _), _)| *s > 0) else {
            continue;
        };
        let mut next = next.clone();
        if *copy != chosen {
            next.version = Some(members[*copy].variant.label.clone());
        }
        let seen: bool = conn
            .query_row(
                "SELECT MAX(h.watched) FROM history h
                   LEFT JOIN series s ON s.source_id = h.source_id AND s.id = h.series_id
                  WHERE h.kind = 'episode' AND h.season = ?1 AND h.episode = ?2
                    AND ((?3 IS NOT NULL AND s.work_key = ?3) OR (h.source_id = ?4 AND h.series_id = ?5))",
                params![next.season, next.episode, key, sid, series_id],
                |r| r.get::<_, Option<bool>>(0),
            )?
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
pub async fn series_detail(state: State<'_, AppState>, source_id: i64, id: String) -> Result<SeriesDetail> {
    let st = state.inner().clone();
    let (members, pref, langs, work, text, history, facts) = {
        let id = id.clone();
        blocking(&st, move |conn| {
            let members = versions::members(conn, "series", source_id, &id)?;
            if members.is_empty() {
                return Err(Error::NotFound(format!("series {id}")));
            }
            let key = versions::work_key(conn, "series", source_id, &id)?;
            let pref = match &key {
                Some(k) => versions::preference(conn, "series", k)?,
                None => None,
            };
            let work = match &key {
                Some(k) => conn
                    .prepare_cached(&format!("{SERIES_WORK_SELECT} WHERE w.kind = 'series' AND w.key = ?1"))?
                    .query_row([k], row_to_series)
                    .optional()?,
                None => None,
            };
            let work = match work {
                Some(w) => w,
                None => conn
                    .prepare_cached(&format!("{SERIES_SELECT} WHERE s.source_id = ?1 AND s.id = ?2"))?
                    .query_row(params![source_id, id], row_to_series)
                    .optional()?
                    .ok_or_else(|| Error::NotFound(format!("series {id}")))?,
            };
            // long texts: the requested copy, else any copy that has them
            let text = conn.query_row(
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
            )?;
            let history: Vec<Watched> = conn
                .prepare_cached(
                    "SELECT h.source_id, h.item_id, h.season, h.episode, h.position, h.watched, h.updated_at
                       FROM history h
                       LEFT JOIN series s ON s.source_id = h.source_id AND s.id = h.series_id
                      WHERE h.kind = 'episode'
                        AND ((?1 IS NOT NULL AND s.work_key = ?1) OR (h.source_id = ?2 AND h.series_id = ?3))",
                )?
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
                .collect::<Result<_, _>>()?;
            let facts = match &key {
                Some(k) => crate::tmdb::facts_for(conn, "series", k, &work.title, work.year)?,
                None => None,
            };
            Ok((members, pref, versions::language_prefs(conn), work, text, history, facts))
        })
        .await?
    };

    // every copy's episodes at once (cached for 12 h)
    let copies: Vec<(i64, String)> = members.iter().map(|m| (m.source_id, m.id.clone())).collect();
    let (details, pending) = details_of(&st, "series", &copies, SERIES_DETAIL_TTL).await;
    let details: Vec<Value> = details.into_iter().map(|d| d.unwrap_or(Value::Null)).collect();
    let per_copy: Vec<Vec<(i64, Vec<Episode>)>> = members
        .iter()
        .zip(&details)
        .map(|(m, d)| parse_episodes(d, &work.title, m.source_id, &m.id))
        .collect();
    let counts: Vec<usize> =
        per_copy.iter().map(|seasons| seasons.iter().filter(|(n, _)| *n > 0).map(|(_, e)| e.len()).sum()).collect();
    let most = counts.iter().copied().max().unwrap_or(0).max(1);
    // tracks seen in each copy (the first of its episodes with any)
    let tracks: Vec<Option<Value>> = {
        let ids: Vec<(i64, Vec<String>)> = per_copy
            .iter()
            .zip(&members)
            .map(|(seasons, m)| (m.source_id, seasons.iter().flat_map(|(_, e)| e.iter().map(|e| e.id.clone())).take(40).collect()))
            .collect();
        blocking(&st, move |conn| {
            ids.iter()
                .map(|(sid, eps)| {
                    for e in eps {
                        if let Some(t) = versions::tracks(conn, *sid, "episode", e)? {
                            return Ok(Some(t));
                        }
                    }
                    Ok(None)
                })
                .collect::<Result<Vec<_>>>()
        })
        .await?
    };
    let sel = versions::choose(&members, pref.as_ref(), &langs, |i| {
        if versions::unavailable(&tracks[i]) { versions::BROKEN } else { counts[i] as f64 / most as f64 }
    });

    // selected copy first, then the others by their own fit
    let mut order: Vec<usize> = (0..members.len()).collect();
    order.sort_by_key(|&i| {
        let m = &members[i];
        (i != sel, -(crate::works::variant::affinity(&m.variant, &langs) + crate::works::variant::score(&m.variant)))
    });
    let union = union_episodes(&per_copy, &order);

    // history rows → (season, episode): by the episode id in its copy, else the stored numbers
    let number_of = |w: &Watched| -> Option<(i64, i64)> {
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
    };
    let mut state_of: HashMap<(i64, i64), &Watched> = HashMap::new();
    for w in &history {
        if let Some(k) = number_of(w)
            && state_of.get(&k).is_none_or(|prev| w.at > prev.at)
        {
            state_of.insert(k, w);
        }
    }

    let mut seasons: Vec<Season> = Vec::new();
    for ((s_no, _), (copy, e)) in &union {
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
                // season names/art from the selected copy, else any copy
                let meta = order.iter().find_map(|&i| {
                    details[i]["seasons"].as_array().and_then(|a| a.iter().find(|m| i64_of(&m["season_number"]) == Some(*s_no)))
                });
                seasons.push(Season {
                    season: *s_no,
                    name: meta
                        .and_then(|m| str_of(&m["name"]))
                        .unwrap_or_else(|| if *s_no == 0 { "Specials".into() } else { format!("Season {s_no}") }),
                    cover: meta.and_then(|m| first_str(m, &["cover_big", "cover"])),
                    overview: meta.and_then(|m| str_of(&m["overview"])),
                    air_date: meta.and_then(|m| str_of(&m["air_date"])),
                    episodes: vec![e],
                });
            }
        }
    }

    // Resume: the most recent unfinished episode, else the one after the
    // most recently finished, else S1E1 — across all copies.
    let flat: Vec<&Episode> = seasons.iter().filter(|s| s.season > 0).flat_map(|s| &s.episodes).collect();
    let latest = history.iter().filter_map(|w| number_of(w).map(|k| (k, w))).max_by_key(|(_, w)| w.at);
    let resume = match latest {
        Some((k, w)) if !w.watched => flat.iter().find(|e| (e.season, e.episode) == k).map(|e| Resume {
            episode_id: e.id.clone(),
            season: e.season,
            episode: e.episode,
            position: w.position,
            started: true,
        }),
        Some((k, _)) => flat.iter().position(|e| (e.season, e.episode) == k).and_then(|i| flat.get(i + 1)).map(|e| {
            Resume { episode_id: e.id.clone(), season: e.season, episode: e.episode, position: 0.0, started: true }
        }),
        None => None,
    }
    .or_else(|| {
        flat.first().map(|e| Resume { episode_id: e.id.clone(), season: e.season, episode: e.episode, position: 0.0, started: false })
    });

    let multi_source = members.iter().any(|m| m.source_id != members[0].source_id);
    let versions: Vec<VersionInfo> = members
        .iter()
        .enumerate()
        .map(|(i, m)| {
            let mut v = m.info(multi_source, i == sel);
            v.seasons = per_copy[i].iter().filter(|(n, e)| *n > 0 && !e.is_empty()).map(|(n, _)| *n).collect();
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
        item.backdrop = order.iter().find_map(|&i| first_url(&details[i]["info"]["backdrop_path"]));
    }
    let info = &details[sel]["info"];
    Ok(SeriesDetail {
        versions_pending: pending,
        plot: text.plot.or_else(|| str_of(&info["plot"])),
        cast: text.cast.or_else(|| str_of(&info["cast"])),
        director: text.director.or_else(|| str_of(&info["director"])),
        release_date: text.release.or_else(|| first_str(info, &["releaseDate", "release_date"])),
        trailer: text.trailer.or_else(|| str_of(&info["youtube_trailer"])),
        seasons,
        resume,
        versions,
        tmdb: facts,
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
                    "SELECT COALESCE(c.epg_id, g.epg_id) FROM channel c LEFT JOIN channel_group g ON g.key = c.group_key
                      WHERE c.source_id = ?1 AND c.id = ?2",
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
    let Ok(x) = sources::xtream_for(&src, http_client(src.user_agent.as_deref())) else {
        return Ok(rows);
    };
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
                .query_map(params![fts, kind, limit * 8], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?
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
        // feeds of the same channel collapse into their group
        let mut channels: Vec<ChannelItem> = Vec::new();
        let mut group_of = conn.prepare_cached("SELECT group_key FROM channel WHERE source_id = ?1 AND id = ?2")?;
        for (sid, id) in ids("live")? {
            if channels.len() as i64 >= limit {
                break;
            }
            if !adult_ok("channel", sid, &id) {
                continue;
            }
            let key: Option<String> = group_of.query_row(params![sid, id], |r| r.get(0)).optional()?.flatten();
            if key.as_ref().is_some_and(|k| channels.iter().any(|c| c.group.as_ref().is_some_and(|g| &g.key == k))) {
                continue;
            }
            let c = match &key {
                Some(k) => channel_group_by_key(conn, k),
                None => channel_by_id(conn, sid, &id),
            };
            if let Ok(c) = c {
                channels.push(c);
            }
        }
        // copies of the same title collapse into their work
        let works = |kind: &str| -> Result<Vec<String>> {
            let table = if kind == "movie" { "movie" } else { "series" };
            let mut key_of = conn.prepare_cached(&format!("SELECT work_key FROM {table} WHERE source_id = ?1 AND id = ?2"))?;
            let mut keys: Vec<String> = Vec::new();
            for (sid, id) in ids(kind)? {
                if keys.len() as i64 >= limit {
                    break;
                }
                if !adult_ok(table, sid, &id) {
                    continue;
                }
                if let Some(Some(k)) = key_of.query_row(params![sid, id], |r| r.get::<_, Option<String>>(0)).optional()?
                    && !keys.contains(&k)
                {
                    keys.push(k);
                }
            }
            Ok(keys)
        };
        let mut movies = Vec::new();
        {
            let mut stmt = conn.prepare_cached(&format!("{} WHERE w.kind = 'movie' AND w.key = ?1", movie_work_select()))?;
            for key in works("movie")? {
                if let Some(m) = stmt.query_row([key], row_to_movie).optional()? {
                    movies.push(m);
                }
            }
        }
        let mut series = Vec::new();
        {
            let mut stmt = conn.prepare_cached(&format!("{SERIES_WORK_SELECT} WHERE w.kind = 'series' AND w.key = ?1"))?;
            for key in works("series")? {
                if let Some(s) = stmt.query_row([key], row_to_series).optional()? {
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
    fn m3u_series_read_like_xtream_ones() {
        let c = crate::db::test_conn();
        c.execute("INSERT INTO series (source_id, id, name, title, position) VALUES (1, 'sh', 'Show', 'Show', 0)", [])
            .unwrap();
        for (id, season, ep, title) in [("a", 1, 2, "Second"), ("b", 1, 1, ""), ("c", 2, 1, "Premiere")] {
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
            .map(|(n, eps)| (n, eps.into_iter().map(|e| (e.id, e.episode, e.title)).collect()))
            .collect();
        assert_eq!(
            shape,
            vec![
                (1, vec![("b".into(), 1, "Episode 1".into()), ("a".into(), 2, "Second".into())]),
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
        let next: Vec<String> = up_next_rows(&c, 20).unwrap().into_iter().map(|u| u.episode.id).collect();
        assert_eq!(next, vec!["c"]);
    }

    /// Two copies of one show: the Nordic one only has season 5.
    fn two_copy_show(c: &Connection) {
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
                .map(|u| (u.episode.series_id, u.episode.id, u.episode.season, u.episode.episode, u.episode.version))
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
        assert_eq!((u[0].episode.id.as_str(), u[0].episode.season, u[0].episode.episode), ("f51", 5, 1));
        assert_eq!(u[0].series.title, "Show");
        assert_eq!(u[0].series.work.version_count, 2);
    }

    #[test]
    fn a_country_lists_its_guide_channels_first() {
        let c = crate::db::test_conn();
        c.execute(
            "INSERT INTO category (source_id, kind, id, name, title, region, position)
             VALUES (1, 'live', 'ent', 'UK| ENTERTAINMENT', 'ENTERTAINMENT', 'UK', 0)",
            [],
        )
        .unwrap();
        // the provider lists its 24/7 feeds (no guide data) before BBC One
        for (id, title, epg, pos) in [("fast", "BAYWATCH", None, 0), ("bbc1", "BBC ONE", Some("BBCOne.uk"), 1)] {
            c.execute(
                "INSERT INTO channel (source_id, id, name, title, category_id, epg_id, position)
                 VALUES (1, ?1, ?2, ?2, 'ent', ?3, ?4)",
                params![id, title, epg, pos],
            )
            .unwrap();
        }
        crate::works::rebuild(&c).unwrap();
        let titles = |q: ChannelQuery| -> Vec<String> {
            query_channels(&c, &q).unwrap().items.into_iter().map(|i| i.title).collect()
        };
        let uk = ChannelQuery { grouped: true, country: Some("UK".into()), ..Default::default() };
        assert_eq!(titles(uk), ["BBC ONE", "BAYWATCH"]);
        let genre = ChannelQuery { grouped: true, genre: Some("Entertainment".into()), ..Default::default() };
        assert_eq!(titles(genre), ["BBC ONE", "BAYWATCH"]);
    }

    #[test]
    fn works_list_one_row_per_title_and_filter_through_copies() {
        let c = crate::db::test_conn();
        two_copy_show(&c);
        let q = |q: MediaQuery| query_works(&c, &q, "series", SERIES_WORK_SELECT, row_to_series).unwrap();
        let all = q(MediaQuery::default());
        assert_eq!((all.total, all.items[0].work.version_count), (1, 2));
        // a category of either copy finds the work
        let nordic = q(MediaQuery { source_id: Some(1), category_id: Some("sc".into()), ..Default::default() });
        assert_eq!(nordic.total, 1);
        let by_service = q(MediaQuery {
            facets: vec![FacetFilter { facet: "service".into(), value: "Apple TV+".into() }],
            ..Default::default()
        });
        assert_eq!(by_service.total, 1);
        let none = q(MediaQuery {
            facets: vec![FacetFilter { facet: "service".into(), value: "Netflix".into() }],
            ..Default::default()
        });
        assert_eq!(none.total, 0);
        let bad = query_works(
            &c,
            &MediaQuery { facets: vec![FacetFilter { facet: "x; DROP".into(), value: "1".into() }], ..Default::default() },
            "series",
            SERIES_WORK_SELECT,
            row_to_series,
        );
        assert!(bad.is_err());
        let f = facets_for(&c, "series", &MediaQuery::default()).unwrap();
        assert_eq!(f.total, 1);
        assert!(f.service.iter().any(|v| v.value == "Apple TV+" && v.count == 1));
        assert!(f.collection.iter().any(|v| v.value == "1:sc" && v.group.as_deref() == Some("Nordic")));
        assert!(f.collection.iter().any(|v| v.label == "Apple+ Series"));
        // counts follow the other filters but not the facet's own choice
        c.execute(
            "INSERT INTO series (source_id, id, name, title, tag, year, tmdb, category_id, position)
             VALUES (1, 'nf', 'x', 'Other', 'NF', 2001, '888', 'ap', 0)",
            [],
        )
        .unwrap();
        crate::works::rebuild(&c).unwrap();
        let netflix = MediaQuery {
            facets: vec![FacetFilter { facet: "service".into(), value: "Netflix".into() }],
            ..Default::default()
        };
        let f = facets_for(&c, "series", &netflix).unwrap();
        assert_eq!(f.total, 1);
        let count = |v: &[FacetValue], value: &str| v.iter().find(|x| x.value == value).map(|x| x.count);
        assert_eq!((count(&f.service, "Netflix"), count(&f.service, "Apple TV+")), (Some(1), Some(1)));
        assert_eq!((count(&f.decade, "2000s"), count(&f.decade, "2010s")), (Some(1), None));
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
