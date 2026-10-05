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
        Ok(Db {
            writer: Mutex::new(writer),
            readers,
        })
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
    // v7 — grouping for browsing (works/): one work per movie/series across
    // provider copies, one group per live channel across stream variants,
    // remembered choices, and track info learned from the files
    r#"
    ALTER TABLE movie ADD COLUMN work_key TEXT;
    ALTER TABLE series ADD COLUMN work_key TEXT;
    ALTER TABLE channel ADD COLUMN group_key TEXT;
    CREATE INDEX movie_by_work ON movie(work_key);
    CREATE INDEX series_by_work ON series(work_key);
    CREATE INDEX channel_by_group ON channel(group_key);

    CREATE TABLE work (
        kind      TEXT NOT NULL,                 -- 'movie' | 'series'
        key       TEXT NOT NULL,                 -- 'tmdb:<id>' | 'title:<norm>|<year>' | 'item:<source>:<id>'
        title     TEXT NOT NULL,
        year      INTEGER,
        poster    TEXT,
        backdrop  TEXT,
        rating    REAL,
        genre     TEXT,                          -- canonical genres, ', ' separated
        added     INTEGER,                       -- newest member
        versions  INTEGER NOT NULL,
        badges    TEXT NOT NULL DEFAULT '',      -- '4K|Dolby Vision|…' over all members
        services  TEXT NOT NULL DEFAULT '',      -- 'Netflix|Apple TV+|…'
        adult     INTEGER NOT NULL DEFAULT 0,
        source_id INTEGER NOT NULL,              -- representative member
        item_id   TEXT NOT NULL,
        PRIMARY KEY (kind, key)
    ) WITHOUT ROWID;
    CREATE INDEX work_by_added ON work(kind, added DESC);
    CREATE INDEX work_by_title ON work(kind, title COLLATE NOCASE);

    CREATE TABLE work_facet (
        kind  TEXT NOT NULL,
        facet TEXT NOT NULL,                     -- service | language | quality | genre | decade | collection
        value TEXT NOT NULL,
        key   TEXT NOT NULL,
        PRIMARY KEY (kind, facet, value, key)
    ) WITHOUT ROWID;
    CREATE INDEX work_facet_by_key ON work_facet(kind, key);

    CREATE TABLE channel_group (
        key       TEXT PRIMARY KEY,              -- '<country>|<normalized title>'
        title     TEXT NOT NULL,
        country   TEXT,
        genre     TEXT NOT NULL,
        logo      TEXT,
        epg_id    TEXT,
        variants  INTEGER NOT NULL,
        adult     INTEGER NOT NULL DEFAULT 0,
        position  INTEGER NOT NULL,
        source_id INTEGER NOT NULL,              -- default variant
        item_id   TEXT NOT NULL
    ) WITHOUT ROWID;
    CREATE INDEX channel_group_nav ON channel_group(country, genre, position);
    CREATE INDEX channel_group_by_genre ON channel_group(genre, position);

    -- the version the user picked (survives re-syncs: keyed by work/group)
    CREATE TABLE work_pref (
        kind       TEXT NOT NULL,
        key        TEXT NOT NULL,
        source_id  INTEGER NOT NULL,
        item_id    TEXT NOT NULL,
        updated_at INTEGER NOT NULL,
        PRIMARY KEY (kind, key)
    ) WITHOUT ROWID;
    CREATE TABLE channel_pref (
        key        TEXT PRIMARY KEY,
        source_id  INTEGER NOT NULL,
        item_id    TEXT NOT NULL,
        updated_at INTEGER NOT NULL
    ) WITHOUT ROWID;

    -- audio/subtitle/video tracks seen in a file (player or probe)
    CREATE TABLE media_info (
        source_id  INTEGER NOT NULL REFERENCES source(id) ON DELETE CASCADE,
        kind       TEXT NOT NULL,                -- 'movie' | 'episode'
        item_id    TEXT NOT NULL,
        json       TEXT NOT NULL,
        updated_at INTEGER NOT NULL,
        PRIMARY KEY (source_id, kind, item_id)
    ) WITHOUT ROWID;
    "#,
    // v8: TMDB details of the catalog's titles (tmdb.rs), shared by all sources
    r#"
    CREATE TABLE tmdb (
        kind       TEXT NOT NULL,                -- 'movie' | 'tv'
        id         TEXT NOT NULL,                -- TMDB id
        json       TEXT,                         -- tmdb::Info; NULL = not on TMDB
        fetched_at INTEGER NOT NULL,
        PRIMARY KEY (kind, id)
    ) WITHOUT ROWID;
    "#,
    // v9 — TMDB ids found by title search for works the provider did not id
    // (tmdb.rs); tmdb_id '' = searched, no acceptable match ("miss" sentinel).
    // The last change-list date the run covered is setting tmdb.changes_since.
    r#"
    CREATE TABLE tmdb_map (
        kind        TEXT NOT NULL,               -- 'movie' | 'tv'
        source_key  TEXT NOT NULL,               -- work key: 'title:<norm>|<year>' | 'item:<source>:<id>'
        tmdb_id     TEXT NOT NULL,
        searched_at INTEGER NOT NULL,
        PRIMARY KEY (kind, source_key)
    ) WITHOUT ROWID;
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

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::OptionalExtension;

    /// v8 → v9 upgrade: applying migrations 1..8 must leave no tmdb_map, and
    /// the upgrade must add it keyed by (kind, source_key).
    #[test]
    fn v9_upgrade_adds_tmdb_map() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        for sql in &MIGRATIONS[..8] {
            c.execute_batch(sql).unwrap();
        }
        c.execute_batch("PRAGMA user_version = 8").unwrap();
        let exists = |c: &Connection| -> bool {
            c.query_row(
                "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'tmdb_map'",
                [],
                |_| Ok(()),
            )
            .optional()
            .unwrap()
            .is_some()
        };
        assert!(!exists(&c));
        migrate(&c).unwrap();
        let version: i64 = c
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version as usize, MIGRATIONS.len());
        assert!(exists(&c));
        let pk: Vec<String> = c
            .prepare("SELECT name FROM pragma_table_info('tmdb_map') WHERE pk > 0 ORDER BY pk")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(pk, ["kind", "source_key"]);
    }
}
