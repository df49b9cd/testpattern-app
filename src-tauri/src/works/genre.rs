//! Genres, countries and decades for browsing.
//!
//! Series carry TMDB genre text, sometimes translated by the provider
//! ("Kriminal / Drama", "Komedi", "Dokumentär", "Virkelighed", "Suç"); movies
//! only have genre-named categories ("EN - DRAMA", "NETFLIX DOCU-MOVIES",
//! "SVENSKA BARN"). Live channels get one genre from their category name
//! ("SPORT", "NEWS", "24/7 COMEDY", "UFC PPV") or, in mixed categories
//! ("AT&T", "TUBI", national packages), from their own name.

/// Display order of genres in the browse panel.
pub const GENRES: &[&str] = &[
    "Action",
    "Adventure",
    "Animation",
    "Anime",
    "Comedy",
    "Stand-up",
    "Crime",
    "Documentary",
    "History",
    "Drama",
    "Family",
    "Kids",
    "Fantasy",
    "Sci-Fi",
    "Horror",
    "Mystery",
    "Thriller",
    "Romance",
    "Reality",
    "Music",
    "Musical",
    "Sport",
    "War & Politics",
    "Western",
    "Christmas",
    "Faith",
    "Soap",
    "Talk",
    "News",
    "Fitness",
    "Audiobooks",
];

/// One genre word (any case, several languages) → canonical genre.
fn genre_word(w: &str) -> Option<&'static str> {
    let w = w.trim().trim_matches(|c: char| !c.is_alphanumeric() && c != '-').to_lowercase();
    Some(match w.as_str() {
        "drama" | "dram" => "Drama",
        "comedy" | "komedi" | "komedie" | "comedia" | "sketch" => "Comedy",
        "crime" | "kriminal" | "kriminalitet" | "suç" | "krimi" => "Crime",
        "documentary" | "documentry" | "dokumentär" | "dokumentar" | "dokumentärer" => "Documentary",
        "action" => "Action",
        "adventure" | "eventyr" | "äventyr" => "Adventure",
        "sci-fi" | "science fiction" | "science-fiction" | "bilim kurgu" | "scifi" => "Sci-Fi",
        "fantasy" | "fantazi" | "fantasi" => "Fantasy",
        "reality" | "reailty" | "virkelighed" | "livsstil" | "reality-tv" => "Reality",
        "mystery" | "mistery" | "mystik" | "mysterium" | "mysterie" => "Mystery",
        "animation" | "animerat" | "animeret" | "animasjon" | "animated" => "Animation",
        "anime" => "Anime",
        "family" | "familj" | "familie" | "aile" => "Family",
        "kids" | "børn" | "barn" | "children" => "Kids",
        "war" | "krig" | "politics" | "politik" | "war & politics" => "War & Politics",
        "soap" | "sæbe" | "såpa" | "såpopera" => "Soap",
        "western" | "västern" | "westerns" => "Western",
        "talk" | "podcast" | "talkshow" => "Talk",
        "news" | "nyheder" | "nyheter" => "News",
        "romance" | "romantic" | "romanc" | "romantik" | "relation" | "romantisk" => "Romance",
        "sport" | "sports" => "Sport",
        "horror" | "skräck" | "gyser" | "skrekk" => "Horror",
        "thriller" => "Thriller",
        "music" | "musik" | "musikk" => "Music",
        "musical" => "Musical",
        "history" | "historia" | "historie" | "historisk" => "History",
        _ => return None,
    })
}

fn push(out: &mut Vec<&'static str>, g: &'static str) {
    if !out.contains(&g) {
        out.push(g);
    }
}

/// TMDB-style genre text in any of the provider's languages.
pub fn from_text(text: &str) -> Vec<&'static str> {
    let mut out = Vec::new();
    // "Action & Adventure", "Sci-Fi & Fantasy", "Action og eventyr"
    let normalized = text.replace(" og ", "/").replace(" och ", "/").replace(" and ", "/").replace('&', "/");
    for part in normalized.split(['/', ',', '|', ';']) {
        for piece in part.split(" - ") {
            if let Some(g) = genre_word(piece) {
                push(&mut out, g);
            } else if piece.split_whitespace().count() > 1 {
                // "Livsstil  Reality  Livsstil": words separated by spaces
                for w in piece.split_whitespace() {
                    if let Some(g) = genre_word(w) {
                        push(&mut out, g);
                    }
                }
            }
        }
    }
    out
}

