//! "Check tracks": which audio, subtitle and video tracks a version of a
//! movie or series offers, read by opening its stream for a moment in a
//! second, silent mpv instance (the provider API doesn't list them).
//!
//! Providers allow one stream per account, so a check runs only while the
//! player is idle, checks run one at a time, and `play` cancels a running
//! check and waits until its connection is closed.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde_json::Value;
use tauri::State;

use crate::catalog;
use crate::error::{Error, Result};
use crate::playback::{PlayRequest, Stream, resolve};
use crate::player::mpv::{EndReason, Event, Mpv};
use crate::player::{LoadOptions, Player, USER_AGENT, file_options};
use crate::state::AppState;
use crate::works::versions;

/// Held for the whole life of a check's mpv instance.
static RUNNING: Mutex<()> = Mutex::new(());
/// The instance of the running check, so `cancel` can stop it.
static CURRENT: Mutex<Option<Arc<Mpv>>> = Mutex::new(None);
static BUSY: AtomicBool = AtomicBool::new(false);

const TIMEOUT: Duration = Duration::from_secs(20);

pub fn running() -> bool {
    BUSY.load(Ordering::SeqCst)
}

/// Stops a running check and waits until its stream is closed.
pub fn cancel() {
    if let Some(mpv) = CURRENT.lock().clone() {
        let _ = mpv.command(&["stop"]);
    }
    drop(RUNNING.lock());
}

/// Opens `stream`, decodes one video frame and returns its track summary
/// (`versions::summarize_tracks`, with HDR taken from the first frame's
/// `video-params.gamma`), or `None` when it didn't open in time.
fn read_tracks(stream: &Stream) -> std::result::Result<Option<Value>, String> {
    let _running = RUNNING
        .try_lock_for(TIMEOUT)
        .ok_or("another check is still running")?;
    BUSY.store(true, Ordering::SeqCst);
    struct Done;
    impl Drop for Done {
        fn drop(&mut self) {
            *CURRENT.lock() = None;
            BUSY.store(false, Ordering::SeqCst);
        }
    }
    let _done = Done;
    let mpv = Arc::new(
        Mpv::new(&[
            ("vo", "null"),
            ("ao", "null"),
            // one software-decoded video frame, for HDR from its gamma
            // (vo=null still runs the filter chain, no GPU/AudioUnit work).
            ("hwdec", "no"),
            ("sid", "no"),
            ("idle", "yes"),
            ("terminal", "no"),
            ("config", "no"),
            ("load-scripts", "no"),
            ("input-default-bindings", "no"),
            ("sub-auto", "no"),
            ("cache", "no"),
            ("network-timeout", "15"),
            ("user-agent", USER_AGENT),
        ])
        .map_err(|e| e.to_string())?,
    );
    mpv.request_log_messages("error");
    mpv.observe("video-params", crate::player::mpv_sys::MPV_FORMAT_NODE)
        .map_err(|e| e.to_string())?;
    *CURRENT.lock() = Some(mpv.clone());
    let opts = file_options(&LoadOptions {
        paused: true,
        user_agent: stream.user_agent.clone(),
        referrer: stream.referrer.clone(),
        ..Default::default()
    });
    let mut urls = std::iter::once(&stream.url).chain(stream.alternates.iter().take(1));
    let deadline = Instant::now() + TIMEOUT;
    let mut url = urls.next();
    if let Some(u) = url {
        mpv.command(&["loadfile", u, "replace", "-1", &opts])
            .map_err(|e| e.to_string())?;
    }
    let mut track_list: Option<Value> = None;
    let mut hdr = false;
    let loadfile_at = Instant::now();
    // Once the track list is known, keep waiting for the first video-params
    // (its gamma decides hdr) until the overall deadline: on slow streams the
    // params arrive many seconds after file-loaded, and a shorter gamma
    // deadline would misreport hdr:false. The loop breaks as soon as any
    // video-params shows up; a stream with no video at all (audio-only) waits
    // the timeout out and is summarized with hdr=false.
    while Instant::now() < deadline {
        match mpv.wait_event(0.25) {
            Some(Event::FileLoaded) => {
                track_list = mpv.get_json("track-list");
                if track_list.is_none() {
                    break; // nothing to summarize: don't hold the stream open
                }
            }
            Some(Event::PropertyChange { name, value })
                if name == "video-params" && track_list.is_some() =>
            {
                hdr = matches!(value["gamma"].as_str(), Some("pq" | "hlg"));
                log::info!(
                    "probe: first video-params gamma after {}ms",
                    loadfile_at.elapsed().as_millis()
                );
                break;
            }
            Some(Event::EndFile {
                reason: EndReason::Error,
                error,
            }) => {
                log::debug!(
                    "probe: stream failed: {}",
                    crate::player::redact(error.as_deref().unwrap_or("?"))
                );
                url = urls.next();
                match url {
                    Some(u) => {
                        mpv.command(&["loadfile", u, "replace", "-1", &opts])
                            .map_err(|e| e.to_string())?;
                        track_list = None;
                        hdr = false;
                    }
                    None => break,
                }
            }
            // cancelled (`play`) or finished otherwise
            Some(Event::EndFile { .. } | Event::Shutdown) => break,
            Some(Event::Log {
                prefix,
                level,
                text,
            }) => {
                log::debug!(
                    "probe mpv[{prefix}] {level}: {}",
                    crate::player::redact(text.trim_end())
                );
            }
            _ => {}
        }
    }
    // audio-only or a failed video decode: track list without gamma (hdr=false)
    let found = track_list.and_then(|t| versions::summarize_tracks(&t, hdr));
    let _ = mpv.command(&["stop"]);
    *CURRENT.lock() = None;
    // the last reference: closes the connection before RUNNING is released
    drop(mpv);
    Ok(found)
}

