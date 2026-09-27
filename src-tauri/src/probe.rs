//! "Check tracks": which audio, subtitle and video tracks a version of a
//! movie or series offers, read by opening its stream for a moment in a
//! second, silent mpv instance (the provider API doesn't list them).
//!
//! Providers allow one stream per account, so a check runs only while the
//! player is idle, checks run one at a time, and `play` cancels a running
//! check and waits until its connection is closed. A cancelled check stores
//! nothing: only a stream that failed to open marks a version unavailable.
//!
//! Nothing is decoded, except for HEVC/AV1/VP9 video without a Dolby Vision
//! profile: whether that is HDR (HDR10, HLG) shows only in a decoded frame,
//! so its first frame is decoded (the providers' "4K Dolby Vision" copies
//! are sometimes plain SDR).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde_json::Value;
use tauri::State;

use crate::catalog;
use crate::error::{Error, Result};
use crate::player::mpv::{EndReason, Event, Mpv};
use crate::player::{LoadOptions, Player, USER_AGENT, file_options};
use crate::playback::{PlayRequest, Stream, resolve};
use crate::state::AppState;
use crate::works::versions;

/// Held for the whole life of a check's mpv instance.
static RUNNING: Mutex<()> = Mutex::new(());
/// The instance of the running check, so `cancel` can stop it.
static CURRENT: Mutex<Option<Arc<Mpv>>> = Mutex::new(None);
static BUSY: AtomicBool = AtomicBool::new(false);
/// Set by `cancel`: the running check ends without a verdict.
static CANCELLED: AtomicBool = AtomicBool::new(false);

const TIMEOUT: Duration = Duration::from_secs(20);
/// Codecs that carry HDR in practice (H.264 practically never does).
const HDR_CODECS: &[&str] = &["hevc", "av1", "vp9"];
/// How long a check waits for its first decoded frame.
const FRAME_WAIT: Duration = Duration::from_secs(5);

pub fn running() -> bool {
    BUSY.load(Ordering::SeqCst)
}

/// Stops a running check and waits until its stream is closed.
pub fn cancel() {
    CANCELLED.store(true, Ordering::SeqCst);
    if let Some(mpv) = CURRENT.lock().clone() {
        let _ = mpv.command(&["stop"]);
    }
    drop(RUNNING.lock());
}

/// What a check found out about one file.
enum Outcome {
    Tracks(Value),
    /// the server didn't open it (in time)
    NotOpened,
    /// playback took over the connection (`cancel`): no verdict
    Cancelled,
}

/// Opens `stream` and reads its track summary (`versions::summarize_tracks`);
/// decodes a frame only when that's the one way to tell HDR.
fn read_tracks(stream: &Stream) -> std::result::Result<Outcome, String> {
    let _running = RUNNING.try_lock_for(TIMEOUT).ok_or("another check is still running")?;
    // `play` cancels only while BUSY, so no cancel of this check is lost here
    CANCELLED.store(false, Ordering::SeqCst);
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
            // no video decoding; tracks are listed whether selected or not.
            // (Without any audio/video chain mpv gives up on the file before
            // `file-loaded`, so audio stays on — decoded into the null output.)
            ("vid", "no"),
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
    // (a cancel that came before CURRENT was set had nothing to stop)
    if let Some(u) = url.filter(|_| !CANCELLED.load(Ordering::SeqCst)) {
        mpv.command(&["loadfile", u, "replace", "-1", &opts]).map_err(|e| e.to_string())?;
    }
    let mut found = None;
    while Instant::now() < deadline && !CANCELLED.load(Ordering::SeqCst) {
        match mpv.wait_event(0.25) {
            Some(Event::FileLoaded) => {
                let list = mpv.get_json("track-list");
                let hdr = list.as_ref().is_some_and(hdr_needs_a_frame) && first_frame_hdr(&mpv);
                // cancelled while decoding: no verdict (the HDR flag would be a guess)
                if !CANCELLED.load(Ordering::SeqCst) {
                    found = list.and_then(|t| versions::summarize_tracks(&t, hdr));
                }
                break;
            }
            Some(Event::EndFile { reason: EndReason::Error, error }) => {
                log::debug!("probe: stream failed: {}", crate::player::redact(error.as_deref().unwrap_or("?")));
                url = urls.next();
                match url {
                    Some(u) => mpv.command(&["loadfile", u, "replace", "-1", &opts]).map_err(|e| e.to_string())?,
                    None => break,
                }
            }
            // cancelled (`play`) or finished otherwise
            Some(Event::EndFile { .. } | Event::Shutdown) => break,
            Some(Event::Log { prefix, level, text }) => {
                log::debug!("probe mpv[{prefix}] {level}: {}", crate::player::redact(text.trim_end()));
            }
            _ => {}
        }
    }
    let _ = mpv.command(&["stop"]);
    *CURRENT.lock() = None;
    // the last reference: closes the connection before RUNNING is released
    drop(mpv);
    Ok(match found {
        Some(t) => Outcome::Tracks(t),
        None if CANCELLED.load(Ordering::SeqCst) => Outcome::Cancelled,
        None => Outcome::NotOpened,
    })
}

