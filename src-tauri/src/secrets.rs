//! Source passwords and other app secrets (the TMDB key, `NAMED`) in the
//! desktop keyring — the Secret Service API that KWallet and GNOME Keyring
//! provide — with the database as the fallback when no keyring is available
//! (T-046).
//!
//! The rest of the app keeps reading passwords through `sources::load` and
//! secrets through `named`: this module fills in-memory caches from the
//! keyring (`startup`, `ensure_loaded`) and whenever a secret is stored.
//! Prompts: storing a secret the user just typed, or a sync the user started,
//! may show the keyring's unlock prompt; background work never does —
//! existing plaintext secrets are moved over only while the keyring is
//! unlocked.

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

/// App secrets besides source passwords that belong in the keyring: the
/// setting that holds one when there is no keyring → the entry's label.
pub const NAMED: &[(&str, &str)] = &[(crate::tmdb::KEY_SETTING, "TMDB API key")];

/// Passwords read from the keyring, by source id.
static CACHE: LazyLock<Mutex<HashMap<i64, String>>> = LazyLock::new(Default::default);
/// `NAMED` secrets read from the keyring, by setting key.
static NAMED_CACHE: LazyLock<Mutex<HashMap<String, String>>> = LazyLock::new(Default::default);

/// What a keyring entry belongs to.
#[derive(Debug, Clone, Copy)]
enum Entry<'a> {
    Source(i64),
    Named(&'a str),
}

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
    let stored: Option<String> = conn
        .query_row(
            "SELECT value FROM setting WHERE key = 'secrets.profile'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(v) = stored.and_then(|v| serde_json::from_str::<String>(&v).ok()) {
        return Ok(v);
    }
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
    );
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

    use super::{Entry, Unlock};
    use crate::error::{Error, Result};

    fn err(e: secret_service::Error) -> Error {
        Error::msg(format!("keyring: {e}"))
    }

    fn attributes(profile: &str, entry: Entry) -> Vec<(&'static str, String)> {
        let (key, value) = match entry {
            Entry::Source(id) => ("source", id.to_string()),
            Entry::Named(name) => ("name", name.to_owned()),
        };
        vec![
            ("application", "testpattern".into()),
            ("profile", profile.into()),
            (key, value),
        ]
    }

    fn map<'a>(attrs: &'a [(&'static str, String)]) -> HashMap<&'a str, &'a str> {
        attrs.iter().map(|(k, v)| (*k, v.as_str())).collect()
    }

    /// Encrypted transfer where the keyring offers it; plain otherwise (the
    /// secret then still only crosses the local session bus).
    async fn connect() -> Result<SecretService<'static>> {
        match SecretService::connect(EncryptionType::Dh).await {
            Ok(ss) => Ok(ss),
            Err(_) => SecretService::connect(EncryptionType::Plain)
                .await
                .map_err(err),
        }
    }

    /// Stores (replaces) the secret; Ok(false) when the keyring is locked and
    /// `unlock` forbids prompting.
    pub async fn store(
        profile: &str,
        entry: Entry<'_>,
        label: &str,
        password: &str,
        unlock: Unlock,
    ) -> Result<bool> {
        let ss = connect().await?;
        let collection = ss.get_default_collection().await.map_err(err)?;
        if collection.is_locked().await.map_err(err)? {
            if unlock == Unlock::Never {
                return Ok(false);
            }
            collection.unlock().await.map_err(err)?;
        }
        collection
            .create_item(
                label,
                map(&attributes(profile, entry)),
                password.as_bytes(),
                true,
                "text/plain",
            )
            .await
            .map_err(err)?;
        Ok(true)
    }

    /// The secret, or Ok(None) when there is no entry. A locked keyring is
    /// unlocked when `unlock` allows it.
    pub async fn load(profile: &str, entry: Entry<'_>, unlock: Unlock) -> Result<Option<String>> {
        let ss = connect().await?;
        let found = ss
            .search_items(map(&attributes(profile, entry)))
            .await
            .map_err(err)?;
        let item = match (found.unlocked.first(), found.locked.first()) {
            (Some(item), _) => item,
            (None, Some(_)) if unlock == Unlock::Never => {
                return Err(Error::msg("keyring: locked"));
            }
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
    pub async fn delete(profile: &str, entry: Entry<'_>, unlock: Unlock) -> Result<()> {
        let ss = connect().await?;
        let found = ss
            .search_items(map(&attributes(profile, entry)))
            .await
            .map_err(err)?;
        if !found.locked.is_empty() {
            if unlock == Unlock::Never {
                return Err(Error::msg("keyring: locked"));
            }
            ss.unlock_all(&found.locked.iter().collect::<Vec<_>>())
                .await
                .map_err(err)?;
        }
        for item in found.unlocked.iter().chain(&found.locked) {
            item.delete().await.map_err(err)?;
        }
        Ok(())
    }
}

/// macOS: the login keychain, items by `security(1)` (a subprocess, but it
/// brings ACL prompts and unlock handling for free; there is no per-session
/// throwaway keychain to ask for instead). Unlock::Never maps to omitting
/// `/usr/bin/security unlock-keychain` — `find-generic-password` on a locked
/// login keychain then fails with "user interaction is not allowed" when a
/// prompt would be needed, which we treat as `locked`.
#[cfg(target_os = "macos")]
mod backend {
    use std::process::Stdio;

    use super::{Entry, Unlock};
    use crate::error::{Error, Result};

    fn err(e: impl std::fmt::Display) -> Error {
        Error::msg(format!("keyring: {e}"))
    }

    /// Tests point the backend at a throwaway keychain this way; production
    /// never sets it, so the login keychain is used.
    fn test_keychain() -> Option<std::ffi::OsString> {
        std::env::var_os("TP_KEYCHAIN").filter(|p| !p.is_empty())
    }

    /// macOS keychain items are identified by (service, account); the label
    /// is display-only and never part of lookups.
    pub(crate) fn attrs(
        profile: &str,
        entry: Entry<'_>,
        label: &str,
    ) -> [(&'static str, String); 3] {
        let value = match entry {
            Entry::Source(id) => id.to_string(),
            Entry::Named(name) => name.to_owned(),
        };
        [
            ("-s", "testpattern".into()),         // kSecAttrService
            ("-a", format!("{profile}:{value}")), // kSecAttrAccount: profile + entry
            ("-l", label.to_owned()),             // kSecAttrLabel (what Keychain Access shows)
        ]
    }

    /// Does this `security` failure mean the keychain is locked (or can't
    /// prompt)? A locked login keychain says "user interaction is not
    /// allowed"; a locked *throwaway* keychain (and some non-interactive
    /// sessions) just exits 128 with empty stderr.
    pub(crate) fn is_locked(status: Option<i32>, stderr: &str) -> bool {
        stderr.contains("user interaction is not allowed")
            || stderr.contains("The keychain is locked")
            || (status == Some(128) && stderr.trim().is_empty())
    }

    /// Is this error one we returned for a locked keychain?
    fn is_error_locked(e: &Error) -> bool {
        e.to_string().contains("keychain is locked")
    }

    fn not_found(stderr: &str) -> bool {
        stderr.contains("could not be found")
    }

    /// `-w` prints the raw password on stdout: strip only the one newline it
    /// adds, so the secret's own bytes (backslashes, quotes, inner newlines)
    /// survive the round-trip. (`security -w` can't *store* a trailing
    /// newline, which is fine — passwords don't end in one.)
    pub(crate) fn parse_w(stdout: &[u8]) -> String {
        let s = String::from_utf8_lossy(stdout);
        s.strip_suffix('\n').unwrap_or(&s).to_owned()
    }

    /// `security <verb> -w` with the given args. Ok(Some(password)) /
    /// Ok(None) when the item is absent; Err on locked keychain or other
    /// failure. Stdin is null: nothing is piped and the child never
    /// inherits the app's stdin.
    pub(crate) fn sec(args: &[(&str, String)], verb: &str) -> Result<Option<String>> {
        let mut cmd = std::process::Command::new("/usr/bin/security");
        cmd.arg(verb);
        for (k, v) in args {
            cmd.arg(k).arg(v);
        }
        cmd.arg("-w"); // print the password, unquoted, on stdout
        if let Some(kc) = test_keychain() {
            cmd.arg(kc);
        }
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let out = cmd.output().map_err(err)?;
        let stderr = String::from_utf8_lossy(&out.stderr);
        if out.status.success() {
            return Ok(Some(parse_w(&out.stdout)));
        }
        if not_found(&stderr) {
            return Ok(None);
        }
        if is_locked(out.status.code(), &stderr) {
            return Err(err("the keychain is locked"));
        }
        Err(err(stderr.trim()))
    }

    /// `security` for verbs with no secret output. Ok(false) when the item
    /// is absent; Err on locked keychain or other failure.
    pub(crate) fn run(args: Vec<String>) -> Result<bool> {
        let mut cmd = std::process::Command::new("/usr/bin/security");
        cmd.args(&args);
        if let Some(kc) = test_keychain() {
            cmd.arg(kc);
        }
        let out = cmd.stdin(Stdio::null()).output().map_err(err)?;
        let stderr = String::from_utf8_lossy(&out.stderr);
        if out.status.success() {
            return Ok(true);
        }
        if not_found(&stderr) {
            return Ok(false);
        }
        if is_locked(out.status.code(), &stderr) {
            return Err(err("the keychain is locked"));
        }
        Err(err(stderr.trim()))
    }

    pub async fn store(
        profile: &str,
        entry: Entry<'_>,
        label: &str,
        password: &str,
        unlock: Unlock,
    ) -> Result<bool> {
        let a = attrs(profile, entry, label);
        // Upsert: add, or update when the entry exists.
        let mut add: Vec<String> = vec!["add-generic-password".into(), "-U".into()];
        for (k, v) in a {
            add.push(k.to_string());
            add.push(v);
        }
        add.push("-w".into());
        add.push(password.into());
        // unlock-keychain prompts when locked; skip it for Unlock::Never
        if unlock == Unlock::Never {
            match run(vec!["show-keychain-info".into()]) {
                Ok(_) => {}
                Err(e) if is_error_locked(&e) => return Ok(false),
                Err(e) => return Err(e), // a real failure, not "locked"
            }
        } else {
            let _ = run(vec!["unlock-keychain".into()]); // may prompt; ignore outcome
        }
        run(add)
    }

    pub async fn load(profile: &str, entry: Entry<'_>, unlock: Unlock) -> Result<Option<String>> {
        // service + account identify the entry; the label is display-only
        let a: Vec<(&str, String)> = attrs(profile, entry, "")
            .into_iter()
            .filter(|(k, _)| *k != "-l")
            .collect();
        match sec(&a, "find-generic-password") {
            Err(e) if unlock == Unlock::Prompt && is_error_locked(&e) => {
                // only now that a lookup failed on a locked keychain: unlock
                // (may prompt once), then retry
                let _ = run(vec!["unlock-keychain".into()]);
                sec(&a, "find-generic-password")
            }
            r => r,
        }
    }

    pub async fn delete(profile: &str, entry: Entry<'_>, unlock: Unlock) -> Result<()> {
        if unlock != Unlock::Never {
            let _ = run(vec!["unlock-keychain".into()]);
        }
        let a = attrs(profile, entry, "");
        let mut d: Vec<String> = vec!["delete-generic-password".into()];
        for (k, v) in a.iter().filter(|(k, _)| *k != "-l") {
            d.push((*k).to_string());
            d.push(v.to_string());
        }
        run(d).map(|_| ())
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod backend {
    //! No keyring integration yet (T-028): secrets stay in the database.
    use super::{Entry, Unlock};
    use crate::error::{Error, Result};

    pub async fn store(_: &str, _: Entry<'_>, _: &str, _: &str, _: Unlock) -> Result<bool> {
        Err(Error::msg("keyring: not supported on this platform"))
    }
    pub async fn load(_: &str, _: Entry<'_>, _: Unlock) -> Result<Option<String>> {
        Err(Error::msg("keyring: not supported on this platform"))
    }
    pub async fn delete(_: &str, _: Entry<'_>, _: Unlock) -> Result<()> {
        Ok(())
    }
}

/// Moves a source's password into the keyring; on success the database
/// keeps no copy. Returns whether it is in the keyring now.
pub async fn move_to_keyring(
    st: &AppState,
    source_id: i64,
    password: &str,
    unlock: Unlock,
) -> bool {
    let (profile, name) = {
        let conn = st.db.write();
        let Ok(profile) = profile_id(&conn) else {
            return false;
        };
        let name: String = conn
            .query_row("SELECT name FROM source WHERE id = ?1", [source_id], |r| {
                r.get(0)
            })
            .unwrap_or_else(|_| format!("source {source_id}"));
        (profile, name)
    };
    let label = format!("testpattern: {name}");
    match timed(
        unlock,
        backend::store(&profile, Entry::Source(source_id), &label, password, unlock),
    )
    .await
    {
        Ok(true) => {
            remember(source_id, password);
            match clear_plaintext(&st.db.write(), source_id) {
                Ok(()) => true,
                Err(e) => {
                    log::warn!(
                        "keyring: stored source {source_id} but could not update the database: {e}"
                    );
                    false
                }
            }
        }
        Ok(false) => {
            log::info!(
                "source {source_id}: keyring locked, password stays in the database for now"
            );
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
    let updated = conn.execute(
        "UPDATE source SET password = NULL, password_in_keyring = 1 WHERE id = ?1",
        [source_id],
    );
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
    match conn
        .execute_batch("VACUUM")
        .and_then(|()| conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(())))
    {
        Ok(()) => log::info!(
            "database rebuilt without old copies of secrets ({} ms)",
            t.elapsed().as_millis()
        ),
        Err(e) => log::warn!("could not rebuild the database: {e}"),
    }
}

/// Drops a source's keyring entry (source removed). May prompt: a locked
/// keyring can't delete anything.
pub async fn forget(st: &AppState, source_id: i64) {
    uncache(source_id);
    let Ok(profile) = profile_id(&st.db.write()) else {
        return;
    };
    if let Err(e) = timed(
        Unlock::Prompt,
        backend::delete(&profile, Entry::Source(source_id), Unlock::Prompt),
    )
    .await
    {
        log::warn!("keyring: could not delete the entry of source {source_id}: {e}");
    }
}

// ------------------------------------------------------------ named secrets

/// The setting marking that a `NAMED` secret lives in the keyring.
fn flag(name: &str) -> String {
    format!("{name}.inKeyring")
}

fn setting(conn: &Connection, key: &str) -> Option<String> {
    conn.query_row("SELECT value FROM setting WHERE key = ?1", [key], |r| {
        r.get::<_, String>(0)
    })
    .optional()
    .ok()
    .flatten()
    .and_then(|v| serde_json::from_str::<serde_json::Value>(&v).ok())
    .and_then(|v| match v {
        serde_json::Value::String(s) => Some(s),
        serde_json::Value::Bool(true) => Some("true".into()),
        _ => None,
    })
    .filter(|v| !v.trim().is_empty())
}

/// Is the `NAMED` secret stored (keyring or database)?
pub fn named_configured(conn: &Connection, name: &str) -> bool {
    setting(conn, &flag(name)).is_some() || setting(conn, name).is_some()
}

/// A `NAMED` secret: from the keyring cache when it lives there (None while
/// the keyring is locked), else the database setting.
pub fn named(conn: &Connection, name: &str) -> Option<String> {
    if setting(conn, &flag(name)).is_some() {
        NAMED_CACHE.lock().get(name).cloned()
    } else {
        setting(conn, name).map(|s| s.trim().to_owned())
    }
}

fn label_of(name: &str) -> &'static str {
    NAMED
        .iter()
        .find(|(k, _)| *k == name)
        .map_or("secret", |(_, l)| l)
}

/// Stores a `NAMED` secret: in the keyring (the database keeps no copy), or
/// — without one — in the database setting `name`. Returns whether it went
/// into the keyring.
pub async fn store_named(st: &AppState, name: &str, secret: &str, unlock: Unlock) -> Result<bool> {
    let profile = profile_id(&st.db.write())?;
    let label = format!("testpattern: {}", label_of(name));
    match timed(
        unlock,
        backend::store(&profile, Entry::Named(name), &label, secret, unlock),
    )
    .await
    {
        Ok(true) => {
            NAMED_CACHE
                .lock()
                .insert(name.to_owned(), secret.to_owned());
            clear_plaintext_setting(&st.db.write(), name)?;
            Ok(true)
        }
        stored => {
            match stored {
                Ok(_) => log::info!("{name}: keyring locked, stored in the database for now"),
                Err(e) => log::info!("{name}: stored in the database ({e})"),
            }
            let conn = st.db.write();
            conn.execute("DELETE FROM setting WHERE key = ?1", [flag(name)])?;
            conn.execute(
                "INSERT INTO setting (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                rusqlite::params![name, serde_json::to_string(secret)?],
            )?;
            NAMED_CACHE.lock().remove(name);
            Ok(false)
        }
    }
}

/// Removes a `NAMED` secret everywhere; the keyring entry goes in the
/// background (a locked keyring asks first).
pub fn forget_named(st: &AppState, name: &str) -> Result<()> {
    {
        let conn = st.db.write();
        clear_plaintext_setting(&conn, name)?;
        conn.execute("DELETE FROM setting WHERE key = ?1", [flag(name)])?;
    }
    NAMED_CACHE.lock().remove(name);
    let (st, name) = (st.clone(), name.to_owned());
    tauri::async_runtime::spawn(async move {
        let Ok(profile) = profile_id(&st.db.write()) else {
            return;
        };
        if let Err(e) = timed(
            Unlock::Prompt,
            backend::delete(&profile, Entry::Named(&name), Unlock::Prompt),
        )
        .await
        {
            log::warn!("keyring: could not delete {name}: {e}");
        }
    });
    Ok(())
}

/// Deletes the database copy of a `NAMED` secret (freed space zeroed, WAL
/// truncated) and marks it as living in the keyring.
fn clear_plaintext_setting(conn: &Connection, name: &str) -> Result<()> {
    conn.query_row("PRAGMA secure_delete = ON", [], |_| Ok(()))?;
    let done = conn
        .execute("DELETE FROM setting WHERE key = ?1", [name])
        .and_then(|_| {
            conn.execute(
                "INSERT OR REPLACE INTO setting (key, value) VALUES (?1, 'true')",
                [flag(name)],
            )
        });
    conn.query_row("PRAGMA secure_delete = OFF", [], |_| Ok(()))?;
    done?;
    conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))?;
    Ok(())
}

/// Loads the keyring passwords that aren't in the cache yet (at startup, and
/// again later when the keyring was locked then) — all, or `only` one source.
/// Without `only`, the `NAMED` secrets that live in the keyring too.
pub async fn ensure_loaded(st: &AppState, only: Option<i64>, unlock: Unlock) {
    let (profile, missing) = {
        let conn = st.db.write();
        let ids: Vec<i64> = conn
            .prepare(
                "SELECT id FROM source WHERE password_in_keyring = 1 AND (?1 IS NULL OR id = ?1)",
            )
            .and_then(|mut s| s.query_map([only], |r| r.get(0))?.collect())
            .unwrap_or_default();
        let mut missing: Vec<Entry<'static>> = ids
            .into_iter()
            .filter(|id| cached(*id).is_none())
            .map(Entry::Source)
            .collect();
        if only.is_none() {
            missing.extend(
                NAMED
                    .iter()
                    .filter(|(name, _)| {
                        setting(&conn, &flag(name)).is_some()
                            && !NAMED_CACHE.lock().contains_key(*name)
                    })
                    .map(|(name, _)| Entry::Named(name)),
            );
        }
        if missing.is_empty() {
            return;
        }
        let Ok(profile) = profile_id(&conn) else {
            return;
        };
        (profile, missing)
    };
    let mut unlock = unlock;
    for entry in missing {
        match (
            timed(unlock, backend::load(&profile, entry, unlock)).await,
            entry,
        ) {
            (Ok(Some(secret)), Entry::Source(id)) => remember(id, &secret),
            (Ok(Some(secret)), Entry::Named(name)) => {
                NAMED_CACHE.lock().insert(name.to_owned(), secret);
            }
            (Ok(None), _) => log::warn!("keyring: nothing stored for {entry:?}"),
            (Err(e), _) => {
                log::warn!("keyring: {entry:?} unavailable ({e})");
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
    let mut blocked = false;
    for (id, password) in plaintext {
        if !move_to_keyring(st, id, &password, Unlock::Never).await {
            blocked = true; // locked or unavailable: the others would fail the same way
            break;
        }
        log::info!("source {id}: password moved to the system keyring");
        moved += 1;
    }
    for (name, _) in NAMED {
        let plain =
            setting(&st.db.read(), name).filter(|_| setting(&st.db.read(), &flag(name)).is_none());
        if blocked || plain.is_none() {
            continue;
        }
        match store_named(
            st,
            name,
            plain.as_deref().unwrap_or_default().trim(),
            Unlock::Never,
        )
        .await
        {
            Ok(true) => {
                log::info!("{name}: moved to the system keyring");
                moved += 1;
            }
            _ => blocked = true,
        }
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
        let ss = SecretService::connect(EncryptionType::Dh)
            .await
            .expect("encrypted session");
        let default = ss
            .get_default_collection()
            .await
            .expect("default collection");
        println!(
            "default collection {:?}, locked: {:?}",
            default.get_label().await,
            default.is_locked().await
        );
    }

    /// Throwaway-keychain roundtrip for the macOS backend — runs the real
    /// backend functions against TP_KEYCHAIN (lock-keychain etc. likewise
    /// go through `run`):
    /// `TP_KEYCHAIN=/tmp/tp-test-keychain.$$.keychain-db cargo test --lib -- --ignored macos_keychain_roundtrip --exact --nocapture`
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore]
    fn macos_keychain_roundtrip() {
        let kc = std::env::var("TP_KEYCHAIN").expect("TP_KEYCHAIN (throwaway keychain path)");
        assert!(
            kc.starts_with("/tmp/tp-test-keychain."),
            "refusing anything but a throwaway keychain: {kc}"
        );

        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            use backend::{delete, load, run, store};
            let profile = "testprofile";
            let label = "testpattern: roundtrip";
            let entry = Entry::Source(4242);
            // backslashes and quotes must round-trip verbatim via `-w`
            let pw = "s3cr3t-\\with\"funny\"chars";

            // store -> load == value
            assert!(matches!(
                store(profile, entry, label, pw, Unlock::Never).await,
                Ok(true)
            ));
            assert_eq!(
                load(profile, entry, Unlock::Never)
                    .await
                    .unwrap()
                    .as_deref(),
                Some(pw)
            );

            // storing again updates in place
            let pw2 = "updated-\\value";
            assert!(matches!(
                store(profile, entry, label, pw2, Unlock::Never).await,
                Ok(true)
            ));
            assert_eq!(
                load(profile, entry, Unlock::Never)
                    .await
                    .unwrap()
                    .as_deref(),
                Some(pw2)
            );

            // an entry that was never stored loads as None
            assert!(matches!(
                load(profile, Entry::Source(9999), Unlock::Never).await,
                Ok(None)
            ));

            // delete -> load is None
            delete(profile, entry, Unlock::Never).await.unwrap();
            assert!(matches!(
                load(profile, entry, Unlock::Never).await,
                Ok(None)
            ));

            // re-add, lock -> store(Never) is Ok(false), load a locked Err;
            // load(Prompt) tries unlock-keychain once and still gets the
            // locked marker (the throwaway keychain's unlock needs -p)
            assert!(matches!(
                store(profile, entry, label, pw, Unlock::Never).await,
                Ok(true)
            ));
            assert!(matches!(run(vec!["lock-keychain".into()]), Ok(true)));
            assert!(matches!(
                store(profile, entry, label, "x", Unlock::Never).await,
                Ok(false)
            ));
            let e = load(profile, entry, Unlock::Never).await.unwrap_err();
            assert!(e.to_string().contains("locked"), "{e}");
            let e = load(profile, entry, Unlock::Prompt).await.unwrap_err();
            assert!(e.to_string().contains("locked"), "{e}");
            eprintln!("locked load error: {e}");
        });
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn keychain_locked_marker_classification() {
        use backend::is_locked;
        // the real "locked, no prompting" message of the login keychain
        assert!(is_locked(
            Some(1),
            "security: SecKeychainSearchCopyNext: user interaction is not allowed."
        ));
        // a locked keychain reported in so many words
        assert!(is_locked(Some(1), "The keychain is locked."));
        // throwaway keychains / non-interactive sessions: bare exit 128
        assert!(is_locked(Some(128), ""));
        assert!(is_locked(Some(128), "  \n"));
        // other failures are not "locked"
        assert!(!is_locked(
            Some(1),
            "The specified item could not be found in the keychain."
        ));
        assert!(!is_locked(Some(1), "security: unable to open keychain"));
        assert!(!is_locked(Some(2), "usage: security ..."));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn w_output_parsing_keeps_the_secret_verbatim() {
        use backend::parse_w;
        assert_eq!(parse_w(b"s3cr3t\n"), "s3cr3t"); // -w adds one newline
        assert_eq!(parse_w(b""), "");
        // no quoting, no unescaping: backslashes, quotes, inner newlines
        assert_eq!(parse_w(b"a\\b\"c\n"), "a\\b\"c");
        assert_eq!(parse_w(b"line1\nline2\n"), "line1\nline2");
        // from_utf8_lossy: odd bytes never fail the load
        assert_eq!(parse_w(b"a\xff\n"), "a\u{fffd}");
    }

    #[test]
    fn named_secrets_come_from_the_keyring_once_moved() {
        let c = crate::db::test_conn();
        let name = "test.secret";
        assert!(!named_configured(&c, name));
        c.execute(
            "INSERT INTO setting (key, value) VALUES (?1, '\"abc123\"')",
            [name],
        )
        .unwrap();
        assert!(named_configured(&c, name));
        assert_eq!(named(&c, name).as_deref(), Some("abc123"));
        clear_plaintext_setting(&c, name).unwrap();
        assert!(
            named_configured(&c, name),
            "still configured: it lives in the keyring"
        );
        assert_eq!(
            named(&c, name),
            None,
            "not loaded from the keyring yet (locked)"
        );
        NAMED_CACHE.lock().insert(name.to_owned(), "abc123".into());
        assert_eq!(named(&c, name).as_deref(), Some("abc123"));
        NAMED_CACHE.lock().remove(name);
    }

    #[test]
    fn moving_a_named_secret_leaves_no_copy_in_the_file() {
        let dir = std::env::temp_dir().join(format!("tp-secrets-named-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("library.db");
        let db = crate::db::Db::open(&path).unwrap();
        let (old, new) = (
            format!("old-{}", "5e2a7c91".repeat(10)),
            format!("new-{}", "b04f13d6".repeat(12)),
        );
        {
            let conn = db.write();
            let set = |v: &str| {
                conn.execute(
                    "INSERT INTO setting (key, value) VALUES ('test.secret', ?1)
                     ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                    [serde_json::to_string(v).unwrap()],
                )
                .unwrap()
            };
            set(&old);
            // settings saved later sit in front of it on the page
            conn.execute(
                "INSERT INTO setting (key, value) VALUES ('ui.zzz', '1')",
                [],
            )
            .unwrap();
            set(&new); // the user entered another key
            conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))
                .unwrap();
            clear_plaintext_setting(&conn, "test.secret").unwrap();
            scrub(&conn);
            assert!(named_configured(&conn, "test.secret"));
        }
        let mut bytes = std::fs::read(&path).unwrap();
        bytes.extend(std::fs::read(dir.join("library.db-wal")).unwrap_or_default());
        for secret in [&old, &new] {
            let piece = &secret.as_bytes()[20..40];
            assert!(
                !bytes.windows(piece.len()).any(|w| w == piece),
                "plaintext still on disk"
            );
        }
        drop(db);
        let _ = std::fs::remove_dir_all(&dir);
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
            conn.execute(
                "UPDATE source SET account_json = ?2 WHERE id = ?1",
                rusqlite::params![id, "{}".repeat(150)],
            )
            .unwrap();
            conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))
                .unwrap();
            clear_plaintext(&conn, id).unwrap();
            scrub(&conn);
            let (pw, flag): (Option<String>, bool) = conn
                .query_row(
                    "SELECT password, password_in_keyring FROM source WHERE id = ?1",
                    [id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .unwrap();
            assert_eq!((pw, flag), (None, true));
        }
        let mut bytes = std::fs::read(&path).unwrap();
        bytes.extend(std::fs::read(dir.join("library.db-wal")).unwrap_or_default());
        let piece = &secret.as_bytes()[40..60];
        assert!(
            !bytes.windows(piece.len()).any(|w| w == piece),
            "plaintext still on disk"
        );
        drop(db);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
