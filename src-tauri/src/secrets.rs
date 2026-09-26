//! Source passwords in the desktop keyring — the Secret Service API that
//! KWallet and GNOME Keyring provide — with the database as the fallback
//! when no keyring is available (T-046).
//!
//! The rest of the app keeps reading passwords through `sources::load`: this
//! module fills an in-memory cache from the keyring (`startup`,
//! `ensure_loaded`) and whenever a password is stored. Prompts: storing a
//! password the user just typed, or a sync the user started, may show the
//! keyring's unlock prompt; background work never does — existing plaintext
//! passwords are moved over only while the keyring is unlocked.

use std::collections::HashMap;
use std::future::Future;
use std::sync::LazyLock;
use std::time::Duration;

use parking_lot::Mutex;
use rusqlite::{Connection, OptionalExtension};

use crate::error::{Error, Result};
use crate::state::AppState;

/// Shown when a source's password is in the keyring but couldn't be read.
pub const UNAVAILABLE: &str = "The password is stored in the system keyring, which is locked or not available. \
     Unlock the keyring (KWallet, GNOME Keyring) and sync again, or enter the password again.";

/// Passwords read from the keyring, by source id.
static CACHE: LazyLock<Mutex<HashMap<i64, String>>> = LazyLock::new(Default::default);

/// Password of a source whose secret lives in the keyring (None: not loaded —
/// keyring locked or unavailable).
pub fn cached(source_id: i64) -> Option<String> {
    CACHE.lock().get(&source_id).cloned()
}

fn remember(source_id: i64, password: &str) {
    CACHE.lock().insert(source_id, password.to_owned());
}

pub fn uncache(source_id: i64) {
    CACHE.lock().remove(&source_id);
}

/// May the keyring show its unlock prompt for this operation?
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unlock {
    /// The user is waiting on this (just typed the password, pressed Sync).
    Prompt,
    /// Background work: a locked keyring is skipped instead.
    Never,
}

impl Unlock {
    /// An unanswered prompt must not stall the app: after this it carries on
    /// without the keyring (the prompt stays open; later syncs try again).
    /// Calls that can't prompt only wait for a keyring that is starting up.
    fn timeout(self) -> Duration {
        match self {
            Unlock::Prompt => Duration::from_secs(90),
            Unlock::Never => Duration::from_secs(20),
        }
    }
}

async fn timed<T>(unlock: Unlock, call: impl Future<Output = Result<T>>) -> Result<T> {
    tokio::time::timeout(unlock.timeout(), call)
        .await
        .map_err(|_| Error::msg("keyring: no answer in time"))?
}

/// A random id per profile, so two profiles (e.g. the test one) never share
/// keyring entries for "source 1".
fn profile_id(conn: &Connection) -> Result<String> {
    let stored: Option<String> =
        conn.query_row("SELECT value FROM setting WHERE key = 'secrets.profile'", [], |r| r.get(0)).optional()?;
    if let Some(v) = stored.and_then(|v| serde_json::from_str::<String>(&v).ok()) {
        return Ok(v);
    }
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos());
    let id = format!("{:016x}", h.finish());
    conn.execute(
        "INSERT OR REPLACE INTO setting (key, value) VALUES ('secrets.profile', ?1)",
        [serde_json::to_string(&id)?],
    )?;
    Ok(id)
}

#[cfg(target_os = "linux")]
mod backend {
    use std::collections::HashMap;

    use secret_service::{EncryptionType, SecretService};

    use super::Unlock;
    use crate::error::{Error, Result};

    fn err(e: secret_service::Error) -> Error {
        Error::msg(format!("keyring: {e}"))
    }

