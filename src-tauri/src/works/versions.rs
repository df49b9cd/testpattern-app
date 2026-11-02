//! The provider copies ("versions") of one movie or series: what each offers,
//! which one plays, and what the player learned about their tracks.

use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use serde_json::Value;
use tauri::State;

use super::variant::{self, Origin, Variant};
use crate::db::now;
use crate::error::{Error, Result};
use crate::settings;
use crate::state::AppState;

/// One copy of a work as the detail pages show it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionInfo {
    pub source_id: i64,
    pub id: String,
    pub label: String,
    pub service: Option<&'static str>,
    pub origin: Origin,
    pub language: Option<&'static str>,
    pub subtitles: Option<&'static str>,
    pub quality: Vec<&'static str>,
    /// provider category, e.g. "Apple+ Series · 4K Dolby Vision"
    pub category: Option<String>,
    /// set when the work spans several sources
    pub source_name: Option<String>,
    pub selected: bool,
    /// container of a movie file ("mkv", "mp4")
    pub ext: Option<String>,
    /// from the provider's detail (resolution, codec, audio channels)
    pub video: Option<crate::catalog::TechInfo>,
    pub audio: Option<crate::catalog::TechInfo>,
    pub duration: Option<i64>,
    /// tracks seen in the file (`media_info`): {audio:[…], subtitles:[…], video:{…}}
    pub tracks: Option<Value>,
    /// movies: this copy's watch state
    pub position: f64,
    pub watched: bool,
    /// series: season numbers and episode count of this copy
    pub seasons: Vec<i64>,
    pub episodes: i64,
}

/// A member row with what version selection needs.
#[derive(Debug, Clone)]
pub struct Member {
    pub source_id: i64,
    pub id: String,
    pub variant: Variant,
    pub category: Option<String>,
    pub source_name: String,
    pub ext: Option<String>,
    pub position: f64,
    pub watched: bool,
    /// last time this copy (or one of its episodes) was played; 0 = never
    pub played_at: i64,
}

impl Member {
    pub fn info(&self, multi_source: bool, selected: bool) -> VersionInfo {
        VersionInfo {
            source_id: self.source_id,
            id: self.id.clone(),
            label: self.variant.label.clone(),
            service: self.variant.service,
            origin: self.variant.origin,
            language: self.variant.language,
            subtitles: self.variant.subtitles,
            quality: self.variant.quality.clone(),
            category: self.category.as_deref().map(crate::names::display_category),
            source_name: multi_source.then(|| self.source_name.clone()),
            selected,
            ext: self.ext.clone(),
            video: None,
            audio: None,
            duration: None,
            tracks: None,
            position: self.position,
            watched: self.watched,
            seasons: Vec::new(),
            episodes: 0,
        }
    }
}

