//! Live channels: listings with now/next, channel groups and their feeds, and
//! the Live page's country/genre navigation.

use super::*;

/// Channel rows with now/next. `grouped`: one row per channel group,
/// titled as the group and playing its chosen variant (`g.source_id`/
/// `g.item_id`), a favorite when any variant is. A feed without its own EPG
/// id uses its group's.
fn channel_select(grouped: bool) -> String {
    let (title, favorite, from) = if grouped {
        (
            "g.title",
            "EXISTS(SELECT 1 FROM channel x JOIN favorite f ON f.source_id = x.source_id AND f.kind = 'live' AND f.item_id = x.id
                     WHERE x.group_key = g.key)",
            "channel_group g JOIN channel c ON c.source_id = g.source_id AND c.id = g.item_id",
        )
    } else {
        (
            "c.title",
            "EXISTS(SELECT 1 FROM favorite f WHERE f.source_id = c.source_id AND f.kind = 'live' AND f.item_id = c.id)",
            "channel c LEFT JOIN channel_group g ON g.key = c.group_key",
        )
    };
    format!(
        "SELECT c.source_id, c.id, c.num, {title}, COALESCE(c.logo, g.logo), COALESCE(c.epg_id, g.epg_id), c.category_id,
                c.badges, c.archive, c.archive_days, {favorite},
                p.title, p.start, p.stop, q.title, q.start, q.stop, g.key, g.variants, g.country, g.genre
           FROM {from}
           LEFT JOIN programme p ON p.source_id = c.source_id AND p.epg_id = COALESCE(c.epg_id, g.epg_id)
                AND p.start = (SELECT MAX(start) FROM programme
                                WHERE source_id = c.source_id AND epg_id = COALESCE(c.epg_id, g.epg_id) AND start <= :now)
                AND p.stop > :now
           LEFT JOIN programme q ON q.source_id = c.source_id AND q.epg_id = COALESCE(c.epg_id, g.epg_id)
                AND q.start = (SELECT MIN(start) FROM programme
                                WHERE source_id = c.source_id AND epg_id = COALESCE(c.epg_id, g.epg_id) AND start > :now)"
    )
}

fn row_to_channel(r: &Row) -> rusqlite::Result<ChannelItem> {
    let brief = |i: usize| -> rusqlite::Result<Option<Brief>> {
        Ok(match r.get::<_, Option<String>>(i)? {
            Some(title) => Some(Brief {
                title,
                start: r.get(i + 1)?,
                stop: r.get(i + 2)?,
            }),
            None => None,
        })
    };
    Ok(ChannelItem {
        source_id: r.get(0)?,
        id: r.get(1)?,
        num: r.get(2)?,
        title: r.get(3)?,
        logo: r.get(4)?,
        epg_id: r.get(5)?,
        category_id: r.get(6)?,
        badges: split_badges(r.get(7)?),
        archive: r.get(8)?,
        archive_days: r.get(9)?,
        favorite: r.get(10)?,
        now: brief(11)?,
        next: brief(14)?,
        group: match r.get::<_, Option<String>>(17)? {
            Some(key) => Some(GroupInfo {
                key,
                variants: r.get(18)?,
                country: r.get(19)?,
                genre: r.get(20)?,
            }),
            None => None,
        },
    })
}

