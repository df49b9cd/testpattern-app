//! Turns noisy provider names into display titles + quality badges.
//!
//!   "UK: BBC ONE LONDON 4K ◉"          → "BBC ONE LONDON"        [4K]
//!   "AT&T: BBC NEWS ᴿᴬᵂ"                → "BBC NEWS"              [RAW]
//!   "UK| SKY CINEMA ᴴᴰ/ᴿᴬᵂ"             → "SKY CINEMA" (UK)       [HD, RAW]
//!   "SC - Cleanskin (2012)"             → "Cleanskin" (2012, tag SC)
//!   "##### AT&T ᴿᴬᵂ ⁶⁰ᶠᵖˢ #####"        → separator

use std::sync::LazyLock;

use regex::Regex;

#[derive(Debug, Default, PartialEq)]
pub struct Cleaned {
    pub title: String,
    pub badges: Vec<&'static str>,
    pub region: Option<String>,
}

#[derive(Debug, Default, PartialEq)]
pub struct CleanedTitle {
    pub title: String,
    pub tag: Option<String>,
    pub year: Option<i32>,
}

/// Maps Unicode superscript/modifier letters to ASCII.
fn unsuper(c: char) -> Option<char> {
    Some(match c {
        '⁰' => '0', '¹' => '1', '²' => '2', '³' => '3', '⁴' => '4',
        '⁵' => '5', '⁶' => '6', '⁷' => '7', '⁸' => '8', '⁹' => '9',
        'ᴬ' => 'A', 'ᴮ' => 'B', 'ᴰ' => 'D', 'ᴱ' => 'E', 'ᴳ' => 'G', 'ᴴ' => 'H',
        'ᴵ' => 'I', 'ᴶ' => 'J', 'ᴷ' => 'K', 'ᴸ' => 'L', 'ᴹ' => 'M', 'ᴺ' => 'N',
        'ᴼ' => 'O', 'ᴾ' => 'P', 'ᴿ' => 'R', 'ᵀ' => 'T', 'ᵁ' => 'U', 'ⱽ' => 'V',
        'ᵂ' => 'W',
        'ᵃ' => 'a', 'ᵇ' => 'b', 'ᶜ' => 'c', 'ᵈ' => 'd', 'ᵉ' => 'e', 'ᶠ' => 'f',
        'ᵍ' => 'g', 'ʰ' => 'h', 'ᶦ' => 'i', 'ⁱ' => 'i', 'ʲ' => 'j', 'ᵏ' => 'k',
        'ˡ' => 'l', 'ᵐ' => 'm', 'ⁿ' => 'n', 'ᵒ' => 'o', 'ᵖ' => 'p', 'ʳ' => 'r',
        'ˢ' => 's', 'ᵗ' => 't', 'ᵘ' => 'u', 'ᵛ' => 'v', 'ʷ' => 'w', 'ˣ' => 'x',
        'ʸ' => 'y', 'ᶻ' => 'z',
        _ => return None,
    })
}

const BADGES: &[(&str, &str)] = &[
    ("8K", "8K"),
    ("4K", "4K"),
    ("UHD", "4K"),
    ("2160P", "4K"),
    ("3840P", "4K"),
    ("FHD", "FHD"),
    ("1080P", "FHD"),
    ("HD", "HD"),
    ("720P", "HD"),
    ("SD", "SD"),
    ("HEVC", "HEVC"),
    ("H265", "HEVC"),
    ("H.265", "HEVC"),
    ("RAW", "RAW"),
    ("60FPS", "60FPS"),
    ("50FPS", "50FPS"),
    ("VIP", "VIP"),
    ("DOLBYVISION", "DOLBY VISION"),
    ("DOLBYAUDIO", "DOLBY AUDIO"),
];

fn badge_for(token: &str) -> Option<&'static str> {
    let t: String = token.chars().filter(|c| c.is_alphanumeric() || *c == '.').collect::<String>().to_uppercase();
    BADGES.iter().find(|(k, _)| *k == t).map(|(_, v)| *v)
}

