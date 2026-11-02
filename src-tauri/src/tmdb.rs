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
    v.as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

fn names(v: &Value) -> Vec<String> {
    v.as_array()
        .map(|a| a.iter().filter_map(|x| s(&x["name"])).collect())
        .unwrap_or_default()
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
        let mut countries: Vec<String> = v["origin_country"]
            .as_array()
            .map(|a| a.iter().filter_map(s).collect())
            .unwrap_or_default();
        if countries.is_empty() {
            countries = v["production_countries"]
                .as_array()
                .map(|a| a.iter().filter_map(|c| s(&c["iso_3166_1"])).collect())
                .unwrap_or_default();
        }
        Info {
            title: s(&v[if tv { "name" } else { "title" }]).unwrap_or_default(),
            original_title: s(&v[if tv {
                "original_name"
            } else {
                "original_title"
            }]),
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
        Client {
            http: http_client(None),
            auth: auth(key),
        }
    }

    async fn get(&self, path: &str) -> Result<reqwest::Response> {
        self.get_with(path, &[]).await
    }

    /// GET `path` with extra query pairs. The pairs are URL-encoded, so a
    /// search `query` may carry any provider text (spaces, '&', accents).
    async fn get_with(&self, path: &str, extra: &[(&str, &str)]) -> Result<reqwest::Response> {
        let mut query: Vec<(&str, &str)> = vec![("language", "en-US")];
        query.extend_from_slice(extra);
        if let Auth::ApiKey(k) = &self.auth {
            query.push(("api_key", k.as_str()));
        }
        let url = reqwest::Url::parse_with_params(&format!("{API}{path}"), &query)
            .map_err(|e| Error::msg(e.to_string()))?;
        let mut req = self.http.get(url).timeout(Duration::from_secs(20));
        if let Auth::Bearer(t) = &self.auth {
            req = req.bearer_auth(t);
        }
        // reqwest errors carry the URL (and with it an API key): drop it
        req.send().await.map_err(|e| Error::from(e.without_url()))
    }

    /// One JSON body with rate limiting (429) and auth (401/403) handled;
    /// 404 comes back as Value::Null (no error).
    async fn json(&self, path: &str, extra: &[(&str, &str)]) -> Result<Value> {
        for attempt in 0..4 {
            let resp = self.get_with(path, extra).await?;
            match resp.status().as_u16() {
                200 => return resp.json().await.map_err(|e| Error::from(e.without_url())),
                401 | 403 => return Err(Error::msg(REJECTED)),
                404 => return Ok(Value::Null),
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

    /// Details of one title; 404 comes back as Fetched::Missing, and the
    /// shared 429/401/403 handling lives in json().
    async fn details(&self, kind: Kind, id: &str) -> Result<Fetched> {
        let v = self.json(&format!("/{}/{id}", kind.as_str()), &[]).await?;
        Ok(if v.is_null() {
            Fetched::Missing
        } else {
            Fetched::Found(Box::new(Info::from_details(kind.as_str(), &v)))
        })
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

/// The configured API key, if any — from the system keyring (secrets.rs),
/// or the database where there is none. None while the keyring is locked.
pub fn key(conn: &Connection) -> Option<String> {
    crate::secrets::named(conn, KEY_SETTING)
}

/// Shown while the key is in a keyring that is locked or not available.
const KEY_LOCKED: &str = "The TMDB key is stored in the system keyring, which is locked or not available. \
     Unlock it (KWallet, GNOME Keyring) and refresh, or enter the key again.";

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
    /// titles without a TMDB id and without a search match (or ruled out)
    pub unmapped: i64,
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
    // unidentified titles, minus the ones search matching already folded
    // into a TMDB group (their mapped key) or the searches ruled out
    let unmapped: i64 = conn.query_row(
        "SELECT COUNT(*) FROM work w
          WHERE (w.key LIKE 'title:%' OR w.key LIKE 'item:%')
            AND w.kind IN ('movie', 'series')
            AND trim(w.title) != ''
            AND COALESCE(NULLIF((SELECT tmdb_id FROM tmdb_map m
                                  WHERE m.kind = CASE w.kind WHEN 'movie' THEN 'movie' ELSE 'tv' END
                                    AND m.source_key = w.key), ''), 'x') = 'x'",
        [],
        |r| r.get(0),
    )?;
    let configured = crate::secrets::named_configured(conn, KEY_SETTING);
    let locked = configured && key(conn).is_none();
    let p = PROGRESS.lock();
    Ok(Status {
        configured,
        running: p.running,
        done: p.done,
        total: p.total,
        known,
        titles,
        unmapped,
        error: p
            .error
            .clone()
            .or_else(|| locked.then(|| KEY_LOCKED.to_owned())),
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
        .filter(|(_, id)| {
            !id.is_empty() && id.len() < 12 && id.bytes().all(|b| b.is_ascii_digit()) && id != "0"
        })
        .map(|(k, id)| (if k == "movie" { Kind::Movie } else { Kind::Tv }, id))
        .collect())
}

/// Setting with the date a change-list run covered ("YYYY-MM-DD"), so a
/// refresh marks what TMDB changed since then stale instead of refetching
/// everything every `MAX_AGE`.
const CHANGES_SETTING: &str = "tmdb.changes_since";
/// TMDB's change lists reach back at most this far.
const CHANGES_MAX_DAYS: i64 = 14;

/// Ids TMDB reported changed: fetch their details again (fetched_at = 0);
/// only rows present are touched.
pub fn mark_changed(conn: &Connection, kind: Kind, ids: &[String]) -> Result<usize> {
    let mut stmt =
        conn.prepare_cached("UPDATE tmdb SET fetched_at = 0 WHERE kind = ?1 AND id = ?2")?;
    let mut n = 0;
    for id in ids {
        if !id.is_empty() && id.len() < 12 && id.bytes().all(|b| b.is_ascii_digit()) {
            n += stmt.execute(params![kind.as_str(), id])?;
        }
    }
    Ok(n)
}

fn changes_since(conn: &Connection) -> Option<chrono::NaiveDate> {
    conn.query_row(
        "SELECT value FROM setting WHERE key = ?1",
        [CHANGES_SETTING],
        |r| r.get::<_, String>(0),
    )
    .ok()
    .and_then(|v| {
        chrono::NaiveDate::parse_from_str(&v, "%Y-%m-%d")
            .ok()
            .filter(|v| !v.to_string().is_empty())
    })
}

fn store_changes_since(conn: &Connection, d: chrono::NaiveDate) -> Result<()> {
    conn.execute(
        "INSERT INTO setting (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![CHANGES_SETTING, d.format("%Y-%m-%d").to_string()],
    )?;
    Ok(())
}

/// The id list of GET /3/{movie|tv}/changes since `since` (clamped to
/// TMDB's 14-day window): paged, 100 ids a page.
async fn changes(
    client: &Client,
    pace: &std::sync::Arc<tokio::sync::Mutex<tokio::time::Interval>>,
    kind: Kind,
    since: chrono::NaiveDate,
) -> Result<Vec<String>> {
    let window = chrono::Utc::now().date_naive() - chrono::Duration::days(CHANGES_MAX_DAYS);
    let start = since.max(window).format("%Y-%m-%d").to_string();
    let mut ids = Vec::new();
    let mut page = 1;
    loop {
        pace.lock().await.tick().await;
        let v = client
            .json(
                &format!("/{}/changes", kind.as_str()),
                &[("start_date", start.as_str()), ("page", &page.to_string())],
            )
            .await?;
        let Some(results) = v["results"].as_array() else {
            break;
        };
        if results.is_empty() {
            break; // an empty page with a big total_pages would pace out 500 requests
        }
        ids.extend(
            results
                .iter()
                .filter_map(|r| r["id"].as_i64().map(|i| i.to_string())),
        );
        // total_pages is trusted only up to TMDB's 500-page cap
        if page >= v["total_pages"].as_i64().unwrap_or(1).min(500) {
            break;
        }
        page += 1;
    }
    Ok(ids)
}

// ------------------------------------------------------------- identify

/// One unidentified work: its own key, a title line and the year to ask
/// TMDB's search with.
struct Candidate {
    kind: Kind,
    source_key: String,
    title: String,
    year: Option<i64>,
}

/// Works without a TMDB id that search may identify: 'title:*' keys plus
/// year-less 'item:*' entries, not searched again within `MAX_AGE`.
fn search_todo(conn: &Connection) -> Result<Vec<Candidate>> {
    let rows = conn
        .prepare(
            "SELECT CASE w.kind WHEN 'movie' THEN 'movie' ELSE 'tv' END, w.key, w.title, w.year
               FROM work w
               LEFT JOIN tmdb_map m ON m.kind = CASE w.kind WHEN 'movie' THEN 'movie' ELSE 'tv' END
                                   AND m.source_key = w.key
              WHERE (w.key LIKE 'title:%' OR w.key LIKE 'item:%')
                AND w.kind IN ('movie', 'series')
                AND (m.searched_at IS NULL OR m.searched_at < ?1)",
        )?
        .query_map([now() - MAX_AGE], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<i64>>(3)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows
        .into_iter()
        .filter(|(_, _, title, _)| !title.trim().is_empty())
        .map(|(kind, source_key, title, year)| Candidate {
            kind: if kind == "movie" {
                Kind::Movie
            } else {
                Kind::Tv
            },
            source_key,
            title,
            year,
        })
        .collect())
}

/// GET /3/search/{movie|tv}: an exact normalized-title match with a fitting
/// year identifies the entry; several such candidates settle none.
async fn search_id(
    client: &Client,
    kind: Kind,
    title: &str,
    year: Option<i64>,
) -> Result<Option<String>> {
    let year_param;
    let mut extra: Vec<(&str, &str)> = vec![("query", title)];
    if let Some(y) = year {
        year_param = y.to_string();
        extra.push((
            if kind == Kind::Movie {
                "primary_release_year"
            } else {
                "first_air_date_year"
            },
            &year_param,
        ));
    }
    let v = client
        .json(&format!("/search/{}", kind.as_str()), &extra)
        .await?;
    if v.is_null() {
        return Ok(None);
    }
    let tv = kind == Kind::Tv;
    let asked = crate::works::norm_title(title);
    let mut found: Option<String> = None;
    for r in v["results"].as_array().into_iter().flatten() {
        let names: Vec<String> = [
            s(&r[if tv { "name" } else { "title" }]),
            s(&r[if tv {
                "original_name"
            } else {
                "original_title"
            }]),
        ]
        .into_iter()
        .flatten()
        .collect();
        if !names.iter().any(|n| crate::works::norm_title(n) == asked) {
            continue;
        }
        let ryear = year_of(&r[if tv { "first_air_date" } else { "release_date" }]);
        if let (Some(a), Some(b)) = (year, ryear)
            && (a - b).abs() > 1
        {
            continue;
        }
        if let Some(id) = r["id"].as_i64().map(|i| i.to_string()) {
            if found.is_some() {
                return Ok(None); // several exact matches: leave it alone
            }
            found = Some(id);
        }
    }
    Ok(found)
}

/// Searches TMDB for the works the provider did not id; hits land in
/// `tmdb_map` (misses as a `tmdb_id = ''` row remembered for `MAX_AGE`).
/// The next regroup folds them into the TMDB group (`works::rebuild`), and
/// their details arrive on the following refresh (todo() at that point).
async fn identify(
    client: &std::sync::Arc<Client>,
    pace: &std::sync::Arc<tokio::sync::Mutex<tokio::time::Interval>>,
    st: &AppState,
    progress: usize,
) -> Result<usize> {
    let candidates = {
        let conn = st.db.read();
        search_todo(&conn)?
    };
    if candidates.is_empty() {
        return Ok(0);
    }
    log::info!(
        "tmdb: searching for {} unidentified titles",
        candidates.len()
    );
    PROGRESS.lock().total += candidates.len() as i64;
    let mut found = 0;
    let mut failures = 0;
    let mut batch: Vec<(&str, String, String)> = Vec::new();
    for (i, c) in candidates.iter().enumerate() {
        pace.lock().await.tick().await;
        if key(&st.db.read()).is_none() {
            break; // the key was removed meanwhile (Settings)
        }
        PROGRESS.lock().done = (progress + i + 1) as i64;
        let id = match search_id(client, c.kind, &c.title, c.year).await {
            Ok(id) => id,
            Err(e) if e.to_string() == REJECTED => {
                // keep the matches already found: they are real work
                store_map(&mut st.db.write(), &batch)?;
                return Err(e);
            }
            Err(e) => {
                failures += 1;
                log::debug!("tmdb search {:?}: {e}", c.title);
                // offline or TMDB down: stop, the next run continues; search
                // is sequential so an outage would otherwise pace through the
                // whole catalog at one slow request per title
                if failures >= 25 && failures * 2 > found + batch.len() {
                    store_map(&mut st.db.write(), &batch)?;
                    return Err(Error::msg(format!("TMDB search is not reachable ({e})")));
                }
                continue;
            }
        };
        let got = id.unwrap_or_default();
        if !got.is_empty() {
            found += 1;
            log::debug!("tmdb: {} '{}' -> {got}", c.kind.as_str(), c.title);
        }
        batch.push((c.kind.as_str(), c.source_key.clone(), got));
        if batch.len() >= 200 {
            store_map(&mut st.db.write(), &batch)?;
            let done = (progress + i + 1) as i64;
            PROGRESS.lock().done = done;
            batch.clear();
        }
    }
    store_map(&mut st.db.write(), &batch)?;
    Ok(found)
}

fn store_map(conn: &mut Connection, rows: &[(&str, String, String)]) -> Result<()> {
    let tx = conn.transaction()?;
    {
        let mut stmt = tx.prepare_cached("INSERT OR REPLACE INTO tmdb_map (kind, source_key, tmdb_id, searched_at) VALUES (?1, ?2, ?3, ?4)")?;
        let t = now();
        for (kind, key, id) in rows {
            stmt.execute(params![kind, key, id, t])?;
        }
    }
    tx.commit()?;
    Ok(())
}

fn store(conn: &mut Connection, rows: &[(Kind, String, Option<Info>)]) -> Result<()> {
    let tx = conn.transaction()?;
    {
        let mut stmt = tx.prepare_cached(
            "INSERT OR REPLACE INTO tmdb (kind, id, json, fetched_at) VALUES (?1, ?2, ?3, ?4)",
        )?;
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
        *p = Progress {
            running: true,
            last_run: p.last_run,
            ..Default::default()
        };
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
    let api_key = {
        let conn = st.db.read();
        key(&conn)
    };
    let Some(api_key) = api_key else { return Ok(0) };

    let client = std::sync::Arc::new(Client::new(&api_key));
    let pace = std::sync::Arc::new(tokio::sync::Mutex::new({
        let mut i = tokio::time::interval(Duration::from_millis(1000 / RATE));
        i.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        i
    }));

    // 1) what TMDB changed since the last run: mark those ids stale so
    // todo() refetches them (instead of everything every MAX_AGE). Before
    // the todo() query — with an up-to-date catalog, this phase alone may
    // make work. The first run ever has no last date: nothing to mark.
    {
        let since = changes_since(&st.db.read());
        let mut covered = since.is_none(); // no prior date: nothing to cover
        if let Some(since) = since {
            let mut marked = 0;
            let mut fetched = true;
            for kind in [Kind::Movie, Kind::Tv] {
                match changes(&client, &pace, kind, since).await {
                    Ok(ids) => marked += mark_changed(&st.db.write(), kind, &ids)?,
                    // TMDB down: keep going, the MAX_AGE fallback still works
                    Err(e) => {
                        fetched = false;
                        log::debug!("tmdb {}/changes: {e}", kind.as_str());
                    }
                }
            }
            log::info!("tmdb: {marked} changed titles marked for refetch");
            covered = fetched;
        }
        // Only advance the window when the changes phase actually covered
        // it — otherwise the failed span would never be marked for refetch.
        if covered {
            store_changes_since(&st.db.write(), chrono::Utc::now().date_naive())?;
        }
    }

    let todo = {
        let conn = st.db.read();
        todo(&conn)?
    };
    if todo.is_empty() {
        // still: titles without a TMDB id may be searchable
        let found = identify(&client, &pace, st, 0).await?;
        if found > 0 {
            regroup(st)?;
        }
        return Ok(found);
    }
    log::info!("tmdb: fetching {} titles", todo.len());
    PROGRESS.lock().total = todo.len() as i64;
    emit(app, st);

    let mut results = futures_util::stream::iter(todo)
        .map({
            let (client, pace) = (client.clone(), pace.clone());
            move |(kind, id): (Kind, String)| {
                let (client, pace) = (client.clone(), pace.clone());
                async move {
                    pace.lock().await.tick().await;
                    let r = client.details(kind, &id).await;
                    (kind, id, r)
                }
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

    // 2) unidentified titles: search TMDB, remember hits in tmdb_map, then
    // the regroup below folds them into the TMDB group (detail rows for
    // the newly mapped ids are this run's remaining `todo` of the next).
    let found = identify(&client, &pace, st, stored).await?;
    regroup(st)?;
    Ok(stored + found)
}

// ---------------------------------------------------------------- commands

#[tauri::command]
pub async fn tmdb_status(state: State<'_, AppState>) -> Result<Status> {
    status(&state.db.read())
}

/// Saves (after checking it with TMDB) or, with an empty key, removes the
/// API key; a saved key starts fetching.
#[tauri::command]
pub async fn tmdb_set_key<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
    key: String,
) -> Result<Status> {
    let key = key.trim().to_owned();
    if !key.is_empty() {
        check_key(&key).await?;
    }
    // the system keyring when there is one (it may ask to be unlocked: the
    // user just typed the key), else the database
    if key.is_empty() {
        crate::secrets::forget_named(state.inner(), KEY_SETTING)?;
    } else {
        crate::secrets::store_named(
            state.inner(),
            KEY_SETTING,
            &key,
            crate::secrets::Unlock::Prompt,
        )
        .await?;
    }
    PROGRESS.lock().error = None;
    spawn(app, state.inner().clone());
    status(&state.db.read())
}

#[tauri::command]
pub async fn tmdb_refresh<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
) -> Result<Status> {
    // the user asked: a keyring locked so far may prompt now
    crate::secrets::ensure_loaded(state.inner(), None, crate::secrets::Unlock::Prompt).await;
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
pub fn facts_for(
    conn: &Connection,
    kind: &str,
    key: &str,
    title: &str,
    year: Option<i64>,
) -> Result<Option<Facts>> {
    Ok(info_for(conn, kind, key)?
        .filter(|i| crate::works::tmdb_fits(i, &[title], year))
        .map(|i| Facts {
            original_language: i
                .language
                .as_deref()
                .and_then(crate::works::lang::name)
                .map(str::to_owned),
            collection: i.collection,
            networks: i.networks,
            countries: i
                .countries
                .iter()
                .map(|c| crate::works::genre::country_name(c))
                .collect(),
        }))
}

/// TMDB details stored for a work key ("tmdb:603"), if any.
pub fn info_for(conn: &Connection, kind: &str, key: &str) -> Result<Option<Info>> {
    let Some(id) = key.strip_prefix("tmdb:") else {
        return Ok(None);
    };
    let kind = crate::works::tmdb_kind(kind);
    Ok(conn
        .query_row(
            "SELECT json FROM tmdb WHERE kind = ?1 AND id = ?2",
            params![kind, id],
            |r| r.get::<_, Option<String>>(0),
        )
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
        assert_eq!(
            (m.year, m.collection.as_deref(), m.genres.len()),
            (Some(2022), Some("Top Gun Collection"), 2)
        );
        assert_eq!(m.countries, vec!["US"]);
        let tv = serde_json::json!({
            "name": "Kastanjemanden", "original_name": "Kastanjemanden", "original_language": "da",
            "genres": [{"name": "Crime"}, {"name": "Mystery"}], "first_air_date": "2021-09-29",
            "networks": [{"name": "Netflix"}], "origin_country": ["DK"], "vote_average": 0
        });
        let t = Info::from_details("tv", &tv);
        assert_eq!(
            (t.title.as_str(), t.language.as_deref(), t.rating),
            ("Kastanjemanden", Some("da"), None)
        );
        assert_eq!(t.networks, vec!["Netflix"]);
        // stored form round-trips; older rows without new fields still load
        let back: Info = serde_json::from_str(&serde_json::to_string(&t).unwrap()).unwrap();
        assert_eq!(back, t);
        let old: Info = serde_json::from_str(r#"{"title":"X"}"#).unwrap();
        assert!(old.genres.is_empty());
    }

    #[test]
    fn a_key_in_a_locked_keyring_is_reported() {
        let c = crate::db::test_conn();
        assert!(!status(&c).unwrap().configured);
        c.execute(
            "INSERT INTO setting (key, value) VALUES ('tmdb.key.inKeyring', 'true')",
            [],
        )
        .unwrap();
        let s = status(&c).unwrap();
        assert!(s.configured);
        assert_eq!(s.error.as_deref(), Some(KEY_LOCKED));
        assert_eq!(key(&c), None);
    }

    #[test]
    fn tokens_and_keys() {
        assert!(matches!(
            auth("eyJhbGciOiJIUzI1NiJ9.eyJhdWQiOiJ4In0.sig"),
            Auth::Bearer(_)
        ));
        assert!(matches!(
            auth(" 0123456789abcdef0123456789abcdef "),
            Auth::ApiKey(_)
        ));
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
        assert_eq!(
            todo(&c).unwrap(),
            vec![
                (Kind::Movie, "22".to_owned()),
                (Kind::Movie, "11".to_owned())
            ]
        );
        c.execute(
            "INSERT INTO tmdb (kind, id, json, fetched_at) VALUES ('movie', '22', NULL, ?1)",
            [now()],
        )
        .unwrap();
        assert_eq!(todo(&c).unwrap(), vec![(Kind::Movie, "11".to_owned())]);
    }

    /// GET /3/movie/changes body: `{results:[{id, adult}], total_pages}`.
    #[test]
    fn changes_response_parses() {
        let v = serde_json::json!({
            "results": [{"id": 603, "adult": false}, {"id": 604, "adult": true}],
            "page": 1, "total_pages": 3, "total_results": 250
        });
        let ids: Vec<String> = v["results"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|r| r["id"].as_i64().map(|i| i.to_string()))
            .collect();
        assert_eq!(ids, ["603", "604"]);
        assert_eq!(v["total_pages"].as_i64(), Some(3));
    }

    /// mark_changed flips present rows to fetched_at = 0 so todo() picks
    /// them up again; unknown ids (no row) are ignored, non-numeric too.
    #[test]
    fn mark_changed_zeroes_fetched_at() {
        let c = crate::db::test_conn();
        c.execute(
            "INSERT INTO movie (source_id, id, name, title, year, tmdb, added, position) VALUES (1, 'a', 'a', 'a', 2000, '11', 1, 0)",
            [],
        )
        .unwrap();
        crate::works::rebuild(&c).unwrap();
        c.execute(
            "INSERT INTO tmdb (kind, id, json, fetched_at) VALUES ('movie', '11', NULL, ?1)",
            [now()],
        )
        .unwrap();
        assert!(todo(&c).unwrap().is_empty());
        let n = mark_changed(&c, Kind::Movie, &["11".into(), "999".into(), "tt13".into()]).unwrap();
        assert_eq!(n, 1);
        assert_eq!(todo(&c).unwrap(), vec![(Kind::Movie, "11".to_owned())]);
    }

    /// tmdb_map rows upsert; the '' sentinel counts as "searched, no match".
    #[test]
    fn tmdb_map_roundtrip() {
        let c = crate::db::test_conn();
        let put = |key: &str, id: &str| {
            c.execute("INSERT OR REPLACE INTO tmdb_map (kind, source_key, tmdb_id, searched_at) VALUES ('movie', ?1, ?2, 1)", params![key, id])
                .unwrap();
        };
        put("title:jungle cruise|2021", "522931");
        put("item:1:xyz", "");
        assert_eq!(
            crate::works::tmdb_id_of(&c, "title:jungle cruise|2021")
                .unwrap()
                .as_deref(),
            Some("522931")
        );
        assert_eq!(crate::works::tmdb_id_of(&c, "item:1:xyz").unwrap(), None);
        assert_eq!(crate::works::tmdb_id_of(&c, "item:1:never").unwrap(), None);
        assert_eq!(crate::works::load_tmdb_map(&c).unwrap().len(), 1);
        // searched_at newer than MAX_AGE keeps the item out of search_todo:
        // m1 has a year so its natural key is 'title:jungle cruise|2021',
        // which the map re-keys to tmdb:522931 at rebuild — but only once
        // TMDB's own (fitting) metadata for 522931 is stored.
        c.execute(
            "INSERT INTO movie (source_id, id, name, title, year, position) VALUES (1, 'm1', 'x', 'Jungle Cruise', 2021, 0)",
            [],
        )
        .unwrap();
        crate::works::rebuild(&c).unwrap();
        assert_eq!(
            c.query_row("SELECT key FROM work WHERE kind = 'movie'", [], |r| r
                .get::<_, String>(0))
                .unwrap(),
            "title:jungle cruise|2021"
        );
        c.execute(
            "INSERT INTO tmdb (kind, id, json, fetched_at) VALUES ('movie', '522931',
                 '{\"title\":\"Jungle Cruise\",\"poster\":\"/p.jpg\"}', 0)",
            [],
        )
        .unwrap();
        crate::works::rebuild(&c).unwrap();
        assert_eq!(
            c.query_row("SELECT key FROM work WHERE kind = 'movie'", [], |r| r
                .get::<_, String>(0))
                .unwrap(),
            "tmdb:522931"
        );
        assert!(search_todo(&c).unwrap().is_empty());
    }

    /// search_todo picks title:* and item:* works not searched lately,
    /// skips empty titles, and skips fresh '' misses.
    #[test]
    fn search_todo_covers_titles_and_items() {
        let c = crate::db::test_conn();
        for (id, title, year) in [
            ("with-year", "A Film", Some(2001i64)),
            ("no-year", "Another Film", None),
            ("empty", " ", Some(2020)),
        ] {
            c.execute("INSERT INTO movie (source_id, id, name, title, year, position) VALUES (1, ?1, ?1, ?2, ?3, 0)", params![id, title, year])
                .unwrap();
        }
        crate::works::rebuild(&c).unwrap();
        let mut keys: Vec<String> = search_todo(&c)
            .unwrap()
            .into_iter()
            .map(|c| c.source_key)
            .collect();
        keys.sort();
        assert_eq!(
            keys,
            [
                "item:1:no-year".to_string(),
                "title:a film|2001".to_string()
            ]
        );
        // a fresh miss keeps the title out
        c.execute(
            "INSERT INTO tmdb_map (kind, source_key, tmdb_id, searched_at) VALUES ('movie', 'title:a film|2001', '', ?1)",
            [now()],
        )
        .unwrap();
        let keys: Vec<String> = search_todo(&c)
            .unwrap()
            .into_iter()
            .map(|c| c.source_key)
            .collect();
        assert_eq!(keys, ["item:1:no-year".to_string()]);
        // unmapped count matches: both still without an accepted match
        assert_eq!(status(&c).unwrap().unmapped, 2);
    }

    /// Status reports the still-unmatched works: mapped and miss-sentinel
    /// keys drop out of `unmapped`.
    #[test]
    fn status_counts_unmapped() {
        let c = crate::db::test_conn();
        for (id, title, year) in [
            ("a", "Film A", Some(2001i64)),
            ("b", "Film B", Some(2002)),
            ("c", "Film C", None),
        ] {
            c.execute("INSERT INTO movie (source_id, id, name, title, year, position) VALUES (1, ?1, ?1, ?2, ?3, 0)", params![id, title, year])
                .unwrap();
        }
        crate::works::rebuild(&c).unwrap();
        assert_eq!(status(&c).unwrap().unmapped, 3);
        c.execute(
            "INSERT INTO tmdb_map (kind, source_key, tmdb_id, searched_at) VALUES ('movie', 'title:film a|2001', '10', 1)",
            [],
        )
        .unwrap();
        c.execute("INSERT INTO tmdb_map (kind, source_key, tmdb_id, searched_at) VALUES ('movie', 'item:1:c', '', 1)", []).unwrap();
        crate::works::rebuild(&c).unwrap();
        // Film A joined a TMDB group; B (never searched) and C (searched,
        // no match) are both still without a TMDB id
        assert_eq!(status(&c).unwrap().unmapped, 2);
    }
}
