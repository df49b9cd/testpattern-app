//! Extended M3U playlist parser (#EXTM3U / #EXTINF with tvg-* attributes)
//! and catch-up URL building (Kodi pvr.iptvsimple conventions).

use std::sync::LazyLock;

use regex::{Captures, Regex};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Entry {
    pub name: String,
    pub tvg_id: Option<String>,
    pub tvg_name: Option<String>,
    pub logo: Option<String>,
    pub group: Option<String>,
    pub chno: Option<i64>,
    pub catchup_days: Option<i64>,
    /// Catch-up scheme: default | append | shift | flussonic | xc (`catchup_url`).
    pub catchup: Option<String>,
    /// URL template (`default`) or suffix (`append`) with placeholders.
    pub catchup_source: Option<String>,
    /// Request headers the stream needs (VLC/Kodi playlist conventions).
    pub user_agent: Option<String>,
    pub referrer: Option<String>,
    pub url: String,
}

/// Catch-up days assumed when a playlist names a scheme but no window.
const DEFAULT_CATCHUP_DAYS: i64 = 5;

impl Entry {
    /// Days of catch-up this entry really offers: a scheme we can build URLs
    /// for, and a window.
    pub fn catchup_window(&self) -> i64 {
        let Some(mode) = self.catchup.as_deref() else { return 0 };
        if catchup_url(mode, self.catchup_source.as_deref(), &self.url, 0, 3600, 0, 0).is_none() {
            return 0;
        }
        self.catchup_days.unwrap_or(DEFAULT_CATCHUP_DAYS).max(0)
    }
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

/// Where an episode entry belongs: "Show S01E02 - Title", "Show S01 E02",
/// "Show 1x02".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EpisodeInfo {
    pub series: String,
    pub season: i64,
    pub episode: i64,
    /// What follows the episode number, if anything ("Title").
    pub title: String,
}

impl Entry {
    /// Series/season/episode of a VOD entry that is an episode (by its name,
    /// or an Xtream `/series/` URL with a parsable name). Live entries never are.
    pub fn episode_info(&self) -> Option<EpisodeInfo> {
        if self.kind() == EntryKind::Live {
            return None;
        }
        static SXXEYY: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new(r"(?i)^(.*?)[\s._-]*\bS(\d{1,3})[\s._-]*E(\d{1,4})\b(.*)$").unwrap()
        });
        static NXNN: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r"(?i)^(.*?)[\s._-]*\b(\d{1,2})x(\d{2,3})\b(.*)$").unwrap());
        let name = self.name.trim();
        let c = SXXEYY.captures(name).or_else(|| NXNN.captures(name))?;
        let series = c[1].trim().trim_end_matches(['-', ':', '|', '.']).trim().to_owned();
        if series.is_empty() {
            return None;
        }
        let mut title = c[4].trim().trim_start_matches(['-', ':', '|', '.']).trim().to_owned();
        if matches!(title.to_ascii_lowercase().as_str(), "mkv" | "mp4" | "avi" | "ts" | "m4v" | "mov" | "webm") {
            title.clear(); // a file name's extension, not a title
        }
        Some(EpisodeInfo { series, season: c[2].parse().ok()?, episode: c[3].parse().ok()?, title })
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
    let mut referrer_hint: Option<String> = None;
    // #EXTM3U catch-up attributes are defaults for every entry
    let mut defaults = Entry::default();

    for raw in text.lines() {
        let line = raw.trim().trim_start_matches('\u{feff}');
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("#EXTM3U") {
            let (attrs, _) = split_attributes(rest);
            for (k, v) in attrs {
                match k.as_str() {
                    "url-tvg" | "x-tvg-url" | "tvg-url" => {
                        out.epg_urls.extend(v.split(',').map(|u| u.trim().to_owned()).filter(|u| !u.is_empty()))
                    }
                    "catchup" | "catchup-type" => defaults.catchup = non_empty(v),
                    "catchup-source" => defaults.catchup_source = non_empty(v),
                    "catchup-days" | "tvg-rec" | "timeshift" => defaults.catchup_days = v.parse().ok(),
                    _ => {}
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
                    "catchup" | "catchup-type" => e.catchup = non_empty(v),
                    "catchup-source" => e.catchup_source = non_empty(v),
                    "user-agent" | "http-user-agent" => e.user_agent = non_empty(v),
                    "referrer" | "referer" | "http-referrer" | "http-referer" => e.referrer = non_empty(v),
                    _ => {}
                }
            }
            e.catchup = e.catchup.or_else(|| defaults.catchup.clone());
            e.catchup_source = e.catchup_source.or_else(|| defaults.catchup_source.clone());
            e.catchup_days = e.catchup_days.or(defaults.catchup_days);
            if e.name.is_empty() {
                e.name = e.tvg_name.clone().unwrap_or_default();
            }
            pending = Some(e);
        } else if let Some(g) = line.strip_prefix("#EXTGRP:") {
            group_hint = non_empty(g.trim().to_owned());
        } else if let Some(opt) = line.strip_prefix("#EXTVLCOPT:") {
            if let Some(ua) = opt.strip_prefix("http-user-agent=") {
                ua_hint = non_empty(ua.trim().to_owned());
            } else if let Some(r) = opt.strip_prefix("http-referrer=").or_else(|| opt.strip_prefix("http-referer=")) {
                referrer_hint = non_empty(r.trim().to_owned());
            }
        } else if line.starts_with('#') {
            continue;
        } else if let Some(mut e) = pending.take() {
            // Kodi convention: "url|User-Agent=…&Referer=…" (most specific, wins)
            let (url, headers) = line.split_once('|').unwrap_or((line, ""));
            e.url = url.trim().to_owned();
            for (k, v) in url::form_urlencoded::parse(headers.as_bytes()) {
                match k.to_ascii_lowercase().as_str() {
                    "user-agent" => e.user_agent = non_empty(v.trim().to_owned()),
                    "referer" | "referrer" => e.referrer = non_empty(v.trim().to_owned()),
                    _ => {}
                }
            }
            if e.group.is_none() {
                e.group = group_hint.take();
            }
            if e.user_agent.is_none() {
                e.user_agent = ua_hint.take();
            }
            if e.referrer.is_none() {
                e.referrer = referrer_hint.take();
            }
            group_hint = None;
            ua_hint = None;
            referrer_hint = None;
            out.entries.push(e);
        }
    }
    out
}