fn push_badge(badges: &mut Vec<&'static str>, b: &'static str) {
    if !badges.contains(&b) {
        badges.push(b);
    }
}

/// Extracts badges from runs of superscript characters (e.g. "ᴴᴰ/ᴿᴬᵂ ⁶⁰ᶠᵖˢ")
/// and returns the name with those runs removed.
fn take_superscripts(name: &str, badges: &mut Vec<&'static str>) -> String {
    let mut out = String::with_capacity(name.len());
    let mut run = String::new();
    // Returns the words that are *not* quality badges (e.g. "ᶜᶦᵗʸ" → "CITY"),
    // which belong to the title.
    let flush = |run: &mut String, badges: &mut Vec<&'static str>| -> String {
        // words like "4K 3840P Dolby Vision": prefer two-word badges
        let words: Vec<&str> = run.split(['/', ' ']).filter(|w| !w.is_empty()).collect();
        let mut kept = Vec::new();
        let mut i = 0;
        while i < words.len() {
            if i + 1 < words.len()
                && let Some(b) = badge_for(&format!("{}{}", words[i], words[i + 1])) {
                    push_badge(badges, b);
                    i += 2;
                    continue;
                }
            match badge_for(words[i]) {
                Some(b) => push_badge(badges, b),
                None => kept.push(words[i].to_uppercase()),
            }
            i += 1;
        }
        run.clear();
        kept.join(" ")
    };
    for c in name.chars() {
        if let Some(a) = unsuper(c) {
            run.push(a);
        } else if !run.is_empty() && (c == '/' || c == ' ') {
            run.push(c);
        } else {
            if !run.is_empty() {
                let kept = flush(&mut run, badges);
                out.push(' ');
                out.push_str(&kept);
                out.push(' ');
            }
            out.push(c);
        }
    }
    if !run.is_empty() {
        let kept = flush(&mut run, badges);
        out.push(' ');
        out.push_str(&kept);
    }
    out
}

static DECOR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[◉•●★☆✪⚽♦◆▶►▬■□▪\u{2066}-\u{2069}\u{FE0F}]").unwrap());
static SPACES: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s{2,}").unwrap());
static PREFIX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*([A-Z0-9][A-Z0-9&+/.]{0,11})\s*[:|]\s+").unwrap());
static SEPARATOR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*(?:#\s*#|(?:[=*~_\-•●▬◆★]\s*){3,})").unwrap());
// "SC - ", and compound ones like "4K-AMZ - ", "4K-A+ - ", "EN-TOP - " (the
// first part a quality or a two-letter code, so "X-MEN - …" stays a title)
static TAG_PREFIX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\s*((?:(?:4K|8K|UHD|FHD|HD|SD|[A-Z]{2})-[A-Z0-9+]{1,5})|[A-Z0-9+]{2,4})\s*-\s+").unwrap()
});
static TRAILING_YEAR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\s*[\(\[]((?:19|20)\d{2})[\)\]]\s*$").unwrap());
static MIDDLE_YEAR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(.{2,}?)\s*\(((?:19|20)\d{2})\)\s+\S.*$").unwrap());
static TRAILING_COUNTRY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\s*\(([A-Z]{2})\)\s*$").unwrap());

pub fn is_separator(name: &str) -> bool {
    SEPARATOR.is_match(name)
}

fn tidy(s: &str) -> String {
    let s = DECOR.replace_all(s, " ");
    let s = SPACES.replace_all(&s, " ");
    s.trim().trim_matches(|c: char| c == '|' || c == '-' || c == ':').trim().to_owned()
}

/// Strips trailing plain-text quality tokens ("... 4K", "... HD", "... HEVC").
fn take_trailing_badges(title: &mut String, badges: &mut Vec<&'static str>) {
    loop {
        let trimmed = title.trim_end();
        let Some(idx) = trimmed.rfind(' ') else { break };
        let last = &trimmed[idx + 1..];
        match badge_for(last) {
            Some(b) if idx > 0 => {
                push_badge(badges, b);
                title.truncate(idx);
            }
            _ => break,
        }
    }
    // "HD/RAW"-style combos
    let trimmed = title.trim_end().to_owned();
    if let Some(idx) = trimmed.rfind(' ') {
        let last = &trimmed[idx + 1..];
        if last.contains('/') && last.split('/').all(|p| badge_for(p).is_some()) {
            for p in last.split('/') {
                push_badge(badges, badge_for(p).unwrap());
            }
            *title = trimmed[..idx].to_owned();
        }
    }
}

