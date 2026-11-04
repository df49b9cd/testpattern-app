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

mod channels;
pub mod genre;
mod grouping;
mod keys;
pub mod lang;
pub mod variant;
pub mod versions;

#[cfg(test)]
use channels::channel_key;
use channels::rebuild_channel_groups;
pub use channels::{channel_rank, channel_variant_label};
use grouping::rebuild_works;
pub use grouping::tmdb_fits;
#[cfg(test)]
pub use keys::tmdb_id_of;
pub use keys::{load_tmdb_map, norm_title};
use keys::{most_common, valid_tmdb};

use rusqlite::Connection;

use crate::error::Result;
use crate::kind::WorkKind;

/// Bump whenever grouping/facet rules change: startup then rebuilds.
/// v5: `tmdb_map` folds search-matched unidentified titles into TMDB groups.
pub const RULES_VERSION: i64 = 5;

/// Recomputes all derived grouping data. Call inside the writer's
/// transaction after the catalog changed.
pub fn rebuild(conn: &Connection) -> Result<()> {
    let t = std::time::Instant::now();
    let movies = rebuild_works(conn, WorkKind::Movie)?;
    let series = rebuild_works(conn, WorkKind::Series)?;
    let channels = rebuild_channel_groups(conn)?;
    conn.execute(
        "INSERT INTO setting (key, value) VALUES ('works.rules', ?1)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        [RULES_VERSION.to_string()],
    )?;
    log::info!(
        "grouped catalog: {movies} movies, {series} series, {channels} channels in {:?}",
        t.elapsed()
    );
    Ok(())
}

