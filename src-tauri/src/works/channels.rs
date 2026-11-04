//! Live channels: feeds folded into one group per channel and country.

use std::collections::HashMap;

use rusqlite::{Connection, params};

use crate::error::Result;

use super::{genre, most_common, norm_title};

/// Variant preference when nothing was chosen yet: full/regular HD streams
/// first, then RAW, 4K, HEVC-only and SD last. Higher is better.
pub fn channel_rank(badges: &str) -> i32 {
    let has = |b: &str| badges.split_whitespace().any(|x| x == b);
    let mut r = 0;
    if has("FHD") {
        r += 50;
    } else if has("HD") {
        r += 45;
    } else if has("RAW") {
        r += 40;
    } else if has("4K") || has("8K") {
        r += 30;
    } else if !has("SD") {
        r += 35;
    }
    if has("HEVC") {
        r -= 10;
    }
    if has("SD") {
        r -= 20;
    }
    r
}

/// Grouping key of a channel within its country: "TV 2 SPORT 1" and
/// "TV2 SPORT 1" are the same channel, and so are its quality feeds
/// ("V Sport Ultra UHD"); East/West feeds and "Sky Sports+" vs "Sky
/// Sports" are not.
pub fn channel_key(country: Option<&str>, title: &str) -> String {
    const QUALITY: &[&str] = &[
        "uhd", "fhd", "hd", "sd", "4k", "8k", "hevc", "h265", "raw", "50fps", "60fps",
    ];
    let norm = norm_title(&title.replace('+', " plus "));
    let mut t = String::new();
    let mut prev = "";
    for w in norm.split_whitespace() {
        // "*MULTI-AUDIO*" = "*MULTI*"
        if !QUALITY.contains(&w) && !(w == "audio" && prev == "multi") {
            t.push_str(w);
        }
        prev = w;
    }
    format!("{}|{}", country.unwrap_or("-"), t)
}

/// Chip text for one variant of a channel: its own badges ("RAW HEVC"),
/// else its category's, plus the category's extras ("VIP", "Dolby Audio").
pub fn channel_variant_label(channel_badges: &str, category_badges: &str) -> String {
    const EXTRAS: &[&str] = &["VIP", "DOLBY AUDIO", "50FPS", "60FPS"];
    let own = crate::catalog::split_badges(channel_badges.to_owned());
    let cat = crate::catalog::split_badges(category_badges.to_owned());
    let mut parts: Vec<String> = if own.is_empty() {
        cat.iter()
            .filter(|b| !EXTRAS.contains(&b.as_str()))
            .cloned()
            .collect()
    } else {
        own
    };
    for b in cat.iter().filter(|b| EXTRAS.contains(&b.as_str())) {
        if !parts.contains(b) {
            parts.push(b.clone());
        }
    }
    let nice = |b: &str| match b {
        "DOLBY AUDIO" => "Dolby Audio".to_owned(),
        "50FPS" | "60FPS" => b.to_lowercase(),
        _ => b.to_owned(),
    };
    let label = parts
        .iter()
        .map(|b| nice(b))
        .collect::<Vec<_>>()
        .join(" · ");
    if label.is_empty() {
        "Standard".to_owned()
    } else {
        label
    }
}