/// Channel names: strip region/provider prefixes, decorations and badges.
pub fn channel(name: &str) -> Cleaned {
    let mut badges = Vec::new();
    let s = take_superscripts(name, &mut badges);
    let mut s = tidy(&s);
    let mut region = None;
    if let Some(m) = PREFIX.captures(&s) {
        // keep event-style names ("US (ESPN+ 358) | ...") untouched
        let rest = s[m.get(0).unwrap().end()..].trim().to_owned();
        if !rest.is_empty() {
            region = Some(m[1].to_owned());
            s = rest;
        }
    }
    take_trailing_badges(&mut s, &mut badges);
    let title = tidy(&s);
    Cleaned { title: if title.is_empty() { tidy(name) } else { title }, badges, region }
}

/// Category names: like channels, but the prefix becomes the region.
pub fn category(name: &str) -> Cleaned {
    let mut c = channel(name);
    if c.title.is_empty() {
        c.title = tidy(name);
    }
    c
}

/// Movie/series names: "EN - Title (2021) (US)" → title, tag, year.
pub fn title(name: &str) -> CleanedTitle {
    let mut badges = Vec::new();
    let s = take_superscripts(name, &mut badges);
    let mut s = tidy(&s);
    let mut tag = None;
    if let Some(m) = TAG_PREFIX.captures(&s) {
        tag = Some(m[1].to_owned());
        s = s[m.get(0).unwrap().end()..].to_owned();
    }
    let mut year = None;
    // strip trailing "(US)" / "(2021)" in any order
    for _ in 0..3 {
        if let Some(m) = TRAILING_COUNTRY.captures(&s) {
            s.truncate(m.get(0).unwrap().start());
            continue;
        }
        if let Some(m) = TRAILING_YEAR.captures(&s) {
            year = year.or_else(|| m[1].parse().ok());
            s.truncate(m.get(0).unwrap().start());
            continue;
        }
        break;
    }
    // "Possible Love (2026) Muhtemel Ask": the year splits the display title
    // from the original-language title
    if year.is_none()
        && let Some(m) = MIDDLE_YEAR.captures(&s) {
            year = m[2].parse().ok();
            s = m[1].to_owned();
        }
    let title = tidy(&s);
    CleanedTitle { title: if title.is_empty() { tidy(name) } else { title }, tag, year }
}

pub fn badges_str(badges: &[&str]) -> String {
    badges.join(" ")
}

