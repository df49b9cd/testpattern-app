//! SQLite storage: the synced catalog (channels, movies, series, EPG), the
//! user's library (favorites, watch progress) and settings.
//!
//! One writer connection plus a few readers in WAL mode, so browsing stays
//! responsive while a large sync or EPG import is being written.

use std::path::Path;

use parking_lot::{Mutex, MutexGuard};
use rusqlite::Connection;

use crate::error::Result;

const READERS: usize = 4;

pub struct Db {
    writer: Mutex<Connection>,
    readers: Vec<Mutex<Connection>>,
}

impl Db {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let writer = Connection::open(path)?;
        configure(&writer)?;
        migrate(&writer)?;
        let readers = (0..READERS)
            .map(|_| {
                let c = Connection::open(path)?;
                configure(&c)?;
                c.execute_batch("PRAGMA query_only = ON;")?;
                Ok(Mutex::new(c))
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Db { writer: Mutex::new(writer), readers })
    }

    /// Exclusive access to the writer connection.
    pub fn write(&self) -> MutexGuard<'_, Connection> {
        self.writer.lock()
    }

    /// A free reader connection (blocks only if all readers are busy).
    pub fn read(&self) -> MutexGuard<'_, Connection> {
        for r in &self.readers {
            if let Some(guard) = r.try_lock() {
                return guard;
            }
        }
        self.readers[0].lock()
    }
}

fn configure(c: &Connection) -> Result<()> {
    c.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = NORMAL;
         PRAGMA foreign_keys = ON;
         PRAGMA temp_store = MEMORY;
         PRAGMA cache_size = -32000;
         PRAGMA busy_timeout = 5000;",
    )?;
    Ok(())
}

