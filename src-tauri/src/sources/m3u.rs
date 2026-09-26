//! Extended M3U playlist parser (#EXTM3U / #EXTINF with tvg-* attributes).

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Entry {
    pub name: String,
    pub tvg_id: Option<String>,
    pub tvg_name: Option<String>,
    pub logo: Option<String>,
    pub group: Option<String>,
    pub chno: Option<i64>,
    pub catchup_days: Option<i64>,
    pub user_agent: Option<String>,
    pub url: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    Live,
    Movie,
    Episode,
}

impl Entry {
    /// Xtream-generated playlists encode the kind in the path; for generic
    /// playlists fall back to the file extension.
    pub fn kind(&self) -> EntryKind {
        let path = self.url.split(['?', '#']).next().unwrap_or("");
        if path.contains("/series/") {
            return EntryKind::Episode;
        }
        if path.contains("/movie/") {
            return EntryKind::Movie;
        }
        let ext = path.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
        match ext.as_str() {
            "mp4" | "mkv" | "avi" | "mov" | "m4v" | "wmv" | "webm" | "mpg" | "mpeg" => EntryKind::Movie,
            _ => EntryKind::Live,
        }
    }
}

#[derive(Debug, Default)]
pub struct Playlist {
    pub epg_urls: Vec<String>,
    pub entries: Vec<Entry>,
}

/// Parses `key="value"` pairs up to the first comma outside quotes.
/// Returns the attributes and the remainder (the display name).
fn split_attributes(s: &str) -> (Vec<(String, String)>, &str) {
    let bytes = s.as_bytes();
    let mut attrs = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b',' => return (attrs, &s[i + 1..]),
            b' ' | b'\t' => i += 1,
            _ => {
                let key_start = i;
                while i < bytes.len() && !matches!(bytes[i], b'=' | b' ' | b',') {
                    i += 1;
                }
                let key = s[key_start..i].to_ascii_lowercase();
                if i < bytes.len() && bytes[i] == b'=' {
                    i += 1;
                    let (value, next) = if i < bytes.len() && (bytes[i] == b'"' || bytes[i] == b'\'') {
                        let q = bytes[i];
                        let vs = i + 1;
                        let mut j = vs;
                        while j < bytes.len() && bytes[j] != q {
                            j += 1;
                        }
                        (&s[vs..j.min(bytes.len())], (j + 1).min(bytes.len()))
                    } else {
                        let vs = i;
                        let mut j = vs;
                        while j < bytes.len() && !matches!(bytes[j], b' ' | b',') {
                            j += 1;
                        }
                        (&s[vs..j], j)
                    };
                    attrs.push((key, value.trim().to_owned()));
                    i = next;
                } else if !key.is_empty() {
                    // bare token (e.g. the duration "-1")
                    attrs.push((key, String::new()));
                }
            }
        }
    }
    (attrs, "")
}

fn non_empty(v: String) -> Option<String> {
    (!v.is_empty()).then_some(v)
}

pub fn parse(text: &str) -> Playlist {
    let mut out = Playlist::default();
    let mut pending: Option<Entry> = None;
    let mut group_hint: Option<String> = None;
    let mut ua_hint: Option<String> = None;

    for raw in text.lines() {
        let line = raw.trim().trim_start_matches('\u{feff}');
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("#EXTM3U") {
            let (attrs, _) = split_attributes(rest);
            for (k, v) in attrs {
                if matches!(k.as_str(), "url-tvg" | "x-tvg-url" | "tvg-url") {
                    out.epg_urls.extend(v.split(',').map(|u| u.trim().to_owned()).filter(|u| !u.is_empty()));
                }
            }
        } else if let Some(rest) = line.strip_prefix("#EXTINF:") {
            let (attrs, name) = split_attributes(rest);
            let mut e = Entry { name: name.trim().to_owned(), ..Default::default() };
            for (k, v) in attrs {
                match k.as_str() {
                    "tvg-id" => e.tvg_id = non_empty(v),
                    "tvg-name" => e.tvg_name = non_empty(v),
                    "tvg-logo" | "logo" => e.logo = non_empty(v),
                    "group-title" => e.group = non_empty(v),
                    "tvg-chno" | "channel-number" => e.chno = v.parse().ok(),
                    "catchup-days" | "tvg-rec" | "timeshift" => e.catchup_days = v.parse().ok(),
                    "user-agent" | "http-user-agent" => e.user_agent = non_empty(v),
                    _ => {}
                }
            }
            if e.name.is_empty() {
                e.name = e.tvg_name.clone().unwrap_or_default();
            }
            pending = Some(e);
        } else if let Some(g) = line.strip_prefix("#EXTGRP:") {
            group_hint = non_empty(g.trim().to_owned());
        } else if let Some(opt) = line.strip_prefix("#EXTVLCOPT:") {
            if let Some(ua) = opt.strip_prefix("http-user-agent=") {
                ua_hint = non_empty(ua.trim().to_owned());
            }
        } else if line.starts_with('#') {
            continue;
        } else if let Some(mut e) = pending.take() {
            e.url = line.to_owned();
            if e.group.is_none() {
                e.group = group_hint.take();
            }
            if e.user_agent.is_none() {
                e.user_agent = ua_hint.take();
            }
            group_hint = None;
            ua_hint = None;
            out.entries.push(e);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_extended_m3u() {
        let pl = parse(concat!(
            "#EXTM3U url-tvg=\"http://epg.example/guide.xml.gz\"\n",
            "#EXTINF:-1 tvg-id=\"bbc1.uk\" tvg-name=\"BBC One\" tvg-logo=\"http://l/bbc.png\" group-title=\"UK, Main\",BBC One HD\n",
            "http://h/live/u/p/1.ts\n",
            "#EXTINF:-1,Plain Channel\n",
            "#EXTGRP:Misc\n",
            "#EXTVLCOPT:http-user-agent=VLC/3.0\n",
            "http://h/plain.m3u8\n",
            "#EXTINF:-1 group-title=\"Movies\",Some Movie (2020)\n",
            "http://h/movie/u/p/99.mkv\n",
        ));
        assert_eq!(pl.epg_urls, vec!["http://epg.example/guide.xml.gz"]);
        assert_eq!(pl.entries.len(), 3);
        let a = &pl.entries[0];
        assert_eq!(a.name, "BBC One HD");
        assert_eq!(a.group.as_deref(), Some("UK, Main"));
        assert_eq!(a.tvg_id.as_deref(), Some("bbc1.uk"));
        assert_eq!(a.kind(), EntryKind::Live);
        let b = &pl.entries[1];
        assert_eq!((b.group.as_deref(), b.user_agent.as_deref()), (Some("Misc"), Some("VLC/3.0")));
        assert_eq!(pl.entries[2].kind(), EntryKind::Movie);
    }
}
