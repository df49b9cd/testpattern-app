//! Programme guide: one channel's programmes and the guide grid.

use base64::Engine;
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::State;

use crate::epg::ProgrammeRow;
use crate::error::Result;
use crate::sources::SourceKind;
use crate::state::{AppState, read_blocking};
use crate::util::json::{i64_of, str_of};
use crate::{epg, sources};

use super::{ChannelItem, ChannelQuery, Page, query_channels};

#[tauri::command]
pub async fn epg_channel(
    state: State<'_, AppState>,
    source_id: i64,
    channel_id: String,
    from: i64,
    to: i64,
) -> Result<Vec<ProgrammeRow>> {
    let st = state.inner().clone();
    let (epg_id, rows) = {
        let channel_id = channel_id.clone();
        read_blocking(&st, move |conn| {
            let epg_id: Option<String> = conn
                .query_row(
                    "SELECT COALESCE(c.epg_id, g.epg_id) FROM channel c LEFT JOIN channel_group g ON g.key = c.group_key
                      WHERE c.source_id = ?1 AND c.id = ?2",
                    params![source_id, channel_id],
                    |r| r.get(0),
                )
                .optional()?
                .flatten();
            let rows = match &epg_id {
                Some(e) => epg::programmes(conn, source_id, e, from, to)?,
                None => Vec::new(),
            };
            Ok((epg_id, rows))
        })
        .await?
    };
    if !rows.is_empty() || epg_id.is_none() {
        return Ok(rows);
    }
    // Fall back to the panel's short EPG (base64 encoded fields).
    let src = read_blocking(&st, move |conn| sources::load(conn, source_id)).await?;
    if src.kind != SourceKind::Xtream {
        return Ok(rows);
    }
    let Ok(x) = sources::xtream_for(&src, st.client(src.user_agent.as_deref())) else {
        return Ok(rows);
    };
    let v = match x.short_epg(&channel_id, 24).await {
        Ok(v) => v,
        Err(e) => {
            log::debug!("short epg {channel_id}: {e}");
            return Ok(rows);
        }
    };
    let b64 = |v: &Value| -> Option<String> {
        let s = str_of(v)?;
        base64::engine::general_purpose::STANDARD
            .decode(s.as_bytes())
            .ok()
            .map(|b| String::from_utf8_lossy(&b).trim().to_owned())
            .or(Some(s))
    };
    let list = v["epg_listings"].as_array().cloned().unwrap_or_default();
    Ok(list
        .iter()
        .filter_map(|p| {
            let start = i64_of(&p["start_timestamp"])?;
            let stop = i64_of(&p["stop_timestamp"])?;
            (stop > from && start < to).then(|| ProgrammeRow {
                epg_id: epg_id.clone().unwrap_or_default(),
                start,
                stop,
                title: b64(&p["title"]).unwrap_or_default(),
                subtitle: None,
                description: b64(&p["description"]),
                category: None,
                episode: None,
                icon: None,
            })
        })
        .collect())
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GuideRow {
    pub channel: ChannelItem,
    pub programmes: Vec<ProgrammeRow>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GuideQuery {
    #[serde(flatten)]
    pub channels: ChannelQuery,
    pub from: i64,
    pub to: i64,
}

/// Channels plus their programmes in a time window (TV guide grid).
#[tauri::command]
pub async fn epg_grid(state: State<'_, AppState>, query: GuideQuery) -> Result<Page<GuideRow>> {
    read_blocking(state.inner(), move |conn| {
        let page = query_channels(conn, &query.channels)?;
        let mut items = Vec::with_capacity(page.items.len());
        for channel in page.items {
            let programmes = match &channel.epg_id {
                Some(e) => epg::programmes(conn, channel.source_id, e, query.from, query.to)?,
                None => Vec::new(),
            };
            items.push(GuideRow {
                channel,
                programmes,
            });
        }
        Ok(Page {
            total: page.total,
            items,
        })
    })
    .await
}
