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
        "player.subsEnabled": false,
        "player.volume": 100,
        "content.showAdult": false,
        "ui.startPage": "home",
    });
    v.as_object().cloned().unwrap_or_default()
}

pub fn get(conn: &Connection, key: &str) -> Value {
    let stored: Option<String> = conn
        .query_row("SELECT value FROM setting WHERE key = ?1", [key], |r| r.get(0))
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
        if let Ok(v) = serde_json::from_str(&v) {
            map.insert(k, v);
        }
    }
    Ok(map)
}

/// Pushes player-related settings into mpv.
pub fn apply_player<R: Runtime>(app: &AppHandle<R>, st: &AppState) {
    let Some(player) = app.try_state::<crate::player::Player>() else { return };
    let conn = st.db.read();
    let set = |name: &str, v: String| {
        if let Err(e) = player.mpv().set_string(name, &v) {
            log::warn!("apply setting {name}: {e}");
        }
    };
    set("hwdec", get_str(&conn, "player.hwdec"));
    set("alang", get_str(&conn, "player.audioLang"));
    set("slang", get_str(&conn, "player.subLang"));
    set("sub-visibility", if get_bool(&conn, "player.subsEnabled") { "yes" } else { "no" }.into());
    if let Some(v) = get(&conn, "player.volume").as_f64() {
        let _ = player.mpv().set_double("volume", v);
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
        apply_player(&app, state.inner());
    }
    Ok(())
}
