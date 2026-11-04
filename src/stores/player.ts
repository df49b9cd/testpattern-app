import { create } from "zustand";
import { api, errorMessage } from "../lib/api";
import { invalidateWatchState, queryClient } from "../lib/queryClient";
import type { CastDevice, CastMeta, CastStatus, Channel, PlayKind, PlayRequest, PlayerEvent, Track } from "../lib/types";

/** What is (or is about to be) playing, with enough context for the UI. */
export interface NowPlaying {
  kind: PlayKind;
  sourceId: number;
  id: string;
  title: string;
  subtitle?: string | null;
  image?: string | null;
  backdrop?: string | null;
  ext?: string | null;
  /** resume position (seconds) */
  start?: number | null;
  /** open paused (continuing here after casting) */
  paused?: boolean;
  // live
  channel?: Channel;
  /** channel list for up/down zapping */
  zapList?: Channel[];
  // episodes
  seriesId?: string;
  seriesTitle?: string;
  season?: number;
  episode?: number;
  // catch-up
  catchupStart?: number;
  catchupMinutes?: number;
}

export interface Chapter {
  title?: string;
  time: number;
}

export interface PlaybackProps {
  pause: boolean;
  timePos: number;
  duration: number;
  buffering: boolean;
  cacheBuffering: number;
  cacheTime: number;
  seeking: boolean;
  seekable: boolean;
  idle: boolean;
  eof: boolean;
  volume: number;
  mute: boolean;
  speed: number;
  tracks: Track[];
  aid: number | false | null;
  sid: number | false | null;
  /** mpv sub-visibility: whether the selected subtitle track is shown (PL-99) */
  subVisible: boolean;
  videoW: number;
  videoH: number;
  fps: number;
  videoCodec: string;
  audioCodec: string;
  hwdec: string;
  aspect: string;
  panscan: number;
  subDelay: number;
  audioDelay: number;
  chapters: Chapter[];
  /** file a live recording is written to; "" when not recording */
  recording: string;
}

export type PlayerStatus = "idle" | "loading" | "playing" | "reconnecting" | "ended" | "error";

const initialProps: PlaybackProps = {
  pause: false,
  timePos: 0,
  duration: 0,
  buffering: false,
  cacheBuffering: 0,
  cacheTime: 0,
  seeking: false,
  seekable: false,
  idle: true,
  eof: false,
  volume: 100,
  mute: false,
  speed: 1,
  tracks: [],
  aid: null,
  sid: null,
  subVisible: false,
  videoW: 0,
  videoH: 0,
  fps: 0,
  videoCodec: "",
  audioCodec: "",
  hwdec: "",
  aspect: "no",
  panscan: 0,
  subDelay: 0,
  audioDelay: 0,
  chapters: [],
  recording: "",
};

interface PlayerStore {
  now: NowPlaying | null;
  status: PlayerStatus;
  error: string | null;
  reconnectAttempt: number;
  props: PlaybackProps;
  /** playing on in a floating window while browsing (components/PipPlayer.tsx) */
  pip: boolean;
  setPip: (on: boolean) => void;
  /** casting to a TV: everything played goes there (src-tauri/src/cast) */
  cast: CastStatus | null;
  /** why casting ended, when it wasn't the user */
  castNotice: string | null;
  play: (np: NowPlaying) => Promise<void>;
  stop: () => Promise<void>;
  /** Moves what's playing (if anything) to a TV, or switches TVs. */
  castTo: (device: CastDevice) => Promise<void>;
  /** Ends casting; a movie or episode continues here, paused. */
  stopCasting: () => Promise<void>;
  onEvent: (e: PlayerEvent) => void;
  onCastStatus: (s: CastStatus) => void;
}

const num = (v: unknown, d = 0) => (typeof v === "number" && Number.isFinite(v) ? v : d);
const bool = (v: unknown) => v === true;
const str = (v: unknown) => (typeof v === "string" ? v : "");
export const trackId = (v: unknown): number | false | null =>
  typeof v === "number" ? v : v === false || v === "no" ? false : null;

