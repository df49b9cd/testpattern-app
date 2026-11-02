//! User settings: a flat key → JSON value store with defaults.

use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Map, Value, json};
use tauri::{AppHandle, Manager, Runtime, State};

use crate::error::Result;
use crate::state::AppState;

pub fn defaults() -> Map<String, Value> {
    let v = json!({
        "player.hwdec": "auto-safe",
        "player.liveFormat": "ts",
        "player.audioLang": "eng,en",
        "player.subLang": "eng,en",
        // subs on by default (PL-100): mpv auto-selects the viewer's
        // subtitle language (slang below); hiding the track by default left
        // first-run users with no subs and no clue why. The player chrome has
        // a quick toggle (PL-99) and Settings keeps the startup default.
        "player.subsEnabled": true,
        "player.volume": 100,
        "content.showAdult": false,
        "ui.startPage": "home",
        "cache.imagesMb": crate::images::DEFAULT_CACHE_MB,
        // empty: ~/Videos/testpattern (playback::player_record)
        "recording.dir": "",
    });
    v.as_object().cloned().unwrap_or_default()
}

pub fn get(conn: &Connection, key: &str) -> Value {
    let stored: Option<String> = conn
        .query_row("SELECT value FROM setting WHERE key = ?1", [key], |r| {
            r.get(0)
        })
        .optional()
        .ok()
        .flatten();
    stored
        .and_then(|s| serde_json::from_str(&s).ok())
        .or_else(|| defaults().remove(key))
        .unwrap_or(Value::Null)
}

pub fn get_bool(conn: &Connection, key: &str) -> bool {
    get(conn, key).as_bool().unwrap_or(false)
}

pub fn get_str(conn: &Connection, key: &str) -> String {
    match get(conn, key) {
        Value::String(s) => s,
        Value::Null => String::new(),
        v => v.to_string(),
    }
}

fn all(conn: &Connection) -> Result<Map<String, Value>> {
    let mut map = defaults();
    let mut stmt = conn.prepare("SELECT key, value FROM setting")?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    for row in rows {
        let (k, v) = row?;
        // secrets stay in the backend (secrets.rs, tmdb.rs), and so does the
        // note that one lives in the keyring
        if crate::secrets::NAMED.iter().any(|(name, _)| k == *name) || k.ends_with(".inKeyring") {
            continue;
        }
        if let Ok(v) = serde_json::from_str(&v) {
            map.insert(k, v);
        }
    }
    Ok(map)
}

/// The mpv sub-visibility value for a connection's `player.subsEnabled`.
pub fn sub_visibility(conn: &Connection) -> &'static str {
    if get_bool(conn, "player.subsEnabled") {
        "yes"
    } else {
        "no"
    }
}

/// Pushes player settings into mpv: all of them (startup) or just `only` —
/// re-applying e.g. `hwdec` mid-playback would needlessly touch the decoder.
pub fn apply_player<R: Runtime>(app: &AppHandle<R>, st: &AppState, only: Option<&str>) {
    let Some(player) = app.try_state::<crate::player::Player>() else {
        return;
    };
    let conn = st.db.read();
    let wanted = |key: &str| only.is_none_or(|o| o == key);
    let set = |name: &str, v: String| {
        if let Err(e) = player.mpv().set_string(name, &v) {
            log::warn!("apply setting {name}: {e}");
        }
    };
    if wanted("player.hwdec") {
        set("hwdec", get_str(&conn, "player.hwdec"));
    }
    if wanted("player.audioLang") {
        set("alang", get_str(&conn, "player.audioLang"));
    }
    if wanted("player.subLang") {
        set("slang", get_str(&conn, "player.subLang"));
    }
    if wanted("player.subsEnabled") {
        set("sub-visibility", sub_visibility(&conn).into());
    }
    // the UI remembers the volume it last set (stores/player.ts)
    if wanted("player.volume")
        && let Some(v) = get(&conn, "player.volume").as_f64()
    {
        let _ = player.mpv().set_double("volume", v.clamp(0.0, 150.0));
    }
}

#[tauri::command]
pub async fn settings_get(state: State<'_, AppState>) -> Result<Map<String, Value>> {
    let conn = state.db.read();
    all(&conn)
}

#[tauri::command]
pub async fn settings_set<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
    key: String,
    value: Value,
) -> Result<()> {
    {
        let conn = state.db.write();
        conn.execute(
            "INSERT INTO setting (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, serde_json::to_string(&value)?],
        )?;
    }
    if key.starts_with("player.") {
        apply_player(&app, state.inner(), Some(&key));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// PL-100: a fresh profile must show auto-selected subtitles; hiding them
    /// by default read as broken subtitles to first-run users.
    #[test]
    fn fresh_profiles_show_subtitles() {
        assert_eq!(defaults()["player.subsEnabled"], json!(true));
    }

    /// PL-100 through apply_player's mapping: a fresh conn (nothing stored)
    /// resolves sub-visibility to "yes"; a stored false flips it to "no".
    #[test]
    fn apply_player_enables_subs_on_fresh_profiles() {
        let conn = crate::db::test_conn();
        assert_eq!(sub_visibility(&conn), "yes");
        conn.execute(
            "INSERT INTO setting (key, value) VALUES ('player.subsEnabled', 'false')",
            [],
        )
        .unwrap();
        assert_eq!(sub_visibility(&conn), "no");
    }
}
