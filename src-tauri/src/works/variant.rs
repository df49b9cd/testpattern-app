//! What a provider copy ("version") of a movie or series is, decoded from its
//! title prefix tag (`NF`, `4K-A+`, `SC-DO`, `EN-CAM`, …) and the category it
//! is listed in ("APPLE+ SERIES ⁴ᴷ ³⁸⁴⁰ᴾ ᴰᵒˡᵇʸ ⱽᶦˢᶦᵒⁿ", "NORDIC HBO MAX",
//! "TOP MOVIES BLURAY (MULTI-SUBS)", …).
//!
//! The tag names the *market* a copy was prepared for (its subtitles), not
//! necessarily its audio: "SC - For All Mankind" is the English original with
//! Nordic subtitles. Audio and subtitle languages proper are learned from the
//! file itself (`media_info`, see `works::tracks`).

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Origin {
    /// streaming-service release (WEB-DL/WEBRip)
    Web,
    Bluray,
    /// broadcaster's play service (SVT Play, TV2 Play, NRK, …)
    Tv,
    /// recorded in a cinema — lowest quality, never preferred
    Cam,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Variant {
    pub service: Option<&'static str>,
    pub origin: Origin,
    /// Market / subtitle language ("English", "Nordic", "Swedish", …).
    pub language: Option<&'static str>,
    /// Extra subtitle info from the category ("Multi-subtitles", "English subtitles").
    pub subtitles: Option<&'static str>,
    /// "4K", "Dolby Vision", "Dolby Audio", "HEVC" — best first.
    pub quality: Vec<&'static str>,
    /// Human label, e.g. "Apple TV+ · 4K Dolby Vision", "Nordic", "Blu-ray".
    pub label: String,
}

/// Service tags and category words → service name.
const SERVICE_TAGS: &[(&str, &str)] = &[
    ("NF", "Netflix"),
    ("AMZ", "Prime Video"),
    ("A+", "Apple TV+"),
    ("D+", "Disney+"),
    ("DSC+", "Discovery+"),
    ("VP", "Viaplay"),
    ("P+", "Paramount+"),
    ("PCOK", "Peacock"),
    ("SHWT", "Showtime"),
    ("CR", "Crunchyroll"),
    ("SKY", "Sky"),
    ("NICK", "Nickelodeon"),
    ("MRVL", "Marvel"),
    ("PRMT", "Paramount Pictures"),
    ("UNV", "Universal"),
    ("DWA", "DreamWorks"),
];

/// Checked in order against the category name (upper case).
const SERVICE_WORDS: &[(&str, &str)] = &[
    ("NETFLIX", "Netflix"),
    ("AMAZON", "Prime Video"),
    ("PRIME VIDEO", "Prime Video"),
    ("APPLE TV+", "Apple TV+"),
    ("APPLE+", "Apple TV+"),
    ("DISNEY+", "Disney+"),
    ("DISCOVERY+", "Discovery+"),
    ("VIAPLAY", "Viaplay"),
    ("PARAMOUNT+", "Paramount+"),
    ("PARAMOUNT PICTURES", "Paramount Pictures"),
    ("PEACOCK", "Peacock"),
    ("SKY SHOWTIME", "SkyShowtime"),
    ("SHOWTIME", "Showtime"),
    ("HBO MAX", "HBO Max"),
    ("CRUNCHYROLL", "Crunchyroll"),
    ("NICKELODEON", "Nickelodeon"),
    ("MARVEL", "Marvel"),
    ("UNIVERSAL", "Universal"),
    ("DREAMWORKS", "DreamWorks"),
    ("SVT PLAY", "SVT Play"),
    ("TV4 PLAY", "TV4 Play"),
    ("TV2 PLAY", "TV 2 Play"),
    ("NRK", "NRK"),
];

const TV_SERVICES: &[&str] = &["SVT Play", "TV4 Play", "TV 2 Play", "NRK"];

fn language_of_tag(tag: &str) -> Option<&'static str> {
    Some(match tag {
        "EN" => "English",
        "SC" => "Nordic",
        "SE" => "Swedish",
        "DK" => "Danish",
        "NO" => "Norwegian",
        "AR" => "Arabic",
        _ => return None,
    })
}