pub fn query_channels(conn: &Connection, q: &ChannelQuery) -> Result<Page<ChannelItem>> {
    let g = q.grouped;
    let mut filters = vec!["c.separator = 0".to_owned()];
    let mut args: Vec<(String, Sql)> = vec![(":now".into(), Sql::Integer(now()))];
    if !settings::get_bool(conn, "content.showAdult") {
        filters.push(if g { "g.adult = 0" } else { "c.adult = 0" }.into());
    }
    match (q.source_id, &q.category_id, g) {
        (Some(s), Some(cat), true) => {
            filters.push("g.key IN (SELECT group_key FROM channel WHERE source_id = :source AND category_id = :cat)".into());
            args.push((":source".into(), Sql::Integer(s)));
            args.push((":cat".into(), Sql::Text(cat.clone())));
        }
        (Some(s), None, true) => {
            filters
                .push("g.key IN (SELECT group_key FROM channel WHERE source_id = :source)".into());
            args.push((":source".into(), Sql::Integer(s)));
        }
        (s, cat, false) => {
            if let Some(s) = s {
                filters.push("c.source_id = :source".into());
                args.push((":source".into(), Sql::Integer(s)));
            }
            if let Some(cat) = cat {
                filters.push("c.category_id = :cat".into());
                args.push((":cat".into(), Sql::Text(cat.clone())));
            }
        }
        _ => {}
    }
    match q.country.as_deref() {
        // channels whose category has no region
        Some("") => filters.push("g.key IS NOT NULL AND g.country IS NULL".into()),
        Some(country) => {
            filters.push("g.country = :country".into());
            args.push((":country".into(), Sql::Text(country.to_owned())));
        }
        None => {}
    }
    if let Some(genre) = &q.genre {
        filters.push("g.genre = :genre".into());
        args.push((":genre".into(), Sql::Text(genre.clone())));
    }
    if q.with_epg {
        filters.push(
            "COALESCE(c.epg_id, g.epg_id) IS NOT NULL
             AND EXISTS (SELECT 1 FROM programme pe WHERE pe.source_id = c.source_id AND pe.epg_id = COALESCE(c.epg_id, g.epg_id))"
                .into(),
        );
    }
    if let Some(text) = q.q.as_deref().filter(|t| !t.trim().is_empty()) {
        filters.push(
            if g {
                "g.title LIKE :q ESCAPE '\\'"
            } else {
                "c.title LIKE :q ESCAPE '\\'"
            }
            .into(),
        );
        args.push((":q".into(), Sql::Text(like_pattern(text))));
    }
    let (join, order) = match (q.favorites, g) {
        // a channel is a favorite since its first favorited variant
        (true, true) => (
            " JOIN (SELECT x.group_key AS key, MIN(f.added_at) AS added_at FROM favorite f
                      JOIN channel x ON x.source_id = f.source_id AND x.id = f.item_id
                     WHERE f.kind = 'live' AND x.group_key IS NOT NULL GROUP BY x.group_key) fv ON fv.key = g.key",
            "fv.added_at".to_owned(),
        ),
        (true, false) => (
            " JOIN favorite fv ON fv.source_id = c.source_id AND fv.kind = 'live' AND fv.item_id = c.id",
            "fv.added_at".to_owned(),
        ),
        // a country: its regular channels first, event feeds last; a genre:
        // the viewer's countries first
        (false, true) if q.country.is_some() => ("", genre_order()),
        (false, true) if q.genre.is_some() => ("", country_order_sql(conn)?),
        (false, true) => ("", "g.position".to_owned()),
        (false, false) => ("", "c.source_id, c.position".to_owned()),
    };
    let where_sql = filters.join(" AND ");
    let from = if g {
        "channel_group g JOIN channel c ON c.source_id = g.source_id AND c.id = g.item_id"
    } else {
        "channel c LEFT JOIN channel_group g ON g.key = c.group_key"
    };

    let count_sql = format!("SELECT COUNT(*) FROM {from}{join} WHERE {where_sql}");
    let count_args: Vec<(&str, &dyn rusqlite::ToSql)> = args
        .iter()
        .filter(|(k, _)| k != ":now")
        .map(|(k, v)| (k.as_str(), v as &dyn rusqlite::ToSql))
        .collect();
    let total: i64 = conn
        .prepare_cached(&count_sql)?
        .query_row(count_args.as_slice(), |r| r.get(0))?;

    let limit = q.limit.unwrap_or(500).clamp(1, 5000);
    let offset = q.offset.unwrap_or(0).max(0);
    let sql = format!(
        "{}{join} WHERE {where_sql} ORDER BY {order} LIMIT {limit} OFFSET {offset}",
        channel_select(g)
    );
    let named: Vec<(&str, &dyn rusqlite::ToSql)> = args
        .iter()
        .map(|(k, v)| (k.as_str(), v as &dyn rusqlite::ToSql))
        .collect();
    let items = conn
        .prepare_cached(&sql)?
        .query_map(named.as_slice(), row_to_channel)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Page { total, items })
}

#[tauri::command]
pub async fn channels(
    state: State<'_, AppState>,
    query: ChannelQuery,
) -> Result<Page<ChannelItem>> {
    read_blocking(state.inner(), move |conn| {
        let t = Instant::now();
        let page = query_channels(conn, &query)?;
        log::debug!(
            "channels {:?}: {} of {} in {:?}",
            query.category_id,
            page.items.len(),
            page.total,
            t.elapsed()
        );
        Ok(page)
    })
    .await
}

