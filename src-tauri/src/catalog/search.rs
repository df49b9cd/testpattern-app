//! Full-text search over channels, movies and series.

use std::time::Instant;

use rusqlite::{OptionalExtension, params};
use serde::Serialize;
use tauri::State;

use crate::error::Result;
use crate::kind::{ItemKind, WorkKind};
use crate::settings;
use crate::state::{AppState, read_blocking};

use super::{
    ChannelItem, MovieItem, SeriesItem, channel_by_id, channel_group_by_key, movie_by_key,
    series_by_key,
};

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
pub async fn search(
    state: State<'_, AppState>,
    q: String,
    limit: Option<i64>,
) -> Result<SearchResults> {
    let limit = limit.unwrap_or(30).clamp(1, 200);
    read_blocking(state.inner(), move |conn| {
        let Some(fts) = fts_query(&q) else {
            return Ok(SearchResults { channels: vec![], movies: vec![], series: vec![] });
        };
        let t = Instant::now();
        let show_adult = settings::get_bool(conn, "content.showAdult");
        let ids = |kind: ItemKind| -> Result<Vec<(i64, String)>> {
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
        for (sid, id) in ids(ItemKind::Live)? {
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
        let works = |kind: WorkKind| -> Result<Vec<String>> {
            let table = kind.table();
            let mut key_of = conn.prepare_cached(&format!("SELECT work_key FROM {table} WHERE source_id = ?1 AND id = ?2"))?;
            let mut keys: Vec<String> = Vec::new();
            for (sid, id) in ids(kind.into())? {
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
        for key in works(WorkKind::Movie)? {
            movies.extend(movie_by_key(conn, &key)?);
        }
        let mut series = Vec::new();
        for key in works(WorkKind::Series)? {
            series.extend(series_by_key(conn, &key)?);
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
    fn fts_queries() {
        assert_eq!(fts_query("bbc news").as_deref(), Some("\"bbc\"* \"news\"*"));
        assert_eq!(fts_query("  \"  ").as_deref(), None);
        assert_eq!(fts_query("top-gun").as_deref(), Some("\"top\"* \"gun\"*"));
    }
}
