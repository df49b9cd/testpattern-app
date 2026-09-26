//! IPTV sources (Xtream Codes accounts and M3U playlists): storage, sync
//! into the local catalog, and EPG refresh.

pub mod m3u;
pub mod xtream;

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{AppHandle, Emitter, Runtime, State};

use crate::db::now;
use crate::error::{Error, Result};
use crate::names;
use crate::state::{AppState, http_client};
use crate::util::fnv1a;
use crate::util::json::{first_str, i64_of, str_of};
use xtream::{AccountInfo, Xtream, rating_of};

pub const SYNC_EVENT: &str = "sync://progress";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SourceKind {
    Xtream,
    M3u,
}

impl SourceKind {
    fn as_str(self) -> &'static str {
        match self {
            SourceKind::Xtream => "xtream",
            SourceKind::M3u => "m3u",
        }
    }
    fn parse(s: &str) -> Self {
        if s == "m3u" { SourceKind::M3u } else { SourceKind::Xtream }
    }
}

/// Full source row (backend only — includes the password).
#[derive(Debug, Clone)]
pub struct SourceRow {
    pub id: i64,
    pub kind: SourceKind,
    pub name: String,
    pub url: String,
    pub alt_urls: Vec<String>,
    pub username: Option<String>,
    pub password: Option<String>,
    pub epg_url: Option<String>,
    pub user_agent: Option<String>,
    pub created_at: i64,
    pub last_sync: Option<i64>,
    pub last_epg_sync: Option<i64>,
    pub sync_error: Option<String>,
    pub account: Option<Value>,
}

