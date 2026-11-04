//! Writing a downloaded catalog into the database (one transaction per
//! source: replaced wholesale, details of items still listed kept).

use super::*;

/// The six lists an Xtream panel serves (`get_*_categories`, `get_*_streams`,
/// `get_series`).
pub(super) struct XtreamLists {
    pub live_categories: Vec<Value>,
    pub movie_categories: Vec<Value>,
    pub series_categories: Vec<Value>,
    pub live: Vec<Value>,
    pub movies: Vec<Value>,
    pub series: Vec<Value>,
}

fn is_adult_name(name: &str) -> bool {
    let n = name.to_uppercase();
    ["ADULT", "XXX", "18+", "PORN", "FOR ADULTS"]
        .iter()
        .any(|k| n.contains(k))
}

pub(super) fn clear_catalog(tx: &Transaction, id: i64) -> Result<()> {
    for table in ["category", "channel", "movie", "series", "episode"] {
        tx.execute(&format!("DELETE FROM {table} WHERE source_id = ?1"), [id])?;
    }
    tx.execute("DELETE FROM search WHERE source_id = ?1", [id])?;
    Ok(())
}

/// Provider detail payloads survive re-syncs ("Up next" and resume read the
/// cached series details); only entries whose item left the catalog go.
fn prune_detail_cache(tx: &Transaction, id: i64) -> Result<()> {
    tx.execute(
        "DELETE FROM detail_cache WHERE source_id = ?1 AND NOT EXISTS (
             SELECT 1 FROM movie m WHERE detail_cache.kind = 'movie' AND m.source_id = ?1 AND m.id = detail_cache.id
             UNION ALL
             SELECT 1 FROM series s WHERE detail_cache.kind = 'series' AND s.source_id = ?1 AND s.id = detail_cache.id)",
        [id],
    )?;
    Ok(())
}

pub(super) fn insert_categories(
    tx: &Transaction,
    id: i64,
    kind: ItemKind,
    cats: &[Value],
) -> Result<HashSet<String>> {
    let mut stmt = tx.prepare(
        "INSERT OR REPLACE INTO category (source_id, kind, id, name, title, region, badges, adult, position)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
    )?;
    let mut adult = HashSet::new();
    for (pos, c) in cats.iter().enumerate() {
        let Some(cid) = str_of(&c["category_id"]) else {
            continue;
        };
        let name = names::unescape(&str_of(&c["category_name"]).unwrap_or_default());
        let cleaned = names::category(&name);
        let is_adult = is_adult_name(&name);
        if is_adult {
            adult.insert(cid.clone());
        }
        stmt.execute(params![
            id,
            kind,
            cid,
            name,
            cleaned.title,
            cleaned.region,
            names::badges_str(&cleaned.badges),
            is_adult,
            pos as i64
        ])?;
    }
    Ok(adult)
}

