//! IPTV sources (Xtream Codes accounts and M3U playlists): storage, sync
//! into the local catalog, and EPG refresh.

mod commands;
pub mod m3u;
mod store;
mod sync;
pub mod xtream;

pub use commands::*;
use store::{XtreamLists, write_m3u, write_xtream};
pub use sync::*;

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{AppHandle, Emitter, Runtime, State};

use crate::db::now;
use crate::error::{Error, Result};
use crate::kind::ItemKind;
use crate::names;
use crate::state::{AppState, http_client, read_blocking, run_blocking, write_blocking};
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
        if s == "m3u" {
            SourceKind::M3u
        } else {
            SourceKind::Xtream
        }
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
    /// Catch-up time correction (T-048).
    pub catchup_shift_minutes: i64,
    /// The password lives in the desktop keyring (secrets.rs), not here.
    pub password_in_keyring: bool,
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
    pub catchup_shift_minutes: i64,
    pub password_in_keyring: bool,
    /// In the keyring, but the keyring was locked or unavailable so far.
    pub password_locked: bool,
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
    /// Catch-up time correction in minutes; `None` keeps the stored value.
    pub catchup_shift_minutes: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    tag = "stage"
)]
pub enum SyncProgress {
    Started { source_id: i64 },
    Step { source_id: i64, message: String },
    Done { source_id: i64, counts: Counts },
    Failed { source_id: i64, error: String },
}

const SOURCE_COLS: &str = "id, kind, name, url, alt_urls, username, password, epg_url, user_agent, \
     created_at, last_sync, last_epg_sync, sync_error, account_json, catchup_shift_minutes, password_in_keyring";

fn row_to_source(r: &rusqlite::Row) -> rusqlite::Result<SourceRow> {
    let alt: String = r.get(4)?;
    let account: Option<String> = r.get(13)?;
    let id: i64 = r.get(0)?;
    let password_in_keyring: bool = r.get(15)?;
    Ok(SourceRow {
        id,
        kind: SourceKind::parse(&r.get::<_, String>(1)?),
        name: r.get(2)?,
        url: r.get(3)?,
        alt_urls: serde_json::from_str(&alt).unwrap_or_default(),
        username: r.get(5)?,
        // keyring passwords come from the cache secrets::startup filled
        password: if password_in_keyring {
            crate::secrets::cached(id)
        } else {
            r.get(6)?
        },
        epg_url: r.get(7)?,
        user_agent: r.get(8)?,
        created_at: r.get(9)?,
        last_sync: r.get(10)?,
        last_epg_sync: r.get(11)?,
        sync_error: r.get(12)?,
        account: account.and_then(|a| serde_json::from_str(&a).ok()),
        catchup_shift_minutes: r.get(14)?,
        password_in_keyring,
    })
}

pub fn load(conn: &Connection, id: i64) -> Result<SourceRow> {
    conn.query_row(
        &format!("SELECT {SOURCE_COLS} FROM source WHERE id = ?1"),
        [id],
        row_to_source,
    )
    .optional()?
    .ok_or_else(|| Error::NotFound(format!("source {id}")))
}

pub fn load_all(conn: &Connection) -> Result<Vec<SourceRow>> {
    let mut stmt = conn.prepare(&format!("SELECT {SOURCE_COLS} FROM source ORDER BY id"))?;
    let rows = stmt
        .query_map([], row_to_source)?
        .collect::<Result<Vec<_>, _>>()?;
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
        has_password: s.password_in_keyring || s.password.as_deref().is_some_and(|p| !p.is_empty()),
        epg_url: s.epg_url,
        user_agent: s.user_agent,
        created_at: s.created_at,
        last_sync: s.last_sync,
        last_epg_sync: s.last_epg_sync,
        sync_error: s.sync_error,
        account: s.account,
        catchup_shift_minutes: s.catchup_shift_minutes,
        password_in_keyring: s.password_in_keyring,
        password_locked: s.password_in_keyring && s.password.is_none(),
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
        && let Some((base, user, pass)) = xtream::parse_playlist_url(&input.url)
    {
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
        .map(|u| {
            if input.kind == SourceKind::Xtream {
                xtream::normalize_base(u)
            } else {
                u.to_owned()
            }
        })
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