/// What the UI sees (no password).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceView {
    pub id: i64,
    pub kind: SourceKind,
    pub name: String,
    pub url: String,
    pub alt_urls: Vec<String>,
    pub username: Option<String>,
    pub has_password: bool,
    pub epg_url: Option<String>,
    pub user_agent: Option<String>,
    pub created_at: i64,
    pub last_sync: Option<i64>,
    pub last_epg_sync: Option<i64>,
    pub sync_error: Option<String>,
    pub account: Option<Value>,
    pub syncing: bool,
    pub counts: Counts,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Counts {
    pub channels: i64,
    pub movies: i64,
    pub series: i64,
    pub programmes: i64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceInput {
    pub kind: SourceKind,
    pub name: Option<String>,
    pub url: String,
    #[serde(default)]
    pub alt_urls: Vec<String>,
    pub username: Option<String>,
    pub password: Option<String>,
    pub epg_url: Option<String>,
    pub user_agent: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase", tag = "stage")]
pub enum SyncProgress {
    Started { source_id: i64 },
    Step { source_id: i64, message: String },
    Done { source_id: i64, counts: Counts },
    Failed { source_id: i64, error: String },
}

const SOURCE_COLS: &str = "id, kind, name, url, alt_urls, username, password, epg_url, user_agent, \
     created_at, last_sync, last_epg_sync, sync_error, account_json";

fn row_to_source(r: &rusqlite::Row) -> rusqlite::Result<SourceRow> {
    let alt: String = r.get(4)?;
    let account: Option<String> = r.get(13)?;
    Ok(SourceRow {
        id: r.get(0)?,
        kind: SourceKind::parse(&r.get::<_, String>(1)?),
        name: r.get(2)?,
        url: r.get(3)?,
        alt_urls: serde_json::from_str(&alt).unwrap_or_default(),
        username: r.get(5)?,
        password: r.get(6)?,
        epg_url: r.get(7)?,
        user_agent: r.get(8)?,
        created_at: r.get(9)?,
        last_sync: r.get(10)?,
        last_epg_sync: r.get(11)?,
        sync_error: r.get(12)?,
        account: account.and_then(|a| serde_json::from_str(&a).ok()),
    })
}

pub fn load(conn: &Connection, id: i64) -> Result<SourceRow> {
    conn.query_row(&format!("SELECT {SOURCE_COLS} FROM source WHERE id = ?1"), [id], row_to_source)
        .optional()?
        .ok_or_else(|| Error::NotFound(format!("source {id}")))
}

pub fn load_all(conn: &Connection) -> Result<Vec<SourceRow>> {
    let mut stmt = conn.prepare(&format!("SELECT {SOURCE_COLS} FROM source ORDER BY id"))?;
    let rows = stmt.query_map([], row_to_source)?.collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

fn counts(conn: &Connection, id: i64) -> Result<Counts> {
    let one = |sql: &str| -> Result<i64> { Ok(conn.query_row(sql, [id], |r| r.get(0))?) };
    Ok(Counts {
        channels: one("SELECT COUNT(*) FROM channel WHERE source_id = ?1 AND separator = 0")?,
        movies: one("SELECT COUNT(*) FROM movie WHERE source_id = ?1")?,
        series: one("SELECT COUNT(*) FROM series WHERE source_id = ?1")?,
        programmes: one("SELECT COUNT(*) FROM programme WHERE source_id = ?1")?,
    })
}

fn view(conn: &Connection, s: SourceRow, syncing: bool) -> Result<SourceView> {
    let counts = counts(conn, s.id)?;
    Ok(SourceView {
        id: s.id,
        kind: s.kind,
        name: s.name,
        url: s.url,
        alt_urls: s.alt_urls,
        username: s.username,
        has_password: s.password.as_deref().is_some_and(|p| !p.is_empty()),
        epg_url: s.epg_url,
        user_agent: s.user_agent,
        created_at: s.created_at,
        last_sync: s.last_sync,
        last_epg_sync: s.last_epg_sync,
        sync_error: s.sync_error,
        account: s.account,
        syncing,
        counts,
    })
}

/// Xtream playlist links pasted as "M3U" are upgraded to full Xtream sources.
fn normalize_input(mut input: SourceInput) -> Result<SourceInput> {
    input.url = input.url.trim().to_owned();
    if input.url.is_empty() {
        return Err(Error::msg("Please enter a server or playlist URL"));
    }
    if input.kind == SourceKind::M3u
        && let Some((base, user, pass)) = xtream::parse_playlist_url(&input.url) {
            input.kind = SourceKind::Xtream;
            input.url = base;
            input.username = Some(user);
            input.password = Some(pass);
        }
    if input.kind == SourceKind::Xtream {
        input.url = xtream::normalize_base(&input.url);
        if input.username.as_deref().unwrap_or("").trim().is_empty() {
            return Err(Error::msg("Username is required"));
        }
    }
    input.alt_urls = input
        .alt_urls
        .iter()
        .map(|u| u.trim())
        .filter(|u| !u.is_empty())
        .map(|u| if input.kind == SourceKind::Xtream { xtream::normalize_base(u) } else { u.to_owned() })
        .collect();
    Ok(input)
}

fn default_name(input: &SourceInput) -> String {
    let host = url::Url::parse(&input.url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_owned))
        .unwrap_or_else(|| input.url.clone());
    match input.kind {
        SourceKind::Xtream => host,
        SourceKind::M3u => format!("{host} playlist"),
    }
}

// ------------------------------------------------------------------ commands

#[tauri::command]
pub async fn sources_list(state: State<'_, AppState>) -> Result<Vec<SourceView>> {
    let st = state.inner().clone();
    tokio::task::spawn_blocking(move || {
        let conn = st.db.read();
        let syncing = st.syncing.lock().clone();
        load_all(&conn)?.into_iter().map(|s| {
            let busy = syncing.contains(&s.id);
            view(&conn, s, busy)
        }).collect()
    })
    .await
    .map_err(|e| Error::msg(e.to_string()))?
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase", tag = "kind")]
pub enum TestResult {
    Xtream { account: AccountInfo },
    M3u { channels: usize, movies: usize, episodes: usize, epg_urls: Vec<String> },
}

/// Validates credentials / playlist without saving anything. While editing a
/// saved source (`id`), an empty password field means the stored password.
#[tauri::command]
pub async fn source_test(state: State<'_, AppState>, input: SourceInput, id: Option<i64>) -> Result<TestResult> {
    let mut input = normalize_input(input)?;
    if let Some(id) = id
        && input.password.as_deref().is_none_or(str::is_empty)
    {
        let conn = state.db.read();
        input.password = load(&conn, id)?.password;
    }
    match input.kind {
        SourceKind::Xtream => {
            let x = Xtream::new(
                &input.url,
                &input.alt_urls,
                input.username.as_deref().unwrap_or(""),
                input.password.as_deref().unwrap_or(""),
                http_client(input.user_agent.as_deref()),
            );
            Ok(TestResult::Xtream { account: x.account().await? })
        }
        SourceKind::M3u => {
            let text = download_text(&http_client(input.user_agent.as_deref()), &input.url).await?;
            let pl = m3u::parse(&text);
            if pl.entries.is_empty() {
                return Err(Error::msg("The playlist is empty or not an M3U file"));
            }
            let mut c = (0, 0, 0);
            for e in &pl.entries {
                match e.kind() {
                    m3u::EntryKind::Live => c.0 += 1,
                    m3u::EntryKind::Movie => c.1 += 1,
                    m3u::EntryKind::Episode => c.2 += 1,
                }
            }
            Ok(TestResult::M3u { channels: c.0, movies: c.1, episodes: c.2, epg_urls: pl.epg_urls })
        }
    }
}

#[tauri::command]
pub async fn source_add<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
    input: SourceInput,
) -> Result<SourceView> {
    let input = normalize_input(input)?;
    let st = state.inner().clone();
    let name = input.name.clone().filter(|n| !n.trim().is_empty()).unwrap_or_else(|| default_name(&input));
    let id = {
        let conn = st.db.write();
        conn.execute(
            "INSERT INTO source (kind, name, url, alt_urls, username, password, epg_url, user_agent, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                input.kind.as_str(),
                name.trim(),
                input.url,
                serde_json::to_string(&input.alt_urls)?,
                input.username.as_deref().map(str::trim),
                input.password,
                input.epg_url.as_deref().map(str::trim).filter(|s| !s.is_empty()),
                input.user_agent.as_deref().map(str::trim).filter(|s| !s.is_empty()),
                now()
            ],
        )?;
        conn.last_insert_rowid()
    };
    spawn_sync(app, st.clone(), id, SyncScope::Full);
    let conn = st.db.read();
    view(&conn, load(&conn, id)?, true)
}

#[tauri::command]
pub async fn source_update(state: State<'_, AppState>, id: i64, input: SourceInput) -> Result<SourceView> {
    let input = normalize_input(input)?;
    let st = state.inner().clone();
    {
        let conn = st.db.write();
        let current = load(&conn, id)?;
        // an empty password field means "keep the stored one"
        let password = input.password.filter(|p| !p.is_empty()).or(current.password);
        conn.execute(
            "UPDATE source SET kind = ?2, name = ?3, url = ?4, alt_urls = ?5, username = ?6, password = ?7,
                    epg_url = ?8, user_agent = ?9 WHERE id = ?1",
            params![
                id,
                input.kind.as_str(),
                input.name.as_deref().map(str::trim).filter(|n| !n.is_empty()).unwrap_or(&current.name),
                input.url,
                serde_json::to_string(&input.alt_urls)?,
                input.username.as_deref().map(str::trim),
                password,
                input.epg_url.as_deref().map(str::trim).filter(|s| !s.is_empty()),
                input.user_agent.as_deref().map(str::trim).filter(|s| !s.is_empty()),
            ],
        )?;
    }
    let conn = st.db.read();
    let busy = st.syncing.lock().contains(&id);
    view(&conn, load(&conn, id)?, busy)
}

#[tauri::command]
pub async fn source_remove(state: State<'_, AppState>, id: i64) -> Result<()> {
    let st = state.inner().clone();
    tokio::task::spawn_blocking(move || -> Result<()> {
        let mut conn = st.db.write();
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM search WHERE source_id = ?1", [id])?;
        tx.execute("DELETE FROM source WHERE id = ?1", [id])?;
        tx.commit()?;
        Ok(())
    })
    .await
    .map_err(|e| Error::msg(e.to_string()))?
}

#[tauri::command]
pub async fn source_sync<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
    id: i64,
    epg_only: Option<bool>,
) -> Result<()> {
    let scope = if epg_only.unwrap_or(false) { SyncScope::Epg } else { SyncScope::Full };
    spawn_sync(app, state.inner().clone(), id, scope);
    Ok(())
}

