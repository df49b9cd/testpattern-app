//! Movie and series listings (one row per work) and their browse facets.

use std::time::Instant;

use rusqlite::types::Value as Sql;
use rusqlite::{Connection, OptionalExtension, Row, params, params_from_iter};
use serde::Serialize;
use tauri::State;

use crate::error::{Error, Result};
use crate::kind::WorkKind;
use crate::settings;
use crate::state::{AppState, read_blocking};

use super::types::{FACETS, like_pattern, work_info};
use super::{MediaQuery, MovieItem, Page, SeriesItem};

/// One movie copy with its work's grouping info.
pub(super) const MOVIE_SELECT: &str = "
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
pub(super) fn movie_work_select() -> String {
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

pub(super) fn row_to_movie(r: &Row) -> rusqlite::Result<MovieItem> {
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
pub(super) const SERIES_SELECT: &str = "
    SELECT s.source_id, s.id, s.title, s.year, s.cover, s.backdrop, s.rating, s.genre, s.last_modified, s.tag,
           EXISTS(SELECT 1 FROM favorite f WHERE f.source_id = s.source_id AND f.kind = 'series' AND f.item_id = s.id),
           s.work_key, w.versions, w.badges, w.services
      FROM series s
      LEFT JOIN work w ON w.kind = 'series' AND w.key = s.work_key";

/// One series work (browse lists), same columns as `SERIES_SELECT`.
pub(super) const SERIES_WORK_SELECT: &str = "
    SELECT w.source_id, w.item_id, w.title, w.year, w.poster, w.backdrop, w.rating, w.genre, w.added, s.tag,
           EXISTS(SELECT 1 FROM favorite f JOIN series fs ON fs.source_id = f.source_id AND fs.id = f.item_id
                   WHERE f.kind = 'series' AND fs.work_key = w.key),
           w.key, w.versions, w.badges, w.services
      FROM work w
      LEFT JOIN series s ON s.source_id = w.source_id AND s.id = w.item_id";

pub(super) fn row_to_series(r: &Row) -> rusqlite::Result<SeriesItem> {
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

/// The movie work `key` (browse-list shape).
pub(super) fn movie_by_key(conn: &Connection, key: &str) -> Result<Option<MovieItem>> {
    Ok(conn
        .prepare_cached(&format!(
            "{} WHERE w.kind = 'movie' AND w.key = ?1",
            movie_work_select()
        ))?
        .query_row([key], row_to_movie)
        .optional()?)
}

/// One movie copy.
pub(super) fn movie_by_item(
    conn: &Connection,
    source_id: i64,
    id: &str,
) -> Result<Option<MovieItem>> {
    Ok(conn
        .prepare_cached(&format!(
            "{MOVIE_SELECT} WHERE m.source_id = ?1 AND m.id = ?2"
        ))?
        .query_row(params![source_id, id], row_to_movie)
        .optional()?)
}

/// The series work `key` (browse-list shape).
pub(super) fn series_by_key(conn: &Connection, key: &str) -> Result<Option<SeriesItem>> {
    Ok(conn
        .prepare_cached(&format!(
            "{SERIES_WORK_SELECT} WHERE w.kind = 'series' AND w.key = ?1"
        ))?
        .query_row([key], row_to_series)
        .optional()?)
}

/// One series copy.
pub(super) fn series_by_item(
    conn: &Connection,
    source_id: i64,
    id: &str,
) -> Result<Option<SeriesItem>> {
    Ok(conn
        .prepare_cached(&format!(
            "{SERIES_SELECT} WHERE s.source_id = ?1 AND s.id = ?2"
        ))?
        .query_row(params![source_id, id], row_to_series)
        .optional()?)
}

/// Episode history rows of a show, all copies counted as one: `?{k}` is the
/// work key (may be NULL), `?{s}`/`?{i}` the copy's source id and series id.
/// Expects `history h` joined to `series s` on the episode's series.
pub(super) fn episode_history_filter(k: usize, s: usize, i: usize) -> String {
    format!(
        "((?{k} IS NOT NULL AND s.work_key = ?{k}) OR (h.source_id = ?{s} AND h.series_id = ?{i}))"
    )
}

/// Browse lists: one row per work. Filters match through the members, so a
/// work shows up in every provider category and facet any copy belongs to.
/// WHERE clause over `work w` for a browse query (`?1` = kind). Facet
/// `skip` is left out — facet counts show what choosing another value of
/// that facet would give.
fn work_filters(
    conn: &Connection,
    q: &MediaQuery,
    kind: WorkKind,
    skip: Option<&str>,
) -> Result<(String, Vec<Sql>)> {
    let table = kind.table();
    let mut args: Vec<Sql> = vec![Sql::Text(kind.as_str().to_owned())];
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
            filters.push(format!(
                "w.key IN (SELECT work_key FROM {table} WHERE source_id = ?{})",
                args.len()
            ));
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
        filters.push(format!(
            "w.key IN (SELECT work_key FROM {table} WHERE title LIKE ?{} ESCAPE '\\')",
            args.len()
        ));
    }
    Ok((filters.join(" AND "), args))
}

fn query_works<T>(
    conn: &Connection,
    q: &MediaQuery,
    kind: WorkKind,
    select: &str,
    map: fn(&Row) -> rusqlite::Result<T>,
) -> Result<Page<T>> {
    let table = kind.table();
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
    read_blocking(state.inner(), move |conn| {
        let t = Instant::now();
        let page = query_works(
            conn,
            &query,
            WorkKind::Movie,
            &movie_work_select(),
            row_to_movie,
        )?;
        log::debug!(
            "movies {:?}/{:?}: {} of {} in {:?}",
            query.category_id,
            query.sort,
            page.items.len(),
            page.total,
            t.elapsed()
        );
        Ok(page)
    })
    .await
}

#[tauri::command]
pub async fn series_list(
    state: State<'_, AppState>,
    query: MediaQuery,
) -> Result<Page<SeriesItem>> {
    read_blocking(state.inner(), move |conn| {
        query_works(
            conn,
            &query,
            WorkKind::Series,
            SERIES_WORK_SELECT,
            row_to_series,
        )
    })
    .await
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
pub async fn work_facets(
    state: State<'_, AppState>,
    kind: WorkKind,
    query: Option<MediaQuery>,
) -> Result<Facets> {
    read_blocking(state.inner(), move |conn| {
        let t = Instant::now();
        let f = facets_for(conn, kind, &query.unwrap_or_default())?;
        log::debug!("{kind} facets in {:?}", t.elapsed());
        Ok(f)
    })
    .await
}

pub fn facets_for(conn: &Connection, kind: WorkKind, q: &MediaQuery) -> Result<Facets> {
    let show_adult = settings::get_bool(conn, "content.showAdult");
    let (where_sql, args) = work_filters(conn, q, kind, None)?;
    let total: i64 = conn
        .prepare_cached(&format!("SELECT COUNT(*) FROM work w WHERE {where_sql}"))?
        .query_row(params_from_iter(args.iter()), |r| r.get(0))?;
    let count = |facet: Option<&str>| -> Result<Vec<(String, String, i64)>> {
        let (where_sql, args) = work_filters(conn, q, kind, facet)?;
        let only = facet
            .map(|f| format!("AND f.facet = '{f}'"))
            .unwrap_or_default();
        Ok(conn
            .prepare_cached(&format!(
                "SELECT f.facet, f.value, COUNT(*) FROM work_facet f
                   JOIN work w ON w.kind = f.kind AND w.key = f.key
                  WHERE f.kind = ?1 {only} AND {where_sql}
                  GROUP BY f.facet, f.value"
            ))?
            .query_map(params_from_iter(args.iter()), |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })?
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
        .prepare_cached(
            "SELECT source_id, id, name, position, adult FROM category WHERE kind = ?1",
        )?
        .query_map([kind], |r| {
            let sid: i64 = r.get(0)?;
            let id: String = r.get(1)?;
            Ok((
                format!("{sid}:{id}"),
                (r.get(2)?, sid, r.get(3)?, r.get(4)?),
            ))
        })?
        .collect::<Result<_, _>>()?;
    let mut collections: Vec<(i64, i64, FacetValue)> = Vec::new();
    for (facet, value, count) in rows {
        let label = value.clone();
        let fv = FacetValue {
            value,
            label,
            count,
            group: None,
        };
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
                    collections.push((
                        *sid,
                        *pos,
                        FacetValue {
                            label,
                            group: Some(group),
                            ..fv
                        },
                    ));
                }
            }
            _ => {}
        }
    }
    let by_count = |v: &mut Vec<FacetValue>| {
        v.sort_by(|a, b| b.count.cmp(&a.count).then(a.label.cmp(&b.label)))
    };
    by_count(&mut out.service);
    by_count(&mut out.language);
    by_count(&mut out.original);
    by_count(&mut out.franchise);
    by_count(&mut out.network);
    out.genre.sort_by_key(|g| {
        crate::works::genre::GENRES
            .iter()
            .position(|x| *x == g.value)
            .unwrap_or(usize::MAX)
    });
    const QUALITY: &[&str] = &["4K", "Dolby Vision", "Dolby Audio", "HEVC", "Blu-ray"];
    out.quality.sort_by_key(|q| {
        QUALITY
            .iter()
            .position(|x| *x == q.value)
            .unwrap_or(usize::MAX)
    });
    // newest decade first, "Older" last
    out.decade.sort_by(|a, b| {
        let key = |v: &str| {
            if v == "Older" {
                0
            } else {
                v.trim_end_matches('s').parse::<i64>().unwrap_or(0)
            }
        };
        key(&b.value).cmp(&key(&a.value))
    });
    collections.sort_by_key(|(sid, pos, _)| (*sid, *pos));
    out.collection = collections.into_iter().map(|(_, _, f)| f).collect();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::super::FacetFilter;
    use super::*;
    use crate::catalog::details::tests::two_copy_show;

    #[test]
    fn works_list_one_row_per_title_and_filter_through_copies() {
        let c = crate::db::test_conn();
        two_copy_show(&c);
        let q = |q: MediaQuery| {
            query_works(&c, &q, WorkKind::Series, SERIES_WORK_SELECT, row_to_series).unwrap()
        };
        let all = q(MediaQuery::default());
        assert_eq!((all.total, all.items[0].work.version_count), (1, 2));
        // a category of either copy finds the work
        let nordic = q(MediaQuery {
            source_id: Some(1),
            category_id: Some("sc".into()),
            ..Default::default()
        });
        assert_eq!(nordic.total, 1);
        let by_service = q(MediaQuery {
            facets: vec![FacetFilter {
                facet: "service".into(),
                value: "Apple TV+".into(),
            }],
            ..Default::default()
        });
        assert_eq!(by_service.total, 1);
        let none = q(MediaQuery {
            facets: vec![FacetFilter {
                facet: "service".into(),
                value: "Netflix".into(),
            }],
            ..Default::default()
        });
        assert_eq!(none.total, 0);
        let bad = query_works(
            &c,
            &MediaQuery {
                facets: vec![FacetFilter {
                    facet: "x; DROP".into(),
                    value: "1".into(),
                }],
                ..Default::default()
            },
            WorkKind::Series,
            SERIES_WORK_SELECT,
            row_to_series,
        );
        assert!(bad.is_err());
        let f = facets_for(&c, WorkKind::Series, &MediaQuery::default()).unwrap();
        assert_eq!(f.total, 1);
        assert!(
            f.service
                .iter()
                .any(|v| v.value == "Apple TV+" && v.count == 1)
        );
        assert!(
            f.collection
                .iter()
                .any(|v| v.value == "1:sc" && v.group.as_deref() == Some("Nordic"))
        );
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
            facets: vec![FacetFilter {
                facet: "service".into(),
                value: "Netflix".into(),
            }],
            ..Default::default()
        };
        let f = facets_for(&c, WorkKind::Series, &netflix).unwrap();
        assert_eq!(f.total, 1);
        let count =
            |v: &[FacetValue], value: &str| v.iter().find(|x| x.value == value).map(|x| x.count);
        assert_eq!(
            (count(&f.service, "Netflix"), count(&f.service, "Apple TV+")),
            (Some(1), Some(1))
        );
        assert_eq!(
            (count(&f.decade, "2000s"), count(&f.decade, "2010s")),
            (Some(1), None)
        );
    }
}