/// Genres implied by a movie/series category name.
pub fn from_category(name: &str) -> Vec<&'static str> {
    let n = crate::names::fold_superscripts(name).to_uppercase();
    let has = |w: &str| n.contains(w);
    let mut out = Vec::new();
    let rules: &[(&[&str], &'static str)] = &[
        (&["DRAMA"], "Drama"),
        (&["HORROR"], "Horror"),
        (&["THRILLER"], "Thriller"),
        (&["ROMANCE"], "Romance"),
        (&["ACTION"], "Action"),
        (&["ADVENTURE"], "Adventure"),
        (&["SCIENCE FICTION", "SCI-FI"], "Sci-Fi"),
        (&["WESTERN"], "Western"),
        (&["CHRISTMAS"], "Christmas"),
        (&["BIBLICAL", "CHRISTIAN"], "Faith"),
        (&["WORKOUT", "FITNESS"], "Fitness"),
        (&["AUDIOBOOK"], "Audiobooks"),
        (&["PODCAST"], "Talk"),
        (&["REALITY"], "Reality"),
        (&["DOCU"], "Documentary"),
        (&["ANIME", "ANIMI", "MANGA"], "Anime"),
        (&["ANIMATION", "ADULT-SWIM"], "Animation"),
        (&["KIDS", "BARN", "BØRN", "CHILDREN"], "Kids"),
        (&["FAMILY"], "Family"),
        (&["WWE", "UFC", "BOXING", "SPORT"], "Sport"),
    ];
    for (words, genre) in rules {
        if words.iter().any(|w| has(w)) {
            push(&mut out, genre);
        }
    }
    if has("STAND-UP") || has("STAND UP") {
        push(&mut out, "Stand-up");
        push(&mut out, "Comedy");
    } else if has("COMEDY") {
        push(&mut out, "Comedy");
    }
    if has("MUSICAL") {
        push(&mut out, "Musical");
    } else if has("MUSIC") || has("CONCERT") {
        push(&mut out, "Music");
    }
    out
}

/// Genres of live channels, in display order: the regular channels first,
/// the (mostly idle) event feeds last.
pub const LIVE_GENRES: &[&str] = &[
    "Entertainment",
    "News",
    "Sports",
    "Movies",
    "Kids",
    "Documentary",
    "Music",
    "24/7",
    "Events & PPV",
];

/// Country codes whose channels speak `language` (a `variant` language
/// name such as "Danish" or "Nordic"), for ordering the Live TV countries.
pub fn countries_for_language(language: &str) -> &'static [&'static str] {
    match language {
        "Danish" => &["DK"],
        "Swedish" => &["SE"],
        "Norwegian" => &["NO"],
        "Finnish" => &["FI"],
        "Icelandic" => &["IS"],
        "Nordic" => &["DK", "SE", "NO", "FI", "IS"],
        "English" => &["UK", "US", "IE", "CA", "AU", "NZ"],
        "German" => &["DE", "AT", "CH"],
        "Dutch" => &["NL", "BE"],
        "French" => &["FR", "BE", "CA"],
        "Spanish" => &["ES", "MX", "AR"],
        "Portuguese" => &["PT", "BR"],
        "Italian" => &["IT"],
        "Polish" => &["PL"],
        "Turkish" => &["TR"],
        "Arabic" => &["AR", "SA", "AE", "EG"],
        _ => &[],
    }
}

fn words_match(hay: &str, words: &[&str]) -> bool {
    // match whole words/phrases: "NEWS" in "SKY NEWS" but not in "NEWSROOM"-less
    let padded = format!(" {} ", hay.replace(['|', ':', '-', '/', '(', ')', '.', '!', '+'], " "));
    words.iter().any(|w| padded.contains(&format!(" {w} ")))
}