// ---------------------------------------------------------------------- sync

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncScope {
    Full,
    Epg,
}

/// Starts a background sync unless one is already running for the source.
pub fn spawn_sync<R: Runtime>(app: AppHandle<R>, st: AppState, id: i64, scope: SyncScope) {
    if !st.syncing.lock().insert(id) {
        return;
    }
    tauri::async_runtime::spawn(async move {
        let _ = app.emit(SYNC_EVENT, SyncProgress::Started { source_id: id });
        let result = run_sync(&app, &st, id, scope).await;
        st.syncing.lock().remove(&id);
        match result {
            Ok(counts) => {
                let _ = app.emit(SYNC_EVENT, SyncProgress::Done { source_id: id, counts });
            }
            Err(e) => {
                log::warn!("sync of source {id} failed: {e}");
                if scope == SyncScope::Full {
                    let conn = st.db.write();
                    let _ = conn.execute("UPDATE source SET sync_error = ?2 WHERE id = ?1", params![id, e.to_string()]);
                }
                let _ = app.emit(SYNC_EVENT, SyncProgress::Failed { source_id: id, error: e.to_string() });
            }
        }
    });
}

/// Syncs every source whose catalog/EPG is older than the given ages.
pub fn sync_stale<R: Runtime>(app: &AppHandle<R>, st: &AppState, catalog_max_age: i64, epg_max_age: i64) {
    let sources = {
        let conn = st.db.read();
        load_all(&conn).unwrap_or_default()
    };
    let t = now();
    for s in sources {
        if s.last_sync.is_none_or(|l| t - l > catalog_max_age) {
            spawn_sync(app.clone(), st.clone(), s.id, SyncScope::Full);
        } else if s.last_epg_sync.is_none_or(|l| t - l > epg_max_age) {
            spawn_sync(app.clone(), st.clone(), s.id, SyncScope::Epg);
        }
    }
}