/// A video track whose HDR shows only in decoded frames: HEVC/AV1/VP9
/// without a Dolby Vision profile (which mpv lists without decoding).
fn hdr_needs_a_frame(track_list: &Value) -> bool {
    track_list.as_array().into_iter().flatten().any(|t| {
        t["type"] == "video"
            && t["image"].as_bool() != Some(true)
            && t["dolby-vision-profile"].is_null()
            && t["codec"].as_str().is_some_and(|c| HDR_CODECS.contains(&c))
    })
}

/// Decodes the first video frame (software, still paused) and reads its
/// transfer function: PQ (HDR10, Dolby Vision's base layer) or HLG. A 4K
/// HEVC frame takes 0.25–0.6 s, ~0.4 s CPU (Ryzen 9 5950X).
fn first_frame_hdr(mpv: &Mpv) -> bool {
    let (started, cpu) = (Instant::now(), cpu_time());
    if mpv.set_string("vid", "auto").is_err() {
        return false;
    }
    while started.elapsed() < FRAME_WAIT && !CANCELLED.load(Ordering::SeqCst) {
        if let Some(gamma) = mpv.get_json("video-params").and_then(|p| p["gamma"].as_str().map(str::to_owned)) {
            log::debug!("probe: first frame ({gamma}) after {:?}, {:?} CPU", started.elapsed(), cpu_time() - cpu);
            return matches!(gamma.as_str(), "pq" | "hlg");
        }
        if matches!(mpv.wait_event(0.05), Some(Event::EndFile { .. } | Event::Shutdown)) {
            break;
        }
    }
    log::debug!("probe: no frame after {:?}", started.elapsed());
    false
}

/// CPU time of the process so far (user + system).
fn cpu_time() -> Duration {
    // SAFETY: getrusage fills the zeroed struct it is given
    let mut u: libc::rusage = unsafe { std::mem::zeroed() };
    unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut u) };
    let us = |t: libc::timeval| t.tv_sec as u64 * 1_000_000 + t.tv_usec as u64;
    Duration::from_micros(us(u.ru_utime) + us(u.ru_stime))
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
        return Err(Error::msg("Stop playback first: the account allows one stream at a time"));
    }
    let st = state.inner().clone();
    // what to open: the movie, or a show's first two episodes (a single
    // episode is sometimes missing on the server)
    let (reqs, item_kind) = {
        let conn = st.db.read();
        match kind.as_str() {
            "movie" => (vec![PlayRequest { kind: "movie".into(), source_id, id: id.clone(), ..Default::default() }], "movie"),
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
        let outcome = tokio::task::spawn_blocking(move || read_tracks(&stream))
            .await
            .map_err(|e| Error::msg(e.to_string()))?
            .map_err(Error::msg)?;
        let verdict = match outcome {
            Outcome::Tracks(_) => "ok",
            Outcome::NotOpened => "no answer",
            Outcome::Cancelled => "cancelled by playback",
        };
        log::info!("checked tracks of {kind} {source_id}:{id} ({}): {verdict}", req.id);
        match outcome {
            Outcome::Tracks(t) => {
                versions::save_tracks(&st.db.write(), source_id, item_kind, &req.id, &t.to_string())?;
                return Ok(Some(t));
            }
            // says nothing about this version (and its stream is the player's now)
            Outcome::Cancelled => return Ok(None),
            Outcome::NotOpened => last = Some(req.id),
        }
    }
    // the server couldn't open it: remembered (the version list says so and
    // the automatic choice avoids it) until a check or playback succeeds
    if let Some(item) = last {
        versions::save_tracks(&st.db.write(), source_id, item_kind, &item, r#"{"unavailable":true}"#)?;
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_a_frame_only_when_hdr_is_unknown_otherwise() {
        let list = |video: Value| serde_json::json!([video, {"type": "audio", "codec": "eac3"}]);
        // HEVC/AV1 without a Dolby Vision profile: only a frame tells
        assert!(hdr_needs_a_frame(&list(serde_json::json!({"type": "video", "codec": "hevc"}))));
        assert!(hdr_needs_a_frame(&list(serde_json::json!({"type": "video", "codec": "av1"}))));
        // Dolby Vision is listed without decoding; H.264 isn't HDR in practice
        assert!(!hdr_needs_a_frame(&list(serde_json::json!({"type": "video", "codec": "hevc", "dolby-vision-profile": 8}))));
        assert!(!hdr_needs_a_frame(&list(serde_json::json!({"type": "video", "codec": "h264"}))));
        // cover art is no video
        assert!(!hdr_needs_a_frame(&list(serde_json::json!({"type": "video", "codec": "hevc", "image": true}))));
        assert!(!hdr_needs_a_frame(&serde_json::json!([])));
    }
}