/// Genre of a live category from its cleaned title (e.g. "SPORT",
/// "24/7 COMEDY", "NOW TV SPORT", "UFC PPV"); `None` for mixed categories.
pub fn live_from_category(title: &str) -> Option<&'static str> {
    let t = crate::names::fold_superscripts(title).to_uppercase();
    let has = |w: &str| t.contains(w);
    if has("PPV") || has("EVENT") || has("REPLAY") {
        return Some("Events & PPV");
    }
    if has("NEWS") || has("NYHETER") {
        return Some("News");
    }
    if has("KIDS") || has("CARTOON") || has("BARN") {
        return Some("Kids");
    }
    if has("SPORT") || has("SOCCER") || has("FOOTBALL") || has("DAZN") || has("ESPN") {
        return Some("Sports");
    }
    // looping channels first: "CINEMA TV SHOWS", "24/7 MOVIES/ACTORS"
    if has("24/7") || has("SERIES") || has("SHOWS") || has("ORIGINAL") || has("ON AIR") {
        return Some("24/7");
    }
    if has("MOVIE") || has("CINEMA") || has("CINE ") || has("FILM") || has("SKY STORE") {
        return Some("Movies");
    }
    if has("DOCUMENTARY") || has("DOCS") || has("DISCOVERY") {
        return Some("Documentary");
    }
    if has("MUSIC") || has("MC VIDEO") {
        return Some("Music");
    }
    if has("REALITY") || has("ENTERTAINMENT") {
        return Some("Entertainment");
    }
    None
}

/// Genre of a channel in a mixed category, from its own (cleaned) name.
pub fn live_from_title(title: &str) -> &'static str {
    let t = title.to_uppercase();
    let rules: &[(&[&str], &'static str)] = &[
        (
            &[
                "NEWS", "NYHETER", "NYHEDER", "CNN", "MSNBC", "CNBC", "BLOOMBERG", "NEWSMAX", "EURONEWS", "AL JAZEERA",
                "ALJAZEERA", "CGTN", "FRANCE 24", "DW", "WEATHER", "C SPAN", "CSPAN", "CBSN", "NEWSNATION", "LIVENOW",
            ],
            "News",
        ),
        (
            &[
                "SPORT", "SPORTS", "ESPN", "ESPN2", "EUROSPORT", "NFL", "NBA", "MLB", "NHL", "GOLF", "TENNIS", "BEIN",
                "DAZN", "RACING", "CRICKET", "F1", "WWE", "UFC", "BOXING", "FOOTBALL", "SOCCER", "HOCKEY", "SPORTSNET",
                "TSN", "MOTORSPORT", "FIGHT", "OLYMPICS",
            ],
            "Sports",
        ),
        (
            &[
                "KIDS", "JUNIOR", "NICK", "NICKELODEON", "NICKTOONS", "CARTOON", "BOOMERANG", "BABY", "DISNEY CHANNEL",
                "DISNEY XD", "TOONS", "CBEEBIES", "CBBC", "POKEMON", "BARNE", "BARN", "TEENNICK",
            ],
            "Kids",
        ),
        (
            &[
                "MOVIE", "MOVIES", "CINEMA", "CINE", "FILM", "FILMS", "HBO", "CINEMAX", "SHOWTIME", "STARZ", "TCM",
                "MGM", "FLIX", "EPIX",
            ],
            "Movies",
        ),
        (
            &[
                "DOCUMENTARY", "DOCS", "DISCOVERY", "NAT GEO", "NATIONAL GEOGRAPHIC", "HISTORY", "ANIMAL PLANET",
                "SCIENCE", "SMITHSONIAN", "EARTH", "CURIOSITY", "NATURE", "INVESTIGATION",
            ],
            "Documentary",
        ),
        (&["MUSIC", "MTV", "VH1", "VEVO", "CMT", "STINGRAY", "TRACE"], "Music"),
    ];
    for (words, genre) in rules {
        if words_match(&t, words) {
            return genre;
        }
    }
    "Entertainment"
}

/// ISO-ish region code from category prefixes → country name.
pub fn country_name(code: &str) -> String {
    let name = match code.to_uppercase().as_str() {
        "US" | "USA" => "United States",
        "UK" | "GB" => "United Kingdom",
        "IE" => "Ireland",
        "SE" => "Sweden",
        "DK" => "Denmark",
        "NO" => "Norway",
        "FI" => "Finland",
        "IS" => "Iceland",
        "DE" => "Germany",
        "AT" => "Austria",
        "CH" => "Switzerland",
        "NL" => "Netherlands",
        "BE" => "Belgium",
        "FR" => "France",
        "ES" => "Spain",
        "PT" => "Portugal",
        "IT" => "Italy",
        "PL" => "Poland",
        "CZ" => "Czechia",
        "HU" => "Hungary",
        "RO" => "Romania",
        "GR" => "Greece",
        "TR" => "Turkey",
        "RU" => "Russia",
        "UA" => "Ukraine",
        "CA" => "Canada",
        "MX" => "Mexico",
        "BR" => "Brazil",
        "AR" => "Argentina",
        "LATAM" | "LAT" => "Latin America",
        "AU" => "Australia",
        "NZ" => "New Zealand",
        "IN" => "India",
        "PK" => "Pakistan",
        "AE" | "UAE" => "United Arab Emirates",
        "SA" => "Saudi Arabia",
        "ARAB" | "AR-AR" => "Arabic",
        "AFR" | "AF" => "Africa",
        "ZA" => "South Africa",
        "NG" => "Nigeria",
        "PH" => "Philippines",
        "EX-YU" | "YU" => "Ex-Yugoslavia",
        _ => return code.to_owned(),
    };
    name.to_owned()
}