fn step<R: Runtime>(app: &AppHandle<R>, id: i64, msg: &str) {
    let _ = app.emit(SYNC_EVENT, SyncProgress::Step { source_id: id, message: msg.to_owned() });
}

async fn run_sync<R: Runtime>(app: &AppHandle<R>, st: &AppState, id: i64, scope: SyncScope) -> Result<Counts> {
    let src = {
        let conn = st.db.read();
        load(&conn, id)?
    };
    let client = http_client(src.user_agent.as_deref());
    let mut epg_urls: Vec<String> = src.epg_url.iter().cloned().collect();

    if scope == SyncScope::Full {
        match src.kind {
            SourceKind::Xtream => {
                let x = xtream_for(&src, client.clone());
                step(app, id, "Signing in…");
                let account = x.account().await?;
                if !account.status.eq_ignore_ascii_case("active") {
                    return Err(Error::msg(format!("Account is {}", account.status.to_lowercase())));
                }
                {
                    let conn = st.db.write();
                    conn.execute(
                        "UPDATE source SET account_json = ?2 WHERE id = ?1",
                        params![id, serde_json::to_string(&account)?],
                    )?;
                }
                step(app, id, "Downloading channels, movies and series…");
                let (lc, vc, sc, ls, vs, ss) = tokio::try_join!(
                    x.list("get_live_categories"),
                    x.list("get_vod_categories"),
                    x.list("get_series_categories"),
                    x.list("get_live_streams"),
                    x.list("get_vod_streams"),
                    x.list("get_series"),
                )?;
                step(app, id, &format!("Saving {} channels, {} movies, {} series…", ls.len(), vs.len(), ss.len()));
                let st2 = st.clone();
                tokio::task::spawn_blocking(move || {
                    let mut conn = st2.db.write();
                    write_xtream(&mut conn, id, XtreamLists { lc, vc, sc, ls, vs, ss })
                })
                .await
                .map_err(|e| Error::msg(e.to_string()))??;
                if epg_urls.is_empty() {
                    epg_urls.push(x.xmltv_url());
                }
            }
            SourceKind::M3u => {
                step(app, id, "Downloading playlist…");
                let text = download_text(&client, &src.url).await?;
                let pl = m3u::parse(&text);
                if pl.entries.is_empty() {
                    return Err(Error::msg("The playlist is empty or not an M3U file"));
                }
                step(app, id, &format!("Saving {} entries…", pl.entries.len()));
                if epg_urls.is_empty() {
                    epg_urls.extend(pl.epg_urls.iter().cloned());
                }
                let st2 = st.clone();
                tokio::task::spawn_blocking(move || {
                    let mut conn = st2.db.write();
                    write_m3u(&mut conn, id, &pl)
                })
                .await
                .map_err(|e| Error::msg(e.to_string()))??;
            }
        }
        let conn = st.db.write();
        conn.execute("UPDATE source SET last_sync = ?2, sync_error = NULL WHERE id = ?1", params![id, now()])?;
    } else if epg_urls.is_empty() {
        match src.kind {
            SourceKind::Xtream => epg_urls.push(xtream_for(&src, client.clone()).xmltv_url()),
            SourceKind::M3u => {
                // the playlist header may name a guide
                if let Ok(text) = download_text(&client, &src.url).await {
                    epg_urls.extend(m3u::parse(&text).epg_urls);
                }
            }
        }
    }

    // EPG is best effort: a broken guide must not fail the catalog sync.
    if !epg_urls.is_empty() {
        step(app, id, "Updating TV guide…");
        match sync_epg(st, id, &client, &epg_urls).await {
            Ok(n) => log::info!("source {id}: imported {n} programmes"),
            Err(e) => {
                log::warn!("source {id}: EPG import failed: {e}");
                if scope == SyncScope::Epg {
                    return Err(e);
                }
            }
        }
    }

    let conn = st.db.read();
    counts(&conn, id)
}

