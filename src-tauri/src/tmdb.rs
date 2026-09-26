//! Optional metadata from TMDB (themoviedb.org) for the titles the provider
//! lists with a TMDB id: genres (the provider's movie list has none),
//! original language, collections, TV networks, ratings and missing
//! artwork. Fetched in the background with the user's own API key and folded
//! into the works by `works::rebuild`.
//!
//! "This product uses the TMDB API but is not endorsed or certified by TMDB."

use std::sync::LazyLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use futures_util::StreamExt;
use parking_lot::Mutex;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{AppHandle, Emitter, Runtime, State};

use crate::db::now;
use crate::error::{Error, Result};
use crate::state::{AppState, http_client};

/// Setting holding the API key (v3) or read access token (v4). Kept out of
/// `settings_get` and logs.
pub const KEY_SETTING: &str = "tmdb.key";
pub const EVENT: &str = "tmdb://progress";
/// API v3 (https://developer.themoviedb.org/openapi/tmdb-api.json)
const API: &str = "https://api.themoviedb.org/3";
pub const IMAGES: &str = "https://image.tmdb.org/t/p";
/// Details are fetched again after this long.
const MAX_AGE: i64 = 30 * 86_400;
/// Requests per second; TMDB tolerates about 40.
const RATE: u64 = 25;
const CONCURRENCY: usize = 8;
/// Regroup the catalog after this many new details (and at the end).
const REGROUP_EVERY: usize = 4000;

/// What TMDB knows about a movie or show — the parts testpattern uses.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Info {
    pub title: String,
    #[serde(default)]
    pub original_title: Option<String>,
    /// ISO 639-1 ("da")
    #[serde(default)]
    pub language: Option<String>,
    /// TMDB genre names ("Science Fiction", "Sci-Fi & Fantasy")
    #[serde(default)]
    pub genres: Vec<String>,
    #[serde(default)]
    pub year: Option<i64>,
    #[serde(default)]
    pub rating: Option<f64>,
    #[serde(default)]
    pub votes: i64,
    /// movies: the collection ("Top Gun Collection")
    #[serde(default)]
    pub collection: Option<String>,
    /// shows: networks ("Apple TV+", "HBO")
    #[serde(default)]
    pub networks: Vec<String>,
    /// production / origin countries (ISO 3166-1)
    #[serde(default)]
    pub countries: Vec<String>,
    /// image paths ("/abc.jpg"), see `IMAGES`
    #[serde(default)]
    pub poster: Option<String>,
    #[serde(default)]
    pub backdrop: Option<String>,
}

fn s(v: &Value) -> Option<String> {
    v.as_str().map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned)
}

fn names(v: &Value) -> Vec<String> {
    v.as_array().map(|a| a.iter().filter_map(|x| s(&x["name"])).collect()).unwrap_or_default()
}

fn year_of(v: &Value) -> Option<i64> {
    s(v).and_then(|d| d.get(..4).and_then(|y| y.parse().ok()))
}

impl Info {
    /// From `GET /movie/{id}` (kind "movie") or `GET /tv/{id}` ("tv").
    pub fn from_details(kind: &str, v: &Value) -> Info {
        let tv = kind == "tv";
        // both kinds have `origin_country` (ISO 3166-1 codes); older movie
        // records only `production_countries`
        let mut countries: Vec<String> =
            v["origin_country"].as_array().map(|a| a.iter().filter_map(s).collect()).unwrap_or_default();
        if countries.is_empty() {
            countries = v["production_countries"]
                .as_array()
                .map(|a| a.iter().filter_map(|c| s(&c["iso_3166_1"])).collect())
                .unwrap_or_default();
        }
        Info {
            title: s(&v[if tv { "name" } else { "title" }]).unwrap_or_default(),
            original_title: s(&v[if tv { "original_name" } else { "original_title" }]),
            language: s(&v["original_language"]),
            genres: names(&v["genres"]),
            year: year_of(&v[if tv { "first_air_date" } else { "release_date" }]),
            rating: v["vote_average"].as_f64().filter(|r| *r > 0.0),
            votes: v["vote_count"].as_i64().unwrap_or(0),
            collection: s(&v["belongs_to_collection"]["name"]),
            networks: names(&v["networks"]),
            countries,
            poster: s(&v["poster_path"]),
            backdrop: s(&v["backdrop_path"]),
        }
    }
}

