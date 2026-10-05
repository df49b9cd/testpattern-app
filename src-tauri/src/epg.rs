//! XMLTV import and programme guide queries.

use std::collections::HashSet;
use std::io::{BufRead, Read};

use quick_xml::XmlVersion;
use quick_xml::events::Event;
use rusqlite::{Connection, params};
use serde::Serialize;

use crate::error::{Error, Result};

#[derive(Debug, Clone, Default)]
pub struct Programme {
    pub epg_id: String,
    pub start: i64,
    pub stop: i64,
    pub title: String,
    pub subtitle: Option<String>,
    pub description: Option<String>,
    pub category: Option<String>,
    pub episode: Option<String>,
    pub icon: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProgrammeRow {
    pub epg_id: String,
    pub start: i64,
    pub stop: i64,
    pub title: String,
    pub subtitle: Option<String>,
    pub description: Option<String>,
    pub category: Option<String>,
    pub episode: Option<String>,
    pub icon: Option<String>,
}

/// "20260926110000 +0200" → unix seconds.
pub fn parse_time(s: &str) -> Option<i64> {
    let s = s.trim();
    if s.len() < 12 {
        return None;
    }
    let (dt, rest) = if s.len() >= 14 && s.as_bytes()[..14].iter().all(u8::is_ascii_digit) {
        (
            chrono::NaiveDateTime::parse_from_str(&s[..14], "%Y%m%d%H%M%S").ok()?,
            &s[14..],
        )
    } else {
        (
            chrono::NaiveDateTime::parse_from_str(&format!("{}00", &s[..12]), "%Y%m%d%H%M%S")
                .ok()?,
            &s[12..],
        )
    };
    let mut ts = dt.and_utc().timestamp();
    let off = rest.trim();
    if off.len() >= 5 && (off.starts_with('+') || off.starts_with('-')) {
        let sign = if off.starts_with('-') { -1 } else { 1 };
        let h: i64 = off[1..3].parse().ok()?;
        let m: i64 = off[3..5].parse().ok()?;
        ts -= sign * (h * 3600 + m * 60);
    }
    Some(ts)
}

fn resolve_entity(name: &str) -> Option<char> {
    Some(match name {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" => ' ',
        _ => {
            let n = name.strip_prefix('#')?;
            let code = if let Some(hex) = n.strip_prefix('x').or_else(|| n.strip_prefix('X')) {
                u32::from_str_radix(hex, 16).ok()?
            } else {
                n.parse().ok()?
            };
            char::from_u32(code)?
        }
    })
}

#[derive(PartialEq)]
enum Field {
    None,
    Title,
    SubTitle,
    Desc,
    Category,
    Episode,
}

/// Streams an XMLTV document, calling `keep(epg_id)` to filter channels and
/// `sink` for every programme inside the [from, to) window.
pub fn parse_xmltv<R: BufRead>(
    reader: R,
    from: i64,
    to: i64,
    keep: impl Fn(&str) -> bool,
    mut sink: impl FnMut(Programme),
) -> Result<usize> {
    let mut xml = quick_xml::Reader::from_reader(reader);
    let mut buf = Vec::with_capacity(64 * 1024);
    let mut cur: Option<Programme> = None;
    let mut field = Field::None;
    let mut text = String::new();
    let mut count = 0usize;

    loop {
        let ev = xml.read_event_into(&mut buf).map_err(|e| {
            Error::msg(format!(
                "EPG XML error at byte {}: {e}",
                xml.buffer_position()
            ))
        })?;
        match ev {
            Event::Start(ref e) | Event::Empty(ref e) => {
                // <title/> etc. carry no text and never see an End event
                let childless = matches!(ev, Event::Empty(_));
                match e.name().as_ref() {
                    "programme" if !childless => {
                        let mut p = Programme::default();
                        let mut ok = true;
                        for a in e.attributes().flatten() {
                            let v = a
                                .normalized_value(XmlVersion::Implicit1_0)
                                .unwrap_or_default();
                            match a.key.as_ref() {
                                "channel" => p.epg_id = v.trim().to_owned(),
                                "start" => p.start = parse_time(&v).unwrap_or(0),
                                "stop" => p.stop = parse_time(&v).unwrap_or(0),
                                _ => {}
                            }
                        }
                        if p.epg_id.is_empty() || p.start == 0 || !keep(&p.epg_id) {
                            ok = false;
                        }
                        if p.stop <= p.start {
                            p.stop = p.start + 1800;
                        }
                        if p.stop < from || p.start >= to {
                            ok = false;
                        }
                        cur = ok.then_some(p);
                    }
                    "title" if cur.is_some() && !childless => {
                        field = Field::Title;
                        text.clear();
                    }
                    "sub-title" if cur.is_some() && !childless => {
                        field = Field::SubTitle;
                        text.clear();
                    }
                    "desc" if cur.is_some() && !childless => {
                        field = Field::Desc;
                        text.clear();
                    }
                    "category" if cur.is_some() && !childless => {
                        field = Field::Category;
                        text.clear();
                    }
                    "episode-num" if cur.is_some() && !childless => {
                        let onscreen = e
                            .attributes()
                            .flatten()
                            .any(|a| a.key.as_ref() == "system" && a.value == "onscreen");
                        let has = cur.as_ref().is_some_and(|p| p.episode.is_some());
                        field = if onscreen || !has {
                            Field::Episode
                        } else {
                            Field::None
                        };
                        text.clear();
                    }
                    "icon" => {
                        if let Some(p) = cur.as_mut() {
                            for a in e.attributes().flatten() {
                                if a.key.as_ref() == "src" {
                                    p.icon = Some(
                                        a.normalized_value(XmlVersion::Implicit1_0)
                                            .unwrap_or_default()
                                            .into_owned(),
                                    );
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            Event::Text(t) if field != Field::None => text.push_str(&t.xml10_content()),
            Event::CData(t) if field != Field::None => text.push_str(&t),
            Event::GeneralRef(r) if field != Field::None => {
                let name = r.into_inner();
                match resolve_entity(&name) {
                    Some(c) => text.push(c),
                    None => {
                        text.push('&');
                        text.push_str(&name);
                        text.push(';');
                    }
                }
            }
            Event::End(e) => match e.name().as_ref() {
                "programme" => {
                    if let Some(p) = cur.take()
                        && !p.title.is_empty()
                    {
                        sink(p);
                        count += 1;
                    }
                    field = Field::None;
                }
                "title" | "sub-title" | "desc" | "category" | "episode-num" => {
                    if let Some(p) = cur.as_mut() {
                        let v = text.trim().to_owned();
                        if !v.is_empty() {
                            match field {
                                Field::Title if p.title.is_empty() => p.title = v,
                                Field::SubTitle if p.subtitle.is_none() => p.subtitle = Some(v),
                                Field::Desc if p.description.is_none() => p.description = Some(v),
                                Field::Category if p.category.is_none() => p.category = Some(v),
                                Field::Episode => p.episode = Some(format_episode(&v)),
                                _ => {}
                            }
                        }
                    }
                    field = Field::None;
                    text.clear();
                }
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    Ok(count)
}

/// xmltv_ns "1.4.0/1" (zero based) → "S02E05"; onscreen values pass through.
fn format_episode(v: &str) -> String {
    let parts: Vec<&str> = v.split('.').collect();
    if parts.len() >= 2 {
        let num = |s: &str| {
            s.split('/')
                .next()
                .and_then(|n| n.trim().parse::<i64>().ok())
        };
        match (num(parts[0]), num(parts[1])) {
            (Some(s), Some(e)) => return format!("S{:02}E{:02}", s + 1, e + 1),
            (None, Some(e)) => return format!("E{:02}", e + 1),
            _ => {}
        }
    }
    v.to_owned()
}

/// Opens a downloaded guide file, transparently gunzipping `.xml.gz`
/// payloads — parsed straight from disk, never held in memory whole.
pub fn open_guide(path: &std::path::Path) -> Result<Box<dyn BufRead + Send>> {
    let mut reader = std::io::BufReader::with_capacity(1 << 16, std::fs::File::open(path)?);
    let gzip = reader.fill_buf()?.starts_with(&[0x1f, 0x8b]);
    Ok(if gzip {
        Box::new(std::io::BufReader::new(flate2::read::MultiGzDecoder::new(
            reader,
        )))
    } else {
        Box::new(reader)
    })
}

/// Days of past programmes to keep: enough for the longest catch-up archive
/// of the source's channels (2..=7 days).
pub fn history_days(conn: &Connection, source_id: i64) -> Result<i64> {
    let longest: i64 = conn.query_row(
        "SELECT COALESCE(MAX(archive_days), 0) FROM channel WHERE source_id = ?1 AND archive = 1",
        [source_id],
        |r| r.get(0),
    )?;
    Ok(longest.clamp(2, 7))
}

/// Decompresses gzip payloads (".xml.gz" guides) transparently.
pub fn maybe_gunzip(bytes: Vec<u8>) -> Result<Vec<u8>> {
    if bytes.starts_with(&[0x1f, 0x8b]) {
        let mut out = Vec::with_capacity(bytes.len() * 8);
        flate2::read::MultiGzDecoder::new(&bytes[..]).read_to_end(&mut out)?;
        Ok(out)
    } else {
        Ok(bytes)
    }
}

/// Result of an XMLTV import.
#[derive(Debug)]
pub struct Imported {
    pub programmes: usize,
    /// The document broke off (IPTV panels generate guides on the fly and a
    /// malformed or truncated tail is common): everything before the error
    /// was imported, channels after it keep their previous programmes.
    pub incomplete: Option<Error>,
}

/// Refreshes a source's programmes from an XMLTV document, keeping
/// `past_days` of history (catch-up) and 8 days ahead. Channels are
/// replaced one at a time as they appear, so a document that breaks off
/// still updates every channel it contains; only a complete document drops
/// the channels that are no longer in it.
pub fn import(
    conn: &mut Connection,
    source_id: i64,
    xml: impl BufRead,
    wanted: &HashSet<String>,
    past_days: i64,
) -> Result<Imported> {
    let now = crate::db::now();
    let (from, to) = (now - past_days * 86400, now + 8 * 86400);
    let tx = conn.transaction()?;
    let mut replaced: HashSet<String> = HashSet::new();
    let mut count = 0usize;
    let parsed = {
        let mut clear = tx.prepare("DELETE FROM programme WHERE source_id = ?1 AND epg_id = ?2")?;
        let mut insert = tx.prepare(
            "INSERT OR REPLACE INTO programme
               (source_id, epg_id, start, stop, title, subtitle, description, category, episode, icon)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        )?;
        let mut db_err = None;
        let parsed = parse_xmltv(
            xml,
            from,
            to,
            |id| wanted.is_empty() || wanted.contains(id),
            |p| {
                if db_err.is_some() {
                    return;
                }
                if !replaced.contains(&p.epg_id) {
                    if let Err(e) = clear.execute(params![source_id, p.epg_id]) {
                        db_err = Some(e);
                        return;
                    }
                    replaced.insert(p.epg_id.clone());
                }
                match insert.execute(params![
                    source_id,
                    p.epg_id,
                    p.start,
                    p.stop,
                    p.title,
                    p.subtitle,
                    p.description,
                    p.category,
                    p.episode,
                    p.icon
                ]) {
                    Ok(_) => count += 1,
                    Err(e) => db_err = Some(e),
                }
            },
        );
        if let Some(e) = db_err {
            return Err(e.into()); // rolls back: the previous guide stays
        }
        parsed
    };
    let incomplete = match parsed {
        Ok(_) => {
            // complete document: channels it no longer lists lose their guide
            let stale: Vec<String> = {
                let mut stmt =
                    tx.prepare("SELECT DISTINCT epg_id FROM programme WHERE source_id = ?1")?;
                let ids = stmt
                    .query_map([source_id], |r| r.get::<_, String>(0))?
                    .collect::<Result<Vec<_>, _>>()?;
                ids.into_iter()
                    .filter(|id| !replaced.contains(id))
                    .collect()
            };
            for id in stale {
                tx.execute(
                    "DELETE FROM programme WHERE source_id = ?1 AND epg_id = ?2",
                    params![source_id, id],
                )?;
            }
            None
        }
        Err(e) if count > 0 => Some(e),
        Err(e) => return Err(e),
    };
    tx.commit()?;
    Ok(Imported {
        programmes: count,
        incomplete,
    })
}

pub fn programmes(
    conn: &Connection,
    source_id: i64,
    epg_id: &str,
    from: i64,
    to: i64,
) -> Result<Vec<ProgrammeRow>> {
    let mut stmt = conn.prepare_cached(
        "SELECT epg_id, start, stop, title, subtitle, description, category, episode, icon
           FROM programme
          WHERE source_id = ?1 AND epg_id = ?2 AND stop > ?3 AND start < ?4
          ORDER BY start",
    )?;
    let rows = stmt
        .query_map(params![source_id, epg_id, from, to], row_to_programme)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn row_to_programme(r: &rusqlite::Row) -> rusqlite::Result<ProgrammeRow> {
    Ok(ProgrammeRow {
        epg_id: r.get(0)?,
        start: r.get(1)?,
        stop: r.get(2)?,
        title: r.get(3)?,
        subtitle: r.get(4)?,
        description: r.get(5)?,
        category: r.get(6)?,
        episode: r.get(7)?,
        icon: r.get(8)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_times() {
        assert_eq!(parse_time("20260926110000 +0200"), Some(1790413200));
        assert_eq!(
            parse_time("20260926090000 +0000"),
            parse_time("20260926110000 +0200")
        );
        assert_eq!(
            parse_time("20260926090000"),
            parse_time("20260926090000 +0000")
        );
    }

    #[test]
    fn parses_programmes() {
        let xml = r#"<?xml version="1.0"?><tv>
          <channel id="a.uk"><display-name>A</display-name></channel>
          <programme start="20260926090000 +0000" stop="20260926100000 +0000" channel="a.uk">
            <title lang="en">Tom &amp; Jerry</title><desc>Cat &#38; mouse</desc>
            <episode-num system="xmltv_ns">1.4.</episode-num><category>Kids</category>
          </programme>
          <programme start="20260926100000 +0000" stop="20260926110000 +0000" channel="b.uk"><title>skip</title></programme>
        </tv>"#;
        let mut out = Vec::new();
        let n = parse_xmltv(
            xml.as_bytes(),
            0,
            i64::MAX,
            |id| id == "a.uk",
            |p| out.push(p),
        )
        .unwrap();
        assert_eq!(n, 1);
        assert_eq!(out[0].title, "Tom & Jerry");
        assert_eq!(out[0].description.as_deref(), Some("Cat & mouse"));
        assert_eq!(out[0].episode.as_deref(), Some("S02E05"));
        assert_eq!(out[0].category.as_deref(), Some("Kids"));
    }

    #[test]
    fn broken_documents_update_what_they_contain() {
        let mut c = crate::db::test_conn();
        let now = crate::db::now();
        let at = |offset: i64| {
            chrono::DateTime::from_timestamp(now + offset, 0)
                .unwrap()
                .format("%Y%m%d%H%M%S +0000")
                .to_string()
        };
        let prog = |ch: &str, title: &str, offset: i64| {
            format!(
                r#"<programme start="{}" stop="{}" channel="{ch}"><title>{title}</title></programme>"#,
                at(offset),
                at(offset + 1800)
            )
        };
        let titles = |c: &Connection| -> Vec<String> {
            let mut s = c
                .prepare("SELECT epg_id || ':' || title FROM programme ORDER BY epg_id, start")
                .unwrap();
            s.query_map([], |r| r.get(0))
                .unwrap()
                .map(|r| r.unwrap())
                .collect()
        };
        let all = HashSet::new();
        let doc = format!(
            "<tv>{}{}{}</tv>",
            prog("a", "old a", 0),
            prog("b", "old b", 0),
            prog("c", "old c", 0)
        );
        assert!(
            import(&mut c, 1, doc.as_bytes(), &all, 2)
                .unwrap()
                .incomplete
                .is_none()
        );
        assert_eq!(titles(&c), ["a:old a", "b:old b", "c:old c"]);

        // breaks off after channel a (a truncated desc, like the provider's)
        let broken = format!(
            "<tv>{}<programme start=\"{}\" stop=\"{}\" channel=\"b\"><title>x</title><desc>cut</tv>",
            prog("a", "new a", 60),
            at(0),
            at(60)
        );
        let r = import(&mut c, 1, broken.as_bytes(), &all, 2).unwrap();
        assert!(r.incomplete.is_some() && r.programmes == 1);
        assert_eq!(titles(&c), ["a:new a", "b:old b", "c:old c"]);

        // a complete document drops channels it no longer lists
        let doc = format!(
            "<tv>{}{}</tv>",
            prog("a", "a again", 0),
            prog("b", "new b", 0)
        );
        import(&mut c, 1, doc.as_bytes(), &all, 2).unwrap();
        assert_eq!(titles(&c), ["a:a again", "b:new b"]);

        // nothing usable at all: an error, and the guide stays
        assert!(import(&mut c, 1, &b"<tv><programme"[..], &all, 2).is_err());
        assert_eq!(titles(&c).len(), 2);
    }

    #[test]
    fn keeps_history_for_the_longest_archive() {
        let mut c = crate::db::test_conn();
        let now = crate::db::now();
        let at = |days_ago: i64| {
            chrono::DateTime::from_timestamp(now - days_ago * 86400, 0)
                .unwrap()
                .format("%Y%m%d%H%M%S +0000")
                .to_string()
        };
        let doc: String = (1..=8)
            .map(|d| format!(r#"<programme start="{}" stop="{}" channel="a"><title>{d} days ago</title></programme>"#, at(d), at(d)))
            .collect();
        let doc = format!("<tv>{doc}</tv>");
        assert_eq!(history_days(&c, 1).unwrap(), 2);
        c.execute(
            "INSERT INTO channel (source_id, id, name, title, archive, archive_days, position) VALUES (1, 'x', 'X', 'X', 1, 5, 0)",
            [],
        )
        .unwrap();
        let days = history_days(&c, 1).unwrap();
        assert_eq!(days, 5);
        // 1..=5 days ago still end inside the window (stop = start + 30 min); 6..=8 don't
        assert_eq!(
            import(&mut c, 1, doc.as_bytes(), &HashSet::new(), days)
                .unwrap()
                .programmes,
            5
        );
        // the old fixed 2-day window kept only 1..=2
        assert_eq!(
            import(&mut c, 1, doc.as_bytes(), &HashSet::new(), 2)
                .unwrap()
                .programmes,
            2
        );
    }

    /// Diagnostic for a real guide: `TP_XMLTV=/path/guide.xml cargo test --lib -- --ignored xmltv_file`
    #[test]
    #[ignore]
    fn xmltv_file() {
        let path = std::env::var("TP_XMLTV").expect("set TP_XMLTV to an XMLTV file");
        let data = maybe_gunzip(std::fs::read(path).unwrap()).unwrap();
        let n = parse_xmltv(&data[..], 0, i64::MAX, |_| true, |_| {}).unwrap();
        println!("{n} programmes");
        assert!(n > 0);
    }
}