pub fn xtream_for(src: &SourceRow, client: reqwest::Client) -> Xtream {
    // prefer the mirror that answered last time
    let preferred = src.account.as_ref().and_then(|a| str_of(&a["baseUrl"]));
    let mut alts = src.alt_urls.clone();
    let mut primary = src.url.clone();
    if let Some(p) = preferred.filter(|p| *p != src.url && alts.contains(p)) {
        alts.retain(|a| *a != p);
        alts.insert(0, primary);
        primary = p;
    }
    Xtream::new(
        &primary,
        &alts,
        src.username.as_deref().unwrap_or(""),
        src.password.as_deref().unwrap_or(""),
        client,
    )
}

async fn download_text(client: &reqwest::Client, url: &str) -> Result<String> {
    let r = client.get(url).timeout(Duration::from_secs(300)).send().await?;
    if !r.status().is_success() {
        return Err(Error::msg(format!("server answered {}", r.status())));
    }
    let bytes = crate::epg::maybe_gunzip(r.bytes().await?.to_vec())?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

async fn sync_epg(st: &AppState, id: i64, client: &reqwest::Client, urls: &[String]) -> Result<usize> {
    let mut last_err = None;
    for url in urls {
        let res = async {
            let r = client.get(url).timeout(Duration::from_secs(600)).send().await?;
            if !r.status().is_success() {
                return Err(Error::msg(format!("guide server answered {}", r.status())));
            }
            let bytes = crate::epg::maybe_gunzip(r.bytes().await?.to_vec())?;
            let st2 = st.clone();
            tokio::task::spawn_blocking(move || -> Result<usize> {
                let wanted: HashSet<String> = {
                    let conn = st2.db.read();
                    let mut stmt = conn.prepare(
                        "SELECT DISTINCT epg_id FROM channel WHERE source_id = ?1 AND epg_id IS NOT NULL",
                    )?;
                    stmt.query_map([id], |r| r.get(0))?.collect::<Result<_, _>>()?
                };
                let mut conn = st2.db.write();
                let n = crate::epg::import(&mut conn, id, &bytes, &wanted)?;
                conn.execute("UPDATE source SET last_epg_sync = ?2 WHERE id = ?1", params![id, now()])?;
                Ok(n)
            })
            .await
            .map_err(|e| Error::msg(e.to_string()))?
        }
        .await;
        match res {
            Ok(n) => return Ok(n),
            Err(e) => last_err = Some(e),
        }
    }
    Err(last_err.unwrap_or_else(|| Error::msg("no guide url")))
}

// ------------------------------------------------------------ catalog writes

struct XtreamLists {
    lc: Vec<Value>,
    vc: Vec<Value>,
    sc: Vec<Value>,
    ls: Vec<Value>,
    vs: Vec<Value>,
    ss: Vec<Value>,
}

fn is_adult_name(name: &str) -> bool {
    let n = name.to_uppercase();
    ["ADULT", "XXX", "18+", "PORN", "FOR ADULTS"].iter().any(|k| n.contains(k))
}

fn clear_catalog(tx: &Transaction, id: i64) -> Result<()> {
    for table in ["category", "channel", "movie", "series"] {
        tx.execute(&format!("DELETE FROM {table} WHERE source_id = ?1"), [id])?;
    }
    tx.execute("DELETE FROM search WHERE source_id = ?1", [id])?;
    Ok(())
}

/// Provider detail payloads survive re-syncs ("Up next" and resume read the
/// cached series details); only entries whose item left the catalog go.
fn prune_detail_cache(tx: &Transaction, id: i64) -> Result<()> {
    tx.execute(
        "DELETE FROM detail_cache WHERE source_id = ?1 AND NOT EXISTS (
             SELECT 1 FROM movie m WHERE detail_cache.kind = 'movie' AND m.source_id = ?1 AND m.id = detail_cache.id
             UNION ALL
             SELECT 1 FROM series s WHERE detail_cache.kind = 'series' AND s.source_id = ?1 AND s.id = detail_cache.id)",
        [id],
    )?;
    Ok(())
}