/// Decodes the handful of HTML entities providers leave in names.
pub fn unescape(s: &str) -> String {
    if !s.contains('&') {
        return s.to_owned();
    }
    s.replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channels() {
        let c = channel("UK: BBC ONE LONDON 4K ◉");
        assert_eq!(c.title, "BBC ONE LONDON");
        assert_eq!(c.badges, vec!["4K"]);
        assert_eq!(c.region.as_deref(), Some("UK"));

        let c = channel("AT&T: BBC NEWS ᴿᴬᵂ");
        assert_eq!(c.title, "BBC NEWS");
        assert_eq!(c.badges, vec!["RAW"]);

        let c = channel("SE: CGTN ᴴᴰ");
        assert_eq!((c.title.as_str(), c.badges), ("CGTN", vec!["HD"]));

        let c = channel("US: ABC 33 BIRMINGHAM AL (WABM) HD");
        assert_eq!(c.title, "ABC 33 BIRMINGHAM AL (WABM)");

        let c = channel("24/7: MILLION DOLLAR LISTING NEW YORK");
        assert_eq!(c.title, "MILLION DOLLAR LISTING NEW YORK");

        let c = channel("US (ESPN+ 358) | NCAA Field Hockey: Syracuse vs. Virginia (2026-09-25 16:03:00)");
        assert!(c.title.starts_with("US (ESPN+ 358)"));
    }

    #[test]
    fn categories() {
        let c = category("UK| SKY CINEMA ᴴᴰ/ᴿᴬᵂ");
        assert_eq!(c.title, "SKY CINEMA");
        assert_eq!(c.region.as_deref(), Some("UK"));
        assert_eq!(c.badges, vec!["HD", "RAW"]);

        let c = category("NETFLIX MOVIES ⁴ᴷ ³⁸⁴⁰ᴾ ᴰᵒˡᵇʸ ⱽᶦˢᶦᵒⁿ");
        assert_eq!(c.title, "NETFLIX MOVIES");
        assert!(c.badges.contains(&"4K"));
        assert!(c.badges.contains(&"DOLBY VISION"));

        let c = category("US| AT&T ᴿᴬᵂ ⁶⁰ᶠᵖˢ");
        assert_eq!(c.title, "AT&T");
        assert_eq!(c.badges, vec!["RAW", "60FPS"]);

        // superscript words that are not quality tags stay in the title
        let c = category("US| TV ᶜᶦᵗʸ ᴿᴬᵂ ⁶⁰ᶠᵖˢ");
        assert_eq!(c.title, "TV CITY");
        assert_eq!(c.badges, vec!["RAW", "60FPS"]);
    }

    #[test]
    fn separators() {
        assert!(is_separator("##### AT&T ᴿᴬᵂ ⁶⁰ᶠᵖˢ #####"));
        assert!(is_separator("####### SKY SPORTS SD #######"));
        assert!(is_separator("## ENTERTAINMENT ᴴᴰ/ᴿᴬᵂ ##"));
        assert!(!is_separator("#1 Hits Radio"));
        assert!(!is_separator("UK: BBC ONE"));
    }

    #[test]
    fn titles() {
        let t = title("SC - Cleanskin (2012)");
        assert_eq!(t, CleanedTitle { title: "Cleanskin".into(), tag: Some("SC".into()), year: Some(2012) });
        let t = title("EN - Quiz Lady  (2023)");
        assert_eq!((t.title.as_str(), t.year), ("Quiz Lady", Some(2023)));
        let t = title("NF - SEAL Team (2017) (US)");
        assert_eq!((t.title.as_str(), t.year), ("SEAL Team", Some(2017)));
        let t = title("SC  - Power (2014) (CA)");
        assert_eq!((t.title.as_str(), t.tag.as_deref()), ("Power", Some("SC")));
        let t = title("SE - The Irishman");
        assert_eq!((t.title.as_str(), t.year), ("The Irishman", None));
        let t = title("NF - Top Gun: Maverick (2022)");
        assert_eq!(t.title, "Top Gun: Maverick");
        let t = title("UNV - Blue Crush (2002)");
        assert_eq!((t.title.as_str(), t.tag.as_deref()), ("Blue Crush", Some("UNV")));
        let t = title("EN - Possible Love (2026) Muhtemel Ask");
        assert_eq!((t.title.as_str(), t.year), ("Possible Love", Some(2026)));
        // a bare trailing number is part of the title, not a year
        let t = title("Blade Runner 2049");
        assert_eq!((t.title.as_str(), t.year), ("Blade Runner 2049", None));
        // compound provider prefixes
        let t = title("4K-AMZ - Hunting With Tigers (2025)");
        assert_eq!(t, CleanedTitle { title: "Hunting With Tigers".into(), tag: Some("4K-AMZ".into()), year: Some(2025) });
        let t = title("4K-A+ - Silo");
        assert_eq!((t.title.as_str(), t.tag.as_deref()), ("Silo", Some("4K-A+")));
        let t = title("EN-TOP - 01. The Shawshank Redemption");
        assert_eq!((t.title.as_str(), t.tag.as_deref()), ("01. The Shawshank Redemption", Some("EN-TOP")));
        let t = title("X-MEN - Apocalypse");
        assert_eq!((t.title.as_str(), t.tag), ("X-MEN - Apocalypse", None));
    }
}
