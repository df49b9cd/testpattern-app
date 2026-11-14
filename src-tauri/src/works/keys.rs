//! Group keys: title normalization, TMDB ids and the search-match map.

use std::collections::HashMap;

use rusqlite::Connection;

use crate::error::Result;

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

pub(super) fn valid_tmdb(t: Option<&str>) -> Option<&str> {
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
    Ok(rows
        .into_iter()
        .map(|(kind, key, id)| ((kind, key), id))
        .collect())
}

/// The mapped TMDB id of a search-matched work key ('title:*' | 'item:*').
#[cfg(test)]
pub fn tmdb_id_of(conn: &Connection, key: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT tmdb_id FROM tmdb_map WHERE source_key = ?1",
            [key],
            |r| r.get::<_, String>(0),
        )
        .ok()
        .filter(|id| !id.is_empty()))
}

pub(super) fn most_common<T: Eq + std::hash::Hash + Clone>(
    items: impl Iterator<Item = (T, i64)>,
) -> Option<T> {
    let mut counts: HashMap<T, (i64, usize)> = HashMap::new();
    for (i, (item, w)) in items.enumerate() {
        let e = counts.entry(item).or_insert((0, i));
        e.0 += w;
    }
    counts
        .into_iter()
        .max_by(|a, b| a.1.0.cmp(&b.1.0).then(b.1.1.cmp(&a.1.1)))
        .map(|(k, _)| k)
}