fn insert_categories(tx: &Transaction, id: i64, kind: &str, cats: &[Value]) -> Result<HashSet<String>> {
    let mut stmt = tx.prepare(
        "INSERT OR REPLACE INTO category (source_id, kind, id, name, title, region, badges, adult, position)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
    )?;
    let mut adult = HashSet::new();
    for (pos, c) in cats.iter().enumerate() {
        let Some(cid) = str_of(&c["category_id"]) else { continue };
        let name = names::unescape(&str_of(&c["category_name"]).unwrap_or_default());
        let cleaned = names::category(&name);
        let is_adult = is_adult_name(&name);
        if is_adult {
            adult.insert(cid.clone());
        }
        stmt.execute(params![
            id,
            kind,
            cid,
            name,
            cleaned.title,
            cleaned.region,
            names::badges_str(&cleaned.badges),
            is_adult,
            pos as i64
        ])?;
    }
    Ok(adult)
}

fn write_xtream(conn: &mut Connection, id: i64, l: XtreamLists) -> Result<()> {
    let tx = conn.transaction()?;
    clear_catalog(&tx, id)?;
    let adult_live = insert_categories(&tx, id, "live", &l.lc)?;
    let adult_vod = insert_categories(&tx, id, "movie", &l.vc)?;
    let adult_series = insert_categories(&tx, id, "series", &l.sc)?;

    let mut search = tx.prepare("INSERT INTO search (title, kind, source_id, item_id) VALUES (?1, ?2, ?3, ?4)")?;
    {
        let mut stmt = tx.prepare(
            "INSERT OR REPLACE INTO channel (source_id, id, num, name, title, logo, epg_id, category_id,
                archive, archive_days, separator, badges, adult, added, position)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
        )?;
        for (pos, s) in l.ls.iter().enumerate() {
            let Some(sid) = str_of(&s["stream_id"]) else { continue };
            let name = names::unescape(&str_of(&s["name"]).unwrap_or_default());
            let separator = names::is_separator(&name);
            let cleaned = names::channel(&name);
            let cat = str_of(&s["category_id"]);
            let adult = i64_of(&s["is_adult"]) == Some(1) || cat.as_ref().is_some_and(|c| adult_live.contains(c));
            stmt.execute(params![
                id,
                sid,
                i64_of(&s["num"]),
                name,
                cleaned.title,
                str_of(&s["stream_icon"]),
                str_of(&s["epg_channel_id"]),
                cat,
                i64_of(&s["tv_archive"]).unwrap_or(0) == 1,
                i64_of(&s["tv_archive_duration"]).unwrap_or(0),
                separator,
                names::badges_str(&cleaned.badges),
                adult,
                i64_of(&s["added"]),
                pos as i64
            ])?;
            if !separator {
                search.execute(params![cleaned.title, "live", id, sid])?;
            }
        }
    }
    {
        let mut stmt = tx.prepare(
            "INSERT OR REPLACE INTO movie (source_id, id, name, title, tag, year, poster, rating, tmdb, trailer,
                added, category_id, ext, adult, position)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
        )?;
        for (pos, m) in l.vs.iter().enumerate() {
            let Some(sid) = str_of(&m["stream_id"]) else { continue };
            let name = names::unescape(&str_of(&m["name"]).unwrap_or_default());
            let t = names::title(&name);
            let cat = str_of(&m["category_id"]);
            let adult = i64_of(&m["is_adult"]) == Some(1) || cat.as_ref().is_some_and(|c| adult_vod.contains(c));
            stmt.execute(params![
                id,
                sid,
                name,
                t.title,
                t.tag,
                t.year,
                str_of(&m["stream_icon"]),
                rating_of(&m["rating"]),
                str_of(&m["tmdb"]),
                str_of(&m["trailer"]),
                i64_of(&m["added"]),
                cat,
                str_of(&m["container_extension"]),
                adult,
                pos as i64
            ])?;
            search.execute(params![t.title, "movie", id, sid])?;
        }
    }
    {
        let mut stmt = tx.prepare(
            "INSERT OR REPLACE INTO series (source_id, id, name, title, tag, year, cover, backdrop, plot, cast_list,
                director, genre, release_date, rating, tmdb, trailer, last_modified, category_id, adult, position)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20)",
        )?;
        for (pos, s) in l.ss.iter().enumerate() {
            let Some(sid) = str_of(&s["series_id"]) else { continue };
            let name = names::unescape(&str_of(&s["name"]).unwrap_or_default());
            let t = names::title(&name);
            let release = first_str(s, &["releaseDate", "release_date"]);
            let year = t.year.or_else(|| release.as_deref().and_then(|r| r.get(..4)?.parse().ok()));
            let backdrop = match &s["backdrop_path"] {
                Value::Array(a) => a.iter().find_map(str_of),
                v => str_of(v),
            };
            let cat = str_of(&s["category_id"]);
            let adult = cat.as_ref().is_some_and(|c| adult_series.contains(c));
            stmt.execute(params![
                id,
                sid,
                name,
                t.title,
                t.tag,
                year,
                str_of(&s["cover"]),
                backdrop,
                str_of(&s["plot"]),
                str_of(&s["cast"]),
                str_of(&s["director"]),
                str_of(&s["genre"]),
                release,
                rating_of(&s["rating"]),
                str_of(&s["tmdb"]),
                str_of(&s["youtube_trailer"]),
                i64_of(&s["last_modified"]),
                cat,
                adult,
                pos as i64
            ])?;
            search.execute(params![t.title, "series", id, sid])?;
        }
    }
    drop(search);
    prune_detail_cache(&tx, id)?;
    tx.commit()?;
    Ok(())
}

