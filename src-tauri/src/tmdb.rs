//! Optional metadata from TMDB (themoviedb.org) for the catalog's titles:
//! genres (the provider's movie list has none), original language,
//! collections, TV networks, ratings and missing artwork. Fetched in the
//! background with the user's own API key and folded into the works by
//! `works::rebuild`. Each run:
//!
//! 1. marks stored details that TMDB changed after they were fetched for
//!    fetching again (`/movie/changes`, `/tv/changes`, day by day since the
//!    last run);
//! 2. searches TMDB once for works the provider lists without an id
//!    (`tmdb_match`; a single result with exactly their title counts);
//! 3. fetches the details of every work with an id that has none or stale
//!    ones.
//!
//! "This product uses the TMDB API but is not endorsed or certified by TMDB."

use std::collections::HashSet;
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
/// Details and searches are redone after this long at the latest; the change
/// lists keep details current in between.
const MAX_AGE: i64 = 180 * 86_400;
/// Requests per second; TMDB tolerates about 40.
const RATE: u64 = 25;
const CONCURRENCY: usize = 8;
/// Regroup the catalog after this many new details (and at the end).
const REGROUP_EVERY: usize = 4000;
/// TMDB's change lists span at most this many days (inclusive dates).
const CHANGES_DAYS: i64 = 14;
/// Setting: time of the last complete change-list scan (unix seconds).
const CHANGES_SETTING: &str = "tmdb.changesAt";
/// TMDB serves at most this many pages of a list.
const MAX_PAGES: i64 = 500;

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

    async fn get(&self, path: &str, params: &[(&str, &str)]) -> Result<reqwest::Response> {
        let mut query = vec![("language", "en-US")];
        query.extend_from_slice(params);
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

    /// A JSON answer, `None` when TMDB doesn't know the resource (404); waits
    /// out rate limiting (429).
    async fn json(&self, path: &str, params: &[(&str, &str)]) -> Result<Option<Value>> {
        for attempt in 0..4 {
            let resp = self.get(path, params).await?;
            match resp.status().as_u16() {
                200 => return Ok(Some(resp.json().await.map_err(|e| Error::from(e.without_url()))?)),
                404 => return Ok(None),
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

    /// Details of one title.
    async fn details(&self, kind: Kind, id: &str) -> Result<Fetched> {
        Ok(match self.json(&format!("/{}/{id}", kind.as_str()), &[]).await? {
            Some(v) => Fetched::Found(Box::new(Info::from_details(kind.as_str(), &v))),
            None => Fetched::Missing,
        })
    }

    /// One page of the ids TMDB changed between two days (inclusive,
    /// "YYYY-MM-DD"), and the number of pages.
    async fn changes(&self, kind: Kind, from: &str, to: &str, page: i64) -> Result<(Vec<String>, i64)> {
        let page = page.to_string();
        let v = self
            .json(&format!("/{}/changes", kind.as_str()), &[("start_date", from), ("end_date", to), ("page", &page)])
            .await?
            .unwrap_or_default();
        let ids = v["results"].as_array().map(|a| a.iter().filter_map(|r| r["id"].as_i64()).map(|i| i.to_string()).collect());
        Ok((ids.unwrap_or_default(), v["total_pages"].as_i64().unwrap_or(1)))
    }

    /// The id of the title called `title` (`pick_match`), searched with its
    /// year when known.
    async fn search(&self, kind: Kind, title: &str, year: Option<i64>) -> Result<Option<String>> {
        // providers write "Batali_ The Fall of…" for "Batali: The Fall of…"
        let query = title.replace('_', " ");
        let year = year.map(|y| y.to_string());
        let mut params = vec![("query", query.as_str())];
        if let Some(y) = &year {
            params.push(("year", y.as_str()));
        }
        let v = self.json(&format!("/search/{}", kind.as_str()), &params).await?;
        Ok(v.and_then(|v| pick_match(&v["results"], kind, title)))
    }
}

/// The TMDB id of the one search result whose title or original title is
/// `title` (normalized, `works::norm_title`); `None` when none or several
/// are — a title that isn't unique stays without an id.
fn pick_match(results: &Value, kind: Kind, title: &str) -> Option<String> {
    let want = crate::works::norm_title(title);
    if want.is_empty() {
        return None;
    }
    let fields = match kind {
        Kind::Movie => ["title", "original_title"],
        Kind::Tv => ["name", "original_name"],
    };
    let mut hits = results
        .as_array()?
        .iter()
        .filter(|r| fields.iter().any(|f| r[*f].as_str().is_some_and(|t| crate::works::norm_title(t) == want)));
    let hit = hits.next()?;
    if hits.next().is_some() {
        return None;
    }
    hit["id"].as_i64().map(|i| i.to_string())
}

/// Spaces requests `1000 / RATE` ms apart, across concurrent tasks.
struct Pace(tokio::sync::Mutex<tokio::time::Interval>);

impl Pace {
    fn new() -> Self {
        let mut i = tokio::time::interval(Duration::from_millis(1000 / RATE));
        i.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        Pace(tokio::sync::Mutex::new(i))
    }

    async fn wait(&self) {
        self.0.lock().await.tick().await;
    }
}

const REJECTED: &str = "TMDB did not accept the API key";

/// Checks a key with one request (`GET /3/authentication`, "validate key").
pub async fn check_key(key: &str) -> Result<()> {
    let resp = Client::new(key).get("/authentication", &[]).await?;
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
    /// titles the provider lists without an id that a search found
    pub found: i64,
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
    let found: i64 = conn.query_row(
        "SELECT COUNT(DISTINCT w.kind || w.key) FROM work w JOIN tmdb_match m ON m.kind = w.kind AND 'tmdb:' || m.tmdb_id = w.key",
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
        found,
        error: p.error.clone().or_else(|| locked.then(|| KEY_LOCKED.to_owned())),
        last_run: p.last_run,
    })
}

fn emit<R: Runtime>(app: &AppHandle<R>, st: &AppState) {
    if let Ok(s) = status(&st.db.read()) {
        let _ = app.emit(EVENT, s);
    }
}

// --------------------------------------------------------------------- job

/// Titles to fetch: never fetched, changed on TMDB (`fetched_at` 0) or older
/// than `MAX_AGE`, newest first.
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
    let Some(api_key) = key(&st.db.read()) else { return Ok(0) };
    let client = std::sync::Arc::new(Client::new(&api_key));
    let pace = std::sync::Arc::new(Pace::new());
    emit(app, st);

    // 1. details TMDB changed since the last run are fetched again
    match refresh_changed(&client, &pace, st).await {
        Ok(n) => log::info!("tmdb: {n} stored titles changed on TMDB"),
        Err(e) if e.to_string() == REJECTED => return Err(e),
        // offline or TMDB down: the next run scans from the same point
        Err(e) => log::warn!("tmdb: change lists unavailable ({e})"),
    }

    // 2. titles the provider lists without an id: searched once
    let searches = search_todo(&st.db.read())?;
    if !searches.is_empty() {
        log::info!("tmdb: searching {} titles without an id", searches.len());
        PROGRESS.lock().total = searches.len() as i64;
        emit(app, st);
        let (c, p) = (client.clone(), pace.clone());
        let mut found = 0usize;
        ask_all(
            app,
            st,
            searches,
            move |s: Unmatched| {
                let (client, pace) = (c.clone(), p.clone());
                async move {
                    pace.wait().await;
                    let r = client.search(s.kind, &s.title, s.year).await;
                    (s, r)
                }
            },
            |batch| {
                found += batch.iter().filter(|(_, id)| id.is_some()).count();
                let rows: Vec<_> = batch.into_iter().map(|(s, id)| (s.work, id)).collect();
                store_found(&mut st.db.write(), &rows)
            },
        )
        .await?;
        log::info!("tmdb: {found} titles without an id found");
        if found > 0 {
            regroup(st)?;
        }
    }
    // the key was removed meanwhile (Settings)
    if key(&st.db.read()).is_none() {
        return Ok(0);
    }

    // 3. details of every work with an id: new, changed or old
    let todo = todo(&st.db.read())?;
    if todo.is_empty() {
        return Ok(0);
    }
    log::info!("tmdb: fetching {} titles", todo.len());
    PROGRESS.lock().total += todo.len() as i64;
    emit(app, st);
    let (mut stored, mut since_regroup) = (0usize, 0usize);
    ask_all(
        app,
        st,
        todo,
        move |(kind, id): (Kind, String)| {
            let (client, pace) = (client.clone(), pace.clone());
            async move {
                pace.wait().await;
                let r = client.details(kind, &id).await;
                ((kind, id), r)
            }
        },
        |batch| {
            let rows: Vec<(Kind, String, Option<Info>)> = batch
                .into_iter()
                .map(|((kind, id), fetched)| {
                    (kind, id, match fetched {
                        Fetched::Found(info) => Some(*info),
                        Fetched::Missing => None,
                    })
                })
                .collect();
            stored += rows.len();
            since_regroup += rows.len();
            store(&mut st.db.write(), &rows)?;
            if since_regroup >= REGROUP_EVERY {
                since_regroup = 0;
                regroup(st)?;
            }
            Ok(())
        },
    )
    .await?;
    regroup(st)?;
    Ok(stored)
}

/// Asks TMDB about every item — `ask` waits for the pace, `CONCURRENCY` run
/// at a time — and hands the answers to `keep` in batches of 100 and at the
/// end. Stops when TMDB rejects the key, when most requests fail (offline:
/// the next run continues), or when the key was removed meanwhile.
async fn ask_all<R, T, V, Fut>(
    app: &AppHandle<R>,
    st: &AppState,
    items: Vec<T>,
    ask: impl FnMut(T) -> Fut,
    mut keep: impl FnMut(Vec<(T, V)>) -> Result<()>,
) -> Result<()>
where
    R: Runtime,
    Fut: std::future::Future<Output = (T, Result<V>)>,
{
    let mut answers = futures_util::stream::iter(items).map(ask).buffer_unordered(CONCURRENCY);
    let mut batch = Vec::new();
    let (mut answered, mut failures) = (0usize, 0usize);
    while let Some((item, r)) = answers.next().await {
        PROGRESS.lock().done += 1;
        match r {
            Ok(v) => {
                answered += 1;
                batch.push((item, v));
            }
            Err(e) if e.to_string() == REJECTED => return Err(e),
            Err(e) => {
                failures += 1;
                log::debug!("tmdb: {e}");
                if failures >= 25 && failures * 2 > answered {
                    return Err(Error::msg(format!("TMDB is not reachable ({e})")));
                }
            }
        }
        if batch.len() >= 100 {
            keep(std::mem::take(&mut batch))?;
            if key(&st.db.read()).is_none() {
                return Ok(());
            }
            emit(app, st);
        }
    }
    keep(batch)
}

// ------------------------------------------------------------ change lists

/// The days (since 1970, UTC; inclusive) whose change lists to scan, and
/// whether details fetched before the first of them must be fetched again:
/// with no scan within the lists' reach (or none at all) their changes can't
/// be listed any more.
fn changes_window(last_scan: Option<i64>, now: i64) -> (i64, i64, bool) {
    let today = now.div_euclid(86_400);
    let earliest = today - CHANGES_DAYS;
    match last_scan.map(|t| t.div_euclid(86_400)) {
        // continuous: from the day of the last scan
        Some(day) if day >= earliest => (day, today, false),
        _ => (earliest, today, true),
    }
}

/// "YYYY-MM-DD" of a day number (since 1970, UTC).
fn date(day: i64) -> String {
    chrono::DateTime::from_timestamp(day * 86_400, 0).map(|d| d.format("%Y-%m-%d").to_string()).unwrap_or_default()
}

/// The ids on TMDB's change list of `kind` for one day (a day has ~60 pages
/// of movies, ~20 of shows).
async fn changed_on(client: &Client, pace: &Pace, kind: Kind, day: i64) -> Result<HashSet<String>> {
    let date = date(day);
    pace.wait().await;
    let (first, pages) = client.changes(kind, &date, &date, 1).await?;
    if pages > MAX_PAGES {
        log::warn!("tmdb: {} changes of {date} exceed {MAX_PAGES} pages; the rest is left out", kind.as_str());
    }
    let mut ids: HashSet<String> = first.into_iter().collect();
    let rest: Vec<Result<(Vec<String>, i64)>> = futures_util::stream::iter(2..=pages.min(MAX_PAGES))
        .map(|n| {
            let date = &date;
            async move {
                pace.wait().await;
                client.changes(kind, date, date, n).await
            }
        })
        .buffer_unordered(CONCURRENCY)
        .collect()
        .await;
    for r in rest {
        ids.extend(r?.0);
    }
    Ok(ids)
}

/// Marks the stored details of `ids` stale (`fetched_at = 0`) — those
/// fetched before the end of `day`, the day TMDB changed them — so `todo`
/// fetches them again; returns how many.
fn mark_changed(conn: &mut Connection, kind: Kind, ids: &HashSet<String>, day: i64) -> Result<usize> {
    let tx = conn.transaction()?;
    let mut marked = 0;
    {
        let ours: HashSet<String> = tx
            .prepare("SELECT id FROM tmdb WHERE kind = ?1 AND fetched_at > 0 AND fetched_at < ?2")?
            .query_map(params![kind.as_str(), (day + 1) * 86_400], |r| r.get(0))?
            .collect::<std::result::Result<_, _>>()?;
        let mut stale = tx.prepare("UPDATE tmdb SET fetched_at = 0 WHERE kind = ?1 AND id = ?2")?;
        for id in ids.intersection(&ours) {
            marked += stale.execute(params![kind.as_str(), id])?;
        }
    }
    tx.commit()?;
    Ok(marked)
}

/// Marks the stored details TMDB changed since they were fetched (all of
/// them after a gap longer than the lists reach), then remembers this scan.
/// Returns how many were marked.
async fn refresh_changed(client: &Client, pace: &Pace, st: &AppState) -> Result<usize> {
    let now = now();
    let (last_scan, stored): (Option<i64>, i64) = {
        let conn = st.db.read();
        (crate::settings::get(&conn, CHANGES_SETTING).as_i64(), conn.query_row("SELECT COUNT(*) FROM tmdb", [], |r| r.get(0))?)
    };
    let mut marked = 0;
    if stored > 0 {
        let (first, last, refetch_older) = changes_window(last_scan, now);
        if refetch_older {
            marked += st
                .db
                .write()
                .execute("UPDATE tmdb SET fetched_at = 0 WHERE fetched_at > 0 AND fetched_at < ?1", [first * 86_400])?;
        }
        // day by day: a change counts only for details fetched before it
        for day in first..=last {
            for kind in [Kind::Movie, Kind::Tv] {
                let ids = changed_on(client, pace, kind, day).await?;
                marked += mark_changed(&mut st.db.write(), kind, &ids, day)?;
            }
        }
    }
    st.db.write().execute(
        "INSERT INTO setting (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![CHANGES_SETTING, now.to_string()],
    )?;
    Ok(marked)
}

// --------------------------------------------------- titles without an id

/// A work the provider lists without a TMDB id, to search for.
struct Unmatched {
    /// its work kind ("movie" | "series") and key (`tmdb_match`)
    work: (&'static str, String),
    kind: Kind,
    title: String,
    year: Option<i64>,
}

/// Works without a TMDB id (`title:`/`item:` keys) not searched for within
/// `MAX_AGE`, newest first. Adult titles aren't TMDB's.
fn search_todo(conn: &Connection) -> Result<Vec<Unmatched>> {
    type Row = (String, String, String, Option<i64>);
    let rows: Vec<Row> = conn
        .prepare(
            "SELECT w.kind, w.key, w.title, w.year FROM work w
               LEFT JOIN tmdb_match m ON m.kind = w.kind AND m.key = w.key
              WHERE (w.key LIKE 'title:%' OR w.key LIKE 'item:%') AND w.adult = 0
                AND (m.key IS NULL OR m.checked_at < ?1)
              ORDER BY w.added IS NULL, w.added DESC",
        )?
        .query_map([now() - MAX_AGE], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
        .collect::<std::result::Result<_, _>>()?;
    Ok(rows
        .into_iter()
        .map(|(kind, key, title, year)| {
            let movie = kind == "movie";
            Unmatched {
                work: (if movie { "movie" } else { "series" }, key),
                kind: if movie { Kind::Movie } else { Kind::Tv },
                title,
                year,
            }
        })
        .collect())
}

/// Remembers search answers; `None` (no single match) is asked again after
/// `MAX_AGE`.
fn store_found(conn: &mut Connection, rows: &[((&'static str, String), Option<String>)]) -> Result<()> {
    let tx = conn.transaction()?;
    {
        let mut stmt =
            tx.prepare_cached("INSERT OR REPLACE INTO tmdb_match (kind, key, tmdb_id, checked_at) VALUES (?1, ?2, ?3, ?4)")?;
        let t = now();
        for ((kind, key), id) in rows {
            stmt.execute(params![kind, key, id, t])?;
        }
    }
    tx.commit()?;
    Ok(())
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
    // the system keyring when there is one (it may ask to be unlocked: the
    // user just typed the key), else the database
    if key.is_empty() {
        crate::secrets::forget_named(state.inner(), KEY_SETTING)?;
    } else {
        crate::secrets::store_named(state.inner(), KEY_SETTING, &key, crate::secrets::Unlock::Prompt).await?;
    }
    PROGRESS.lock().error = None;
    spawn(app, state.inner().clone());
    status(&state.db.read())
}

#[tauri::command]
pub async fn tmdb_refresh<R: Runtime>(app: AppHandle<R>, state: State<'_, AppState>) -> Result<Status> {
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
    fn a_key_in_a_locked_keyring_is_reported() {
        let c = crate::db::test_conn();
        assert!(!status(&c).unwrap().configured);
        c.execute("INSERT INTO setting (key, value) VALUES ('tmdb.key.inKeyring', 'true')", []).unwrap();
        let s = status(&c).unwrap();
        assert!(s.configured);
        assert_eq!(s.error.as_deref(), Some(KEY_LOCKED));
        assert_eq!(key(&c), None);
    }

    #[test]
    fn tokens_and_keys() {
        assert!(matches!(auth("eyJhbGciOiJIUzI1NiJ9.eyJhdWQiOiJ4In0.sig"), Auth::Bearer(_)));
        assert!(matches!(auth(" 0123456789abcdef0123456789abcdef "), Auth::ApiKey(_)));
    }

    #[test]
    fn change_lists_continue_from_the_last_scan() {
        const DAY: i64 = 86_400;
        let now = 20_000 * DAY + 3600;
        // never scanned: the lists' 14 days, details from before are fetched again
        assert_eq!(changes_window(None, now), (19_986, 20_000, true));
        // scanned yesterday: from that day on (dates are inclusive)
        assert_eq!(changes_window(Some(now - DAY), now), (19_999, 20_000, false));
        assert_eq!(changes_window(Some(now - 14 * DAY), now), (19_986, 20_000, false));
        // a longer gap can't be listed
        assert_eq!(changes_window(Some(now - 15 * DAY), now), (19_986, 20_000, true));
        assert_eq!(date(20_000), "2024-10-04");
    }

    #[test]
    fn a_search_counts_only_one_exact_title() {
        let movies = serde_json::json!([
            {"id": 1, "title": "Dealer", "original_title": "Dealer"},
            {"id": 2, "title": "The Dealer", "original_title": "The Dealer"},
        ]);
        assert_eq!(pick_match(&movies, Kind::Movie, "DEALER").as_deref(), Some("1"));
        assert_eq!(pick_match(&movies, Kind::Movie, "Other"), None);
        assert_eq!(pick_match(&movies, Kind::Movie, "!!"), None);
        // original titles count, punctuation and accents don't
        let shows = serde_json::json!([
            {"id": 158916, "name": "The Marked Heart", "original_name": "Pálpito"},
            {"id": 9, "name": "Batali: The Fall of a Superstar Chef", "original_name": "Batali: The Fall of a Superstar Chef"},
        ]);
        assert_eq!(pick_match(&shows, Kind::Tv, "Palpito").as_deref(), Some("158916"));
        assert_eq!(pick_match(&shows, Kind::Tv, "Batali_ The Fall of a Superstar Chef").as_deref(), Some("9"));
        // not unique: no id
        let two = serde_json::json!([{"id": 1, "title": "Sherlock Holmes"}, {"id": 2, "title": "Sherlock Holmes"}]);
        assert_eq!(pick_match(&two, Kind::Movie, "Sherlock Holmes"), None);
    }

    #[test]
    fn changed_and_unmatched_titles_are_fetched() {
        let mut c = crate::db::test_conn();
        for (id, tmdb, year) in [("a", Some("11"), 2000), ("b", None, 2001)] {
            c.execute(
                "INSERT INTO movie (source_id, id, name, title, year, tmdb, position) VALUES (1, ?1, ?1, ?1, ?2, ?3, 0)",
                params![id, year, tmdb],
            )
            .unwrap();
        }
        crate::works::rebuild(&c).unwrap();
        c.execute("INSERT INTO tmdb (kind, id, json, fetched_at) VALUES ('movie', '11', NULL, ?1)", [now()]).unwrap();
        assert!(todo(&c).unwrap().is_empty());
        // TMDB lists it as changed (among ids the catalog doesn't have): on a
        // day before its details were fetched, that's in them already…
        let changed: HashSet<String> = ["11", "999"].map(String::from).into();
        let today = now().div_euclid(86_400);
        assert_eq!(mark_changed(&mut c, Kind::Movie, &changed, today - 1).unwrap(), 0);
        // …a change of the same day may not be: fetched again
        assert_eq!(mark_changed(&mut c, Kind::Movie, &changed, today).unwrap(), 1);
        assert_eq!(todo(&c).unwrap(), vec![(Kind::Movie, "11".to_owned())]);

        // the title without an id is searched once…
        let s = search_todo(&c).unwrap();
        assert_eq!(s.iter().map(|u| (u.work.1.as_str(), u.year)).collect::<Vec<_>>(), vec![("title:b|2001", Some(2001))]);
        store_found(&mut c, &[(s[0].work.clone(), Some("22".into()))]).unwrap();
        assert!(search_todo(&c).unwrap().is_empty());
        // …and joins the work of the id found, whose details are fetched next
        crate::works::rebuild(&c).unwrap();
        assert!(todo(&c).unwrap().contains(&(Kind::Movie, "22".to_owned())));
        assert_eq!(status(&c).unwrap().found, 1);
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
