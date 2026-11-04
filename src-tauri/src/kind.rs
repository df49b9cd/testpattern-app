//! What a catalog item is. The same lowercase names are used in the
//! database (`kind` columns), on the wire to the UI, and in the `search`
//! index, so a typo can't silently select the wrong table.

use std::fmt;

use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ToSql, ToSqlOutput, ValueRef};
use serde::{Deserialize, Serialize};

/// Any catalog item: categories, favorites, history and track info are
/// keyed by it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ItemKind {
    Live,
    Movie,
    Series,
    Episode,
}

impl ItemKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ItemKind::Live => "live",
            ItemKind::Movie => "movie",
            ItemKind::Series => "series",
            ItemKind::Episode => "episode",
        }
    }

    /// The kind's own catalog table.
    pub fn table(self) -> &'static str {
        match self {
            ItemKind::Live => "channel",
            ItemKind::Movie => "movie",
            ItemKind::Series => "series",
            ItemKind::Episode => "episode",
        }
    }
}

/// The kinds that are grouped into works (copies of the same film or show
/// across categories and sources).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WorkKind {
    Movie,
    Series,
}

impl WorkKind {
    pub fn as_str(self) -> &'static str {
        ItemKind::from(self).as_str()
    }

    pub fn table(self) -> &'static str {
        ItemKind::from(self).table()
    }

    /// The tmdb table's spelling: our "series" are TMDB's "tv".
    pub fn tmdb(self) -> &'static str {
        match self {
            WorkKind::Movie => "movie",
            WorkKind::Series => "tv",
        }
    }
}

impl From<WorkKind> for ItemKind {
    fn from(k: WorkKind) -> Self {
        match k {
            WorkKind::Movie => ItemKind::Movie,
            WorkKind::Series => ItemKind::Series,
        }
    }
}

/// One catalog row: the source it came from and the provider's id there.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ItemRef {
    pub source_id: i64,
    pub id: String,
}

impl ItemRef {
    pub fn new(source_id: i64, id: String) -> Self {
        ItemRef { source_id, id }
    }
}

/// What `play` opens: a catalog item, or a past programme of a channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PlayKind {
    #[default]
    Live,
    Movie,
    Episode,
    Catchup,
}

impl PlayKind {
    /// The catalog item behind a file whose tracks are worth remembering
    /// (the version picker shows them): movies and episodes.
    pub fn file_item(self) -> Option<ItemKind> {
        match self {
            PlayKind::Movie => Some(ItemKind::Movie),
            PlayKind::Episode => Some(ItemKind::Episode),
            PlayKind::Live | PlayKind::Catchup => None,
        }
    }
}

impl fmt::Display for PlayKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            PlayKind::Live => "live",
            PlayKind::Movie => "movie",
            PlayKind::Episode => "episode",
            PlayKind::Catchup => "catchup",
        })
    }
}

impl fmt::Display for ItemKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl fmt::Display for WorkKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl ToSql for ItemKind {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(ToSqlOutput::from(self.as_str()))
    }
}

impl FromSql for ItemKind {
    fn column_result(v: ValueRef<'_>) -> FromSqlResult<Self> {
        match v.as_str()? {
            "live" => Ok(ItemKind::Live),
            "movie" => Ok(ItemKind::Movie),
            "series" => Ok(ItemKind::Series),
            "episode" => Ok(ItemKind::Episode),
            other => Err(FromSqlError::Other(
                format!("unknown item kind {other:?}").into(),
            )),
        }
    }
}

impl ToSql for WorkKind {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(ToSqlOutput::from(self.as_str()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_names_match_the_database() {
        for k in [
            ItemKind::Live,
            ItemKind::Movie,
            ItemKind::Series,
            ItemKind::Episode,
        ] {
            assert_eq!(serde_json::to_value(k).unwrap(), k.as_str());
            assert_eq!(
                serde_json::from_value::<ItemKind>(k.as_str().into()).unwrap(),
                k
            );
        }
        assert!(serde_json::from_str::<WorkKind>("\"live\"").is_err());
        let c = rusqlite::Connection::open_in_memory().unwrap();
        let k: ItemKind = c
            .query_row("SELECT ?1", [ItemKind::Episode], |r| r.get(0))
            .unwrap();
        assert_eq!(k, ItemKind::Episode);
        assert_eq!(WorkKind::Series.table(), "series");
        assert_eq!(WorkKind::Series.tmdb(), "tv");
        assert_eq!(ItemKind::Live.table(), "channel");
    }
}
