//! Read-side commands: categories, channel/movie/series listings, details,
//! programme guide and search.

mod categories;
mod channels;
mod details;
mod guide;
mod search;
mod works;

pub use categories::*;
pub use channels::*;
pub use details::*;
pub use guide::*;
pub use search::*;
pub use works::*;

use std::collections::{BTreeMap, HashMap};
use std::time::Instant;

use base64::Engine;
use rusqlite::types::Value as Sql;
use rusqlite::{Connection, OptionalExtension, Row, params, params_from_iter};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::State;

use crate::db::now;
use crate::epg::{self, ProgrammeRow};
use crate::error::{Error, Result};
use crate::kind::{ItemKind, WorkKind};
use crate::settings;
use crate::sources::{self, SourceKind};
use crate::state::{AppState, http_client, read_blocking, write_blocking};
use crate::util::json::{f64_of, first_str, i64_of, str_of};
use crate::works::versions::{self, VersionInfo};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Page<T> {
    pub total: i64,
    pub items: Vec<T>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CategoryItem {
    pub source_id: i64,
    pub id: String,
    pub title: String,
    pub region: Option<String>,
    pub badges: Vec<String>,
    pub count: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Brief {
    pub title: String,
    pub start: i64,
    pub stop: i64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelItem {
    pub source_id: i64,
    pub id: String,
    pub num: Option<i64>,
    pub title: String,
    pub logo: Option<String>,
    pub epg_id: Option<String>,
    pub category_id: Option<String>,
    pub badges: Vec<String>,
    pub archive: bool,
    pub archive_days: i64,
    pub favorite: bool,
    pub now: Option<Brief>,
    pub next: Option<Brief>,
    /// the channel this feed belongs to (all its quality variants)
    pub group: Option<GroupInfo>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupInfo {
    pub key: String,
    /// number of feeds (quality variants) of the channel
    pub variants: i64,
    pub country: Option<String>,
    pub genre: String,
}

/// Grouping info shared by movie and series items (see `works`): the work
/// key and what its versions offer.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkInfo {
    pub key: Option<String>,
    /// number of provider copies of this title (details list them in `versions`)
    pub version_count: i64,
    /// "4K", "Dolby Vision", "Dolby Audio", "HEVC", "Blu-ray"
    pub quality: Vec<String>,
    pub services: Vec<String>,
}

fn work_info(
    key: Option<String>,
    versions: Option<i64>,
    badges: Option<String>,
    services: Option<String>,
) -> WorkInfo {
    let split = |s: Option<String>| -> Vec<String> {
        s.unwrap_or_default()
            .split('|')
            .filter(|x| !x.is_empty())
            .map(str::to_owned)
            .collect()
    };
    WorkInfo {
        version_count: versions.unwrap_or(1),
        quality: split(badges),
        services: split(services),
        key,
    }
}

/// A movie as listed: in browse lists one row per work (`work` table) whose
/// `source_id`/`id` name a representative copy.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MovieItem {
    pub source_id: i64,
    pub id: String,
    pub title: String,
    pub year: Option<i64>,
    pub poster: Option<String>,
    pub rating: Option<f64>,
    pub added: Option<i64>,
    pub tag: Option<String>,
    pub ext: Option<String>,
    pub favorite: bool,
    /// 0..1 watch progress, when started.
    pub progress: Option<f64>,
    pub watched: bool,
    #[serde(flatten)]
    pub work: WorkInfo,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SeriesItem {
    pub source_id: i64,
    pub id: String,
    pub title: String,
    pub year: Option<i64>,
    pub cover: Option<String>,
    pub backdrop: Option<String>,
    pub rating: Option<f64>,
    pub genre: Option<String>,
    pub last_modified: Option<i64>,
    pub tag: Option<String>,
    pub favorite: bool,
    #[serde(flatten)]
    pub work: WorkInfo,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelQuery {
    pub source_id: Option<i64>,
    pub category_id: Option<String>,
    /// one row per channel (`channel_group`) playing its chosen variant,
    /// instead of every provider feed
    #[serde(default)]
    pub grouped: bool,
    /// channel groups of one country ("DK") / live genre ("Sports")
    pub country: Option<String>,
    pub genre: Option<String>,
    #[serde(default)]
    pub favorites: bool,
    /// Only channels that have programme data (TV guide).
    #[serde(default)]
    pub with_epg: bool,
    pub q: Option<String>,
    pub offset: Option<i64>,
    pub limit: Option<i64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaQuery {
    /// with `category_id`: a provider category ("collection")
    pub source_id: Option<i64>,
    pub category_id: Option<String>,
    #[serde(default)]
    pub favorites: bool,
    pub q: Option<String>,
    /// 'added' (default) | 'title' | 'rating' | 'year'
    pub sort: Option<String>,
    /// browse facets that must all match (`work_facet`)
    #[serde(default)]
    pub facets: Vec<FacetFilter>,
    pub offset: Option<i64>,
    pub limit: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FacetFilter {
    pub facet: String,
    pub value: String,
}

const FACETS: &[&str] = &[
    "service",
    "language",
    "quality",
    "genre",
    "decade",
    "collection",
    "original",
    "franchise",
    "network",
];

/// Badges are stored space separated; multi-word ones ("DOLBY VISION") are
/// re-joined here.
pub(crate) fn split_badges(s: String) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for w in s.split_whitespace() {
        match (out.last_mut(), w) {
            (Some(last), "VISION" | "AUDIO") if last == "DOLBY" => {
                last.push(' ');
                last.push_str(w);
            }
            _ => out.push(w.to_owned()),
        }
    }
    out
}

fn like_pattern(q: &str) -> String {
    let escaped = q
        .trim()
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_");
    format!("%{escaped}%")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn badge_joining() {
        assert_eq!(
            split_badges("4K DOLBY VISION HEVC".into()),
            vec!["4K", "DOLBY VISION", "HEVC"]
        );
    }
}
