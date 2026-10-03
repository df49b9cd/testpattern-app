//! Grouping of the catalog for browsing.
//!
//! Providers list every copy of a title separately — "For All Mankind"
//! exists 6× (Netflix, Apple TV+, Apple TV+ 4K Dolby Vision, English, Nordic,
//! Nordic 4K), a UK sports channel up to 16× (RAW, HEVC, SD, HD, 4K, backup
//! streams). This module folds them:
//!
//! * **works** — one row per movie / series (`work`), members point at it
//!   through `movie.work_key` / `series.work_key`; browse facets in
//!   `work_facet` (service, language, quality, genre, decade, collection =
//!   provider category; with TMDB details (`tmdb.rs`) also original
//!   language, franchise = TMDB collection, network).
//! * **channel groups** — one row per channel and country (`channel_group`),
//!   members through `channel.group_key`, with a genre for navigation.
//!
//! Everything here is derived data: `rebuild` recomputes it inside the sync
//! transaction, after a source is removed, and at startup whenever
//! `RULES_VERSION` changes.

pub mod genre;
pub mod lang;
pub mod variant;
pub mod versions;

use std::collections::{BTreeSet, HashMap};

use rusqlite::{Connection, params};

use crate::error::Result;
use variant::Variant;

/// Bump whenever grouping/facet rules change: startup then rebuilds.
/// v5: `tmdb_map` folds search-matched unidentified titles into TMDB groups.
pub const RULES_VERSION: i64 = 5;

/// Title for grouping: lower case, diacritics folded, `&` = "and",
/// punctuation dropped ("Love & Anarchy" = "love and anarchy").
pub fn norm_title(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut gap = false;
    let push_word_char = |out: &mut String, c: char, gap: &mut bool| {
        if *gap && !out.is_empty() {
            out.push(' ');
        }
        *gap = false;
        out.push(c);
    };
    for ch in s.chars() {
        if ch == '&' {
            gap = true;
            for c in "and".chars() {
                push_word_char(&mut out, c, &mut gap);
            }
            gap = true;
            continue;
        }
        for lower in ch.to_lowercase() {
            let folded: &str = match lower {
                'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' => "a",
                'æ' => "ae",
                'ç' | 'č' | 'ć' => "c",
                'è' | 'é' | 'ê' | 'ë' | 'ě' | 'ē' => "e",
                'ì' | 'í' | 'î' | 'ï' | 'ı' => "i",
                'ñ' | 'ń' => "n",
                'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ő' => "o",
                'œ' => "oe",
                'ù' | 'ú' | 'û' | 'ü' | 'ů' | 'ű' => "u",
                'ý' | 'ÿ' => "y",
                'ß' => "ss",
                'ğ' => "g",
                'ş' | 'š' | 'ś' => "s",
                'ł' => "l",
                'ž' | 'ź' | 'ż' => "z",
                'ř' => "r",
                'đ' => "d",
                _ => "",
            };
            if !folded.is_empty() {
                for c in folded.chars() {
                    push_word_char(&mut out, c, &mut gap);
                }
            } else if lower.is_alphanumeric() {
                push_word_char(&mut out, lower, &mut gap);
            } else {
                gap = true;
            }
        }
    }
    out
}

fn valid_tmdb(t: Option<&str>) -> Option<&str> {
    let t = t?.trim();
    (!t.is_empty() && t != "0" && !t.eq_ignore_ascii_case("null")).then_some(t)
}

/// TMDB ids found by title search (db.rs v9, tmdb.rs): (kind, source key) →
/// TMDB id. `kind` is the tmdb table's spelling ('movie' | 'tv'); ids stored
/// as '' are remembered misses, not mappings.
pub fn load_tmdb_map(conn: &Connection) -> Result<HashMap<(String, String), String>> {
    let rows: Vec<(String, String, String)> = conn
        .prepare("SELECT kind, source_key, tmdb_id FROM tmdb_map WHERE tmdb_id != ''")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<Result<_, _>>()?;
    Ok(rows.into_iter().map(|(kind, key, id)| ((kind, key), id)).collect())
}

