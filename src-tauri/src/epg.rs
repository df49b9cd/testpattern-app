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
        (chrono::NaiveDateTime::parse_from_str(&s[..14], "%Y%m%d%H%M%S").ok()?, &s[14..])
    } else {
        (chrono::NaiveDateTime::parse_from_str(&format!("{}00", &s[..12]), "%Y%m%d%H%M%S").ok()?, &s[12..])
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
        let ev = xml
            .read_event_into(&mut buf)
            .map_err(|e| Error::msg(format!("EPG XML error at byte {}: {e}", xml.buffer_position())))?;
        match ev {
            Event::Start(ref e) | Event::Empty(ref e) => {
                // <title/> etc. carry no text and never see an End event
                let childless = matches!(ev, Event::Empty(_));
                match e.name().as_ref() {
                    "programme" if !childless => {
                        let mut p = Programme::default();
                        let mut ok = true;
                        for a in e.attributes().flatten() {
                            let v = a.normalized_value(XmlVersion::Implicit1_0).unwrap_or_default();
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
                        let onscreen = e.attributes().flatten().any(|a| {
                            a.key.as_ref() == "system" && a.value == "onscreen"
                        });
                        let has = cur.as_ref().is_some_and(|p| p.episode.is_some());
                        field = if onscreen || !has { Field::Episode } else { Field::None };
                        text.clear();
                    }
                    "icon" => {
                        if let Some(p) = cur.as_mut() {
                            for a in e.attributes().flatten() {
                                if a.key.as_ref() == "src" {
                                    p.icon = Some(a.normalized_value(XmlVersion::Implicit1_0).unwrap_or_default().into_owned());
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
                        && !p.title.is_empty() {
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
        let num = |s: &str| s.split('/').next().and_then(|n| n.trim().parse::<i64>().ok());
        match (num(parts[0]), num(parts[1])) {
            (Some(s), Some(e)) => return format!("S{:02}E{:02}", s + 1, e + 1),
            (None, Some(e)) => return format!("E{:02}", e + 1),
            _ => {}
        }
    }
    v.to_owned()
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

/// Replaces a source's programmes with a fresh XMLTV import.
pub fn import(conn: &mut Connection, source_id: i64, xml: &[u8], wanted: &HashSet<String>) -> Result<usize> {
    let now = crate::db::now();
    let (from, to) = (now - 2 * 86400, now + 8 * 86400);
    let tx = conn.transaction()?;
    tx.execute("DELETE FROM programme WHERE source_id = ?1", [source_id])?;
    let count = {
        let mut stmt = tx.prepare(
            "INSERT OR REPLACE INTO programme
               (source_id, epg_id, start, stop, title, subtitle, description, category, episode, icon)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        )?;
        let mut err = None;
        let n = parse_xmltv(
            xml,
            from,
            to,
            |id| wanted.is_empty() || wanted.contains(id),
            |p| {
                if err.is_some() {
                    return;
                }
                if let Err(e) = stmt.execute(params![
                    source_id, p.epg_id, p.start, p.stop, p.title, p.subtitle, p.description, p.category, p.episode, p.icon
                ]) {
                    err = Some(e);
                }
            },
        )?;
        if let Some(e) = err {
            return Err(e.into());
        }
        n
    };
    tx.commit()?;
    Ok(count)
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
        assert_eq!(parse_time("20260926090000 +0000"), parse_time("20260926110000 +0200"));
        assert_eq!(parse_time("20260926090000"), parse_time("20260926090000 +0000"));
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
        let n = parse_xmltv(xml.as_bytes(), 0, i64::MAX, |id| id == "a.uk", |p| out.push(p)).unwrap();
        assert_eq!(n, 1);
        assert_eq!(out[0].title, "Tom & Jerry");
        assert_eq!(out[0].description.as_deref(), Some("Cat & mouse"));
        assert_eq!(out[0].episode.as_deref(), Some("S02E05"));
        assert_eq!(out[0].category.as_deref(), Some("Kids"));
    }
}