/** mpv property change → the store fields it updates (null: not tracked). */
export function applyProp(p: PlaybackProps, name: string, v: unknown): Partial<PlaybackProps> | null {
  switch (name) {
    case "pause":
      return { pause: bool(v) };
    case "time-pos":
      return { timePos: num(v, p.timePos) };
    case "duration":
      return { duration: num(v) };
    case "paused-for-cache":
      return { buffering: bool(v) };
    case "cache-buffering-state":
      return { cacheBuffering: num(v) };
    case "demuxer-cache-time":
      return { cacheTime: num(v) };
    case "seeking":
      return { seeking: bool(v) };
    case "seekable":
      return { seekable: bool(v) };
    case "idle-active":
      return { idle: bool(v) };
    case "eof-reached":
      return { eof: bool(v) };
    case "volume":
      return { volume: num(v, 100) };
    case "mute":
      return { mute: bool(v) };
    case "speed":
      return { speed: num(v, 1) };
    case "track-list":
      return { tracks: Array.isArray(v) ? (v as Track[]) : [] };
    case "aid":
      return { aid: trackId(v) };
    case "sid":
      return { sid: trackId(v) };
    case "sub-visibility":
      return { subVisible: bool(v) };
    case "video-params": {
      const vp = (v ?? {}) as Record<string, unknown>;
      return { videoW: num(vp.w), videoH: num(vp.h) };
    }
    case "estimated-vf-fps":
      return { fps: num(v) };
    case "video-codec":
      return { videoCodec: str(v) };
    case "audio-codec-name":
      return { audioCodec: str(v) };
    case "hwdec-current":
      return { hwdec: str(v) };
    case "video-aspect-override":
      return { aspect: str(v) || "no" };
    case "panscan":
      return { panscan: num(v) };
    case "sub-delay":
      return { subDelay: num(v) };
    case "audio-delay":
      return { audioDelay: num(v) };
    case "chapter-list":
      return { chapters: Array.isArray(v) ? (v as Chapter[]) : [] };
    case "stream-record":
      return { recording: str(v) };
    default:
      return null;
  }
}

function playRequest(np: NowPlaying): PlayRequest {
  return {
    kind: np.kind,
    sourceId: np.sourceId,
    id: np.id,
    ext: np.ext,
    start: np.start,
    title: np.title,
    catchupStart: np.catchupStart,
    catchupMinutes: np.catchupMinutes,
    paused: np.paused,
  };
}

/** What the TV shows about the item. */
export function castMeta(np: NowPlaying): CastMeta {
  return { title: np.title, subtitle: np.subtitle ?? null, image: np.backdrop ?? np.image ?? null };
}

/** Cast status → the player status the UI shows. */
export function castPlayerStatus(s: CastStatus): { status: PlayerStatus; error: string | null } {
  switch (s.state) {
    case "connecting":
    case "loading":
      return { status: "loading", error: null };
    case "ended":
      return { status: "ended", error: null };
    case "error":
      return { status: "error", error: s.error ?? "The TV could not play this stream" };
    default:
      return { status: "playing", error: null };
  }
}

/** Cast status → the playback props the UI reads (mpv's are idle meanwhile). */
export function castProps(p: PlaybackProps, s: CastStatus): PlaybackProps {
  return {
    ...p,
    pause: s.state === "paused",
    timePos: s.position,
    duration: s.duration,
    buffering: s.state === "buffering",
    cacheTime: 0,
    seeking: false,
    seekable: !s.live,
    idle: false,
    eof: s.state === "ended",
    volume: Math.round(s.volume * 100),
    mute: s.muted,
    speed: 1,
    tracks: [],
    chapters: [],
    recording: "",
  };
}