// ------------------------------------------------------------------ client

/// TMDB's two title types (our "series" are "tv").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Movie,
    Tv,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Movie => "movie",
            Kind::Tv => "tv",
        }
    }
}

enum Auth {
    /// v4 read access token (a JWT): `Authorization: Bearer …`
    Bearer(String),
    /// v3 API key: `?api_key=…` (never logged: errors drop the URL)
    ApiKey(String),
}

fn auth(key: &str) -> Auth {
    let key = key.trim();
    if key.starts_with("eyJ") && key.matches('.').count() == 2 {
        Auth::Bearer(key.to_owned())
    } else {
        Auth::ApiKey(key.to_owned())
    }
}

enum Fetched {
    Found(Box<Info>),
    /// not on TMDB (404): remembered, not asked again for `MAX_AGE`
    Missing,
}

struct Client {
    http: reqwest::Client,
    auth: Auth,
}

impl Client {
    fn new(key: &str) -> Self {
        Client { http: http_client(None), auth: auth(key) }
    }

    async fn get(&self, path: &str) -> Result<reqwest::Response> {
        let mut query = vec![("language", "en-US")];
        if let Auth::ApiKey(k) = &self.auth {
            query.push(("api_key", k.as_str()));
        }
        let url = reqwest::Url::parse_with_params(&format!("{API}{path}"), &query).map_err(|e| Error::msg(e.to_string()))?;
        let mut req = self.http.get(url).timeout(Duration::from_secs(20));
        if let Auth::Bearer(t) = &self.auth {
            req = req.bearer_auth(t);
        }
        // reqwest errors carry the URL (and with it an API key): drop it
        req.send().await.map_err(|e| Error::from(e.without_url()))
    }

    /// Details of one title; waits out rate limiting (429).
    async fn details(&self, kind: Kind, id: &str) -> Result<Fetched> {
        for attempt in 0..4 {
            let resp = self.get(&format!("/{}/{id}", kind.as_str())).await?;
            match resp.status().as_u16() {
                200 => {
                    let v: Value = resp.json().await.map_err(|e| Error::from(e.without_url()))?;
                    return Ok(Fetched::Found(Box::new(Info::from_details(kind.as_str(), &v))));
                }
                404 => return Ok(Fetched::Missing),
                401 | 403 => return Err(Error::msg(REJECTED)),
                429 => {
                    let wait = resp
                        .headers()
                        .get("retry-after")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|v| v.parse::<u64>().ok())
                        .unwrap_or(2 << attempt);
                    tokio::time::sleep(Duration::from_secs(wait.min(30))).await;
                }
                code => return Err(Error::Network(format!("TMDB answered {code}"))),
            }
        }
        Err(Error::Network("TMDB kept rate-limiting".into()))
    }
}

const REJECTED: &str = "TMDB did not accept the API key";

/// Checks a key with one request (`GET /3/authentication`, "validate key").
pub async fn check_key(key: &str) -> Result<()> {
    let resp = Client::new(key).get("/authentication").await?;
    match resp.status().as_u16() {
        200 => Ok(()),
        401 | 403 => Err(Error::msg(REJECTED)),
        code => Err(Error::Network(format!("TMDB answered {code}"))),
    }
}

/// The configured API key, if any.
pub fn key(conn: &Connection) -> Option<String> {
    let k = crate::settings::get_str(conn, KEY_SETTING).trim().to_owned();
    (!k.is_empty()).then_some(k)
}

// ------------------------------------------------------------------ status

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub configured: bool,
    pub running: bool,
    /// titles checked in the current run, of `total`
    pub done: i64,
    pub total: i64,
    /// catalog titles (TMDB ids) with details stored
    pub known: i64,
    /// catalog titles with a TMDB id
    pub titles: i64,
    pub error: Option<String>,
    pub last_run: Option<i64>,
}

#[derive(Default)]
struct Progress {
    running: bool,
    done: i64,
    total: i64,
    error: Option<String>,
    last_run: Option<i64>,
}