    fn attributes<'a>(profile: &'a str, source: &'a str) -> HashMap<&'a str, &'a str> {
        HashMap::from([("application", "testpattern"), ("profile", profile), ("source", source)])
    }

    /// Encrypted transfer where the keyring offers it; plain otherwise (the
    /// secret then still only crosses the local session bus).
    async fn connect() -> Result<SecretService<'static>> {
        match SecretService::connect(EncryptionType::Dh).await {
            Ok(ss) => Ok(ss),
            Err(_) => SecretService::connect(EncryptionType::Plain).await.map_err(err),
        }
    }

    /// Stores (replaces) the secret; Ok(false) when the keyring is locked and
    /// `unlock` forbids prompting.
    pub async fn store(profile: &str, source_id: i64, label: &str, password: &str, unlock: Unlock) -> Result<bool> {
        let ss = connect().await?;
        let collection = ss.get_default_collection().await.map_err(err)?;
        if collection.is_locked().await.map_err(err)? {
            if unlock == Unlock::Never {
                return Ok(false);
            }
            collection.unlock().await.map_err(err)?;
        }
        let source = source_id.to_string();
        collection
            .create_item(label, attributes(profile, &source), password.as_bytes(), true, "text/plain")
            .await
            .map_err(err)?;
        Ok(true)
    }

    /// The secret, or Ok(None) when there is no entry. A locked keyring is
    /// unlocked when `unlock` allows it.
    pub async fn load(profile: &str, source_id: i64, unlock: Unlock) -> Result<Option<String>> {
        let ss = connect().await?;
        let source = source_id.to_string();
        let found = ss.search_items(attributes(profile, &source)).await.map_err(err)?;
        let item = match (found.unlocked.first(), found.locked.first()) {
            (Some(item), _) => item,
            (None, Some(_)) if unlock == Unlock::Never => return Err(Error::msg("keyring: locked")),
            (None, Some(item)) => {
                ss.unlock_all(&[item]).await.map_err(err)?;
                item
            }
            (None, None) => return Ok(None),
        };
        let secret = item.get_secret().await.map_err(err)?;
        Ok(Some(String::from_utf8_lossy(&secret).into_owned()))
    }

    /// Removes the entry, unlocking the keyring first when `unlock` allows it.
    pub async fn delete(profile: &str, source_id: i64, unlock: Unlock) -> Result<()> {
        let ss = connect().await?;
        let source = source_id.to_string();
        let found = ss.search_items(attributes(profile, &source)).await.map_err(err)?;
        if !found.locked.is_empty() {
            if unlock == Unlock::Never {
                return Err(Error::msg("keyring: locked"));
            }
            ss.unlock_all(&found.locked.iter().collect::<Vec<_>>()).await.map_err(err)?;
        }
        for item in found.unlocked.iter().chain(&found.locked) {
            item.delete().await.map_err(err)?;
        }
        Ok(())
    }
}

#[cfg(not(target_os = "linux"))]
mod backend {
    //! No keyring integration yet (T-028): passwords stay in the database.
    use super::Unlock;
    use crate::error::{Error, Result};

    pub async fn store(_: &str, _: i64, _: &str, _: &str, _: Unlock) -> Result<bool> {
        Err(Error::msg("keyring: not supported on this platform"))
    }
    pub async fn load(_: &str, _: i64, _: Unlock) -> Result<Option<String>> {
        Err(Error::msg("keyring: not supported on this platform"))
    }
    pub async fn delete(_: &str, _: i64, _: Unlock) -> Result<()> {
        Ok(())
    }
}

/// Moves a source's password into the keyring; on success the database
/// keeps no copy. Returns whether it is in the keyring now.
pub async fn move_to_keyring(st: &AppState, source_id: i64, password: &str, unlock: Unlock) -> bool {
    let (profile, name) = {
        let conn = st.db.write();
        let Ok(profile) = profile_id(&conn) else { return false };
        let name: String = conn
            .query_row("SELECT name FROM source WHERE id = ?1", [source_id], |r| r.get(0))
            .unwrap_or_else(|_| format!("source {source_id}"));
        (profile, name)
    };
    let label = format!("testpattern: {name}");
    match timed(unlock, backend::store(&profile, source_id, &label, password, unlock)).await {
        Ok(true) => {
            remember(source_id, password);
            match clear_plaintext(&st.db.write(), source_id) {
                Ok(()) => true,
                Err(e) => {
                    log::warn!("keyring: stored source {source_id} but could not update the database: {e}");
                    false
                }
            }
        }
        Ok(false) => {
            log::info!("source {source_id}: keyring locked, password stays in the database for now");
            false
        }
        Err(e) => {
            log::info!("source {source_id}: password stays in the database ({e})");
            false
        }
    }
}

/// Drops the database copy without leaving it in freed pages or the WAL.
fn clear_plaintext(conn: &Connection, source_id: i64) -> Result<()> {
    conn.query_row("PRAGMA secure_delete = ON", [], |_| Ok(()))?;
    let updated = conn.execute("UPDATE source SET password = NULL, password_in_keyring = 1 WHERE id = ?1", [source_id]);
    conn.query_row("PRAGMA secure_delete = OFF", [], |_| Ok(()))?;
    updated?;
    // best effort: readers in the middle of a query keep older frames alive
    conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))?;
    Ok(())
}

/// Earlier rewrites of a source row (sync status, account info) leave older
/// copies of it in freed space that `secure_delete` never saw: after moving
/// existing passwords out, rebuild the file once.
fn scrub(conn: &Connection) {
    let t = std::time::Instant::now();
    match conn.execute_batch("VACUUM").and_then(|()| conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(())))
    {
        Ok(()) => log::info!("database rebuilt without old password copies ({} ms)", t.elapsed().as_millis()),
        Err(e) => log::warn!("could not rebuild the database: {e}"),
    }
}

/// Drops a source's keyring entry (source removed). May prompt: a locked
/// keyring can't delete anything.
pub async fn forget(st: &AppState, source_id: i64) {
    uncache(source_id);
    let Ok(profile) = profile_id(&st.db.write()) else { return };
    if let Err(e) = timed(Unlock::Prompt, backend::delete(&profile, source_id, Unlock::Prompt)).await {
        log::warn!("keyring: could not delete the entry of source {source_id}: {e}");
    }
}