// ------------------------------------------------------------- catch-up

/// Catch-up stream URL for a programme starting at `start` (unix seconds)
/// and lasting `duration` seconds, per the playlist's scheme:
///
/// - `default`: `source` is the full URL template
/// - `append`: `source` is appended to the stream URL
/// - `shift`: `?utc=<start>&lutc=<now>` appended (Stalker/Ministra style)
/// - `flussonic`: `…/index.m3u8` → `…/index-<start>-<duration>.m3u8`,
///   `…/mpegts` → `…/timeshift_abs-<start>.ts`
/// - `xc`: Xtream Codes live URL → its `/timeshift/…` URL
///
/// Placeholders: `{utc}`/`${start}`, `{utcend}`/`${end}`, `{lutc}`/`${now}`/
/// `${timestamp}`, `{duration}` (s; `{duration:60}` = minutes), `{offset}`
/// (now − start, same divisor syntax), `{utc:Y-m-d H:M:S}` (formatted, UTC)
/// and `{Y}` `{m}` `{d}` `{H}` `{M}` `{S}` (start in *local* time — like Kodi;
/// Xtream servers expect their own time zone, usually the viewer's). `shift`
/// (seconds, the source's catch-up time correction) moves the local-time
/// values only; `{utc}` & co. are absolute and stay exact.
pub fn catchup_url(
    mode: &str,
    source: Option<&str>,
    stream_url: &str,
    start: i64,
    duration: i64,
    now: i64,
    shift: i64,
) -> Option<String> {
    let fill = |template: &str| fill_placeholders(template, start, duration, now, shift);
    match mode.to_ascii_lowercase().as_str() {
        "default" => source.filter(|s| s.contains("://")).map(fill),
        "append" => source.filter(|s| !s.is_empty()).map(|s| format!("{stream_url}{}", fill(s))),
        "shift" | "timeshift" => {
            let sep = if stream_url.contains('?') { '&' } else { '?' };
            Some(format!("{stream_url}{sep}utc={start}&lutc={now}"))
        }
        "flussonic" | "flussonic-hls" | "flussonic-ts" | "fs" => flussonic_url(stream_url, start, duration),
        "xc" => xtream_url(stream_url, start + shift, duration),
        _ => None,
    }
}

fn fill_placeholders(template: &str, start: i64, duration: i64, now: i64, shift: i64) -> String {
    static PLACEHOLDER: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"\$?\{([A-Za-z-]+)(?::([^}]*))?\}").unwrap());
    PLACEHOLDER
        .replace_all(template, |c: &Captures| {
            let arg = c.get(2).map(|m| m.as_str());
            let divided = |v: i64| arg.and_then(|a| a.parse::<i64>().ok()).filter(|d| *d > 0).map_or(v, |d| v / d);
            let stamp = |ts: i64| arg.map_or_else(|| ts.to_string(), |fmt| format_time(ts, fmt, false));
            match &c[1] {
                "utc" | "start" => stamp(start),
                "utcend" | "end" => stamp(start + duration),
                "lutc" | "now" | "timestamp" => now.to_string(),
                "duration" => divided(duration).to_string(),
                "offset" => divided(now - start).to_string(),
                one @ ("Y" | "m" | "d" | "H" | "M" | "S") => format_time(start + shift, one, true),
                _ => c[0].to_owned(),
            }
        })
        .into_owned()
}