const MIGRATIONS: &[&str] = &[
    // v1 — initial schema
    r#"
    CREATE TABLE source (
        id            INTEGER PRIMARY KEY,
        kind          TEXT NOT NULL,             -- 'xtream' | 'm3u'
        name          TEXT NOT NULL,
        url           TEXT NOT NULL,             -- server base url or playlist url
        alt_urls      TEXT NOT NULL DEFAULT '[]',-- JSON array of mirror base urls
        username      TEXT,
        password      TEXT,
        epg_url       TEXT,
        user_agent    TEXT,
        created_at    INTEGER NOT NULL,
        last_sync     INTEGER,
        last_epg_sync INTEGER,
        sync_error    TEXT,
        account_json  TEXT
    );

    CREATE TABLE category (
        source_id INTEGER NOT NULL REFERENCES source(id) ON DELETE CASCADE,
        kind      TEXT NOT NULL,                 -- 'live' | 'movie' | 'series'
        id        TEXT NOT NULL,
        name      TEXT NOT NULL,                 -- raw provider name
        title     TEXT NOT NULL,                 -- cleaned display name
        region    TEXT,
        badges    TEXT NOT NULL DEFAULT '',      -- space separated quality tags
        adult     INTEGER NOT NULL DEFAULT 0,
        position  INTEGER NOT NULL,
        PRIMARY KEY (source_id, kind, id)
    ) WITHOUT ROWID;

    CREATE TABLE channel (
        source_id    INTEGER NOT NULL REFERENCES source(id) ON DELETE CASCADE,
        id           TEXT NOT NULL,
        num          INTEGER,
        name         TEXT NOT NULL,
        title        TEXT NOT NULL,
        logo         TEXT,
        epg_id       TEXT,
        category_id  TEXT,
        archive      INTEGER NOT NULL DEFAULT 0,
        archive_days INTEGER NOT NULL DEFAULT 0,
        url          TEXT,                       -- direct url (m3u sources)
        separator    INTEGER NOT NULL DEFAULT 0,
        badges       TEXT NOT NULL DEFAULT '',
        adult        INTEGER NOT NULL DEFAULT 0,
        added        INTEGER,
        position     INTEGER NOT NULL,
        PRIMARY KEY (source_id, id)
    ) WITHOUT ROWID;
    CREATE INDEX channel_by_category ON channel(source_id, category_id, position);
    CREATE INDEX channel_by_epg ON channel(epg_id);

    CREATE TABLE movie (
        source_id   INTEGER NOT NULL REFERENCES source(id) ON DELETE CASCADE,
        id          TEXT NOT NULL,
        name        TEXT NOT NULL,
        title       TEXT NOT NULL,
        tag         TEXT,                        -- provider prefix, e.g. 'EN', 'NF'
        year        INTEGER,
        poster      TEXT,
        rating      REAL,
        tmdb        TEXT,
        trailer     TEXT,
        added       INTEGER,
        category_id TEXT,
        ext         TEXT,
        url         TEXT,
        adult       INTEGER NOT NULL DEFAULT 0,
        position    INTEGER NOT NULL,
        PRIMARY KEY (source_id, id)
    ) WITHOUT ROWID;
    CREATE INDEX movie_by_category ON movie(source_id, category_id, position);
    CREATE INDEX movie_by_added ON movie(added DESC);

    CREATE TABLE series (
        source_id     INTEGER NOT NULL REFERENCES source(id) ON DELETE CASCADE,
        id            TEXT NOT NULL,
        name          TEXT NOT NULL,
        title         TEXT NOT NULL,
        tag           TEXT,
        year          INTEGER,
        cover         TEXT,
        backdrop      TEXT,
        plot          TEXT,
        cast_list     TEXT,
        director      TEXT,
        genre         TEXT,
        release_date  TEXT,
        rating        REAL,
        tmdb          TEXT,
        trailer       TEXT,
        last_modified INTEGER,
        category_id   TEXT,
        adult         INTEGER NOT NULL DEFAULT 0,
        position      INTEGER NOT NULL,
        PRIMARY KEY (source_id, id)
    ) WITHOUT ROWID;
    CREATE INDEX series_by_category ON series(source_id, category_id, position);
    CREATE INDEX series_by_modified ON series(last_modified DESC);

    -- provider detail payloads (get_vod_info / get_series_info)
    CREATE TABLE detail_cache (
        source_id  INTEGER NOT NULL REFERENCES source(id) ON DELETE CASCADE,
        kind       TEXT NOT NULL,
        id         TEXT NOT NULL,
        json       TEXT NOT NULL,
        fetched_at INTEGER NOT NULL,
        PRIMARY KEY (source_id, kind, id)
    ) WITHOUT ROWID;

    CREATE TABLE programme (
        source_id   INTEGER NOT NULL REFERENCES source(id) ON DELETE CASCADE,
        epg_id      TEXT NOT NULL,
        start       INTEGER NOT NULL,
        stop        INTEGER NOT NULL,
        title       TEXT NOT NULL,
        subtitle    TEXT,
        description TEXT,
        category    TEXT,
        episode     TEXT,
        icon        TEXT,
        PRIMARY KEY (source_id, epg_id, start)
    ) WITHOUT ROWID;
    CREATE INDEX programme_by_stop ON programme(stop);

    CREATE TABLE favorite (
        source_id INTEGER NOT NULL REFERENCES source(id) ON DELETE CASCADE,
        kind      TEXT NOT NULL,
        item_id   TEXT NOT NULL,
        added_at  INTEGER NOT NULL,
        PRIMARY KEY (source_id, kind, item_id)
    ) WITHOUT ROWID;

    -- watch progress for movies/episodes, recently watched channels
    CREATE TABLE history (
        source_id  INTEGER NOT NULL REFERENCES source(id) ON DELETE CASCADE,
        kind       TEXT NOT NULL,                -- 'live' | 'movie' | 'episode'
        item_id    TEXT NOT NULL,
        series_id  TEXT,
        season     INTEGER,
        episode    INTEGER,
        title      TEXT NOT NULL,
        subtitle   TEXT,
        image      TEXT,
        backdrop   TEXT,
        ext        TEXT,
        position   REAL NOT NULL DEFAULT 0,
        duration   REAL NOT NULL DEFAULT 0,
        watched    INTEGER NOT NULL DEFAULT 0,
        updated_at INTEGER NOT NULL,
        PRIMARY KEY (source_id, kind, item_id)
    ) WITHOUT ROWID;
    CREATE INDEX history_recent ON history(updated_at DESC);

    CREATE TABLE setting (
        key   TEXT PRIMARY KEY,
        value TEXT NOT NULL
    ) WITHOUT ROWID;

    CREATE VIRTUAL TABLE search USING fts5(
        title,
        kind UNINDEXED,
        source_id UNINDEXED,
        item_id UNINDEXED,
        tokenize = 'unicode61 remove_diacritics 2',
        prefix = '2 3 4'
    );
    "#,
    // v2 — M3U catch-up scheme per channel (sources::m3u::catchup_url)
    r#"
    ALTER TABLE channel ADD COLUMN catchup_mode TEXT;
    ALTER TABLE channel ADD COLUMN catchup_source TEXT;
    "#,
    // v3 — request headers some M3U streams need (sources::m3u::Entry)
    r#"
    ALTER TABLE channel ADD COLUMN user_agent TEXT;
    ALTER TABLE channel ADD COLUMN referrer TEXT;
    ALTER TABLE movie ADD COLUMN user_agent TEXT;
    ALTER TABLE movie ADD COLUMN referrer TEXT;
    "#,
    // v4 — per-source catch-up time correction (T-048)
    r#"
    ALTER TABLE source ADD COLUMN catchup_shift_minutes INTEGER NOT NULL DEFAULT 0;
    "#,
    // v5 — episodes of M3U series (Xtream series come from get_series_info)
    r#"
    CREATE TABLE episode (
        source_id  INTEGER NOT NULL REFERENCES source(id) ON DELETE CASCADE,
        series_id  TEXT NOT NULL,
        id         TEXT NOT NULL,
        season     INTEGER NOT NULL,
        episode    INTEGER NOT NULL,
        title      TEXT NOT NULL,
        image      TEXT,
        ext        TEXT,
        url        TEXT NOT NULL,
        user_agent TEXT,
        referrer   TEXT,
        position   INTEGER NOT NULL,
        PRIMARY KEY (source_id, id)
    ) WITHOUT ROWID;
    CREATE INDEX episode_by_series ON episode(source_id, series_id, season, episode);
    "#,
    // v6 — source passwords in the desktop keyring (secrets.rs)
    r#"
    ALTER TABLE source ADD COLUMN password_in_keyring INTEGER NOT NULL DEFAULT 0;
    "#,
];

fn migrate(c: &Connection) -> Result<()> {
    let version: i64 = c.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    let version = version.max(0) as usize;
    for (i, sql) in MIGRATIONS.iter().enumerate().skip(version) {
        let tx = c.unchecked_transaction()?;
        tx.execute_batch(sql)?;
        tx.execute_batch(&format!("PRAGMA user_version = {}", i + 1))?;
        tx.commit()?;
        log::info!("database migrated to v{}", i + 1);
    }
    Ok(())
}

/// Unit tests: an in-memory database with the full schema and source 1.
#[cfg(test)]
pub fn test_conn() -> Connection {
    let c = Connection::open_in_memory().unwrap();
    c.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
    migrate(&c).unwrap();
    c.execute("INSERT INTO source (id, kind, name, url, created_at) VALUES (1, 'xtream', 'test', 'http://h.tv', 0)", [])
        .unwrap();
    c
}

pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default()
}