/// Loads the keyring passwords that aren't in the cache yet (at startup, and
/// again later when the keyring was locked then) — all, or `only` one source.
pub async fn ensure_loaded(st: &AppState, only: Option<i64>, unlock: Unlock) {
    let (profile, missing) = {
        let conn = st.db.write();
        let ids: Vec<i64> = conn
            .prepare("SELECT id FROM source WHERE password_in_keyring = 1 AND (?1 IS NULL OR id = ?1)")
            .and_then(|mut s| s.query_map([only], |r| r.get(0))?.collect())
            .unwrap_or_default();
        let missing: Vec<i64> = ids.into_iter().filter(|id| cached(*id).is_none()).collect();
        if missing.is_empty() {
            return;
        }
        let Ok(profile) = profile_id(&conn) else { return };
        (profile, missing)
    };
    let mut unlock = unlock;
    for id in missing {
        match timed(unlock, backend::load(&profile, id, unlock)).await {
            Ok(Some(password)) => remember(id, &password),
            Ok(None) => log::warn!("keyring: no password stored for source {id}"),
            Err(e) => {
                log::warn!("keyring: password of source {id} unavailable ({e})");
                // one prompt per attempt: the user said no (or isn't there)
                unlock = Unlock::Never;
            }
        }
    }
}

/// At startup, before anything syncs: load the keyring passwords (may prompt
/// once — syncing needs them), then move remaining plaintext passwords over
/// while the keyring is unlocked (never prompts).
pub async fn startup(st: &AppState) {
    ensure_loaded(st, None, Unlock::Prompt).await;
    let plaintext: Vec<(i64, String)> = {
        let conn = st.db.read();
        conn.prepare("SELECT id, password FROM source WHERE password_in_keyring = 0 AND COALESCE(password, '') <> ''")
            .and_then(|mut s| s.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect())
            .unwrap_or_default()
    };
    let mut moved = 0;
    for (id, password) in plaintext {
        if !move_to_keyring(st, id, &password, Unlock::Never).await {
            break; // locked or unavailable: the others would fail the same way
        }
        log::info!("source {id}: password moved to the system keyring");
        moved += 1;
    }
    if moved > 0 {
        let st = st.clone();
        let _ = tokio::task::spawn_blocking(move || scrub(&st.db.write())).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_id_is_stable_per_profile() {
        let c = crate::db::test_conn();
        let a = profile_id(&c).unwrap();
        assert_eq!(a.len(), 16);
        assert_eq!(profile_id(&c).unwrap(), a);
        assert_ne!(profile_id(&crate::db::test_conn()).unwrap(), a);
    }

    /// Read-only look at the session's real keyring (no prompt, no writes):
    /// `cargo test --lib -- --ignored keyring_probe --nocapture`.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    #[ignore]
    async fn keyring_probe() {
        use secret_service::{EncryptionType, SecretService};
        let ss = SecretService::connect(EncryptionType::Dh).await.expect("encrypted session");
        let default = ss.get_default_collection().await.expect("default collection");
        println!("default collection {:?}, locked: {:?}", default.get_label().await, default.is_locked().await);
    }

    #[test]
    fn clearing_the_plaintext_leaves_no_copy_in_the_file() {
        let dir = std::env::temp_dir().join(format!("tp-secrets-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("library.db");
        let db = crate::db::Db::open(&path).unwrap();
        // long enough that SQLite's reuse of the freed cell wouldn't overwrite it all
        let secret = format!("pw-{}", "8c1f0e5a9d".repeat(12));
        {
            let conn = db.write();
            conn.execute(
                "INSERT INTO source (kind, name, url, password, created_at) VALUES ('xtream', 'x', 'http://h', ?1, 0)",
                [&secret],
            )
            .unwrap();
            let id = conn.last_insert_rowid();
            // a source added later sits in front of it on the page, so SQLite
            // can't simply grow over the old row when it is rewritten
            conn.execute("INSERT INTO source (kind, name, url, created_at) VALUES ('m3u', 'm', 'http://m', 0)", [])
                .unwrap();
            // what syncs do over time: rewrite the row at another size
            conn.execute("UPDATE source SET account_json = ?2 WHERE id = ?1", rusqlite::params![id, "{}".repeat(150)])
                .unwrap();
            conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(())).unwrap();
            clear_plaintext(&conn, id).unwrap();
            scrub(&conn);
            let (pw, flag): (Option<String>, bool) = conn
                .query_row("SELECT password, password_in_keyring FROM source WHERE id = ?1", [id], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })
                .unwrap();
            assert_eq!((pw, flag), (None, true));
        }
        let mut bytes = std::fs::read(&path).unwrap();
        bytes.extend(std::fs::read(dir.join("library.db-wal")).unwrap_or_default());
        let piece = &secret.as_bytes()[40..60];
        assert!(!bytes.windows(piece.len()).any(|w| w == piece), "plaintext still on disk");
        drop(db);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