pub fn channel_by_id(conn: &Connection, source_id: i64, id: &str) -> Result<ChannelItem> {
    let sql = format!(
        "{} WHERE c.source_id = :source AND c.id = :id",
        channel_select(false)
    );
    conn.prepare_cached(&sql)?
        .query_row(
            &[
                (":now", &now() as &dyn rusqlite::ToSql),
                (":source", &source_id),
                (":id", &id),
            ],
            row_to_channel,
        )
        .optional()?
        .ok_or_else(|| Error::NotFound(format!("channel {id}")))
}

#[tauri::command]
pub async fn channel(
    state: State<'_, AppState>,
    source_id: i64,
    id: String,
) -> Result<ChannelItem> {
    read_blocking(state.inner(), move |conn| {
        channel_by_id(conn, source_id, &id)
    })
    .await
}

/// A channel group as a row: its title, playing its chosen variant.
pub fn channel_group_by_key(conn: &Connection, key: &str) -> Result<ChannelItem> {
    let sql = format!("{} WHERE g.key = :key", channel_select(true));
    conn.prepare_cached(&sql)?
        .query_row(
            &[(":now", &now() as &dyn rusqlite::ToSql), (":key", &key)],
            row_to_channel,
        )
        .optional()?
        .ok_or_else(|| Error::NotFound(format!("channel {key}")))
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveCountry {
    /// region code from the provider's categories ("DK"); None = no region
    pub code: Option<String>,
    pub name: String,
    pub count: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveCell {
    pub country: Option<String>,
    pub genre: String,
    pub count: i64,
}

/// Live TV navigation: channel groups per country and genre.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveNav {
    /// the viewer's countries first (player languages), then by size
    pub countries: Vec<LiveCountry>,
    /// genres in display order
    pub genres: Vec<String>,
    pub cells: Vec<LiveCell>,
}

/// `ORDER BY` for channel groups: genres in `LIVE_GENRES` order; within one,
/// channels with guide data first — the linear channels (BBC One, ITV, …)
/// ahead of a provider's 24/7 and FAST feeds, which rarely have any.
fn genre_order() -> String {
    let cases: String = crate::works::genre::LIVE_GENRES
        .iter()
        .enumerate()
        .map(|(i, g)| format!(" WHEN '{}' THEN {i}", g.replace('\'', "''")))
        .collect();
    format!("CASE g.genre{cases} ELSE 99 END, g.epg_id IS NULL, g.position")
}

/// `ORDER BY` for channel groups: countries as `live_nav` lists them, each
/// with its guide channels first.
fn country_order_sql(conn: &Connection) -> Result<String> {
    let cases: String = live_nav_for(conn)?
        .countries
        .iter()
        .enumerate()
        .filter_map(|(i, c)| {
            c.code
                .as_ref()
                .map(|code| format!(" WHEN '{}' THEN {i}", code.replace('\'', "''")))
        })
        .collect();
    Ok(format!(
        "CASE g.country{cases} ELSE 9999 END, g.epg_id IS NULL, g.position"
    ))
}

pub fn live_nav_for(conn: &Connection) -> Result<LiveNav> {
    let show_adult = settings::get_bool(conn, "content.showAdult");
    let cells: Vec<LiveCell> = conn
        .prepare_cached(
            "SELECT country, genre, COUNT(*) FROM channel_group WHERE ?1 OR adult = 0 GROUP BY country, genre",
        )?
        .query_map([show_adult], |r| Ok(LiveCell { country: r.get(0)?, genre: r.get(1)?, count: r.get(2)? }))?
        .collect::<Result<_, _>>()?;
    let mut countries: Vec<LiveCountry> = Vec::new();
    for c in &cells {
        match countries.iter_mut().find(|x| x.code == c.country) {
            Some(x) => x.count += c.count,
            None => countries.push(LiveCountry {
                code: c.country.clone(),
                name: c
                    .country
                    .as_deref()
                    .map(crate::works::genre::country_name)
                    .unwrap_or_else(|| "Other".into()),
                count: c.count,
            }),
        }
    }
    let home: Vec<&str> = versions::language_prefs(conn)
        .into_iter()
        .flat_map(|l| {
            crate::works::genre::countries_for_language(l)
                .iter()
                .copied()
        })
        .collect();
    let rank = |c: &LiveCountry| {
        c.code
            .as_deref()
            .and_then(|code| home.iter().position(|h| *h == code))
            .unwrap_or(usize::MAX)
    };
    countries.sort_by(|a, b| {
        rank(a)
            .cmp(&rank(b))
            .then(a.code.is_none().cmp(&b.code.is_none()))
            .then(b.count.cmp(&a.count))
            .then(a.name.cmp(&b.name))
    });
    let genres = crate::works::genre::LIVE_GENRES
        .iter()
        .filter(|g| cells.iter().any(|c| c.genre == **g))
        .map(|g| g.to_string())
        .collect();
    Ok(LiveNav {
        countries,
        genres,
        cells,
    })
}

#[tauri::command]
pub async fn live_nav(state: State<'_, AppState>) -> Result<LiveNav> {
    read_blocking(state.inner(), live_nav_for).await
}

/// One provider feed of a channel, for the variant chips.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelVariant {
    #[serde(flatten)]
    pub channel: ChannelItem,
    /// "RAW · HEVC", "HD · VIP", "4K"
    pub label: String,
    /// provider category, e.g. "Now TV Sport · HD RAW"
    pub category: Option<String>,
    /// the one that plays for this channel
    pub selected: bool,
}