static PROGRESS: LazyLock<Mutex<Progress>> = LazyLock::new(Default::default);
static RUNNING: AtomicBool = AtomicBool::new(false);

pub fn status(conn: &Connection) -> Result<Status> {
    let (titles, known): (i64, i64) = conn.query_row(
        "SELECT COUNT(*), COUNT(t.id) FROM work w
           LEFT JOIN tmdb t ON t.kind = CASE w.kind WHEN 'movie' THEN 'movie' ELSE 'tv' END
                           AND t.id = substr(w.key, 6) AND t.json IS NOT NULL
          WHERE w.key LIKE 'tmdb:%'",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let p = PROGRESS.lock();
    Ok(Status {
        configured: key(conn).is_some(),
        running: p.running,
        done: p.done,
        total: p.total,
        known,
        titles,
        error: p.error.clone(),
        last_run: p.last_run,
    })
}

fn emit<R: Runtime>(app: &AppHandle<R>, st: &AppState) {
    if let Ok(s) = status(&st.db.read()) {
        let _ = app.emit(EVENT, s);
    }
}

// --------------------------------------------------------------------- job

/// Titles to fetch: never fetched or older than `MAX_AGE`, newest first.
fn todo(conn: &Connection) -> Result<Vec<(Kind, String)>> {
    let rows = conn
        .prepare(
            "SELECT CASE w.kind WHEN 'movie' THEN 'movie' ELSE 'tv' END, substr(w.key, 6)
               FROM work w
               LEFT JOIN tmdb t ON t.kind = CASE w.kind WHEN 'movie' THEN 'movie' ELSE 'tv' END AND t.id = substr(w.key, 6)
              WHERE w.key LIKE 'tmdb:%' AND (t.id IS NULL OR t.fetched_at < ?1)
              ORDER BY w.added IS NULL, w.added DESC",
        )?
        .query_map([now() - MAX_AGE], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows
        .into_iter()
        // ids are numbers; anything else is provider noise
        .filter(|(_, id)| !id.is_empty() && id.len() < 12 && id.bytes().all(|b| b.is_ascii_digit()) && id != "0")
        .map(|(k, id)| (if k == "movie" { Kind::Movie } else { Kind::Tv }, id))
        .collect())
}

fn store(conn: &mut Connection, rows: &[(Kind, String, Option<Info>)]) -> Result<()> {
    let tx = conn.transaction()?;
    {
        let mut stmt = tx.prepare_cached("INSERT OR REPLACE INTO tmdb (kind, id, json, fetched_at) VALUES (?1, ?2, ?3, ?4)")?;
        let t = now();
        for (kind, id, info) in rows {
            let json = info.as_ref().map(serde_json::to_string).transpose()?;
            stmt.execute(params![kind.as_str(), id, json, t])?;
        }
    }
    tx.commit()?;
    Ok(())
}

fn regroup(st: &AppState) -> Result<()> {
    let mut conn = st.db.write();
    let tx = conn.transaction()?;
    crate::works::rebuild(&tx)?;
    tx.commit()?;
    Ok(())
}

/// Fetches missing TMDB details in the background (one run at a time;
/// no-op without a key).
pub fn spawn<R: Runtime>(app: AppHandle<R>, st: AppState) {
    if key(&st.db.read()).is_none() || RUNNING.swap(true, Ordering::SeqCst) {
        return;
    }
    {
        let mut p = PROGRESS.lock();
        *p = Progress { running: true, last_run: p.last_run, ..Default::default() };
    }
    tauri::async_runtime::spawn(async move {
        let result = run(&app, &st).await;
        {
            let mut p = PROGRESS.lock();
            p.running = false;
            p.last_run = Some(now());
            p.error = result.as_ref().err().map(|e| e.to_string());
        }
        RUNNING.store(false, Ordering::SeqCst);
        match &result {
            Ok(n) => log::info!("tmdb: {n} titles updated"),
            Err(e) => log::warn!("tmdb: {e}"),
        }
        emit(&app, &st);
    });
}

async fn run<R: Runtime>(app: &AppHandle<R>, st: &AppState) -> Result<usize> {
    let (api_key, todo) = {
        let conn = st.db.read();
        (key(&conn), todo(&conn)?)
    };
    let Some(api_key) = api_key else { return Ok(0) };
    if todo.is_empty() {
        return Ok(0);
    }
    log::info!("tmdb: fetching {} titles", todo.len());
    PROGRESS.lock().total = todo.len() as i64;
    emit(app, st);

    let client = std::sync::Arc::new(Client::new(&api_key));
    let pace = std::sync::Arc::new(tokio::sync::Mutex::new({
        let mut i = tokio::time::interval(Duration::from_millis(1000 / RATE));
        i.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        i
    }));
    let mut results = futures_util::stream::iter(todo)
        .map(move |(kind, id): (Kind, String)| {
            let (client, pace) = (client.clone(), pace.clone());
            async move {
                pace.lock().await.tick().await;
                let r = client.details(kind, &id).await;
                (kind, id, r)
            }
        })
        .buffer_unordered(CONCURRENCY);

    let mut batch: Vec<(Kind, String, Option<Info>)> = Vec::new();
    let (mut stored, mut since_regroup, mut failures) = (0usize, 0usize, 0usize);
    while let Some((kind, id, r)) = results.next().await {
        PROGRESS.lock().done += 1;
        match r {
            Ok(Fetched::Found(info)) => batch.push((kind, id, Some(*info))),
            Ok(Fetched::Missing) => batch.push((kind, id, None)),
            Err(e) if e.to_string() == REJECTED => return Err(e),
            Err(e) => {
                failures += 1;
                log::debug!("tmdb {}/{id}: {e}", kind.as_str());
                // offline or TMDB down: stop, the next run continues
                if failures >= 25 && failures * 2 > stored + batch.len() {
                    return Err(Error::msg(format!("TMDB is not reachable ({e})")));
                }
            }
        }
        if batch.len() >= 100 {
            stored += batch.len();
            since_regroup += batch.len();
            store(&mut st.db.write(), &batch)?;
            batch.clear();
            // the key was removed meanwhile (Settings)
            if key(&st.db.read()).is_none() {
                regroup(st)?;
                return Ok(stored);
            }
            if since_regroup >= REGROUP_EVERY {
                since_regroup = 0;
                regroup(st)?;
            }
            emit(app, st);
        }
    }
    stored += batch.len();
    store(&mut st.db.write(), &batch)?;
    regroup(st)?;
    Ok(stored)
}

// ---------------------------------------------------------------- commands

#[tauri::command]
pub async fn tmdb_status(state: State<'_, AppState>) -> Result<Status> {
    status(&state.db.read())
}

/// Saves (after checking it with TMDB) or, with an empty key, removes the
/// API key; a saved key starts fetching.
#[tauri::command]
pub async fn tmdb_set_key<R: Runtime>(app: AppHandle<R>, state: State<'_, AppState>, key: String) -> Result<Status> {
    let key = key.trim().to_owned();
    if !key.is_empty() {
        check_key(&key).await?;
    }
    {
        let conn = state.db.write();
        if key.is_empty() {
            conn.execute("DELETE FROM setting WHERE key = ?1", [KEY_SETTING])?;
        } else {
            conn.execute(
                "INSERT INTO setting (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![KEY_SETTING, serde_json::to_string(&key)?],
            )?;
        }
    }
    PROGRESS.lock().error = None;
    spawn(app, state.inner().clone());
    status(&state.db.read())
}

#[tauri::command]
pub async fn tmdb_refresh<R: Runtime>(app: AppHandle<R>, state: State<'_, AppState>) -> Result<Status> {
    spawn(app, state.inner().clone());
    status(&state.db.read())
}

/// What the detail pages show from TMDB.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Facts {
    /// "Danish"
    pub original_language: Option<String>,
    /// movies: "Top Gun Collection"
    pub collection: Option<String>,
    /// shows: "Apple TV+"
    pub networks: Vec<String>,
    /// "Denmark", "United States"
    pub countries: Vec<String>,
}