fn language_of_category(cat: &str) -> Option<&'static str> {
    let first = cat.split([' ', '-']).find(|w| !w.is_empty()).unwrap_or("");
    Some(match first {
        "EN" | "ENGLISH" => "English",
        "NORDIC" => "Nordic",
        "SVENSKA" | "SVENSK" => "Swedish",
        "DANSKE" | "DANSK" => "Danish",
        "NORGE" | "NORSK" => "Norwegian",
        "TURKSIH" | "TURKISH" => "Turkish",
        _ if cat.contains("ITALIAN") => "Italian",
        _ => return None,
    })
}

/// Normalizes superscript letters so "⁴ᴷ ᴰᵒˡᵇʸ ⱽᶦˢᶦᵒⁿ" reads "4K DOLBY VISION".
fn plain_upper(s: &str) -> String {
    crate::names::fold_superscripts(s).to_uppercase()
}

/// Decodes a copy. `tag` is the cleaned prefix (`names::title().tag`),
/// `category` the raw provider category name.
pub fn parse(tag: Option<&str>, category: Option<&str>) -> Variant {
    let tag = tag.unwrap_or("").trim().to_uppercase();
    let cat = plain_upper(category.unwrap_or(""));

    // "4K-A+", "SC-DO", "EN-CAM", "EN-TOP": base tag + modifiers
    let mut parts: Vec<&str> = tag.split('-').filter(|p| !p.is_empty()).collect();
    let mut quality: Vec<&'static str> = Vec::new();
    let mut origin = Origin::Unknown;
    let mut uhd = false;
    let mut dolby_audio = false;
    let mut cam = false;
    let mut bluray = false;
    parts.retain(|p| match *p {
        "4K" => {
            uhd = true;
            false
        }
        "DO" => {
            dolby_audio = true;
            false
        }
        "CAM" => {
            cam = true;
            false
        }
        "TOP" => {
            bluray = cat.contains("BLURAY");
            // "EN-TOP" is the IMDb top 250 list: a collection, not a release
            false
        }
        _ => true,
    });
    let base = parts.first().copied().unwrap_or("");

    let service = SERVICE_TAGS
        .iter()
        .find(|(t, _)| *t == base)
        .map(|(_, s)| *s)
        // "D+" is both Disney+ and Discovery+: the category decides
        .map(|s| {
            if s == "Disney+" && cat.contains("DISCOVERY") {
                "Discovery+"
            } else {
                s
            }
        })
        .or_else(|| {
            SERVICE_WORDS
                .iter()
                .find(|(w, _)| cat.contains(w))
                .map(|(_, s)| *s)
        });

    if cat.contains("BLURAY") {
        bluray = true;
    }
    if cam {
        origin = Origin::Cam;
    } else if bluray {
        origin = Origin::Bluray;
    } else if let Some(s) = service {
        origin = if TV_SERVICES.contains(&s) {
            Origin::Tv
        } else {
            Origin::Web
        };
    }

    if uhd || cat.contains("4K") || cat.contains("3840P") || cat.contains("2160P") {
        quality.push("4K");
    }
    if cat.contains("DOLBY VISION") {
        quality.push("Dolby Vision");
    }
    if dolby_audio || cat.contains("DOLBY AUDIO") {
        quality.push("Dolby Audio");
    }
    if cat.contains("HEVC") {
        quality.push("HEVC");
    }

    let language = language_of_tag(base).or_else(|| language_of_category(&cat));
    let subtitles = if cat.contains("MULTI-SUBS") {
        Some("Multi-subtitles")
    } else if cat.contains("SUB EN") || cat.contains("SUB ENG") {
        Some("English subtitles")
    } else if cat.contains("(MULTI)") {
        Some("Multi-language")
    } else {
        None
    };

    let mut head: Vec<String> = Vec::new();
    match origin {
        Origin::Bluray => head.push("Blu-ray".into()),
        Origin::Cam => head.push("CAM".into()),
        _ => {}
    }
    if let Some(s) = service {
        head.push(s.into());
    }
    // a service copy's market only matters when it is not the default English one
    if let Some(l) = language
        && (service.is_none() || l != "English")
    {
        head.push(l.into());
    }
    if head.is_empty() {
        head.push(subtitles.unwrap_or("Standard").into());
    } else if let Some(sub) = subtitles {
        head.push(sub.into());
    }
    let mut label = head.join(" · ");
    if !quality.is_empty() {
        label.push_str(" · ");
        label.push_str(&quality.join(" "));
    }

    Variant {
        service,
        origin,
        language,
        subtitles,
        quality,
        label,
    }
}