/// Startup: rebuild when the rules changed since the data was grouped.
pub fn rebuild_if_stale(conn: &mut Connection) -> Result<()> {
    let stored: Option<String> = conn
        .query_row(
            "SELECT value FROM setting WHERE key = 'works.rules'",
            [],
            |r| r.get(0),
        )
        .ok();
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
    use rusqlite::params;

    #[test]
    fn normalizes_titles_for_grouping() {
        assert_eq!(norm_title("Love & Anarchy"), "love and anarchy");
        assert_eq!(norm_title("Kärlek & Anarki"), "karlek and anarki");
        assert_eq!(
            norm_title("DAHMER - Monster: The Jeffrey Dahmer Story"),
            "dahmer monster the jeffrey dahmer story"
        );
        assert_eq!(
            norm_title("Dahmer – Monster: The Jeffrey Dahmer Story"),
            "dahmer monster the jeffrey dahmer story"
        );
        assert_eq!(norm_title("Børn"), "born");
        assert_eq!(norm_title("  WALL·E "), "wall e");
    }

    fn insert_series(
        c: &Connection,
        id: &str,
        tag: &str,
        title: &str,
        year: Option<i64>,
        tmdb: Option<&str>,
        cat: &str,
    ) {
        c.execute(
            "INSERT INTO series (source_id, id, name, title, tag, year, tmdb, category_id, genre, position)
             VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, 'Drama / Kriminal', 0)",
            params![id, format!("{tag} - {title}"), title, tag, year, tmdb, cat],
        )
        .unwrap();
    }

    fn setup() -> Connection {
        let c = crate::db::test_conn();
        for (id, name) in [
            ("nf", "NETFLIX  SERIES"),
            ("ap", "APPLE+ SERIES"),
            ("ap4", "APPLE+ SERIES ⁴ᴷ ³⁸⁴⁰ᴾ ᴰᵒˡᵇʸ ⱽᶦˢᶦᵒⁿ"),
            ("sc", "NORDIC SERIES"),
            ("dk", "DANSK SERIE"),
        ] {
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
        insert_series(
            &c,
            "1",
            "NF",
            "For All Mankind",
            Some(2019),
            Some("87917"),
            "nf",
        );
        insert_series(
            &c,
            "2",
            "A+",
            "For All Mankind",
            Some(2019),
            Some("87917"),
            "ap",
        );
        insert_series(
            &c,
            "3",
            "4K-A+",
            "For All Mankind",
            Some(2019),
            Some("87917"),
            "ap4",
        );
        insert_series(&c, "4", "SC", "For All Mankind", Some(2019), None, "sc"); // no id: joins by title + year
        insert_series(
            &c,
            "5",
            "DK",
            "Kastanjemanden",
            Some(2021),
            Some("127865"),
            "dk",
        );
        insert_series(
            &c,
            "6",
            "NF",
            "The Chestnut Man",
            Some(2021),
            Some("127865"),
            "nf",
        );
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
            (
                "item:1:7".into(),
                "Weekly Show".into(),
                1,
                "".into(),
                "".into()
            )
        );
        let fam = works.iter().find(|w| w.0 == "tmdb:87917").unwrap();
        assert_eq!(
            (fam.1.as_str(), fam.2, fam.3.as_str()),
            ("For All Mankind", 4, "4K|Dolby Vision")
        );
        assert_eq!(fam.4, "Netflix|Apple TV+");
        // the international title wins over the local one
        assert_eq!(
            works.iter().find(|w| w.0 == "tmdb:127865").unwrap().1,
            "The Chestnut Man"
        );

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
        let key: String = c
            .query_row("SELECT work_key FROM series WHERE id = '4'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(key, "tmdb:87917");
    }

    #[test]
    fn channel_variants_group_per_country() {
        let c = crate::db::test_conn();
        for (id, title, region) in [
            ("sp", "SPORT", "UK"),
            ("now", "NOW TV SPORT", "UK"),
            ("att", "AT&T", "US"),
        ] {
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
        ch(
            "2",
            "SKY SPORTS F1",
            "RAW HEVC",
            "sp",
            Some("SkySportsF1.uk"),
        );
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
            .query_map([], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })
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
        c.execute(
            "INSERT INTO favorite (source_id, kind, item_id, added_at) VALUES (1, 'live', '1', 5)",
            [],
        )
        .unwrap();
        rebuild(&c).unwrap();
        let playing = |c: &Connection| -> String {
            c.query_row(
                "SELECT item_id FROM channel_group WHERE key = 'UK|skysportsf1'",
                [],
                |r| r.get(0),
            )
            .unwrap()
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
        for (id, json) in [
            ("361743", info("Top Gun: Maverick", 2022)),
            ("99", info("Other Show", 2015)),
        ] {
            c.execute(
                "INSERT INTO tmdb (kind, id, json, fetched_at) VALUES ('movie', ?1, ?2, 0)",
                params![id, json],
            )
            .unwrap();
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
        for want in [
            ("genre", "Action"),
            ("genre", "Drama"),
            ("original", "English"),
            ("franchise", "Top Gun Collection"),
        ] {
            assert!(
                f.contains(&(want.0.into(), want.1.into())),
                "{want:?} in {f:?}"
            );
        }
        let (genre, rating, poster): (String, f64, String) = c
            .query_row(
                "SELECT genre, rating, poster FROM work WHERE key = 'tmdb:361743'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!((genre.as_str(), rating), ("Action, Drama", 8.2));
        assert!(poster.ends_with("/w500/p.jpg"));
        assert!(
            !facets("tmdb:99")
                .iter()
                .any(|(f, _)| f == "genre" || f == "franchise")
        );
    }

    #[test]
    fn channel_keys_and_variant_labels() {
        assert_eq!(
            channel_key(Some("DK"), "V Sport Ultra UHD *MULTI*"),
            channel_key(Some("DK"), "V SPORT ULTRA SD *MULTI*")
        );
        assert_ne!(
            channel_key(Some("UK"), "SKY SPORTS+"),
            channel_key(Some("UK"), "SKY SPORTS")
        );
        assert_eq!(
            channel_key(Some("UK"), "SKY SPORTS +"),
            channel_key(Some("UK"), "SKY SPORTS+")
        );
        assert_eq!(channel_key(None, "TV 2 / Fyn"), "-|tv2fyn");
        assert_eq!(
            channel_key(Some("DK"), "V SPORT ULTRA *MULTI-AUDIO*"),
            channel_key(Some("DK"), "V Sport Ultra *MULTI*")
        );
        assert_eq!(
            channel_variant_label("RAW HEVC", "RAW VIP DOLBY AUDIO"),
            "RAW · HEVC · VIP · Dolby Audio"
        );
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
        assert_eq!(
            c.query_row("SELECT key FROM work WHERE kind = 'movie'", [], |r| r
                .get::<_, String>(0))
                .unwrap(),
            "title:jungle cruise|2021"
        );
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
        let (key, poster): (String, String) = c
            .query_row(
                "SELECT key, poster FROM work WHERE kind = 'movie'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(key, "tmdb:522931");
        assert!(poster.ends_with("/w500/p.jpg"));
        assert_eq!(
            c.query_row("SELECT work_key FROM movie WHERE id = 'm'", [], |r| r
                .get::<_, String>(0))
                .unwrap(),
            key
        );
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
    fn variant_ranking() {
        assert!(channel_rank("HD") > channel_rank("RAW"));
        assert!(channel_rank("RAW") > channel_rank("RAW HEVC"));
        assert!(channel_rank("") > channel_rank("SD"));
        assert!(channel_rank("4K") > channel_rank("SD HEVC"));
    }
}