/// TMDB facts of a work, when its details are stored and fit its title.
pub fn facts_for(conn: &Connection, kind: &str, key: &str, title: &str, year: Option<i64>) -> Result<Option<Facts>> {
    Ok(info_for(conn, kind, key)?.filter(|i| crate::works::tmdb_fits(i, &[title], year)).map(|i| Facts {
        original_language: i.language.as_deref().and_then(crate::works::lang::name).map(str::to_owned),
        collection: i.collection,
        networks: i.networks,
        countries: i.countries.iter().map(|c| crate::works::genre::country_name(c)).collect(),
    }))
}

/// TMDB details stored for a work key ("tmdb:603"), if any.
pub fn info_for(conn: &Connection, kind: &str, key: &str) -> Result<Option<Info>> {
    let Some(id) = key.strip_prefix("tmdb:") else { return Ok(None) };
    let kind = if kind == "movie" { "movie" } else { "tv" };
    Ok(conn
        .query_row("SELECT json FROM tmdb WHERE kind = ?1 AND id = ?2", params![kind, id], |r| r.get::<_, Option<String>>(0))
        .optional()?
        .flatten()
        .and_then(|j| serde_json::from_str(&j).ok()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_movie_and_show_details() {
        let movie = serde_json::json!({
            "title": "Top Gun: Maverick", "original_title": "Top Gun: Maverick", "original_language": "en",
            "genres": [{"id": 28, "name": "Action"}, {"id": 18, "name": "Drama"}], "release_date": "2022-05-21",
            "vote_average": 8.2, "vote_count": 9000, "belongs_to_collection": {"id": 1, "name": "Top Gun Collection"},
            "production_countries": [{"iso_3166_1": "US", "name": "United States of America"}],
            "poster_path": "/p.jpg", "backdrop_path": "/b.jpg"
        });
        let m = Info::from_details("movie", &movie);
        assert_eq!((m.year, m.collection.as_deref(), m.genres.len()), (Some(2022), Some("Top Gun Collection"), 2));
        assert_eq!(m.countries, vec!["US"]);
        let tv = serde_json::json!({
            "name": "Kastanjemanden", "original_name": "Kastanjemanden", "original_language": "da",
            "genres": [{"name": "Crime"}, {"name": "Mystery"}], "first_air_date": "2021-09-29",
            "networks": [{"name": "Netflix"}], "origin_country": ["DK"], "vote_average": 0
        });
        let t = Info::from_details("tv", &tv);
        assert_eq!((t.title.as_str(), t.language.as_deref(), t.rating), ("Kastanjemanden", Some("da"), None));
        assert_eq!(t.networks, vec!["Netflix"]);
        // stored form round-trips; older rows without new fields still load
        let back: Info = serde_json::from_str(&serde_json::to_string(&t).unwrap()).unwrap();
        assert_eq!(back, t);
        let old: Info = serde_json::from_str(r#"{"title":"X"}"#).unwrap();
        assert!(old.genres.is_empty());
    }

    #[test]
    fn tokens_and_keys() {
        assert!(matches!(auth("eyJhbGciOiJIUzI1NiJ9.eyJhdWQiOiJ4In0.sig"), Auth::Bearer(_)));
        assert!(matches!(auth(" 0123456789abcdef0123456789abcdef "), Auth::ApiKey(_)));
    }

    #[test]
    fn fetches_newest_titles_with_numeric_ids_only() {
        let c = crate::db::test_conn();
        for (id, tmdb, added) in [("a", "11", 1), ("b", "22", 3), ("c", "tt123", 2)] {
            c.execute(
                "INSERT INTO movie (source_id, id, name, title, year, tmdb, added, position) VALUES (1, ?1, ?1, ?1, 2000, ?2, ?3, 0)",
                params![id, tmdb, added],
            )
            .unwrap();
        }
        crate::works::rebuild(&c).unwrap();
        assert_eq!(todo(&c).unwrap(), vec![(Kind::Movie, "22".to_owned()), (Kind::Movie, "11".to_owned())]);
        c.execute("INSERT INTO tmdb (kind, id, json, fetched_at) VALUES ('movie', '22', NULL, ?1)", [now()]).unwrap();
        assert_eq!(todo(&c).unwrap(), vec![(Kind::Movie, "11".to_owned())]);
    }
}