export const usePlayer = create<PlayerStore>((set, get) => ({
  now: null,
  status: "idle",
  error: null,
  reconnectAttempt: 0,
  props: initialProps,
  pip: false,
  setPip: (on) => set({ pip: on && !!get().now && !get().cast }),
  cast: null,
  castNotice: null,

  play: async (np) => {
    const prev = get().now;
    // leaving a VOD item: persist where we stopped
    if (prev && prev !== np && (await saveProgress(prev, get().props))) invalidateWatchState();
    set({
      now: np,
      status: "loading",
      error: null,
      reconnectAttempt: 0,
      props: { ...get().props, timePos: np.start ?? 0, duration: 0, tracks: [], chapters: [], eof: false, buffering: false },
    });
    try {
      const cast = get().cast;
      // the first item starts the session on the TV, later ones reuse it
      if (cast) await api.castPlay(playRequest(np), castMeta(np), cast.deviceId);
      else await api.play(playRequest(np));
    } catch (e) {
      set({ status: "error", error: errorMessage(e) });
    }
  },

  stop: async () => {
    const { now, props, cast } = get();
    const saved = !!now && (await saveProgress(now, props));
    set({ now: null, status: "idle", error: null, pip: false, cast: null });
    await (cast ? api.castStop() : api.stop()).catch(() => {});
    // the page we return to may show this item's progress
    if (saved) invalidateWatchState();
  },

  castTo: async (device) => {
    const { now, props, cast } = get();
    if (cast?.deviceId === device.id) return;
    // VOD continues where it is; live starts at "now"
    const np = now && now.kind !== "live" ? { ...now, start: props.timePos || now.start, paused: false } : now;
    if (now && (await saveProgress(now, props))) invalidateWatchState();
    set({
      cast: {
        deviceId: device.id,
        deviceName: device.name,
        state: np ? "connecting" : "ready",
        position: np?.start ?? 0,
        duration: 0,
        live: now?.kind === "live",
        volume: 1,
        muted: false,
        error: null,
      },
      castNotice: null,
      pip: false,
      ...(np ? { now: np, status: "loading" as const, error: null } : {}),
    });
    if (!np) return;
    try {
      await api.castPlay(playRequest(np), castMeta(np), device.id);
    } catch (e) {
      set({ cast: null, castNotice: errorMessage(e), status: "error", error: errorMessage(e) });
    }
  },

  stopCasting: async () => {
    if (!get().cast) return;
    set({ cast: null });
    await api.castStop().catch(() => {});
    continueHere();
  },

  onCastStatus: (s) => {
    const cur = get().cast;
    // a session that already ended here (or the one a TV switch replaced)
    if (!cur || s.deviceId !== cur.deviceId) return;
    if (s.state === "stopped") {
      set({ cast: null, castNotice: s.error ?? `Casting to ${s.deviceName} ended` });
      continueHere();
      return;
    }
    set({ cast: s, props: castProps(get().props, s), ...(get().now ? castPlayerStatus(s) : {}) });
  },

  onEvent: (e) => {
    // mpv is stopped while casting; its last events must not touch the TV's state
    if (get().cast) return;
    switch (e.type) {
      case "prop": {
        const patch = applyProp(get().props, e.name, e.value);
        if (patch?.volume !== undefined && patch.volume !== get().props.volume) rememberVolume(patch.volume);
        if (patch) set({ props: { ...get().props, ...patch } });
        break;
      }
      case "start-file":
        if (get().now) set({ status: "loading" });
        break;
      case "file-loaded":
      case "restart":
        if (get().now) set({ status: "playing", error: null, reconnectAttempt: 0 });
        break;
      case "reconnecting":
        set({ status: "reconnecting", reconnectAttempt: e.attempt });
        break;
      case "end-file":
        if (!get().now) break;
        if (e.reason === "eof") set({ status: "ended" });
        else if (e.reason === "error")
          set({ status: "error", error: e.error ? `Playback failed: ${e.error}` : "Playback failed" });
        break;
      default:
        break;
    }
  },
}));

/** After casting: a movie or episode continues here (paused), live TV stops. */
function continueHere() {
  const { now, props } = usePlayer.getState();
  // the TV's volume was showing: back to this computer's
  void Promise.all([api.get<number>("volume"), api.get<boolean>("mute")])
    .then(([volume, mute]) =>
      usePlayer.setState((s) => ({ props: { ...s.props, volume: volume ?? s.props.volume, mute: mute ?? s.props.mute } })),
    )
    .catch(() => {});
  if (!now) return;
  if (now.kind === "live") {
    usePlayer.setState({ now: null, status: "idle", error: null, props: { ...props, eof: false } });
    return;
  }
  void usePlayer.getState().play({ ...now, start: props.timePos || now.start, paused: true });
}

/**
 * Playback controls for whichever side is playing: this computer (mpv) or
 * the TV being cast to.
 */
export const control = {
  setPause(pause: boolean) {
    if (usePlayer.getState().cast) return api.castControl(pause ? "pause" : "play").catch(() => {});
    return api.set("pause", pause).catch(() => {});
  },
  seekTo(seconds: number) {
    const { cast, props } = usePlayer.getState();
    if (!cast) return api.command("seek", seconds.toFixed(2), "absolute").catch(() => {});
    const t = Math.max(0, props.duration > 0 ? Math.min(seconds, props.duration - 1) : seconds);
    usePlayer.setState({ props: { ...props, timePos: t } });
    return api.castControl("seek", t).catch(() => {});
  },
  seekBy(seconds: number) {
    const { cast, props } = usePlayer.getState();
    if (!cast) return api.command("seek", seconds, "relative").catch(() => {});
    return control.seekTo(props.timePos + seconds);
  },
  /** mpv scale: 0…150 (the TV's 0…100) */
  setVolume(volume: number) {
    if (usePlayer.getState().cast) return api.castControl("volume", Math.min(100, volume) / 100).catch(() => {});
    return api.set("volume", volume).catch(() => {});
  },
  setMute(mute: boolean) {
    if (usePlayer.getState().cast) return api.castControl("mute", mute ? 1 : 0).catch(() => {});
    return api.set("mute", mute).catch(() => {});
  },
};