pub(super) fn rebuild_channel_groups(conn: &Connection) -> Result<usize> {
    struct Ch {
        source_id: i64,
        id: String,
        title: String,
        logo: Option<String>,
        epg_id: Option<String>,
        badges: String,
        country: Option<String>,
        category_title: Option<String>,
        adult: bool,
        order: i64,
    }
    let mut stmt = conn.prepare(
        "SELECT c.source_id, c.id, c.title, c.logo, c.epg_id, c.badges, k.region, k.title, c.adult,
                COALESCE(k.position, 0) * 100000 + c.position
           FROM channel c
           LEFT JOIN category k ON k.source_id = c.source_id AND k.kind = 'live' AND k.id = c.category_id
          WHERE c.separator = 0
          ORDER BY c.source_id, 10",
    )?;
    let rows: Vec<Ch> = stmt
        .query_map([], |r| {
            Ok(Ch {
                source_id: r.get(0)?,
                id: r.get(1)?,
                title: r.get(2)?,
                logo: r.get(3)?,
                epg_id: r.get(4)?,
                badges: r.get(5)?,
                country: r.get(6)?,
                category_title: r.get(7)?,
                adult: r.get(8)?,
                order: r.get(9)?,
            })
        })?
        .collect::<Result<_, _>>()?;
    drop(stmt);

    let mut groups: HashMap<String, Vec<&Ch>> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    for c in &rows {
        let key = channel_key(c.country.as_deref(), &c.title);
        let e = groups.entry(key.clone()).or_default();
        if e.is_empty() {
            order.push(key);
        }
        e.push(c);
    }

    // the variant that plays: the user's pick, else a favorited one, else the best
    let prefs: HashMap<String, (i64, String)> = conn
        .prepare("SELECT key, source_id, item_id FROM channel_pref")?
        .query_map([], |r| Ok((r.get(0)?, (r.get(1)?, r.get(2)?))))?
        .collect::<Result<_, _>>()?;
    let favorites: HashMap<(i64, String), i64> = conn
        .prepare("SELECT source_id, item_id, added_at FROM favorite WHERE kind = 'live'")?
        .query_map([], |r| Ok(((r.get(0)?, r.get(1)?), r.get(2)?)))?
        .collect::<Result<_, _>>()?;

    conn.execute("DELETE FROM channel_group", [])?;
    conn.execute(
        "UPDATE channel SET group_key = NULL WHERE separator = 1",
        [],
    )?;
    let mut set_key =
        conn.prepare("UPDATE channel SET group_key = ?3 WHERE source_id = ?1 AND id = ?2")?;
    let mut insert = conn.prepare(
        "INSERT INTO channel_group (key, title, country, genre, logo, epg_id, variants, adult, position, source_id, item_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
    )?;
    for (pos, key) in order.iter().enumerate() {
        let cs = &groups[key];
        for c in cs {
            set_key.execute(params![c.source_id, c.id, key])?;
        }
        let best = cs
            .iter()
            .max_by_key(|c| (channel_rank(&c.badges), c.epg_id.is_some() as i32, -c.order))
            .copied()
            .expect("groups are never empty");
        let picked = prefs
            .get(key)
            .and_then(|(sid, id)| cs.iter().find(|c| c.source_id == *sid && c.id == *id));
        let favorite = cs
            .iter()
            .filter_map(|c| {
                favorites
                    .get(&(c.source_id, c.id.clone()))
                    .map(|at| (*at, c))
            })
            .max_by_key(|(at, _)| *at)
            .map(|(_, c)| c);
        let chosen = picked.or(favorite).copied().unwrap_or(best);
        let title = most_common(cs.iter().map(|c| (c.title.clone(), 1)))
            .unwrap_or_else(|| best.title.clone());
        let category_genre = most_common(
            cs.iter()
                .filter_map(|c| {
                    c.category_title
                        .as_deref()
                        .and_then(genre::live_from_category)
                })
                .map(|g| (g, 1)),
        );
        let genre = category_genre.unwrap_or_else(|| genre::live_from_title(&title));
        let epg_id = most_common(cs.iter().filter_map(|c| c.epg_id.clone()).map(|e| (e, 1)));
        let logo = chosen
            .logo
            .clone()
            .or_else(|| best.logo.clone())
            .or_else(|| cs.iter().find_map(|c| c.logo.clone()));
        insert.execute(params![
            key,
            title,
            chosen.country,
            genre,
            logo,
            epg_id,
            cs.len() as i64,
            cs.iter().any(|c| c.adult),
            pos as i64,
            chosen.source_id,
            chosen.id
        ])?;
    }
    Ok(order.len())
}