/// The mapped TMDB id of a search-matched work key ('title:*' | 'item:*').
#[cfg(test)]
pub fn tmdb_id_of(conn: &Connection, key: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row("SELECT tmdb_id FROM tmdb_map WHERE source_key = ?1", [key], |r| r.get::<_, String>(0))
        .ok()
        .filter(|id| !id.is_empty()))
}

/// One catalog row as far as grouping cares.
struct Member {
    source_id: i64,
    id: String,
    title: String,
    year: Option<i64>,
    tmdb: Option<String>,
    category: Option<String>,
    category_id: Option<String>,
    poster: Option<String>,
    backdrop: Option<String>,
    rating: Option<f64>,
    genre_text: Option<String>,
    added: Option<i64>,
    adult: bool,
    variant: Variant,
}

impl Member {
    /// English and streaming-service copies carry the international title and
    /// artwork; local-market copies sometimes a translated title.
    fn weight(&self) -> i64 {
        let v = &self.variant;
        if v.origin == variant::Origin::Cam {
            0
        } else if v.language == Some("English") || (v.service.is_some() && v.language.is_none()) {
            3
        } else {
            1
        }
    }
}

/// Group keys: TMDB id; an entry without one joins the TMDB group with the
/// same normalized title + year when that is unambiguous, otherwise groups by
/// title + year; without a year it stays on its own.
fn assign_keys(members: &[Member]) -> Vec<String> {
    let mut by_title_year: HashMap<(String, i64), BTreeSet<String>> = HashMap::new();
    for m in members {
        if let (Some(t), Some(y)) = (valid_tmdb(m.tmdb.as_deref()), m.year) {
            by_title_year.entry((norm_title(&m.title), y)).or_default().insert(format!("tmdb:{t}"));
        }
    }
    members
        .iter()
        .map(|m| {
            if let Some(t) = valid_tmdb(m.tmdb.as_deref()) {
                return format!("tmdb:{t}");
            }
            let norm = norm_title(&m.title);
            match m.year {
                Some(y) => match by_title_year.get(&(norm.clone(), y)) {
                    Some(keys) if keys.len() == 1 => keys.iter().next().unwrap().clone(),
                    _ => format!("title:{norm}|{y}"),
                },
                None => format!("item:{}:{}", m.source_id, m.id),
            }
        })
        .collect()
}

fn most_common<T: Eq + std::hash::Hash + Clone>(items: impl Iterator<Item = (T, i64)>) -> Option<T> {
    let mut counts: HashMap<T, (i64, usize)> = HashMap::new();
    for (i, (item, w)) in items.enumerate() {
        let e = counts.entry(item).or_insert((0, i));
        e.0 += w;
    }
    counts.into_iter().max_by(|a, b| a.1.0.cmp(&b.1.0).then(b.1.1.cmp(&a.1.1))).map(|(k, _)| k)
}