/// Checks the tracks of one version of a movie (`kind` "movie") or series
/// ("series": its first episode) and remembers them for the version list.
#[tauri::command]
pub async fn version_probe(
    state: State<'_, AppState>,
    player: State<'_, Player>,
    kind: String,
    source_id: i64,
    id: String,
) -> Result<Option<Value>> {
    if player.busy() {
        return Err(Error::msg(
            "Stop playback first: the account allows one stream at a time",
        ));
    }
    let st = state.inner().clone();
    // what to open: the movie, or a show's first two episodes (a single
    // episode is sometimes missing on the server)
    let (reqs, item_kind) = {
        let conn = st.db.read();
        match kind.as_str() {
            "movie" => (
                vec![PlayRequest {
                    kind: "movie".into(),
                    source_id,
                    id: id.clone(),
                    ..Default::default()
                }],
                "movie",
            ),
            "series" => {
                let seasons = catalog::stored_episodes(&conn, source_id, &id, "")?;
                let regular = seasons.iter().filter(|(n, _)| *n > 0).flat_map(|(_, e)| e);
                let reqs: Vec<PlayRequest> = regular
                    .chain(seasons.iter().filter(|(n, _)| *n <= 0).flat_map(|(_, e)| e))
                    .take(2)
                    .map(|e| PlayRequest {
                        kind: "episode".into(),
                        source_id,
                        id: e.id.clone(),
                        ext: e.ext.clone(),
                        ..Default::default()
                    })
                    .collect();
                if reqs.is_empty() {
                    return Err(Error::msg("This version has no episodes yet"));
                }
                (reqs, "episode")
            }
            other => return Err(Error::msg(format!("cannot check {other}"))),
        }
    };
    let mut last = None;
    for req in reqs {
        if player.busy() {
            return Ok(None);
        }
        let stream = resolve(&st.db.read(), &req)?;
        let found = tokio::task::spawn_blocking(move || read_tracks(&stream))
            .await
            .map_err(|e| Error::msg(e.to_string()))?
            .map_err(Error::msg)?;
        log::info!(
            "checked tracks of {kind} {source_id}:{id} ({}): {}",
            req.id,
            if found.is_some() { "ok" } else { "no answer" }
        );
        if let Some(t) = found {
            versions::save_tracks(
                &st.db.write(),
                source_id,
                item_kind,
                &req.id,
                &t.to_string(),
            )?;
            return Ok(Some(t));
        }
        last = Some(req.id);
    }
    // the server couldn't open it: remembered (the version list says so and
    // the automatic choice avoids it) until a check or playback succeeds
    if let Some(item) = last {
        versions::save_tracks(
            &st.db.write(),
            source_id,
            item_kind,
            &item,
            r#"{"unavailable":true}"#,
        )?;
    }
    Ok(None)
}
