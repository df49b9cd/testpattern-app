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
use tauri::{Manager, Runtime, State};

use crate::catalog;
use crate::error::{Error, Result};
use crate::kind::{ItemKind, PlayKind, WorkKind};
use crate::playback::{PlayRequest, Stream, resolve};
use crate::player::mpv::{EndReason, Event, Mpv};
use crate::player::{LoadOptions, Player, USER_AGENT, file_options};
use crate::state::{AppState, read_blocking, run_blocking, write_blocking};
use crate::works::versions;

const TIMEOUT: Duration = Duration::from_secs(20);

/// The version checker, managed Tauri state (one check at a time).
#[derive(Default)]
pub struct Probe {
    /// Held for the whole life of a check's mpv instance.
    running: Mutex<()>,
    /// The instance of the running check, so `cancel` can stop it.
    current: Mutex<Option<Arc<Mpv>>>,
    busy: AtomicBool,
}

impl Probe {
    pub fn running(&self) -> bool {
        self.busy.load(Ordering::SeqCst)
    }

    /// Stops a running check and waits until its stream is closed.
    pub fn cancel(&self) {
        if let Some(mpv) = self.current.lock().clone() {
            let _ = mpv.command(&["stop"]);
        }
        drop(self.running.lock());
    }

    /// `cancel` off the async runtime, when a check is running at all.
    pub async fn cancel_running(self: &Arc<Self>) -> Result<()> {
        if self.running() {
            let probe = self.clone();
            run_blocking(move || probe.cancel()).await?;
        }
        Ok(())
    }

    /// Opens `stream`, decodes one video frame and returns its track summary
    /// (`versions::summarize_tracks`, with HDR taken from the first frame's
    /// `video-params.gamma`), or `None` when it didn't open in time.
    fn read_tracks(&self, stream: &Stream) -> std::result::Result<Option<Value>, String> {
        let _running = self
            .running
            .try_lock_for(TIMEOUT)
            .ok_or("another check is still running")?;
        self.busy.store(true, Ordering::SeqCst);
        struct Done<'a>(&'a Probe);
        impl Drop for Done<'_> {
            fn drop(&mut self) {
                *self.0.current.lock() = None;
                self.0.busy.store(false, Ordering::SeqCst);
            }
        }
        let _done = Done(self);
        read_with(stream, &self.current)
    }
}

fn read_with(
    stream: &Stream,
    current: &Mutex<Option<Arc<Mpv>>>,
) -> std::result::Result<Option<Value>, String> {
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
    *current.lock() = Some(mpv.clone());
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
    *current.lock() = None;
    // the last reference: closes the connection before `running` is released
    drop(mpv);
    Ok(found)
}

/// Checks the tracks of one version of a movie, or of a series (its first
/// episode), and remembers them for the version list.
#[tauri::command]
pub async fn version_probe<R: Runtime>(
    app: tauri::AppHandle<R>,
    state: State<'_, AppState>,
    player: State<'_, Player>,
    probe: State<'_, Arc<Probe>>,
    kind: WorkKind,
    source_id: i64,
    id: String,
) -> Result<Option<Value>> {
    let casting = || {
        app.try_state::<crate::cast::Cast>()
            .is_some_and(|c| c.busy())
    };
    if player.busy() || casting() {
        return Err(Error::msg(
            "Stop playback first: the account allows one stream at a time",
        ));
    }
    let st = state.inner().clone();
    // what to open: the movie, or a show's first two episodes (a single
    // episode is sometimes missing on the server)
    let (reqs, item_kind) = match kind {
        WorkKind::Movie => (
            vec![PlayRequest {
                kind: PlayKind::Movie,
                source_id,
                id: id.clone(),
                ..Default::default()
            }],
            ItemKind::Movie,
        ),
        WorkKind::Series => {
            let id = id.clone();
            let seasons = read_blocking(&st, move |conn| {
                catalog::stored_episodes(conn, source_id, &id, "")
            })
            .await?;
            let regular = seasons.iter().filter(|(n, _)| *n > 0).flat_map(|(_, e)| e);
            let reqs: Vec<PlayRequest> = regular
                .chain(seasons.iter().filter(|(n, _)| *n <= 0).flat_map(|(_, e)| e))
                .take(2)
                .map(|e| PlayRequest {
                    kind: PlayKind::Episode,
                    source_id,
                    id: e.id.clone(),
                    ext: e.ext.clone(),
                    ..Default::default()
                })
                .collect();
            if reqs.is_empty() {
                return Err(Error::msg("This version has no episodes yet"));
            }
            (reqs, ItemKind::Episode)
        }
    };
    let mut last = None;
    for req in reqs {
        if player.busy() || casting() {
            return Ok(None);
        }
        let stream = {
            let req = req.clone();
            read_blocking(&st, move |conn| resolve(conn, &req)).await?
        };
        let probe = probe.inner().clone();
        let found = run_blocking(move || probe.read_tracks(&stream))
            .await?
            .map_err(Error::msg)?;
        log::info!(
            "checked tracks of {kind} {source_id}:{id} ({}): {}",
            req.id,
            if found.is_some() { "ok" } else { "no answer" }
        );
        if let Some(t) = found {
            let (item, json) = (req.id.clone(), t.to_string());
            write_blocking(&st, move |conn| {
                versions::save_tracks(conn, source_id, item_kind, &item, &json)
            })
            .await?;
            return Ok(Some(t));
        }
        last = Some(req.id);
    }
    // the server couldn't open it: remembered (the version list says so and
    // the automatic choice avoids it) until a check or playback succeeds
    if let Some(item) = last {
        write_blocking(&st, move |conn| {
            versions::save_tracks(conn, source_id, item_kind, &item, r#"{"unavailable":true}"#)
        })
        .await?;
    }
    Ok(None)
}