/**
 * Subtitles on/off from the player chrome (PL-99): persists the setting and
 * settings.rs applies mpv `sub-visibility` right away. The settings cache is
 * patched so both toggles (player + Settings page) stay in sync; a failed
 * write rolls the patch back instead of diverging from the backend.
 */
export function setSubsEnabled(v: boolean): Promise<void> {
  const cur = queryClient.getQueryData<Record<string, unknown>>(["settings"]);
  queryClient.setQueryData(["settings"], { ...cur, "player.subsEnabled": v });
  return api.setSetting("player.subsEnabled", v).catch((e) => {
    queryClient.setQueryData(["settings"], (c: Record<string, unknown> | undefined) => ({ ...c, "player.subsEnabled": cur?.["player.subsEnabled"] }));
    throw e;
  });
}

/** Toggle-the-subtitles helper shared by Player.tsx ("S" key) and its menu. */
export function toggleSubs(): void {
  void setSubsEnabled(!usePlayer.getState().props.subVisible).catch(() => {});
}

/**
 * What the S key does with subtitles (PL-100): with several subtitle tracks
 * it cycles through them (off → 1 → 2 → … → off), like mpv's `cycle sid`;
 * with exactly one it toggles visibility (the persisted setting).
 * Returns null when there is nothing to switch (no tracks at all).
 */
export function subKeyAction(props: PlaybackProps): { cycle: number | false } | { toggle: boolean } | null {
  const tracks = props.tracks.filter((t) => t.type === "sub");
  if (tracks.length > 1) {
    const cur = typeof props.sid === "number" ? tracks.findIndex((t) => t.id === props.sid) : -1;
    // off / unknown selects the first track; past the last wraps to "no"
    if (cur < 0) return { cycle: tracks[0].id };
    return { cycle: cur < tracks.length - 1 ? tracks[cur + 1].id : false };
  }
  if (tracks.length === 1) return { toggle: !props.subVisible };
  return null;
}

/** What the S key may do next — for the PL-100 one-shot hint text. */
export function subKeyHint(props: PlaybackProps): "cycle" | "on" | "off" | null {
  const a = subKeyAction(props);
  if (!a) return null;
  if ("cycle" in a) return "cycle";
  return a.toggle ? "on" : "off";
}

let volumeTimer = 0;
/** Persists the volume (applied at startup, settings.rs) once it settles. */
function rememberVolume(volume: number) {
  window.clearTimeout(volumeTimer);
  volumeTimer = window.setTimeout(() => void api.setSetting("player.volume", Math.round(volume)).catch(() => {}), 1000);
}

/**
 * Stores VOD resume positions (movies / episodes); true when saved. Without a
 * duration nothing is known about the item yet (still opening, failed, or
 * already unloaded), so nothing is written.
 */
export async function saveProgress(np: NowPlaying, p: PlaybackProps): Promise<boolean> {
  if (np.kind !== "movie" && np.kind !== "episode") return false;
  if (p.duration <= 0) return false;
  return api
    .updateHistory({
      kind: np.kind,
      sourceId: np.sourceId,
      itemId: np.id,
      seriesId: np.seriesId ?? null,
      season: np.season ?? null,
      episode: np.episode ?? null,
      title: np.kind === "episode" ? (np.seriesTitle ?? np.title) : np.title,
      subtitle: np.kind === "episode" ? np.title : (np.subtitle ?? null),
      image: np.image ?? null,
      backdrop: np.backdrop ?? null,
      ext: np.ext ?? null,
      position: p.timePos,
      duration: p.duration,
    })
    .then(() => true)
    .catch(() => false);
}

/**
 * Places the video in a sub-rectangle of the window (live preview), or back
 * to fullscreen when `rect` is null. mpv letterboxes inside the rectangle.
 */
export async function setVideoViewport(rect: DOMRect | null): Promise<void> {
  const W = window.innerWidth;
  const H = window.innerHeight;
  const clamp = (v: number) => Math.min(0.95, Math.max(0, v));
  const m = rect
    ? { left: rect.left / W, right: (W - rect.right) / W, top: rect.top / H, bottom: (H - rect.bottom) / H }
    : { left: 0, right: 0, top: 0, bottom: 0 };
  await Promise.all(
    Object.entries(m).map(([side, v]) => api.set(`video-margin-ratio-${side}`, clamp(v)).catch(() => {})),
  );
}
