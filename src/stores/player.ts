import { create } from "zustand";
import { api, errorMessage } from "../lib/api";
import { invalidateWatchState } from "../lib/queryClient";
import type { Channel, PlayKind, PlayerEvent, Track } from "../lib/types";

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
  play: (np: NowPlaying) => Promise<void>;
  stop: () => Promise<void>;
  onEvent: (e: PlayerEvent) => void;
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

export const usePlayer = create<PlayerStore>((set, get) => ({
  now: null,
  status: "idle",
  error: null,
  reconnectAttempt: 0,
  props: initialProps,
  pip: false,
  setPip: (on) => set({ pip: on && !!get().now }),

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
      await api.play({
        kind: np.kind,
        sourceId: np.sourceId,
        id: np.id,
        ext: np.ext,
        start: np.start,
        title: np.title,
        catchupStart: np.catchupStart,
        catchupMinutes: np.catchupMinutes,
      });
    } catch (e) {
      set({ status: "error", error: errorMessage(e) });
    }
  },

  stop: async () => {
    const { now, props } = get();
    const saved = !!now && (await saveProgress(now, props));
    set({ now: null, status: "idle", error: null, pip: false });
    await api.stop().catch(() => {});
    // the page we return to may show this item's progress
    if (saved) invalidateWatchState();
  },

  onEvent: (e) => {
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