/// Picture/release quality as a tie-breaker (language fit comes first, see
/// `affinity`). CAM copies are never chosen while anything else exists.
pub fn score(v: &Variant) -> i32 {
    let mut s = 0;
    if v.origin == Origin::Cam {
        s -= 1000;
    }
    if v.quality.contains(&"4K") {
        s += 6;
    }
    if v.quality.contains(&"Dolby Vision") {
        s += 3;
    }
    if v.quality.contains(&"Dolby Audio") {
        s += 2;
    }
    match v.origin {
        Origin::Bluray => s += 5,
        Origin::Web => s += 4,
        Origin::Tv => s += 3,
        _ => {}
    }
    s
}

/// Languages in order of preference, from the player's subtitle and audio
/// language settings ("dan,da" → Danish, Nordic; "eng,en" → English).
pub fn language_prefs(sub_langs: &str, audio_langs: &str) -> Vec<&'static str> {
    let mut out: Vec<&'static str> = Vec::new();
    for code in sub_langs.split(',').chain(audio_langs.split(',')) {
        let names: &[&'static str] = match code.trim().to_lowercase().as_str() {
            "en" | "eng" => &["English"],
            "da" | "dan" => &["Danish", "Nordic"],
            "sv" | "swe" => &["Swedish", "Nordic"],
            "no" | "nor" | "nb" | "nob" | "nn" | "nno" => &["Norwegian", "Nordic"],
            "fi" | "fin" => &["Nordic"],
            "tr" | "tur" => &["Turkish"],
            "it" | "ita" => &["Italian"],
            "ar" | "ara" => &["Arabic"],
            _ => &[],
        };
        for n in names {
            if !out.contains(n) {
                out.push(n);
            }
        }
    }
    out
}

