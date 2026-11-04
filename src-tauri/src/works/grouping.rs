//! Movies and series: copies folded into works, with their browse facets.

use super::*;

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
            by_title_year
                .entry((norm_title(&m.title), y))
                .or_default()
                .insert(format!("tmdb:{t}"));
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
                    Some(keys) if keys.len() == 1 => keys.iter().next().expect("len is 1").clone(),
                    _ => format!("title:{norm}|{y}"),
                },
                None => format!("item:{}:{}", m.source_id, m.id),
            }
        })
        .collect()
}

fn load_members(conn: &Connection, kind: WorkKind) -> Result<Vec<Member>> {
    let table = kind.table();
    let (art, backdrop, genre, added) = match kind {
        WorkKind::Movie => ("x.poster", "NULL", "NULL", "x.added"),
        WorkKind::Series => ("x.cover", "x.backdrop", "x.genre", "x.last_modified"),
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

/// TMDB details stored for the kind's titles (`tmdb.rs`), by TMDB id.
fn load_tmdb(conn: &Connection, kind: WorkKind) -> Result<HashMap<String, crate::tmdb::Info>> {
    let kind = kind.tmdb();
    let rows: Vec<(String, String)> = conn
        .prepare("SELECT id, json FROM tmdb WHERE kind = ?1 AND json IS NOT NULL")?
        .query_map([kind], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_, _>>()?;
    Ok(rows
        .into_iter()
        .filter_map(|(id, j)| serde_json::from_str(&j).ok().map(|i| (id, i)))
        .collect())
}

/// Is TMDB's title the one the provider means? Providers sometimes attach
/// another title's id (a show's id to a movie): the title or the year must fit.
pub fn tmdb_fits(info: &crate::tmdb::Info, titles: &[&str], year: Option<i64>) -> bool {
    let names: Vec<String> = [Some(info.title.as_str()), info.original_title.as_deref()]
        .into_iter()
        .flatten()
        .map(norm_title)
        .collect();
    let title_fits = titles.iter().map(|t| norm_title(t)).any(|t| {
        names.iter().any(|n| {
            *n == t
                || (t.len() >= 6
                    && n.len() >= 6
                    && (n.contains(t.as_str()) || t.contains(n.as_str())))
        })
    });
    let year_fits = matches!((year, info.year), (Some(a), Some(b)) if (a - b).abs() <= 1);
    title_fits || year_fits
}

/// Recomputes `work` / `work_facet` and the members' `work_key` for one kind.
pub(super) fn rebuild_works(conn: &Connection, kind: WorkKind) -> Result<usize> {
    let members = load_members(conn, kind)?;
    let mut keys = assign_keys(&members);
    let table = kind.table();
    let tmdb = load_tmdb(conn, kind)?;
    // titles identified by search (tmdb.rs) join the TMDB group the search
    // found — but only when TMDB's stored data actually fits the members, or
    // a wrong match would infect the whole group assign_keys merged, and a
    // NULL-json row ("not on TMDB") would hide the provider metadata; tmdb is
    // pre-filtered to json IS NOT NULL, so a miss means: don't remap
    let search = kind.tmdb();
    let mapped = load_tmdb_map(conn)?;
    let remap: HashMap<String, String> = {
        let mut by_key: HashMap<&str, Vec<&Member>> = HashMap::new();
        for (m, k) in members.iter().zip(&keys) {
            by_key.entry(k.as_str()).or_default().push(m);
        }
        let mut remap = HashMap::new();
        for (k, ms) in by_key {
            if !(k.starts_with("title:") || k.starts_with("item:")) {
                continue;
            }
            let fits = mapped
                .get(&(search.to_owned(), k.to_owned()))
                .and_then(|id| tmdb.get(id))
                .is_some_and(|info| {
                    let titles: Vec<&str> = ms.iter().map(|m| m.title.as_str()).collect();
                    let year = most_common(ms.iter().filter_map(|m| m.year.map(|y| (y, 1))));
                    tmdb_fits(info, &titles, year)
                });
            if fits {
                let id = mapped[&(search.to_owned(), k.to_owned())].clone();
                remap.insert(k.to_owned(), format!("tmdb:{id}"));
            }
        }
        remap
    };
    for k in &mut keys {
        if let Some(new) = remap.get(k) {
            *k = new.clone();
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
        let mut set_key = conn.prepare(&format!(
            "UPDATE {table} SET work_key = ?3 WHERE source_id = ?1 AND id = ?2"
        ))?;
        for (m, k) in members.iter().zip(&keys) {
            set_key.execute(params![m.source_id, m.id, k])?;
        }
    }
    let mut insert_work = conn.prepare(
        "INSERT INTO work (kind, key, title, year, poster, backdrop, rating, genre, added, versions, badges, services,
                           adult, source_id, item_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
    )?;
    let mut insert_facet = conn.prepare(
        "INSERT OR IGNORE INTO work_facet (kind, facet, value, key) VALUES (?1, ?2, ?3, ?4)",
    )?;

    for key in &order {
        let ms = &groups[key];
        // metadata from the best-weighted copy (ties: the one with artwork, newest)
        let rep = ms
            .iter()
            .max_by_key(|m| (m.weight(), m.poster.is_some() as i64, m.added.unwrap_or(0)))
            .copied()
            .expect("groups are never empty");
        let title = most_common(ms.iter().map(|m| (m.title.clone(), m.weight().max(1))))
            .unwrap_or_else(|| rep.title.clone());
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
        let mut rating = ms
            .iter()
            .filter_map(|m| m.rating)
            .fold(None, |a: Option<f64>, r| Some(a.map_or(r, |a| a.max(r))));
        let mut year = year;
        let info = key
            .strip_prefix("tmdb:")
            .and_then(|id| tmdb.get(id))
            .filter(|i| {
                let titles: Vec<&str> = ms.iter().map(|m| m.title.as_str()).collect();
                tmdb_fits(i, &titles, year)
            });
        if let Some(i) = info {
            // the provider's own data wins; TMDB fills the gaps
            year = year.or(i.year);
            if rating.is_none_or(|r| r <= 0.0) && i.votes >= 20 {
                rating = i.rating;
            }
            poster = poster.or_else(|| {
                i.poster
                    .as_ref()
                    .map(|p| format!("{}/w500{p}", crate::tmdb::IMAGES))
            });
            backdrop = backdrop.or_else(|| {
                i.backdrop
                    .as_ref()
                    .map(|p| format!("{}/w1280{p}", crate::tmdb::IMAGES))
            });
        }

        let mut genres: Vec<&'static str> = Vec::new();
        let mut services: Vec<&'static str> = Vec::new();
        let mut languages: Vec<&'static str> = Vec::new();
        let mut quality: Vec<&'static str> = Vec::new();
        for m in ms {
            for g in m
                .genre_text
                .as_deref()
                .map(genre::from_text)
                .unwrap_or_default()
            {
                if !genres.contains(&g) {
                    genres.push(g);
                }
            }
            for g in m
                .category
                .as_deref()
                .map(genre::from_category)
                .unwrap_or_default()
            {
                if !genres.contains(&g) {
                    genres.push(g);
                }
            }
            if let Some(s) = m.variant.service
                && !services.contains(&s)
            {
                services.push(s);
            }
            for l in [m.variant.language, m.variant.subtitles]
                .into_iter()
                .flatten()
            {
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
        genres.sort_by_key(|g| {
            genre::GENRES
                .iter()
                .position(|x| x == g)
                .unwrap_or(usize::MAX)
        });
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
        let mut facet = |facet: &str, value: &str| {
            insert_facet
                .execute(params![kind, facet, value, key])
                .map(|_| ())
        };
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