/// "2019" → "2010s"; everything before 1960 is "Older".
pub fn decade(year: Option<i64>) -> Option<String> {
    let y = year.filter(|y| (1880..=2100).contains(y))?;
    Some(if y < 1960 { "Older".to_owned() } else { format!("{}s", y / 10 * 10) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_translated_tmdb_genres() {
        assert_eq!(from_text("Drama / Sci-Fi & Fantasy / War & Politics"), vec!["Drama", "Sci-Fi", "Fantasy", "War & Politics"]);
        assert_eq!(from_text("Kriminal / Drama / Action & Adventure"), vec!["Crime", "Drama", "Action", "Adventure"]);
        assert_eq!(from_text("Drama / Kriminalitet"), vec!["Drama", "Crime"]);
        assert_eq!(from_text("Action og eventyr, Komedie"), vec!["Action", "Adventure", "Comedy"]);
        assert_eq!(from_text("Dokumentär"), vec!["Documentary"]);
        assert_eq!(from_text("Livsstil  Reality  Livsstil"), vec!["Reality"]);
        assert!(from_text("").is_empty());
    }

    #[test]
    fn genres_from_categories() {
        assert_eq!(from_category("EN - DRAMA"), vec!["Drama"]);
        assert_eq!(from_category("NETFLIX STAND-UP COMEDY"), vec!["Stand-up", "Comedy"]);
        assert_eq!(from_category("NETFLIX DOCU-SERIES"), vec!["Documentary"]);
        assert_eq!(from_category("SVENSKA BARN"), vec!["Kids"]);
        assert_eq!(from_category("EN - MUSICAL"), vec!["Musical"]);
        assert_eq!(from_category("EN - CONCERTS"), vec!["Music"]);
        assert_eq!(from_category("EN - MANGA/ANIME"), vec!["Anime"]);
        assert!(from_category("NETFLIX MOVIES").is_empty());
    }

    #[test]
    fn live_genres() {
        assert_eq!(live_from_category("SPORT"), Some("Sports"));
        assert_eq!(live_from_category("NOW TV SPORT"), Some("Sports"));
        assert_eq!(live_from_category("UFC PPV"), Some("Events & PPV"));
        assert_eq!(live_from_category("24/7 COMEDY"), Some("24/7"));
        assert_eq!(live_from_category("24/7 KIDS/FAMILY"), Some("Kids"));
        assert_eq!(live_from_category("SKY CINEMA"), Some("Movies"));
        assert_eq!(live_from_category("NEWS"), Some("News"));
        assert_eq!(live_from_category("AT&T"), None);
        assert_eq!(live_from_title("BBC NEWS"), "News");
        assert_eq!(live_from_title("ESPN2"), "Sports");
        assert_eq!(live_from_title("DISNEY JUNIOR"), "Kids");
        assert_eq!(live_from_title("HBO SIGNATURE"), "Movies");
        assert_eq!(live_from_title("NAT GEO WILD"), "Documentary");
        assert_eq!(live_from_title("MTV LIVE"), "Music");
        assert_eq!(live_from_title("ION"), "Entertainment");
        // whole words only
        assert_eq!(live_from_title("DWELLINGS TV"), "Entertainment");
    }

    #[test]
    fn countries_and_decades() {
        assert_eq!(country_name("UK"), "United Kingdom");
        assert_eq!(country_name("DK"), "Denmark");
        assert_eq!(country_name("XX"), "XX");
        assert_eq!(decade(Some(2019)).as_deref(), Some("2010s"));
        assert_eq!(decade(Some(1955)).as_deref(), Some("Older"));
        assert_eq!(decade(None), None);
    }
}