fn load_members(conn: &Connection, kind: &str) -> Result<Vec<Member>> {
    let (table, art, backdrop, genre, added) = match kind {
        "movie" => ("movie", "x.poster", "NULL", "NULL", "x.added"),
        _ => ("series", "x.cover", "x.backdrop", "x.genre", "x.last_modified"),
    };
    let sql = format!(
        "SELECT x.source_id, x.id, x.title, x.year, x.tmdb, x.tag, k.name, x.category_id, {art}, {backdrop},
                x.rating, {genre}, {added}, x.adult
           FROM {table} x
           LEFT JOIN category k ON k.source_id = x.source_id AND k.kind = ?1 AND k.id = x.category_id"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map([kind], |r| {
            let tag: Option<String> = r.get(5)?;
            let category: Option<String> = r.get(6)?;
            Ok(Member {
                source_id: r.get(0)?,
                id: r.get(1)?,
                title: r.get(2)?,
                year: r.get(3)?,
                tmdb: r.get(4)?,
                variant: variant::parse(tag.as_deref(), category.as_deref()),
                category,
                category_id: r.get(7)?,
                poster: r.get(8)?,
                backdrop: r.get(9)?,
                rating: r.get(10)?,
                genre_text: r.get(11)?,
                added: r.get(12)?,
                adult: r.get(13)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// The tmdb table's spelling of a work kind: our "series" are TMDB's "tv".
pub fn tmdb_kind(kind: &str) -> &str {
    if kind == "movie" { "movie" } else { "tv" }
}

/// Recomputes `work` / `work_facet` and the members' `work_key` for one kind.
/// TMDB details stored for the kind's titles (`tmdb.rs`), by TMDB id.
fn load_tmdb(conn: &Connection, kind: &str) -> Result<HashMap<String, crate::tmdb::Info>> {
    let kind = tmdb_kind(kind);
    let rows: Vec<(String, String)> = conn
        .prepare("SELECT id, json FROM tmdb WHERE kind = ?1 AND json IS NOT NULL")?
        .query_map([kind], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_, _>>()?;
    Ok(rows.into_iter().filter_map(|(id, j)| serde_json::from_str(&j).ok().map(|i| (id, i))).collect())
}

/// Is TMDB's title the one the provider means? Providers sometimes attach
/// another title's id (a show's id to a movie): the title or the year must fit.
pub fn tmdb_fits(info: &crate::tmdb::Info, titles: &[&str], year: Option<i64>) -> bool {
    let names: Vec<String> =
        [Some(info.title.as_str()), info.original_title.as_deref()].into_iter().flatten().map(norm_title).collect();
    let title_fits = titles.iter().map(|t| norm_title(t)).any(|t| {
        names.iter().any(|n| *n == t || (t.len() >= 6 && n.len() >= 6 && (n.contains(t.as_str()) || t.contains(n.as_str()))))
    });
    let year_fits = matches!((year, info.year), (Some(a), Some(b)) if (a - b).abs() <= 1);
    title_fits || year_fits
}

fn rebuild_works(conn: &Connection, kind: &str) -> Result<usize> {
    let members = load_members(conn, kind)?;
    let mut keys = assign_keys(&members);
    let table = if kind == "movie" { "movie" } else { "series" };
    let tmdb = load_tmdb(conn, kind)?;
    // titles identified by search (tmdb.rs) join the TMDB group the search
    // found; `tmdb_fits` below still guards the group against bad matches
    let search = tmdb_kind(kind);
    let mapped = load_tmdb_map(conn)?;
    for k in &mut keys {
        if !(k.starts_with("title:") || k.starts_with("item:")) {
            continue;
        }
        if let Some(id) = mapped.get(&(search.to_owned(), k.clone())) {
            *k = format!("tmdb:{id}");
        }
    }

    let mut groups: HashMap<&str, Vec<&Member>> = HashMap::new();
    let mut order: Vec<&str> = Vec::new();
    for (m, k) in members.iter().zip(&keys) {
        let e = groups.entry(k.as_str()).or_default();
        if e.is_empty() {
            order.push(k.as_str());
        }
        e.push(m);
    }

    conn.execute("DELETE FROM work WHERE kind = ?1", [kind])?;
    conn.execute("DELETE FROM work_facet WHERE kind = ?1", [kind])?;
    {
        let mut set_key = conn.prepare(&format!("UPDATE {table} SET work_key = ?3 WHERE source_id = ?1 AND id = ?2"))?;
        for (m, k) in members.iter().zip(&keys) {
            set_key.execute(params![m.source_id, m.id, k])?;
        }
    }
    let mut insert_work = conn.prepare(
        "INSERT INTO work (kind, key, title, year, poster, backdrop, rating, genre, added, versions, badges, services,
                           adult, source_id, item_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
    )?;
    let mut insert_facet =
        conn.prepare("INSERT OR IGNORE INTO work_facet (kind, facet, value, key) VALUES (?1, ?2, ?3, ?4)")?;

    for key in &order {
        let ms = &groups[key];
        // metadata from the best-weighted copy (ties: the one with artwork, newest)
        let rep = ms
            .iter()
            .max_by_key(|m| (m.weight(), m.poster.is_some() as i64, m.added.unwrap_or(0)))
            .copied()
            .unwrap();
        let title = most_common(ms.iter().map(|m| (m.title.clone(), m.weight().max(1)))).unwrap_or_else(|| rep.title.clone());
        let year = most_common(ms.iter().filter_map(|m| m.year.map(|y| (y, 1))));
        let by_weight = |f: &dyn Fn(&Member) -> Option<String>| -> Option<String> {
            let mut best: Option<(i64, String)> = None;
            for m in ms {
                if let Some(v) = f(m).filter(|v| !v.is_empty())
                    && best.as_ref().is_none_or(|(w, _)| m.weight() > *w)
                {
                    best = Some((m.weight(), v));
                }
            }
            best.map(|(_, v)| v)
        };
        let mut poster = by_weight(&|m| m.poster.clone());
        let mut backdrop = by_weight(&|m| m.backdrop.clone());
        let mut rating = ms.iter().filter_map(|m| m.rating).fold(None, |a: Option<f64>, r| Some(a.map_or(r, |a| a.max(r))));
        let mut year = year;
        let info = key.strip_prefix("tmdb:").and_then(|id| tmdb.get(id)).filter(|i| {
            let titles: Vec<&str> = ms.iter().map(|m| m.title.as_str()).collect();
            tmdb_fits(i, &titles, year)
        });
        if let Some(i) = info {
            // the provider's own data wins; TMDB fills the gaps
            year = year.or(i.year);
            if rating.is_none_or(|r| r <= 0.0) && i.votes >= 20 {
                rating = i.rating;
            }
            poster = poster.or_else(|| i.poster.as_ref().map(|p| format!("{}/w500{p}", crate::tmdb::IMAGES)));
            backdrop = backdrop.or_else(|| i.backdrop.as_ref().map(|p| format!("{}/w1280{p}", crate::tmdb::IMAGES)));
        }

        let mut genres: Vec<&'static str> = Vec::new();
        let mut services: Vec<&'static str> = Vec::new();
        let mut languages: Vec<&'static str> = Vec::new();
        let mut quality: Vec<&'static str> = Vec::new();
        for m in ms {
            for g in m.genre_text.as_deref().map(genre::from_text).unwrap_or_default() {
                if !genres.contains(&g) {
                    genres.push(g);
                }
            }
            for g in m.category.as_deref().map(genre::from_category).unwrap_or_default() {
                if !genres.contains(&g) {
                    genres.push(g);
                }
            }
            if let Some(s) = m.variant.service
                && !services.contains(&s)
            {
                services.push(s);
            }
            for l in [m.variant.language, m.variant.subtitles].into_iter().flatten() {
                if !languages.contains(&l) {
                    languages.push(l);
                }
            }
            for q in &m.variant.quality {
                if !quality.contains(q) {
                    quality.push(q);
                }
            }
            if m.variant.origin == variant::Origin::Bluray && !quality.contains(&"Blu-ray") {
                quality.push("Blu-ray");
            }
        }
        if let Some(i) = info {
            for g in genre::from_text(&i.genres.join(", ")) {
                if !genres.contains(&g) {
                    genres.push(g);
                }
            }
        }
        genres.sort_by_key(|g| genre::GENRES.iter().position(|x| x == g).unwrap_or(usize::MAX));
        let added = ms.iter().filter_map(|m| m.added).max();
        let adult = ms.iter().any(|m| m.adult);

        insert_work.execute(params![
            kind,
            key,
            title,
            year,
            poster,
            backdrop,
            rating,
            (!genres.is_empty()).then(|| genres.join(", ")),
            added,
            ms.len() as i64,
            quality.join("|"),
            services.join("|"),
            adult,
            rep.source_id,
            rep.id
        ])?;
        let mut facet = |facet: &str, value: &str| insert_facet.execute(params![kind, facet, value, key]).map(|_| ());
        for g in &genres {
            facet("genre", g)?;
        }
        for s in &services {
            facet("service", s)?;
        }
        for l in &languages {
            facet("language", l)?;
        }
        for q in &quality {
            facet("quality", q)?;
        }
        if let Some(d) = genre::decade(year) {
            facet("decade", &d)?;
        }
        for m in ms {
            if let Some(c) = &m.category_id {
                facet("collection", &format!("{}:{c}", m.source_id))?;
            }
        }
        if let Some(i) = info {
            if let Some(l) = i.language.as_deref().and_then(lang::name) {
                facet("original", l)?;
            }
            if let Some(c) = &i.collection {
                facet("franchise", c)?;
            }
            for n in &i.networks {
                facet("network", n)?;
            }
        }
    }
    Ok(order.len())
}

// ----------------------------------------------------------- live channels

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
    const QUALITY: &[&str] = &["uhd", "fhd", "hd", "sd", "4k", "8k", "hevc", "h265", "raw", "50fps", "60fps"];
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
    let mut parts: Vec<String> =
        if own.is_empty() { cat.iter().filter(|b| !EXTRAS.contains(&b.as_str())).cloned().collect() } else { own };
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
    let label = parts.iter().map(|b| nice(b)).collect::<Vec<_>>().join(" · ");
    if label.is_empty() { "Standard".to_owned() } else { label }
}

fn rebuild_channel_groups(conn: &Connection) -> Result<usize> {
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
    conn.execute("UPDATE channel SET group_key = NULL WHERE separator = 1", [])?;
    let mut set_key = conn.prepare("UPDATE channel SET group_key = ?3 WHERE source_id = ?1 AND id = ?2")?;
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
            .unwrap();
        let picked = prefs.get(key).and_then(|(sid, id)| cs.iter().find(|c| c.source_id == *sid && c.id == *id));
        let favorite = cs
            .iter()
            .filter_map(|c| favorites.get(&(c.source_id, c.id.clone())).map(|at| (*at, c)))
            .max_by_key(|(at, _)| *at)
            .map(|(_, c)| c);
        let chosen = picked.or(favorite).copied().unwrap_or(best);
        let title = most_common(cs.iter().map(|c| (c.title.clone(), 1))).unwrap_or_else(|| best.title.clone());
        let category_genre =
            most_common(cs.iter().filter_map(|c| c.category_title.as_deref().and_then(genre::live_from_category)).map(|g| (g, 1)));
        let genre = category_genre.unwrap_or_else(|| genre::live_from_title(&title));
        let epg_id = most_common(cs.iter().filter_map(|c| c.epg_id.clone()).map(|e| (e, 1)));
        let logo = chosen.logo.clone().or_else(|| best.logo.clone()).or_else(|| cs.iter().find_map(|c| c.logo.clone()));
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

/// Recomputes all derived grouping data. Call inside the writer's
/// transaction after the catalog changed.
pub fn rebuild(conn: &Connection) -> Result<()> {
    let t = std::time::Instant::now();
    let movies = rebuild_works(conn, "movie")?;
    let series = rebuild_works(conn, "series")?;
    let channels = rebuild_channel_groups(conn)?;
    conn.execute(
        "INSERT INTO setting (key, value) VALUES ('works.rules', ?1)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        [RULES_VERSION.to_string()],
    )?;
    log::info!("grouped catalog: {movies} movies, {series} series, {channels} channels in {:?}", t.elapsed());
    Ok(())
}

/// Startup: rebuild when the rules changed since the data was grouped.
pub fn rebuild_if_stale(conn: &mut Connection) -> Result<()> {
    let stored: Option<String> =
        conn.query_row("SELECT value FROM setting WHERE key = 'works.rules'", [], |r| r.get(0)).ok();
    if stored.as_deref().and_then(|v| v.parse::<i64>().ok()) == Some(RULES_VERSION) {
        return Ok(());
    }
    let tx = conn.transaction()?;
    rebuild(&tx)?;
    tx.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_titles_for_grouping() {
        assert_eq!(norm_title("Love & Anarchy"), "love and anarchy");
        assert_eq!(norm_title("Kärlek & Anarki"), "karlek and anarki");
        assert_eq!(norm_title("DAHMER - Monster: The Jeffrey Dahmer Story"), "dahmer monster the jeffrey dahmer story");
        assert_eq!(norm_title("Dahmer – Monster: The Jeffrey Dahmer Story"), "dahmer monster the jeffrey dahmer story");
        assert_eq!(norm_title("Børn"), "born");
        assert_eq!(norm_title("  WALL·E "), "wall e");
    }

    fn insert_series(c: &Connection, id: &str, tag: &str, title: &str, year: Option<i64>, tmdb: Option<&str>, cat: &str) {
        c.execute(
            "INSERT INTO series (source_id, id, name, title, tag, year, tmdb, category_id, genre, position)
             VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, 'Drama / Kriminal', 0)",
            params![id, format!("{tag} - {title}"), title, tag, year, tmdb, cat],
        )
        .unwrap();
    }

    fn setup() -> Connection {
        let c = crate::db::test_conn();
        for (id, name) in [("nf", "NETFLIX  SERIES"), ("ap", "APPLE+ SERIES"), ("ap4", "APPLE+ SERIES ⁴ᴷ ³⁸⁴⁰ᴾ ᴰᵒˡᵇʸ ⱽᶦˢᶦᵒⁿ"), ("sc", "NORDIC SERIES"), ("dk", "DANSK SERIE")] {
            c.execute(
                "INSERT INTO category (source_id, kind, id, name, title, position) VALUES (1, 'series', ?1, ?2, ?2, 0)",
                params![id, name],
            )
            .unwrap();
        }
        c
    }

    #[test]
    fn copies_of_a_show_become_one_work() {
        let c = setup();
        insert_series(&c, "1", "NF", "For All Mankind", Some(2019), Some("87917"), "nf");
        insert_series(&c, "2", "A+", "For All Mankind", Some(2019), Some("87917"), "ap");
        insert_series(&c, "3", "4K-A+", "For All Mankind", Some(2019), Some("87917"), "ap4");
        insert_series(&c, "4", "SC", "For All Mankind", Some(2019), None, "sc"); // no id: joins by title + year
        insert_series(&c, "5", "DK", "Kastanjemanden", Some(2021), Some("127865"), "dk");
        insert_series(&c, "6", "NF", "The Chestnut Man", Some(2021), Some("127865"), "nf");
        insert_series(&c, "7", "SC", "Weekly Show", None, None, "sc");
        insert_series(&c, "8", "SC", "Weekly Show", None, None, "sc"); // no id, no year: stays apart
        rebuild(&c).unwrap();

        let works: Vec<(String, String, i64, String, String)> = c
            .prepare("SELECT key, title, versions, badges, services FROM work WHERE kind = 'series' ORDER BY key")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(works.len(), 4);
        assert_eq!(
            works[0],
            ("item:1:7".into(), "Weekly Show".into(), 1, "".into(), "".into())
        );
        let fam = works.iter().find(|w| w.0 == "tmdb:87917").unwrap();
        assert_eq!((fam.1.as_str(), fam.2, fam.3.as_str()), ("For All Mankind", 4, "4K|Dolby Vision"));
        assert_eq!(fam.4, "Netflix|Apple TV+");
        // the international title wins over the local one
        assert_eq!(works.iter().find(|w| w.0 == "tmdb:127865").unwrap().1, "The Chestnut Man");

        let facets: Vec<(String, String)> = c
            .prepare("SELECT facet, value FROM work_facet WHERE kind = 'series' AND key = 'tmdb:87917' ORDER BY facet, value")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        let has = |f: &str, v: &str| facets.iter().any(|(a, b)| a == f && b == v);
        assert!(has("genre", "Drama") && has("genre", "Crime"));
        assert!(has("service", "Apple TV+") && has("language", "Nordic") && has("quality", "4K"));
        assert!(has("decade", "2010s") && has("collection", "1:ap4"));
        let key: String = c.query_row("SELECT work_key FROM series WHERE id = '4'", [], |r| r.get(0)).unwrap();
        assert_eq!(key, "tmdb:87917");
    }

    #[test]
    fn channel_variants_group_per_country() {
        let c = crate::db::test_conn();
        for (id, title, region) in [("sp", "SPORT", "UK"), ("now", "NOW TV SPORT", "UK"), ("att", "AT&T", "US")] {
            c.execute(
                "INSERT INTO category (source_id, kind, id, name, title, region, position) VALUES (1, 'live', ?1, ?2, ?2, ?3, 0)",
                params![id, title, region],
            )
            .unwrap();
        }
        let ch = |id: &str, title: &str, badges: &str, cat: &str, epg: Option<&str>| {
            c.execute(
                "INSERT INTO channel (source_id, id, name, title, badges, category_id, epg_id, position)
                 VALUES (1, ?1, ?2, ?2, ?3, ?4, ?5, 0)",
                params![id, title, badges, cat, epg],
            )
            .unwrap();
        };
        ch("1", "SKY SPORTS F1", "RAW", "sp", Some("SkySportsF1.uk"));
        ch("2", "SKY SPORTS F1", "RAW HEVC", "sp", Some("SkySportsF1.uk"));
        ch("3", "SKY SPORTS F1", "SD", "sp", Some("SkySportsF1.uk"));
        ch("4", "SKY SPORTS F1", "HD", "sp", Some("SkySportsF1.uk"));
        ch("5", "SKY SPORTS F1", "4K", "now", Some("skysportsf1.uk"));
        ch("6", "BBC NEWS", "RAW", "att", None);
        ch("7", "TV2 SPORT 1", "", "sp", None);
        ch("8", "TV 2 SPORT 1", "HD", "sp", None);
        rebuild(&c).unwrap();
        let groups: Vec<(String, String, String, i64, String)> = c
            .prepare("SELECT key, title, genre, variants, item_id FROM channel_group ORDER BY key")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(groups.len(), 3);
        let f1 = groups.iter().find(|g| g.0 == "UK|skysportsf1").unwrap();
        // five variants, the regular HD one plays by default
        assert_eq!((f1.2.as_str(), f1.3, f1.4.as_str()), ("Sports", 5, "4"));
        let news = groups.iter().find(|g| g.0 == "US|bbcnews").unwrap();
        assert_eq!(news.2, "News"); // mixed category: genre from the name
        assert_eq!(groups.iter().find(|g| g.0 == "UK|tv2sport1").unwrap().3, 2);

        // a favorited variant plays, and the user's pick wins over that
        c.execute("INSERT INTO favorite (source_id, kind, item_id, added_at) VALUES (1, 'live', '1', 5)", []).unwrap();
        rebuild(&c).unwrap();
        let playing = |c: &Connection| -> String {
            c.query_row("SELECT item_id FROM channel_group WHERE key = 'UK|skysportsf1'", [], |r| r.get(0)).unwrap()
        };
        assert_eq!(playing(&c), "1");
        c.execute("INSERT INTO channel_pref (key, source_id, item_id, updated_at) VALUES ('UK|skysportsf1', 1, '3', 0)", [])
            .unwrap();
        rebuild(&c).unwrap();
        assert_eq!(playing(&c), "3");
    }

    #[test]
    fn tmdb_fills_genres_and_facets() {
        let c = crate::db::test_conn();
        c.execute(
            "INSERT INTO movie (source_id, id, name, title, year, tmdb, position) VALUES (1, 'm', 'x', 'Top Gun: Maverick', 2022, '361743', 0)",
            [],
        )
        .unwrap();
        // a wrong id: TMDB's title and year don't fit the provider's
        c.execute(
            "INSERT INTO movie (source_id, id, name, title, year, tmdb, position) VALUES (1, 'w', 'x', 'Some Film', 2001, '99', 0)",
            [],
        )
        .unwrap();
        let info = |title: &str, year: i64| {
            serde_json::json!({"title": title, "language": "en", "genres": ["Action", "Drama"], "year": year,
                               "collection": "Top Gun Collection", "rating": 8.2, "votes": 900, "poster": "/p.jpg"})
            .to_string()
        };
        for (id, json) in [("361743", info("Top Gun: Maverick", 2022)), ("99", info("Other Show", 2015))] {
            c.execute("INSERT INTO tmdb (kind, id, json, fetched_at) VALUES ('movie', ?1, ?2, 0)", params![id, json]).unwrap();
        }
        rebuild(&c).unwrap();
        let facets = |key: &str| -> Vec<(String, String)> {
            c.prepare("SELECT facet, value FROM work_facet WHERE key = ?1 ORDER BY facet, value")
                .unwrap()
                .query_map([key], |r| Ok((r.get(0)?, r.get(1)?)))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap()
        };
        let f = facets("tmdb:361743");
        for want in [("genre", "Action"), ("genre", "Drama"), ("original", "English"), ("franchise", "Top Gun Collection")] {
            assert!(f.contains(&(want.0.into(), want.1.into())), "{want:?} in {f:?}");
        }
        let (genre, rating, poster): (String, f64, String) = c
            .query_row("SELECT genre, rating, poster FROM work WHERE key = 'tmdb:361743'", [], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
            .unwrap();
        assert_eq!((genre.as_str(), rating), ("Action, Drama", 8.2));
        assert!(poster.ends_with("/w500/p.jpg"));
        assert!(!facets("tmdb:99").iter().any(|(f, _)| f == "genre" || f == "franchise"));
    }

    #[test]
    fn channel_keys_and_variant_labels() {
        assert_eq!(channel_key(Some("DK"), "V Sport Ultra UHD *MULTI*"), channel_key(Some("DK"), "V SPORT ULTRA SD *MULTI*"));
        assert_ne!(channel_key(Some("UK"), "SKY SPORTS+"), channel_key(Some("UK"), "SKY SPORTS"));
        assert_eq!(channel_key(Some("UK"), "SKY SPORTS +"), channel_key(Some("UK"), "SKY SPORTS+"));
        assert_eq!(channel_key(None, "TV 2 / Fyn"), "-|tv2fyn");
        assert_eq!(channel_key(Some("DK"), "V SPORT ULTRA *MULTI-AUDIO*"), channel_key(Some("DK"), "V Sport Ultra *MULTI*"));
        assert_eq!(channel_variant_label("RAW HEVC", "RAW VIP DOLBY AUDIO"), "RAW · HEVC · VIP · Dolby Audio");
        assert_eq!(channel_variant_label("", "HD RAW"), "HD · RAW");
        assert_eq!(channel_variant_label("RAW 50FPS", "RAW"), "RAW · 50fps");
        assert_eq!(channel_variant_label("", ""), "Standard");
    }

    /// The tmdb_map lookup used by rebuild (identify_test in tmdb.rs feeds it).
    #[test]
    fn rebuild_applies_tmdb_map() {
        let c = crate::db::test_conn();
        c.execute(
            "INSERT INTO movie (source_id, id, name, title, year, position) VALUES (1, 'm', 'x', 'Jungle Cruise', 2021, 0)",
            [],
        )
        .unwrap();
        rebuild(&c).unwrap();
        assert_eq!(c.query_row("SELECT key FROM work WHERE kind = 'movie'", [], |r| r.get::<_, String>(0)).unwrap(), "title:jungle cruise|2021");
        c.execute(
            "INSERT INTO tmdb_map (kind, source_key, tmdb_id, searched_at) VALUES ('movie', 'title:jungle cruise|2021', '522931', 0)",
            [],
        )
        .unwrap();
        c.execute(
            "INSERT INTO tmdb (kind, id, json, fetched_at) VALUES ('movie', '522931',
                 '{\"title\":\"Jungle Cruise\",\"poster\":\"/p.jpg\"}', 0)",
            [],
        )
        .unwrap();
        rebuild(&c).unwrap();
        let (key, poster): (String, String) =
            c.query_row("SELECT key, poster FROM work WHERE kind = 'movie'", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!(key, "tmdb:522931");
        assert!(poster.ends_with("/w500/p.jpg"));
        assert_eq!(c.query_row("SELECT work_key FROM movie WHERE id = 'm'", [], |r| r.get::<_, String>(0)).unwrap(), key);
    }

    /// A miss sentinel ('' is stored) does not remap works and does not
    /// count in load_tmdb_map; tmdb_id_of filters it.
    #[test]
    fn tmdb_map_miss_is_not_a_mapping() {
        let c = crate::db::test_conn();
        c.execute("INSERT INTO tmdb_map (kind, source_key, tmdb_id, searched_at) VALUES ('movie', 'item:1:x', '', 0)", []).unwrap();
        rebuild(&c).unwrap();
        let m = load_tmdb_map(&c).unwrap();
        assert!(m.is_empty());
        assert_eq!(tmdb_id_of(&c, "item:1:x").unwrap(), None);
    }

    #[test]
    fn variant_ranking() {        assert!(channel_rank("HD") > channel_rank("RAW"));
        assert!(channel_rank("RAW") > channel_rank("RAW HEVC"));
        assert!(channel_rank("") > channel_rank("SD"));
        assert!(channel_rank("4K") > channel_rank("SD HEVC"));
    }
}