/// `fmt` letters Y m d H M S → zero-padded date parts; anything else is literal.
fn format_time(ts: i64, fmt: &str, local: bool) -> String {
    let utc = chrono::DateTime::from_timestamp(ts, 0).unwrap_or_default();
    let t = if local { utc.with_timezone(&chrono::Local).naive_local() } else { utc.naive_utc() };
    fmt.chars()
        .map(|c| match c {
            'Y' => t.format("%Y").to_string(),
            'm' => t.format("%m").to_string(),
            'd' => t.format("%d").to_string(),
            'H' => t.format("%H").to_string(),
            'M' => t.format("%M").to_string(),
            'S' => t.format("%S").to_string(),
            other => other.to_string(),
        })
        .collect()
}

fn flussonic_url(url: &str, start: i64, duration: i64) -> Option<String> {
    let (path, query) = url.split_once('?').map_or((url, None), |(p, q)| (p, Some(q)));
    let query = query.map(|q| format!("?{q}")).unwrap_or_default();
    if let Some(base) = path.strip_suffix(".m3u8") {
        return Some(format!("{base}-{start}-{duration}.m3u8{query}"));
    }
    path.strip_suffix("/mpegts").map(|base| format!("{base}/timeshift_abs-{start}.ts{query}"))
}

fn xtream_url(url: &str, start: i64, duration: i64) -> Option<String> {
    static LIVE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^(https?://[^/?#]+)/(?:live/)?([^/?#]+)/([^/?#]+)/(\d+)(?:\.[A-Za-z0-9]+)?$").unwrap());
    let c = LIVE.captures(url)?;
    let minutes = ((duration + 59) / 60).max(1);
    Some(format!("{}/timeshift/{}/{}/{minutes}/{}/{}.ts", &c[1], &c[2], &c[3], format_time(start, "Y-m-d:H-M", true), &c[4]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_episodes() {
        let e = |name: &str, url: &str| Entry { name: name.into(), url: url.into(), ..Default::default() }.episode_info();
        let info = |series: &str, season, episode, title: &str| {
            Some(EpisodeInfo { series: series.into(), season, episode, title: title.into() })
        };
        assert_eq!(e("Slow Horses S02E03 - Hard Lessons", "http://h/series/u/p/1.mkv"), info("Slow Horses", 2, 3, "Hard Lessons"));
        assert_eq!(e("EN - Silo (2023) S01 E10", "http://h/series/u/p/2.mp4"), info("EN - Silo (2023)", 1, 10, ""));
        assert_eq!(e("The.Office.US.s09e23.mkv", "http://h/vod/office.mkv"), info("The.Office.US", 9, 23, ""));
        assert_eq!(e("Friends 1x02 The One with the Sonogram", "http://h/f.mp4"), info("Friends", 1, 2, "The One with the Sonogram"));
        // not episodes: live channels, plain movies, numbers that aren't SxxEyy
        assert_eq!(e("Sports S01E01 Live", "http://h/live/sports.ts"), None);
        assert_eq!(e("Se7en (1995)", "http://h/movie/u/p/3.mkv"), None);
        assert_eq!(e("1917", "http://h/movie/u/p/4.mkv"), None);
        assert_eq!(e("S01E01", "http://h/series/u/p/5.mkv"), None); // no series name
    }

    #[test]
    fn stream_headers() {
        let pl = parse(concat!(
            "#EXTM3U\n",
            "#EXTINF:-1 http-referrer=\"https://site.example/\",Attributes\n",
            "#EXTVLCOPT:http-user-agent=VLC/3.0\n",
            "http://h/a.m3u8\n",
            "#EXTINF:-1,VLC options\n",
            "#EXTVLCOPT:http-referrer=https://ref.example/\n",
            "http://h/b.ts\n",
            "#EXTINF:-1 user-agent=\"Old\",Kodi pipe\n",
            "http://h/c.m3u8?token=1|User-Agent=Mozilla%2F5.0%20(X11)&Referer=https%3A%2F%2Fk.example%2F\n",
            "#EXTINF:-1,Plain\n",
            "http://h/d.ts\n",
        ));
        let h = |i: usize| (pl.entries[i].url.as_str(), pl.entries[i].user_agent.as_deref(), pl.entries[i].referrer.as_deref());
        assert_eq!(h(0), ("http://h/a.m3u8", Some("VLC/3.0"), Some("https://site.example/")));
        assert_eq!(h(1), ("http://h/b.ts", None, Some("https://ref.example/")));
        assert_eq!(h(2), ("http://h/c.m3u8?token=1", Some("Mozilla/5.0 (X11)"), Some("https://k.example/")));
        assert_eq!(h(3), ("http://h/d.ts", None, None));
    }

    #[test]
    fn catchup_attributes_with_playlist_defaults() {
        let pl = parse(concat!(
            "#EXTM3U catchup=\"shift\" catchup-days=\"3\"\n",
            "#EXTINF:-1 tvg-id=\"a\",Uses the defaults\n",
            "http://h/a.m3u8\n",
            "#EXTINF:-1 catchup=\"default\" catchup-source=\"http://h/arch/a?s={utc}&d={duration:60}\" catchup-days=\"7\",Own scheme\n",
            "http://h/a2.m3u8\n",
        ));
        let (a, b) = (&pl.entries[0], &pl.entries[1]);
        assert_eq!((a.catchup.as_deref(), a.catchup_days, a.catchup_window()), (Some("shift"), Some(3), 3));
        assert_eq!((b.catchup.as_deref(), b.catchup_window()), (Some("default"), 7));
        // a scheme without a usable template offers nothing; a scheme without days gets the default window
        let none = Entry { catchup: Some("default".into()), url: "http://h/x.ts".into(), ..Default::default() };
        assert_eq!(none.catchup_window(), 0);
        let open = Entry { catchup: Some("shift".into()), url: "http://h/x.ts".into(), ..Default::default() };
        assert_eq!(open.catchup_window(), DEFAULT_CATCHUP_DAYS);
    }

    #[test]
    fn builds_catchup_urls() {
        let (start, dur, now) = (1_790_413_200, 5400, 1_790_420_000); // 2026-09-26 09:00:00 UTC, 90 min
        let url = |mode: &str, src: Option<&str>, stream: &str| catchup_url(mode, src, stream, start, dur, now, 0);
        assert_eq!(
            url("default", Some("http://h/arch/7?start=${start}&end={utcend}&min={duration:60}&t={utc:Y-m-d H:M}"), "http://h/7.ts").unwrap(),
            "http://h/arch/7?start=1790413200&end=1790418600&min=90&t=2026-09-26 09:00"
        );
        assert_eq!(url("append", Some("?utc={utc}&lutc={lutc}"), "http://h/7.m3u8").unwrap(), "http://h/7.m3u8?utc=1790413200&lutc=1790420000");
        assert_eq!(url("shift", None, "http://h/7.ts?token=x").unwrap(), "http://h/7.ts?token=x&utc=1790413200&lutc=1790420000");
        assert_eq!(url("flussonic", None, "http://f/ch/index.m3u8?t=1").unwrap(), "http://f/ch/index-1790413200-5400.m3u8?t=1");
        assert_eq!(url("fs", None, "http://f/ch/mpegts").unwrap(), "http://f/ch/timeshift_abs-1790413200.ts");
        let local = format_time(start, "Y-m-d:H-M", true);
        assert_eq!(url("xc", None, "http://x.tv:80/live/u/p/42.ts").unwrap(), format!("http://x.tv:80/timeshift/u/p/90/{local}/42.ts"));
        assert_eq!(url("xc", None, "http://x.tv/u/p/42").unwrap(), format!("http://x.tv/timeshift/u/p/90/{local}/42.ts"));
        // not buildable
        assert!(url("default", Some("?relative"), "http://h/7.ts").is_none());
        assert!(url("flussonic", None, "http://f/ch/stream.ts").is_none());
        assert!(url("xc", None, "http://h/playlist/7.ts").is_none());
        assert!(url("vod", None, "http://h/7.ts").is_none());

        // a -1 h correction moves local-time values, never the absolute ones
        let shifted = catchup_url("default", Some("http://h/{H}-{M}?s={utc}"), "http://h/7.ts", start, dur, now, -3600).unwrap();
        assert_eq!(shifted, format!("http://h/{}?s=1790413200", format_time(start - 3600, "H-M", true)));
        let xc = catchup_url("xc", None, "http://x.tv/live/u/p/42.ts", start, dur, now, -3600).unwrap();
        assert!(xc.ends_with(&format!("/{}/42.ts", format_time(start - 3600, "Y-m-d:H-M", true))));
    }

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