pub fn variants_of(conn: &Connection, key: &str) -> Result<Vec<ChannelVariant>> {
    let chosen: Option<(i64, String)> = conn
        .query_row(
            "SELECT source_id, item_id FROM channel_group WHERE key = ?1",
            [key],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    type Row = (i64, String, String, Option<String>, Option<String>, i64);
    let rows: Vec<Row> = conn
        .prepare_cached(
            "SELECT c.source_id, c.id, c.badges, k.badges, k.name, COALESCE(k.position, 0) * 100000 + c.position
               FROM channel c
               LEFT JOIN category k ON k.source_id = c.source_id AND k.kind = 'live' AND k.id = c.category_id
              WHERE c.group_key = ?1",
        )?
        .query_map([key], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)))?
        .collect::<Result<_, _>>()?;
    let mut out: Vec<(i32, i64, ChannelVariant)> = Vec::new();
    for (sid, id, badges, cat_badges, cat_name, order) in rows {
        let channel = channel_by_id(conn, sid, &id)?;
        let selected = chosen.as_ref().is_some_and(|(s, i)| *s == sid && *i == id);
        let label =
            crate::works::channel_variant_label(&badges, cat_badges.as_deref().unwrap_or(""));
        let category = cat_name.as_deref().map(crate::names::display_category);
        out.push((
            crate::works::channel_rank(&badges),
            order,
            ChannelVariant {
                channel,
                label,
                category,
                selected,
            },
        ));
    }
    out.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    // identical feeds (backups in the same category) get a number
    let mut seen: HashMap<String, usize> = HashMap::new();
    let mut variants: Vec<ChannelVariant> = out.into_iter().map(|(_, _, v)| v).collect();
    for v in &mut variants {
        let n = seen.entry(v.label.clone()).or_insert(0);
        *n += 1;
        if *n > 1 {
            v.label = format!("{} ({n})", v.label);
        }
    }
    Ok(variants)
}

#[tauri::command]
pub async fn channel_variants(
    state: State<'_, AppState>,
    key: String,
) -> Result<Vec<ChannelVariant>> {
    read_blocking(state.inner(), move |conn| variants_of(conn, &key)).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_country_lists_its_guide_channels_first() {
        let c = crate::db::test_conn();
        c.execute(
            "INSERT INTO category (source_id, kind, id, name, title, region, position)
             VALUES (1, 'live', 'ent', 'UK| ENTERTAINMENT', 'ENTERTAINMENT', 'UK', 0)",
            [],
        )
        .unwrap();
        // the provider lists its 24/7 feeds (no guide data) before BBC One
        for (id, title, epg, pos) in [
            ("fast", "BAYWATCH", None, 0),
            ("bbc1", "BBC ONE", Some("BBCOne.uk"), 1),
        ] {
            c.execute(
                "INSERT INTO channel (source_id, id, name, title, category_id, epg_id, position)
                 VALUES (1, ?1, ?2, ?2, 'ent', ?3, ?4)",
                params![id, title, epg, pos],
            )
            .unwrap();
        }
        crate::works::rebuild(&c).unwrap();
        let titles = |q: ChannelQuery| -> Vec<String> {
            query_channels(&c, &q)
                .unwrap()
                .items
                .into_iter()
                .map(|i| i.title)
                .collect()
        };
        let uk = ChannelQuery {
            grouped: true,
            country: Some("UK".into()),
            ..Default::default()
        };
        assert_eq!(titles(uk), ["BBC ONE", "BAYWATCH"]);
        let genre = ChannelQuery {
            grouped: true,
            genre: Some("Entertainment".into()),
            ..Default::default()
        };
        assert_eq!(titles(genre), ["BBC ONE", "BAYWATCH"]);
    }
}