pub(super) fn write_xtream(conn: &mut Connection, id: i64, l: XtreamLists) -> Result<()> {
    let tx = conn.transaction()?;
    clear_catalog(&tx, id)?;
    let adult_live = insert_categories(&tx, id, ItemKind::Live, &l.live_categories)?;
    let adult_vod = insert_categories(&tx, id, ItemKind::Movie, &l.movie_categories)?;
    let adult_series = insert_categories(&tx, id, ItemKind::Series, &l.series_categories)?;

    let mut search =
        tx.prepare("INSERT INTO search (title, kind, source_id, item_id) VALUES (?1, ?2, ?3, ?4)")?;
    {
        let mut stmt = tx.prepare(
            "INSERT OR REPLACE INTO channel (source_id, id, num, name, title, logo, epg_id, category_id,
                archive, archive_days, separator, badges, adult, added, position)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
        )?;
        for (pos, s) in l.live.iter().enumerate() {
            let Some(sid) = str_of(&s["stream_id"]) else {
                continue;
            };
            let name = names::unescape(&str_of(&s["name"]).unwrap_or_default());
            let separator = names::is_separator(&name);
            let cleaned = names::channel(&name);
            let cat = str_of(&s["category_id"]);
            let adult = i64_of(&s["is_adult"]) == Some(1)
                || cat.as_ref().is_some_and(|c| adult_live.contains(c));
            stmt.execute(params![
                id,
                sid,
                i64_of(&s["num"]),
                name,
                cleaned.title,
                str_of(&s["stream_icon"]),
                str_of(&s["epg_channel_id"]),
                cat,
                i64_of(&s["tv_archive"]).unwrap_or(0) == 1,
                i64_of(&s["tv_archive_duration"]).unwrap_or(0),
                separator,
                names::badges_str(&cleaned.badges),
                adult,
                i64_of(&s["added"]),
                pos as i64
            ])?;
            if !separator {
                search.execute(params![cleaned.title, ItemKind::Live, id, sid])?;
            }
        }
    }
    {
        let mut stmt = tx.prepare(
            "INSERT OR REPLACE INTO movie (source_id, id, name, title, tag, year, poster, rating, tmdb, trailer,
                added, category_id, ext, adult, position)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
        )?;
        for (pos, m) in l.movies.iter().enumerate() {
            let Some(sid) = str_of(&m["stream_id"]) else {
                continue;
            };
            let name = names::unescape(&str_of(&m["name"]).unwrap_or_default());
            let t = names::title(&name);
            let cat = str_of(&m["category_id"]);
            let adult = i64_of(&m["is_adult"]) == Some(1)
                || cat.as_ref().is_some_and(|c| adult_vod.contains(c));
            stmt.execute(params![
                id,
                sid,
                name,
                t.title,
                t.tag,
                t.year,
                str_of(&m["stream_icon"]),
                rating_of(&m["rating"]),
                str_of(&m["tmdb"]),
                str_of(&m["trailer"]),
                i64_of(&m["added"]),
                cat,
                str_of(&m["container_extension"]),
                adult,
                pos as i64
            ])?;
            search.execute(params![t.title, ItemKind::Movie, id, sid])?;
        }
    }
    {
        let mut stmt = tx.prepare(
            "INSERT OR REPLACE INTO series (source_id, id, name, title, tag, year, cover, backdrop, plot, cast_list,
                director, genre, release_date, rating, tmdb, trailer, last_modified, category_id, adult, position)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20)",
        )?;
        for (pos, s) in l.series.iter().enumerate() {
            let Some(sid) = str_of(&s["series_id"]) else {
                continue;
            };
            let name = names::unescape(&str_of(&s["name"]).unwrap_or_default());
            let t = names::title(&name);
            let release = first_str(s, &["releaseDate", "release_date"]);
            let year = t
                .year
                .or_else(|| release.as_deref().and_then(|r| r.get(..4)?.parse().ok()));
            let backdrop = match &s["backdrop_path"] {
                Value::Array(a) => a.iter().find_map(str_of),
                v => str_of(v),
            };
            let cat = str_of(&s["category_id"]);
            let adult = cat.as_ref().is_some_and(|c| adult_series.contains(c));
            stmt.execute(params![
                id,
                sid,
                name,
                t.title,
                t.tag,
                year,
                str_of(&s["cover"]),
                backdrop,
                str_of(&s["plot"]),
                str_of(&s["cast"]),
                str_of(&s["director"]),
                str_of(&s["genre"]),
                release,
                rating_of(&s["rating"]),
                str_of(&s["tmdb"]),
                str_of(&s["youtube_trailer"]),
                i64_of(&s["last_modified"]),
                cat,
                adult,
                pos as i64
            ])?;
            search.execute(params![t.title, ItemKind::Series, id, sid])?;
        }
    }
    drop(search);
    prune_detail_cache(&tx, id)?;
    crate::works::rebuild(&tx)?;
    tx.commit()?;
    Ok(())
}