/// Work key of one copy.
pub fn work_key(conn: &Connection, kind: &str, source_id: i64, id: &str) -> Result<Option<String>> {
    let table = if kind == "movie" { "movie" } else { "series" };
    Ok(conn
        .query_row(
            &format!("SELECT work_key FROM {table} WHERE source_id = ?1 AND id = ?2"),
            params![source_id, id],
            |r| r.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten())
}

/// All copies of a work (just the given one when it isn't grouped yet).
pub fn members(conn: &Connection, kind: &str, source_id: i64, id: &str) -> Result<Vec<Member>> {
    let key = work_key(conn, kind, source_id, id)?;
    let (table, ext, played) = if kind == "movie" {
        (
            "movie",
            "x.ext",
            "(SELECT h.position || '|' || h.duration || '|' || h.watched || '|' || h.updated_at FROM history h
               WHERE h.source_id = x.source_id AND h.kind = 'movie' AND h.item_id = x.id)",
        )
    } else {
        (
            "series",
            "NULL",
            "(SELECT '0|0|0|' || MAX(h.updated_at) FROM history h
               WHERE h.source_id = x.source_id AND h.kind = 'episode' AND h.series_id = x.id)",
        )
    };
    let filter = if key.is_some() {
        "x.work_key = ?1"
    } else {
        "x.source_id = ?2 AND x.id = ?3"
    };
    let sql = format!(
        "SELECT x.source_id, x.id, x.tag, k.name, src.name, {ext}, {played}
           FROM {table} x
           LEFT JOIN category k ON k.source_id = x.source_id AND k.kind = ?4 AND k.id = x.category_id
           LEFT JOIN source src ON src.id = x.source_id
          WHERE {filter}
          ORDER BY x.source_id, x.position"
    );
    let rows = conn
        .prepare_cached(&sql)?
        .query_map(params![key, source_id, id, kind], |r| {
            let tag: Option<String> = r.get(2)?;
            let category: Option<String> = r.get(3)?;
            let played: Option<String> = r.get(6)?;
            let mut parts = played.as_deref().unwrap_or("").split('|');
            let mut num = || {
                parts
                    .next()
                    .and_then(|p| p.parse::<f64>().ok())
                    .unwrap_or(0.0)
            };
            let (position, _duration, watched, played_at) =
                (num(), num(), num() != 0.0, num() as i64);
            Ok(Member {
                source_id: r.get(0)?,
                id: r.get(1)?,
                variant: variant::parse(tag.as_deref(), category.as_deref()),
                category,
                source_name: r.get::<_, Option<String>>(4)?.unwrap_or_default(),
                ext: r.get(5)?,
                position,
                watched,
                played_at,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn preference(conn: &Connection, kind: &str, key: &str) -> Result<Option<(i64, String)>> {
    Ok(conn
        .query_row(
            "SELECT source_id, item_id FROM work_pref WHERE kind = ?1 AND key = ?2",
            params![kind, key],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?)
}

pub fn language_prefs(conn: &Connection) -> Vec<&'static str> {
    variant::language_prefs(
        &settings::get_str(conn, "player.subLang"),
        &settings::get_str(conn, "player.audioLang"),
    )
}

/// `completeness` of a copy the server couldn't open ("check tracks"):
/// chosen automatically only when nothing else is left.
pub const BROKEN: f64 = -50.0;

/// Tracks entry of a copy whose check failed.
pub fn unavailable(tracks: &Option<Value>) -> bool {
    tracks
        .as_ref()
        .is_some_and(|t| t["unavailable"] == Value::Bool(true))
}

/// Which copy plays: the one the user picked, else the one they were last
/// watching, else the best fit for their languages (then completeness for
/// series — `completeness(i)` in 0..=1, `BROKEN` for copies that failed to
/// open — then picture quality; never CAM).
pub fn choose(
    members: &[Member],
    pref: Option<&(i64, String)>,
    langs: &[&str],
    completeness: impl Fn(usize) -> f64,
) -> usize {
    if let Some((sid, id)) = pref
        && let Some(i) = members
            .iter()
            .position(|m| m.source_id == *sid && m.id == *id)
    {
        return i;
    }
    if let Some((i, _)) = members
        .iter()
        .enumerate()
        .filter(|(_, m)| m.played_at > 0)
        .max_by_key(|(_, m)| m.played_at)
    {
        return i;
    }
    let rank = |i: usize| -> f64 {
        let m = &members[i];
        (variant::affinity(&m.variant, langs) + variant::score(&m.variant)) as f64
            + 20.0 * completeness(i)
    };
    (0..members.len())
        .max_by(|a, b| rank(*a).total_cmp(&rank(*b)).then(b.cmp(a)))
        .unwrap_or(0)
}

/// Tracks the player (or a probe) saw in a file.
pub fn tracks(
    conn: &Connection,
    source_id: i64,
    kind: &str,
    item_id: &str,
) -> Result<Option<Value>> {
    Ok(conn
        .query_row(
            "SELECT json FROM media_info WHERE source_id = ?1 AND kind = ?2 AND item_id = ?3",
            params![source_id, kind, item_id],
            |r| r.get::<_, String>(0),
        )
        .optional()?
        .and_then(|j| serde_json::from_str(&j).ok()))
}

/// What a file offers, from mpv's `track-list` (+ HDR from `video-params`,
/// seen during playback or — the probe — from one decoded frame):
/// `{audio:[{lang,codec,channels,title}], subtitles:[{lang,codec,title}],
/// video:{codec,width,height,hdr}}`. Subtitle files mpv found next to the
/// stream are not part of the copy and are left out.
pub fn summarize_tracks(track_list: &Value, hdr: bool) -> Option<Value> {
    let tracks = track_list.as_array().filter(|a| !a.is_empty())?;
    let (mut audio, mut subtitles, mut video) = (Vec::new(), Vec::new(), Value::Null);
    for t in tracks {
        if t["external"].as_bool() == Some(true) {
            continue;
        }
        match t["type"].as_str() {
            Some("audio") => audio.push(serde_json::json!({
                "lang": t["lang"], "codec": t["codec"], "channels": t["demux-channel-count"], "title": t["title"],
            })),
            Some("sub") => subtitles.push(serde_json::json!({ "lang": t["lang"], "codec": t["codec"], "title": t["title"] })),
            Some("video") if video.is_null() && t["image"].as_bool() != Some(true) => {
                video = serde_json::json!({
                    "codec": t["codec"], "width": t["demux-w"], "height": t["demux-h"],
                    "hdr": hdr || !t["dolby-vision-profile"].is_null(),
                })
            }
            _ => {}
        }
    }
    Some(serde_json::json!({ "audio": audio, "subtitles": subtitles, "video": video }))
}

/// Stores what playback (or a probe) saw in a movie or episode file.
pub fn save_tracks(
    conn: &Connection,
    source_id: i64,
    kind: &str,
    item_id: &str,
    json: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO media_info (source_id, kind, item_id, json, updated_at) VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(source_id, kind, item_id) DO UPDATE SET json = excluded.json, updated_at = excluded.updated_at",
        params![source_id, kind, item_id, json, now()],
    )?;
    Ok(())
}

/// Remembers the copy the user picked for a movie or series.
#[tauri::command]
pub async fn work_prefer(
    state: State<'_, AppState>,
    kind: String,
    source_id: i64,
    id: String,
) -> Result<()> {
    if kind != "movie" && kind != "series" {
        return Err(Error::msg(format!("no versions for {kind}")));
    }
    let conn = state.db.write();
    let key = work_key(&conn, &kind, source_id, &id)?
        .ok_or_else(|| Error::NotFound(format!("{kind} {id}")))?;
    conn.execute(
        "INSERT INTO work_pref (kind, key, source_id, item_id, updated_at) VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(kind, key) DO UPDATE SET source_id = excluded.source_id, item_id = excluded.item_id,
                                              updated_at = excluded.updated_at",
        params![kind, key, source_id, id, now()],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(tag: &str, cat: &str, played_at: i64) -> Member {
        Member {
            source_id: 1,
            id: format!("{tag}-{cat}"),
            variant: variant::parse(Some(tag), Some(cat)),
            category: Some(cat.into()),
            source_name: "s".into(),
            ext: None,
            position: 0.0,
            watched: false,
            played_at,
        }
    }

    #[test]
    fn summarizes_the_track_list() {
        let list = serde_json::json!([
            {"id": 1, "type": "video", "codec": "hevc", "demux-w": 3840, "demux-h": 2160, "dolby-vision-profile": 8},
            {"id": 1, "type": "audio", "lang": "eng", "codec": "eac3", "demux-channel-count": 6},
            {"id": 2, "type": "audio", "lang": "dan", "codec": "aac", "demux-channel-count": 2, "title": "Synstolkning"},
            {"id": 1, "type": "sub", "lang": "dan", "codec": "subrip"},
            {"id": 2, "type": "sub", "lang": "swe", "codec": "subrip", "external": true},
        ]);
        let s = summarize_tracks(&list, false).unwrap();
        assert_eq!(s["video"]["height"], 2160);
        assert_eq!(s["video"]["hdr"], true);
        assert_eq!(s["audio"].as_array().unwrap().len(), 2);
        assert_eq!(s["audio"][1]["title"], "Synstolkning");
        assert_eq!(s["subtitles"].as_array().unwrap().len(), 1);
        assert!(summarize_tracks(&serde_json::json!([]), false).is_none());

        let c = crate::db::test_conn();
        c.execute(
            "INSERT INTO movie (source_id, id, name, title, position) VALUES (1, 'm', 'M', 'M', 0)",
            [],
        )
        .unwrap();
        save_tracks(&c, 1, "movie", "m", &s.to_string()).unwrap();
        save_tracks(&c, 1, "movie", "m", &s.to_string()).unwrap();
        assert_eq!(
            tracks(&c, 1, "movie", "m").unwrap().unwrap()["audio"][0]["lang"],
            "eng"
        );
    }

    #[test]
    fn summarize_tracks_sets_hdr_from_flag() {
        // a plain HDR10 file: no DV profile in the track list, gamma said pq/hlg
        let list = serde_json::json!([
            {"id": 1, "type": "video", "codec": "hevc", "demux-w": 3840, "demux-h": 2160},
        ]);
        assert_eq!(summarize_tracks(&list, true).unwrap()["video"]["hdr"], true);
    }

    #[test]
    fn summarize_tracks_hdr_false_without_gamma_or_dv() {
        let list = serde_json::json!([
            {"id": 1, "type": "video", "codec": "hevc", "demux-w": 3840, "demux-h": 2160},
        ]);
        assert_eq!(
            summarize_tracks(&list, false).unwrap()["video"]["hdr"],
            false
        );
    }

    #[test]
    fn summarize_tracks_hdr_from_dv_profile() {
        // DV is flagged even without a gamma reading (profile is in the track list)
        let list = serde_json::json!([
            {"id": 1, "type": "video", "codec": "hevc", "demux-w": 3840, "demux-h": 2160, "dolby-vision-profile": 5},
        ]);
        assert_eq!(
            summarize_tracks(&list, false).unwrap()["video"]["hdr"],
            true
        );
    }

    #[test]
    fn picks_a_version() {
        let english = ["English"];
        let danish = ["Danish", "Nordic", "English"];
        let v = vec![
            m("EN-CAM", "EN - NEW RELEASE", 0),
            m("SC", "NORDIC SERIES", 0),
            m("NF", "NETFLIX  SERIES", 0),
            m("4K-A+", "APPLE+ SERIES ⁴ᴷ ³⁸⁴⁰ᴾ ᴰᵒˡᵇʸ ⱽᶦˢᶦᵒⁿ", 0),
        ];
        let full = |_: usize| 1.0;
        // English viewers: the best international copy
        assert_eq!(choose(&v, None, &english, full), 3);
        // Danish viewers: the Nordic copy
        assert_eq!(choose(&v, None, &danish, full), 1);
        // a copy missing most episodes loses
        assert_eq!(
            choose(&v, None, &english, |i| if i == 3 { 0.2 } else { 1.0 }),
            2
        );
        // the one being watched wins over the default
        let mut watching = v.clone();
        watching[2].played_at = 100;
        assert_eq!(choose(&watching, None, &danish, full), 2);
        // an explicit choice wins over everything
        let pick = (1, v[0].id.clone());
        assert_eq!(choose(&watching, Some(&pick), &danish, full), 0);
        // a copy the server can't open loses to anything else
        assert_eq!(
            choose(&v, None, &danish, |i| if i == 1 { BROKEN } else { 1.0 }),
            3
        );
        assert!(unavailable(&Some(serde_json::json!({"unavailable": true}))));
        assert!(!unavailable(&None));
    }
}