fn write_m3u(conn: &mut Connection, id: i64, pl: &m3u::Playlist) -> Result<()> {
    let tx = conn.transaction()?;
    clear_catalog(&tx, id)?;

    // categories come from group-title, separately for live and VOD
    let mut cats: HashMap<(&'static str, String), String> = HashMap::new();
    let mut cat_stmt = tx.prepare(
        "INSERT OR REPLACE INTO category (source_id, kind, id, name, title, region, badges, adult, position)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
    )?;
    let mut ch_stmt = tx.prepare(
        "INSERT OR REPLACE INTO channel (source_id, id, num, name, title, logo, epg_id, category_id,
            archive, archive_days, url, separator, badges, adult, position)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
    )?;
    let mut mv_stmt = tx.prepare(
        "INSERT OR REPLACE INTO movie (source_id, id, name, title, tag, year, poster, category_id, ext, url, adult, position)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
    )?;
    let mut search = tx.prepare("INSERT INTO search (title, kind, source_id, item_id) VALUES (?1, ?2, ?3, ?4)")?;
    let mut seen: HashSet<String> = HashSet::new();

    for (pos, e) in pl.entries.iter().enumerate() {
        let kind = match e.kind() {
            m3u::EntryKind::Live => "live",
            _ => "movie",
        };
        let group = e.group.clone().unwrap_or_else(|| "Uncategorized".into());
        let next_pos = cats.len() as i64;
        let cat_id = cats
            .entry((kind, group.clone()))
            .or_insert_with(|| {
                let cid = format!("{:x}", fnv1a(group.as_bytes()));
                let c = names::category(&group);
                let _ = cat_stmt.execute(params![
                    id,
                    kind,
                    cid,
                    group,
                    c.title,
                    c.region,
                    names::badges_str(&c.badges),
                    is_adult_name(&group),
                    next_pos
                ]);
                cid
            })
            .clone();
        let adult = is_adult_name(&group);

        // ids must survive re-syncs (favorites/history key on them), and
        // tokenized URLs change, so hash the identity rather than the URL
        let key = format!("{kind}\0{group}\0{}\0{}", e.name, e.tvg_id.as_deref().unwrap_or(""));
        let mut item_id = format!("{:x}", fnv1a(key.as_bytes()));
        let mut n = 1;
        while !seen.insert(item_id.clone()) {
            item_id = format!("{:x}", fnv1a(format!("{key}\0{n}").as_bytes()));
            n += 1;
        }

        if kind == "live" {
            let separator = names::is_separator(&e.name);
            let cleaned = names::channel(&e.name);
            ch_stmt.execute(params![
                id,
                item_id,
                e.chno,
                e.name,
                cleaned.title,
                e.logo,
                e.tvg_id,
                cat_id,
                // `play` can't build M3U catch-up URLs yet (WORKLOG T-039), so
                // don't offer it; the advertised days are kept for later.
                false,
                e.catchup_days.unwrap_or(0),
                e.url,
                separator,
                names::badges_str(&cleaned.badges),
                adult,
                pos as i64
            ])?;
            if !separator {
                search.execute(params![cleaned.title, "live", id, item_id])?;
            }
        } else {
            let t = names::title(&e.name);
            let ext = e.url.split(['?', '#']).next().and_then(|p| p.rsplit_once('.')).map(|(_, x)| x.to_owned());
            mv_stmt.execute(params![
                id, item_id, e.name, t.title, t.tag, t.year, e.logo, cat_id, ext, e.url, adult, pos as i64
            ])?;
            search.execute(params![t.title, "movie", id, item_id])?;
        }
    }
    drop((cat_stmt, ch_stmt, mv_stmt, search));
    prune_detail_cache(&tx, id)?;
    tx.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_conn;
    use serde_json::json;

    fn series_lists(ids: &[&str]) -> XtreamLists {
        XtreamLists {
            lc: vec![],
            vc: vec![],
            sc: vec![json!({"category_id": "1", "category_name": "EN| DRAMA"})],
            ls: vec![],
            vs: vec![],
            ss: ids.iter().map(|id| json!({"series_id": id, "name": format!("EN - Show {id} (2024)"), "category_id": "1"})).collect(),
        }
    }

    #[test]
    fn resync_keeps_details_of_items_still_listed() {
        let mut c = test_conn();
        write_xtream(&mut c, 1, series_lists(&["10", "11"])).unwrap();
        for id in ["10", "11"] {
            c.execute("INSERT INTO detail_cache (source_id, kind, id, json, fetched_at) VALUES (1, 'series', ?1, '{}', 0)", [id])
                .unwrap();
        }
        // "11" left the provider's catalog
        write_xtream(&mut c, 1, series_lists(&["10"])).unwrap();
        let kept: Vec<String> =
            c.prepare("SELECT id FROM detail_cache").unwrap().query_map([], |r| r.get(0)).unwrap().map(|r| r.unwrap()).collect();
        assert_eq!(kept, vec!["10"]);
        let title: String = c.query_row("SELECT title FROM series WHERE id = '10'", [], |r| r.get(0)).unwrap();
        assert_eq!(title, "Show 10");
    }

    #[test]
    fn m3u_channels_do_not_offer_catchup_yet() {
        let mut c = test_conn();
        let pl = m3u::parse("#EXTM3U\n#EXTINF:-1 catchup-days=\"7\" group-title=\"News\",News 24\nhttp://h/news.m3u8\n");
        write_m3u(&mut c, 1, &pl).unwrap();
        let row: (bool, i64) = c.query_row("SELECT archive, archive_days FROM channel", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!(row, (false, 7));
    }
}