/// How well a copy's market fits the preferred languages: 80 for the first
/// preference, 60, 40, then 20; 0 for none. Steps are larger than any
/// quality difference (`score`), so language fit always decides first.
/// Service copies without a market tag are the international (English)
/// releases.
pub fn affinity(v: &Variant, prefs: &[&str]) -> i32 {
    // service releases and multi-subtitle Blu-rays are the international copies
    let international =
        v.service.is_some() || v.origin == Origin::Bluray || v.subtitles == Some("Multi-subtitles");
    let lang = v
        .language
        .or(if international { Some("English") } else { None });
    match lang.and_then(|l| prefs.iter().position(|p| *p == l)) {
        Some(i) => 20 * (4 - i.min(3) as i32),
        None => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn label(tag: &str, cat: &str) -> String {
        parse(Some(tag), Some(cat)).label
    }

    #[test]
    fn labels_the_for_all_mankind_copies() {
        assert_eq!(label("NF", "NETFLIX  SERIES"), "Netflix");
        assert_eq!(label("A+", "APPLE+ SERIES"), "Apple TV+");
        assert_eq!(
            label("4K-A+", "APPLE+ SERIES ⁴ᴷ ³⁸⁴⁰ᴾ ᴰᵒˡᵇʸ ⱽᶦˢᶦᵒⁿ"),
            "Apple TV+ · 4K Dolby Vision"
        );
        assert_eq!(label("EN", "ENGLISH SERIES"), "English");
        assert_eq!(label("SC", "NORDIC SERIES"), "Nordic");
        assert_eq!(
            label("4K-SC", "NORDIC SERIES ⁴ᴷ ³⁸⁴⁰ᴾ ᴰᴼᴸᴮʸ ᴬᵁᴰᴵᴼ"),
            "Nordic · 4K Dolby Audio"
        );
    }

    #[test]
    fn services_from_nordic_collections() {
        let v = parse(Some("SC"), Some("NORDIC HBO MAX"));
        assert_eq!(
            (v.service, v.language, v.origin),
            (Some("HBO Max"), Some("Nordic"), Origin::Web)
        );
        assert_eq!(v.label, "HBO Max · Nordic");
        assert_eq!(
            parse(Some("SC"), Some("NORDIC SKY SHOWTIME")).service,
            Some("SkyShowtime")
        );
        assert_eq!(
            parse(Some("SE"), Some("SVENSK SVT PLAY")).origin,
            Origin::Tv
        );
    }

    #[test]
    fn origins_and_quality() {
        let v = parse(Some("TOP"), Some("TOP MOVIES BLURAY (MULTI-SUBS)"));
        assert_eq!(
            (v.origin, v.label.as_str()),
            (Origin::Bluray, "Blu-ray · Multi-subtitles")
        );
        let cam = parse(Some("EN-CAM"), Some("EN - NEW RELEASE"));
        assert_eq!(
            (cam.origin, cam.label.as_str()),
            (Origin::Cam, "CAM · English")
        );
        assert!(score(&cam) < score(&parse(Some("EN"), Some("EN - NEW RELEASE"))));
        assert_eq!(
            label("SC-DO", "NORDIC FILM ᴰᴼᴸᴮʸ ᴬᵁᴰᴵᴼ"),
            "Nordic · Dolby Audio"
        );
        assert_eq!(label("NF", "NETFLIX HEVC"), "Netflix · HEVC");
        assert_eq!(
            label("EN", "TURKSIH SERIES (SUB EN)"),
            "English · English subtitles"
        );
        assert_eq!(
            label("", "TURKSIH SERIES (SUB EN)"),
            "Turkish · English subtitles"
        );
        // the IMDb top-250 list is not a Blu-ray release
        assert_eq!(
            parse(Some("EN-TOP"), Some("EN - IMDB TOP 250")).origin,
            Origin::Unknown
        );
    }

    #[test]
    fn disney_or_discovery() {
        assert_eq!(
            parse(Some("D+"), Some("DISCOVERY+ MOVIES")).service,
            Some("Discovery+")
        );
        assert_eq!(
            parse(Some("4K-D+"), Some("DISNEY+ MOVIES ⁴ᴷ ³⁸⁴⁰ᴾ")).service,
            Some("Disney+")
        );
    }

    #[test]
    fn the_best_copy_wins_by_default() {
        let a = parse(Some("A+"), Some("APPLE+ SERIES"));
        let b = parse(Some("4K-A+"), Some("APPLE+ SERIES ⁴ᴷ ³⁸⁴⁰ᴾ ᴰᵒˡᵇʸ ⱽᶦˢᶦᵒⁿ"));
        let c = parse(Some("EN"), Some("ENGLISH SERIES"));
        assert!(score(&b) > score(&a) && score(&a) > score(&c));
    }

    #[test]
    fn language_fit_comes_from_the_player_settings() {
        let danish = language_prefs("dan,da", "eng,en");
        assert_eq!(danish, vec!["Danish", "Nordic", "English"]);
        let dk = parse(Some("DK"), Some("DANSK SERIE"));
        let sc = parse(Some("SC"), Some("NORDIC SERIES"));
        let nf = parse(Some("NF"), Some("NETFLIX  SERIES"));
        assert!(affinity(&dk, &danish) > affinity(&sc, &danish));
        assert!(affinity(&sc, &danish) > affinity(&nf, &danish));
        let english = language_prefs("eng,en", "eng,en");
        assert!(affinity(&nf, &english) > affinity(&sc, &english));
        assert_eq!(affinity(&sc, &english), 0);
    }
}