pub(super) fn write_m3u(conn: &mut Connection, id: i64, pl: &m3u::Playlist) -> Result<()> {
    let tx = conn.transaction()?;
    clear_catalog(&tx, id)?;

    // categories come from group-title, separately for live and VOD
    let mut cats: HashMap<(ItemKind, String), String> = HashMap::new();
    let mut cat_stmt = tx.prepare(
        "INSERT OR REPLACE INTO category (source_id, kind, id, name, title, region, badges, adult, position)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
    )?;
    let mut ch_stmt = tx.prepare(
        "INSERT OR REPLACE INTO channel (source_id, id, num, name, title, logo, epg_id, category_id,
            archive, archive_days, url, separator, badges, adult, position, catchup_mode, catchup_source,
            user_agent, referrer)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)",
    )?;
    let mut mv_stmt = tx.prepare(
        "INSERT OR REPLACE INTO movie (source_id, id, name, title, tag, year, poster, category_id, ext, url, adult, position,
            user_agent, referrer)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
    )?;
    let mut sr_stmt = tx.prepare(
        "INSERT OR REPLACE INTO series (source_id, id, name, title, tag, year, cover, category_id, adult, position)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
    )?;
    let mut ep_stmt = tx.prepare(
        "INSERT OR REPLACE INTO episode (source_id, series_id, id, season, episode, title, image, ext, url,
            user_agent, referrer, position)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
    )?;
    let mut search =
        tx.prepare("INSERT INTO search (title, kind, source_id, item_id) VALUES (?1, ?2, ?3, ?4)")?;
    let mut seen: HashSet<String> = HashSet::new();
    let mut series_seen: HashSet<String> = HashSet::new();

    for (pos, e) in pl.entries.iter().enumerate() {
        // "Show S01E02" / Xtream `/series/` entries become series + episodes
        let episode = e.episode_info();
        let kind = match e.kind() {
            m3u::EntryKind::Live => ItemKind::Live,
            _ if episode.is_some() => ItemKind::Series,
            _ => ItemKind::Movie,
        };
        let group = e.group.clone().unwrap_or_else(|| "Uncategorized".into());
        let cat_id = match cats.get(&(kind, group.clone())) {
            Some(cid) => cid.clone(),
            None => {
                let cid = format!("{:x}", fnv1a(group.as_bytes()));
                let c = names::category(&group);
                cat_stmt.execute(params![
                    id,
                    kind,
                    cid,
                    group,
                    c.title,
                    c.region,
                    names::badges_str(&c.badges),
                    is_adult_name(&group),
                    cats.len() as i64
                ])?;
                cats.insert((kind, group.clone()), cid.clone());
                cid
            }
        };
        let adult = is_adult_name(&group);

        // ids must survive re-syncs (favorites/history key on them), and
        // tokenized URLs change, so hash the identity rather than the URL
        let key = format!(
            "{kind}\0{group}\0{}\0{}",
            e.name,
            e.tvg_id.as_deref().unwrap_or("")
        );
        let mut item_id = format!("{:x}", fnv1a(key.as_bytes()));
        let mut n = 1;
        while !seen.insert(item_id.clone()) {
            item_id = format!("{:x}", fnv1a(format!("{key}\0{n}").as_bytes()));
            n += 1;
        }

        if kind == ItemKind::Live {
            let separator = names::is_separator(&e.name);
            let cleaned = names::channel(&e.name);
            // offered only when `play` can build the catch-up URL
            let days = e.catchup_window();
            ch_stmt.execute(params![
                id,
                item_id,
                e.chno,
                e.name,
                cleaned.title,
                e.logo,
                e.tvg_id,
                cat_id,
                days > 0,
                days,
                e.url,
                separator,
                names::badges_str(&cleaned.badges),
                adult,
                pos as i64,
                e.catchup,
                e.catchup_source,
                e.user_agent,
                e.referrer
            ])?;
            if !separator {
                search.execute(params![cleaned.title, ItemKind::Live, id, item_id])?;
            }
        } else if let Some(info) = episode {
            let t = names::title(&info.series);
            let series_id = format!(
                "{:x}",
                fnv1a(format!("series\0{group}\0{}", t.title.to_lowercase()).as_bytes())
            );
            if series_seen.insert(series_id.clone()) {
                sr_stmt.execute(params![
                    id,
                    series_id,
                    info.series,
                    t.title,
                    t.tag,
                    t.year,
                    e.logo,
                    cat_id,
                    adult,
                    pos as i64
                ])?;
                search.execute(params![t.title, ItemKind::Series, id, series_id])?;
            }
            let ext = e
                .url
                .split(['?', '#'])
                .next()
                .and_then(|p| p.rsplit_once('.'))
                .map(|(_, x)| x.to_owned());
            ep_stmt.execute(params![
                id,
                series_id,
                item_id,
                info.season,
                info.episode,
                info.title,
                e.logo,
                ext,
                e.url,
                e.user_agent,
                e.referrer,
                pos as i64
            ])?;
        } else {
            let t = names::title(&e.name);
            let ext = e
                .url
                .split(['?', '#'])
                .next()
                .and_then(|p| p.rsplit_once('.'))
                .map(|(_, x)| x.to_owned());
            mv_stmt.execute(params![
                id,
                item_id,
                e.name,
                t.title,
                t.tag,
                t.year,
                e.logo,
                cat_id,
                ext,
                e.url,
                adult,
                pos as i64,
                e.user_agent,
                e.referrer
            ])?;
            search.execute(params![t.title, ItemKind::Movie, id, item_id])?;
        }
    }
    drop((cat_stmt, ch_stmt, mv_stmt, sr_stmt, ep_stmt, search));
    prune_detail_cache(&tx, id)?;
    crate::works::rebuild(&tx)?;
    tx.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_conn;
    use serde_json::json;

    fn series_lists(ids: &[&str]) -> XtreamLists {
        XtreamLists {
            live_categories: vec![],
            movie_categories: vec![],
            series_categories: vec![json!({"category_id": "1", "category_name": "EN| DRAMA"})],
            live: vec![],
            movies: vec![],
            series: ids.iter().map(|id| json!({"series_id": id, "name": format!("EN - Show {id} (2024)"), "category_id": "1"})).collect(),
        }
    }

    #[test]
    fn resync_keeps_details_of_items_still_listed() {
        let mut c = test_conn();
        write_xtream(&mut c, 1, series_lists(&["10", "11"])).unwrap();
        for id in ["10", "11"] {
            c.execute("INSERT INTO detail_cache (source_id, kind, id, json, fetched_at) VALUES (1, 'series', ?1, '{}', 0)", [id])
                .unwrap();
        }
        // "11" left the provider's catalog
        write_xtream(&mut c, 1, series_lists(&["10"])).unwrap();
        let kept: Vec<String> = c
            .prepare("SELECT id FROM detail_cache")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(kept, vec!["10"]);
        let title: String = c
            .query_row("SELECT title FROM series WHERE id = '10'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(title, "Show 10");
    }

    #[test]
    fn m3u_episodes_become_series() {
        let mut c = test_conn();
        let pl = m3u::parse(concat!(
            "#EXTM3U\n",
            "#EXTINF:-1 tvg-logo=\"http://i/slow.jpg\" group-title=\"Series: Apple\",EN - Slow Horses (2022) S01E02 - Failure's Contagious\n",
            "http://h/series/u/p/12.mkv\n",
            "#EXTINF:-1 group-title=\"Series: Apple\",EN - Slow Horses (2022) S01E01\n",
            "http://h/series/u/p/11.mkv\n",
            "#EXTINF:-1 group-title=\"Series: Apple\",EN - Slow Horses (2022) S02E01 - Last Stop\n",
            "http://h/series/u/p/21.mkv\n",
            "#EXTINF:-1 group-title=\"Series: Nordic\",Borgen S01 E01\n",
            "http://h/borgen/101.mp4\n",
            "#EXTINF:-1 group-title=\"Movies\",Heat (1995)\n",
            "http://h/movie/u/p/7.mkv\n",
        ));
        write_m3u(&mut c, 1, &pl).unwrap();
        fn count(c: &Connection, sql: &str) -> i64 {
            c.query_row(sql, [], |r| r.get(0)).unwrap()
        }
        assert_eq!(count(&c, "SELECT COUNT(*) FROM series"), 2);
        assert_eq!(count(&c, "SELECT COUNT(*) FROM episode"), 4);
        assert_eq!(count(&c, "SELECT COUNT(*) FROM movie"), 1);
        assert_eq!(
            count(&c, "SELECT COUNT(*) FROM category WHERE kind = 'series'"),
            2
        );
        let (sid, title, year, cover): (String, String, Option<i64>, Option<String>) = c
            .query_row(
                "SELECT id, title, year, cover FROM series WHERE title = 'Slow Horses'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(
            (title.as_str(), year, cover.as_deref()),
            ("Slow Horses", Some(2022), Some("http://i/slow.jpg"))
        );
        let eps: Vec<(i64, i64, String)> = c
            .prepare("SELECT season, episode, title FROM episode WHERE series_id = ?1 ORDER BY season, episode")
            .unwrap()
            .query_map([&sid], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(
            eps,
            [
                (1, 1, "".into()),
                (1, 2, "Failure's Contagious".into()),
                (2, 1, "Last Stop".into())
            ]
        );
        // ids survive a re-sync (history and favorites key on them)
        write_m3u(&mut c, 1, &pl).unwrap();
        assert_eq!(
            count(
                &c,
                &format!("SELECT COUNT(*) FROM series WHERE id = '{sid}'")
            ),
            1
        );
    }

    #[test]
    fn m3u_channels_offer_catchup_only_when_playable() {
        let mut c = test_conn();
        let pl = m3u::parse(concat!(
            "#EXTM3U\n",
            "#EXTINF:-1 catchup-days=\"7\" group-title=\"News\",No scheme\n",
            "http://h/news.m3u8\n",
            "#EXTINF:-1 catchup=\"append\" catchup-source=\"?utc={utc}\" catchup-days=\"3\" group-title=\"News\",Append\n",
            "http://h/sport.m3u8\n",
            "#EXTINF:-1 catchup=\"default\" catchup-days=\"3\" group-title=\"News\",No template\n",
            "http://h/film.m3u8\n",
        ));
        write_m3u(&mut c, 1, &pl).unwrap();
        let rows: Vec<(String, bool, i64, Option<String>)> = c
            .prepare(
                "SELECT name, archive, archive_days, catchup_mode FROM channel ORDER BY position",
            )
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(
            rows,
            vec![
                ("No scheme".into(), false, 0, None),
                ("Append".into(), true, 3, Some("append".into())),
                ("No template".into(), false, 0, Some("default".into())),
            ]
        );
    }
}
