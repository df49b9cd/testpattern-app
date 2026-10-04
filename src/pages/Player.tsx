import clsx from "clsx";
import { useQuery } from "@tanstack/react-query";
import {
  AudioLines,
  ChevronLeft,
  Circle,
  ChevronDown,
  ChevronUp,
  Gauge,
  Info,
  List,
  Loader2,
  Maximize,
  Minimize,
  PictureInPicture2,
  Pause,
  Play,
  RectangleHorizontal,
  RotateCcw,
  RotateCw,
  SkipForward,
  Subtitles,
  Volume1,
  Volume2,
  VolumeX,
} from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { useNavigate } from "react-router";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { api } from "../lib/api";
import { inTauri } from "../lib/bridge";
import { clock, elapsed, hhmm, nowUnix } from "../lib/format";
import { channelNowPlaying, playEpisode } from "../lib/play";
import type { Channel, Episode, Track } from "../lib/types";
import { usePlayer, setSubsEnabled } from "../stores/player";
import { queryClient } from "../lib/queryClient";
import { ChannelLogo } from "../components/media";
import { Badge, Button, IconButton, LiveDot, ProgressBar } from "../components/ui";

const HIDE_AFTER = 3000;

async function toggleFullscreen() {
  if (inTauri) {
    const w = getCurrentWindow();
    await w.setFullscreen(!(await w.isFullscreen()));
  } else if (document.fullscreenElement) {
    await document.exitFullscreen();
  } else {
    await document.documentElement.requestFullscreen();
  }
}

async function isFullscreen() {
  return inTauri ? getCurrentWindow().isFullscreen() : !!document.fullscreenElement;
}

export function PlayerPage() {
  const navigate = useNavigate();
  const now = usePlayer((s) => s.now);
  const status = usePlayer((s) => s.status);
  const error = usePlayer((s) => s.error);
  const p = usePlayer((s) => s.props);
  const reconnectAttempt = usePlayer((s) => s.reconnectAttempt);

  const [chrome, setChrome] = useState(true);
  const [panel, setPanel] = useState<null | "channels" | "audio" | "subs" | "aspect" | "speed">(null);
  const [info, setInfo] = useState(false);
  const [fullscreen, setFullscreen] = useState(false);
  const [flash, setFlash] = useState<null | "play" | "pause">(null);
  const [digits, setDigits] = useState("");
  const [notice, setNotice] = useState<string | null>(null);
  const noticeTimer = useRef(0);
  const say = useCallback((text: string) => {
    setNotice(text);
    window.clearTimeout(noticeTimer.current);
    noticeTimer.current = window.setTimeout(() => setNotice(null), 4000);
  }, []);
  const hideTimer = useRef(0);
  const digitTimer = useRef(0);

  // PL-100: mpv picked a subtitle track (slang) but subs are hidden — tell the
  // first-run user once how to turn them on instead of leaving them clueless
  const sid = p.sid;
  const subVisible = p.subVisible;
  useEffect(() => {
    if (typeof sid !== "number" || subVisible) return;
    const seen = queryClient.getQueryData<Record<string, unknown>>(["settings"])?.["ui.subHintSeen"];
    if (seen === true) return;
    say("Subtitles are off — press S to turn them on");
    queryClient.setQueryData(["settings"], (cur: Record<string, unknown> | undefined) => ({ ...cur, "ui.subHintSeen": true }));
    void api.setSetting("ui.subHintSeen", true).catch(() => {});
  }, [sid, subVisible, say]);

  const live = now?.kind === "live";
  const vod = now?.kind === "movie" || now?.kind === "episode" || now?.kind === "catchup";

  // nothing to show → back out (unless we are already leaving)
  const exiting = useRef(false);
  useEffect(() => {
    if (!now && !exiting.current) {
      exiting.current = true;
      navigate(-1);
    }
  }, [now, navigate]);

  useEffect(() => {
    void isFullscreen().then(setFullscreen);
    // this is the full-screen player: no picture-in-picture while it's open
    usePlayer.getState().setPip(false);
  }, []);

  const poke = useCallback(() => {
    setChrome(true);
    window.clearTimeout(hideTimer.current);
    hideTimer.current = window.setTimeout(() => {
      if (!usePlayer.getState().props.pause) {
        setChrome(false);
        setPanel(null);
      }
    }, HIDE_AFTER);
  }, []);
  useEffect(() => {
    poke();
    return () => window.clearTimeout(hideTimer.current);
  }, [poke]);
  // keep chrome up while paused / failing
  const pinned = p.pause || status !== "playing" || panel !== null;
  const visible = chrome || pinned;

  const exit = useCallback(async () => {
    if (await isFullscreen()) {
      await toggleFullscreen();
      setFullscreen(false);
      return;
    }
    exiting.current = true;
    const cur = usePlayer.getState().now;
    navigate(-1);
    // live keeps playing (preview on the Live TV page); VOD saves & stops
    if (cur && cur.kind !== "live") await usePlayer.getState().stop();
  }, [navigate]);

  // Picture-in-picture (T-034): keep playing in a small window while browsing
  const toPip = useCallback(async () => {
    if (await isFullscreen()) {
      await toggleFullscreen();
      setFullscreen(false);
    }
    exiting.current = true;
    usePlayer.getState().setPip(true);
    // back to where the user came from (react-router keeps the history index)
    const idx = (window.history.state as { idx?: number } | null)?.idx ?? 0;
    if (idx > 0) navigate(-1);
    else navigate("/", { replace: true });
  }, [navigate]);

  // Live TV can be paused (mpv keeps buffering). The normal read-ahead gap
  // between playback and buffer end is noted at pause time, so "behind" only
  // counts real delay.
  const [liveGap, setLiveGap] = useState<number | null>(null);
  useEffect(() => setLiveGap(null), [now?.id]);

  const togglePause = useCallback(() => {
    const { props: cur, now: np } = usePlayer.getState();
    const paused = cur.pause;
    if (!paused && np?.kind === "live") setLiveGap((g) => g ?? Math.max(0, cur.cacheTime - cur.timePos));
    void api.set("pause", !paused);
    setFlash(paused ? "play" : "pause");
    window.setTimeout(() => setFlash(null), 500);
  }, []);

  const seekBy = useCallback((secs: number) => {
    void api.command("seek", secs, "relative");
    say(secs < 0 ? `« ${secs} s` : `» +${secs} s`);
    poke();
  }, [poke, say]);

  const nudgeVolume = useCallback((delta: number) => {
    const st = usePlayer.getState();
    void api.set("volume", Math.min(150, st.props.volume + delta)).then(() => {
      // mpv applies async; read what it has a moment later so the chip shows
      // the real level, not the pre-clamp guess.
      window.setTimeout(() => {
        void api.get("volume").then((v) => {
          if (typeof v === "number") say(`Volume ${Math.round(v)}`);
        }).catch(() => {});
      }, 60);
    }).catch(() => {});
    poke();
  }, [say, poke]);

  const toggleMute = useCallback(() => {
    const st = usePlayer.getState();
    void api.set("mute", !st.props.mute).then(() => say(st.props.mute ? "Sound on" : "Muted")).catch(() => {});
    poke();
  }, [say, poke]);

  // ---- live zapping
  const zapList = now?.zapList ?? [];
  // by feed, else by channel (another feed of it may be playing)
  const groupKey = now?.channel?.group?.key;
  let zapIndex = live ? zapList.findIndex((c) => c.sourceId === now?.sourceId && c.id === now?.id) : -1;
  if (live && zapIndex < 0 && groupKey) zapIndex = zapList.findIndex((c) => c.group?.key === groupKey);
  const zapTo = useCallback(
    (c: Channel) => {
      void usePlayer.getState().play(channelNowPlaying(c, usePlayer.getState().now?.zapList));
      poke();
    },
    [poke],
  );
  const zap = useCallback(
    (dir: number) => {
      if (!zapList.length) return;
      const i = zapIndex < 0 ? 0 : (zapIndex + dir + zapList.length) % zapList.length;
      zapTo(zapList[i]);
    },
    [zapList, zapIndex, zapTo],
  );

  // ---- recording (live TV, T-035)
  const recordingFile = p.recording;
  const lastRecording = useRef("");
  const [recordingSince, setRecordingSince] = useState(0);
  useEffect(() => {
    if (recordingFile && !lastRecording.current) setRecordingSince(Date.now());
    // ended by the user, a channel change or a failed stream: say where it went
    if (!recordingFile && lastRecording.current) say(`Recording saved: ${lastRecording.current.split("/").pop()}`);
    lastRecording.current = recordingFile;
  }, [recordingFile, say]);
  const toggleRecord = useCallback(() => {
    const on = !usePlayer.getState().props.recording;
    api.record(on).catch((e) => say(`Can't record: ${String(e)}`));
  }, [say]);

  // ---- keyboard
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.target as HTMLElement)?.tagName === "INPUT") return;
      const k = e.key;
      let handled = true;
      if (k === " " || k === "k" || k === "K") togglePause();
      else if (k === "ArrowLeft" && !live) seekBy(e.shiftKey ? -60 : -10);
      else if (k === "ArrowRight" && !live) seekBy(e.shiftKey ? 60 : 30);
      else if ((k === "ArrowUp" && live) || k === "PageUp") zap(-1);
      else if ((k === "ArrowDown" && live) || k === "PageDown") zap(1);
      else if (k === "ArrowUp" && !live) nudgeVolume(5);
      else if (k === "ArrowDown" && !live) nudgeVolume(-5);
      else if (k === "f" || k === "F") void toggleFullscreen().then(() => isFullscreen().then(setFullscreen));
      else if (k === "m" || k === "M") toggleMute();
      else if (k === "Escape") {
        if (panel) setPanel(null);
        else void exit();
      } else if (k === "a" || k === "A") void api.command("cycle", "audio");
      else if (k === "s" || k === "S") {
        const on = !usePlayer.getState().props.subVisible;
        setSubsEnabled(on);
        say(on ? "Subtitles on" : "Subtitles off");
      } else if (k === "i" || k === "I") setInfo((v) => !v);
      else if ((k === "r" || k === "R") && live) toggleRecord();
      else if (k === "p" || k === "P") void toPip();
      else if ((k === "l" || k === "L" || k === "c" || k === "C") && live) setPanel((v) => (v === "channels" ? null : "channels"));
      else if (/^[0-9]$/.test(k) && live) {
        const next = (digits + k).slice(-4);
        setDigits(next);
        window.clearTimeout(digitTimer.current);
        digitTimer.current = window.setTimeout(() => {
          const target = zapList.find((c) => String(c.num) === next);
          if (target) zapTo(target);
          setDigits("");
        }, 1400);
      } else handled = false;
      if (handled) {
        e.preventDefault();
        poke();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [togglePause, seekBy, zap, zapList, zapTo, exit, panel, digits, live, poke, toggleRecord, toPip, nudgeVolume, toggleMute]);

  if (!now) return null;
  const busy = status === "loading" || status === "reconnecting" || (p.buffering && status === "playing");

  return (
    <div
      className={clsx("fixed inset-0 select-none", !visible && "cursor-none")}
      onMouseMove={poke}
      onClick={(e) => {
        if (e.target === e.currentTarget) {
          if (panel) setPanel(null);
          else togglePause();
        }
      }}
      onDoubleClick={(e) => {
        if (e.target === e.currentTarget) void toggleFullscreen().then(() => isFullscreen().then(setFullscreen));
      }}
    >
      {/* center state */}
      <div className="pointer-events-none absolute inset-0 grid place-items-center">
        {busy && (
          <div className="flex flex-col items-center gap-3 rounded-2xl bg-black/50 px-6 py-5 backdrop-blur-md">
            <Loader2 className="size-9 animate-spin text-white" />
            <span className="text-sm font-medium text-white/80">
              {status === "reconnecting" ? `Reconnecting… (attempt ${reconnectAttempt})` : status === "loading" ? (live ? "Tuning…" : "Loading…") : "Buffering…"}
            </span>
          </div>
        )}
        {flash && (
          <div className="grid size-20 place-items-center rounded-full bg-black/50 text-white backdrop-blur animate-fade-in">
            {flash === "play" ? <Play className="size-9 translate-x-0.5 fill-current" /> : <Pause className="size-9 fill-current" />}
          </div>
        )}
        {digits && <div className="absolute right-10 top-24 rounded-2xl bg-black/60 px-5 py-3 text-4xl font-bold tabular-nums text-white backdrop-blur">{digits}</div>}
      </div>

      {status === "error" && (
        <div className="absolute inset-0 grid place-items-center bg-black/60">
          <div className="flex max-w-md flex-col items-center gap-4 rounded-3xl bg-panel/95 p-8 text-center ring-1 ring-white/10">
            <h2 className="text-xl font-bold">Can’t play this stream</h2>
            <p className="text-sm text-dim">{error}</p>
            <div className="flex gap-3">
              <Button variant="primary" icon={<RotateCw className="size-4" />} onClick={() => void usePlayer.getState().play(now)}>
                Try again
              </Button>
              <Button onClick={() => void exit()}>Back</Button>
            </div>
          </div>
        </div>
      )}

      {/* VOD files are opened with keep-open: at the end mpv pauses on the
          last frame and sets eof-reached instead of ending the file */}
      {(status === "ended" || p.eof) && now.kind === "episode" && <NextEpisode />}

      {/* top bar */}
      <div
        className={clsx(
          "absolute inset-x-0 top-0 flex items-start gap-4 bg-gradient-to-b from-black/80 via-black/40 to-transparent px-8 pb-16 pt-6 transition-opacity duration-300",
          visible ? "opacity-100" : "pointer-events-none opacity-0",
        )}
      >
        <IconButton label="Back" variant="glass" onClick={() => void exit()}>
          <ChevronLeft className="size-5" />
        </IconButton>
        {live && now.image && <ChannelLogo src={now.image} title={now.title} size={44} className="size-11 bg-black/30" />}
        <div className="min-w-0 flex-1 pt-0.5">
          <h1 className="truncate text-xl font-bold drop-shadow">{now.title}</h1>
          {now.subtitle && <p className="truncate text-sm text-white/70">{now.subtitle}</p>}
        </div>
        <div className="flex items-center gap-2">
          {recordingFile && <RecordingBadge since={recordingSince} />}
          <TechBadges />
          <Clock />
        </div>
      </div>
      {/* the REC light stays on when the controls hide */}
      {recordingFile && !visible && (
        <div className="pointer-events-none absolute right-8 top-7">
          <RecordingBadge since={recordingSince} />
        </div>
      )}

      {info && <StatsOverlay />}
      {notice && (
        <div className="pointer-events-none absolute left-1/2 top-24 -translate-x-1/2 rounded-xl bg-black/70 px-4 py-2 text-sm text-white backdrop-blur animate-fade-in">
          {notice}
        </div>
      )}

      {/* bottom bar */}
      <div
        className={clsx(
          "absolute inset-x-0 bottom-0 bg-gradient-to-t from-black/85 via-black/50 to-transparent px-8 pb-6 pt-24 transition-opacity duration-300",
          visible ? "opacity-100" : "pointer-events-none opacity-0",
        )}
      >
        {live ? <LiveInfo channel={now.channel} /> : vod ? <SeekBar onSeek={poke} /> : null}
        <div className="mt-4 flex items-center gap-2">
          <IconButton label={p.pause ? "Play" : "Pause"} variant="solid" size="lg" onClick={togglePause}>
            {p.pause ? <Play className="size-6 translate-x-0.5 fill-current" /> : <Pause className="size-6 fill-current" />}
          </IconButton>
          {vod && (
            <>
              <IconButton label="Back 10 seconds" onClick={() => seekBy(-10)} className="text-white">
                <RotateCcw className="size-5" />
              </IconButton>
              <IconButton label="Forward 30 seconds" onClick={() => seekBy(30)} className="text-white">
                <RotateCw className="size-5" />
              </IconButton>
            </>
          )}
          {live && zapList.length > 1 && (
            <>
              <IconButton label="Previous channel" onClick={() => zap(-1)} className="text-white">
                <ChevronUp className="size-5" />
              </IconButton>
              <IconButton label="Next channel" onClick={() => zap(1)} className="text-white">
                <ChevronDown className="size-5" />
              </IconButton>
            </>
          )}
          {live && (
            <IconButton
              label={recordingFile ? "Stop recording (R)" : "Record (R)"}
              onClick={toggleRecord}
              className={clsx("text-white", recordingFile && "bg-live/25")}
            >
              <Circle className={clsx("size-5", recordingFile && "fill-live text-live")} />
            </IconButton>
          )}
          {live && liveGap !== null && (
            <BehindLive
              gap={liveGap}
              onLive={() => {
                setLiveGap(null);
                const cur = usePlayer.getState().now;
                if (cur) void usePlayer.getState().play(cur);
              }}
            />
          )}
          <div className="flex-1" />
          <Volume />
          <MenuButton label="Audio" icon={<AudioLines className="size-5" />} open={panel === "audio"} onToggle={() => setPanel(panel === "audio" ? null : "audio")}>
            <TrackMenu type="audio" onDone={() => setPanel(null)} />
          </MenuButton>
          <MenuButton label="Subtitles" icon={<Subtitles className="size-5" />} open={panel === "subs"} onToggle={() => setPanel(panel === "subs" ? null : "subs")}>
            <TrackMenu type="sub" onDone={() => setPanel(null)} />
          </MenuButton>
          <MenuButton label="Picture" icon={<RectangleHorizontal className="size-5" />} open={panel === "aspect"} onToggle={() => setPanel(panel === "aspect" ? null : "aspect")}>
            <AspectMenu onDone={() => setPanel(null)} />
          </MenuButton>
          {vod && (
            <MenuButton label="Speed" icon={<Gauge className="size-5" />} open={panel === "speed"} onToggle={() => setPanel(panel === "speed" ? null : "speed")}>
              <SpeedMenu onDone={() => setPanel(null)} />
            </MenuButton>
          )}
          {live && (
            <IconButton label="Channels (L)" active={panel === "channels"} onClick={() => setPanel(panel === "channels" ? null : "channels")} className="text-white">
              <List className="size-5" />
            </IconButton>
          )}
          <IconButton label="Picture in picture (P)" onClick={() => void toPip()} className="text-white">
            <PictureInPicture2 className="size-5" />
          </IconButton>
          <IconButton label="Info (I)" active={info} onClick={() => setInfo((v) => !v)} className="text-white">
            <Info className="size-5" />
          </IconButton>
          <IconButton
            label={fullscreen ? "Exit fullscreen (F)" : "Fullscreen (F)"}
            onClick={() => void toggleFullscreen().then(() => isFullscreen().then(setFullscreen))}
            className="text-white"
          >
            {fullscreen ? <Minimize className="size-5" /> : <Maximize className="size-5" />}
          </IconButton>
        </div>
      </div>

      {live && panel === "channels" && <ChannelPanel channels={zapList} current={now.id} onPick={zapTo} />}
    </div>
  );
}

// ------------------------------------------------------------------ parts

function Clock() {
  const [t, setT] = useState(() => new Date());
  useEffect(() => {
    const i = window.setInterval(() => setT(new Date()), 15_000);
    return () => window.clearInterval(i);
  }, []);
  return <span className="text-lg font-semibold tabular-nums text-white/85 drop-shadow">{t.toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" })}</span>;
}

function RecordingBadge({ since }: { since: number }) {
  const [, tick] = useState(0);
  useEffect(() => {
    const i = window.setInterval(() => tick((x) => x + 1), 1000);
    return () => window.clearInterval(i);
  }, []);
  return (
    <span className="mr-1 inline-flex items-center gap-1.5 rounded-md bg-live px-2 py-0.5 text-[11px] font-bold uppercase tracking-wide text-white">
      <span className="size-1.5 animate-pulse rounded-full bg-white" />
      Rec {clock((Date.now() - since) / 1000)}
    </span>
  );
}

function TechBadges() {
  const p = usePlayer((s) => s.props);
  const res = p.videoH >= 2000 ? "4K" : p.videoH >= 1000 ? "1080p" : p.videoH >= 700 ? "720p" : p.videoH > 0 ? `${p.videoH}p` : null;
  const audio = p.tracks.find((t) => t.type === "audio" && t.selected);
  const ch = audio?.["demux-channel-count"];
  return (
    <div className="mr-2 hidden items-center gap-1.5 md:flex">
      {res && <Badge className="bg-white/15 text-white">{res}</Badge>}
      {p.fps > 45 && <Badge className="bg-white/15 text-white">{Math.round(p.fps)}fps</Badge>}
      {ch && ch > 2 && <Badge className="bg-white/15 text-white">{ch === 6 ? "5.1" : ch === 8 ? "7.1" : `${ch}ch`}</Badge>}
    </div>
  );
}

function SeekBar({ onSeek }: { onSeek: () => void }) {
  const p = usePlayer((s) => s.props);
  const bar = useRef<HTMLDivElement>(null);
  const [hover, setHover] = useState<number | null>(null);
  const [drag, setDrag] = useState<number | null>(null);
  const dur = p.duration > 0 ? p.duration : 0;
  const pos = drag ?? p.timePos;
  const frac = (clientX: number) => {
    const r = bar.current!.getBoundingClientRect();
    return Math.min(1, Math.max(0, (clientX - r.left) / r.width));
  };
  const commit = (f: number) => {
    void api.command("seek", (f * dur).toFixed(2), "absolute");
    onSeek();
  };
  return (
    <div className="flex items-center gap-4">
      <span className="w-16 text-right text-sm font-semibold tabular-nums text-white/90">{clock(pos)}</span>
      <div
        ref={bar}
        className="group relative h-6 flex-1 cursor-pointer"
        onPointerMove={(e) => {
          if (!dur) return;
          const f = frac(e.clientX);
          setHover(f);
          if (drag !== null) setDrag(f * dur);
        }}
        onPointerLeave={() => setHover(null)}
        onPointerDown={(e) => {
          if (!dur) return;
          e.currentTarget.setPointerCapture(e.pointerId);
          setDrag(frac(e.clientX) * dur);
        }}
        onPointerUp={(e) => {
          if (drag !== null) commit(frac(e.clientX));
          setDrag(null);
        }}
      >
        <div className="absolute inset-x-0 top-1/2 h-1 -translate-y-1/2 rounded-full bg-white/20 transition-[height] group-hover:h-1.5">
          {dur > 0 && (
            <>
              <div className="absolute inset-y-0 left-0 rounded-full bg-white/30" style={{ width: `${Math.min(100, (p.cacheTime / dur) * 100)}%` }} />
              <div className="absolute inset-y-0 left-0 rounded-full bg-accent" style={{ width: `${Math.min(100, (pos / dur) * 100)}%` }} />
            </>
          )}
          {p.chapters.map((c) => (
            <span key={c.time} className="absolute top-0 h-full w-0.5 bg-black/60" style={{ left: `${(c.time / (dur || 1)) * 100}%` }} />
          ))}
        </div>
        {dur > 0 && (
          <div
            className="absolute top-1/2 size-3.5 -translate-x-1/2 -translate-y-1/2 rounded-full bg-white shadow-lg opacity-0 transition-opacity group-hover:opacity-100"
            style={{ left: `${Math.min(100, (pos / dur) * 100)}%`, opacity: drag !== null ? 1 : undefined }}
          />
        )}
        {hover !== null && dur > 0 && (
          <div className="absolute -top-8 -translate-x-1/2 rounded-md bg-black/80 px-2 py-1 text-xs font-semibold tabular-nums text-white" style={{ left: `${hover * 100}%` }}>
            {clock(hover * dur)}
          </div>
        )}
      </div>
      <span className="w-16 text-sm font-semibold tabular-nums text-white/60">{dur ? `-${clock(dur - pos)}` : "--:--"}</span>
    </div>
  );
}

function LiveInfo({ channel }: { channel?: Channel }) {
  const t = nowUnix();
  const epg = useQuery({
    queryKey: ["epg", channel?.sourceId, channel?.id],
    queryFn: () => api.epg(channel!.sourceId, channel!.id, t - 3600, t + 6 * 3600),
    enabled: !!channel,
    refetchInterval: 5 * 60_000,
  });
  const [, tick] = useState(0);
  useEffect(() => {
    const i = window.setInterval(() => tick((x) => x + 1), 30_000);
    return () => window.clearInterval(i);
  }, []);
  const list = epg.data ?? [];
  const cur = list.find((p) => p.start <= nowUnix() && p.stop > nowUnix());
  const next = list.find((p) => p.start >= (cur?.stop ?? nowUnix()));
  return (
    <div className="flex items-end gap-6">
      <div className="min-w-0 flex-1">
        <div className="mb-1.5 flex items-center gap-2">
          <span className="inline-flex items-center gap-1.5 rounded-md bg-live px-2 py-0.5 text-[11px] font-bold uppercase tracking-wide text-white">
            <span className="size-1.5 rounded-full bg-white" /> Live
          </span>
          {cur && (
            <span className="text-sm tabular-nums text-white/70">
              {hhmm(cur.start)} – {hhmm(cur.stop)}
            </span>
          )}
        </div>
        <div className="truncate text-lg font-semibold text-white">{cur?.title ?? channel?.title ?? "Live"}</div>
        {cur && <ProgressBar value={elapsed(cur.start, cur.stop)} tone="white" className="mt-2 max-w-3xl bg-white/20" />}
      </div>
      {next && (
        <div className="hidden min-w-0 max-w-sm text-right lg:block">
          <div className="text-xs font-semibold uppercase tracking-wider text-white/50">Next · {hhmm(next.start)}</div>
          <div className="truncate text-sm font-medium text-white/80">{next.title}</div>
        </div>
      )}
    </div>
  );
}

/** Shown after pausing live TV: how far behind we are, and a way back. */
function BehindLive({ gap, onLive }: { gap: number; onLive: () => void }) {
  const p = usePlayer((s) => s.props);
  const behind = Math.max(0, p.cacheTime - p.timePos - gap);
  return (
    <Button size="sm" variant="glass" className="ml-2" icon={<SkipForward className="size-4" />} onClick={onLive}>
      {behind >= 1 ? `${clock(behind)} behind · Go live` : "Go live"}
    </Button>
  );
}

function Volume() {
  const volume = usePlayer((s) => s.props.volume);
  const mute = usePlayer((s) => s.props.mute);
  const Icon = mute || volume === 0 ? VolumeX : volume < 50 ? Volume1 : Volume2;
  return (
    <div className="group/vol flex items-center">
      <IconButton label={mute ? "Unmute (M)" : "Mute (M)"} onClick={() => void api.set("mute", !mute)} className="text-white">
        <Icon className="size-5" />
      </IconButton>
      <input
        type="range"
        min={0}
        max={130}
        step={1}
        value={mute ? 0 : Math.round(volume)}
        onChange={(e) => {
          void api.set("volume", Number(e.target.value));
          if (mute) void api.set("mute", false);
        }}
        className="h-1 w-0 cursor-pointer accent-white opacity-0 transition-all duration-200 group-hover/vol:w-24 group-hover/vol:opacity-100"
        aria-label="Volume"
      />
    </div>
  );
}

function MenuButton({ label, icon, open, onToggle, children }: { label: string; icon: ReactNode; open: boolean; onToggle: () => void; children: ReactNode }) {
  return (
    <div className="relative">
      <IconButton label={label} active={open} onClick={onToggle} className="text-white">
        {icon}
      </IconButton>
      {open && (
        <div className="absolute bottom-12 right-0 z-10 max-h-[50vh] min-w-60 overflow-y-auto rounded-2xl bg-panel/95 p-1.5 shadow-2xl ring-1 ring-white/10 backdrop-blur-xl animate-fade-in">
          <div className="px-3 pb-1 pt-2 text-[11px] font-bold uppercase tracking-wider text-faint">{label}</div>
          {children}
        </div>
      )}
    </div>
  );
}

function MenuItem({ active, onClick, children, hint }: { active: boolean; onClick: () => void; children: ReactNode; hint?: ReactNode }) {
  return (
    <button
      onClick={onClick}
      className={clsx("flex w-full items-center gap-3 rounded-xl px-3 py-2 text-left text-sm transition-colors", active ? "bg-accent/20 text-fg" : "text-dim hover:bg-white/[0.06] hover:text-fg")}
    >
      <span className={clsx("size-1.5 shrink-0 rounded-full", active ? "bg-accent-strong" : "bg-transparent")} />
      <span className="min-w-0 flex-1 truncate">{children}</span>
      {hint && <span className="shrink-0 text-xs text-faint">{hint}</span>}
    </button>
  );
}

const LANGS: Record<string, string> = {
  eng: "English", en: "English", dan: "Danish", da: "Danish", swe: "Swedish", sv: "Swedish", nor: "Norwegian", nob: "Norwegian",
  no: "Norwegian", fin: "Finnish", fi: "Finnish", ger: "German", deu: "German", de: "German", fre: "French", fra: "French",
  fr: "French", spa: "Spanish", es: "Spanish", ita: "Italian", it: "Italian", dut: "Dutch", nld: "Dutch", nl: "Dutch",
  pol: "Polish", pl: "Polish", por: "Portuguese", pt: "Portuguese", tur: "Turkish", tr: "Turkish", ara: "Arabic", ar: "Arabic",
  rus: "Russian", ru: "Russian", jpn: "Japanese", ja: "Japanese", kor: "Korean", ko: "Korean", chi: "Chinese", zho: "Chinese",
  hin: "Hindi", hun: "Hungarian", ces: "Czech", cze: "Czech", gre: "Greek", ell: "Greek", heb: "Hebrew", ice: "Icelandic",
};

function trackLabel(t: Track): string {
  const lang = t.lang ? (LANGS[t.lang.toLowerCase()] ?? t.lang.toUpperCase()) : null;
  const title = t.title && t.title !== lang ? t.title : null;
  return [lang, title].filter(Boolean).join(" · ") || `Track ${t.id}`;
}

function TrackMenu({ type, onDone }: { type: "audio" | "sub"; onDone: () => void }) {
  const tracks = usePlayer((s) => s.props.tracks).filter((t) => t.type === type);
  const current = usePlayer((s) => (type === "audio" ? s.props.aid : s.props.sid));
  const subVisible = usePlayer((s) => s.props.subVisible);
  const pick = (v: number | "no") => {
    void api.set(type === "audio" ? "aid" : "sid", v);
    // picking a track while subtitles are hidden must show it (PL-100);
    // "Off" persists the disabling so it survives the next session
    if (type === "sub") setSubsEnabled(v !== "no");
    onDone();
  };
  return (
    <div className="flex flex-col">
      {type === "sub" && (
        <MenuItem active={subVisible} onClick={() => { setSubsEnabled(!subVisible); onDone(); }} hint="S">
          Subtitles on/off
        </MenuItem>
      )}
      {type === "sub" && <div className="mx-3 my-1 border-t border-white/[0.06]" />}
      {type === "sub" && (
        <MenuItem active={current === false || current === null} onClick={() => pick("no")}>
          Off
        </MenuItem>
      )}
      {tracks.length === 0 && <div className="px-3 py-2 text-sm text-faint">No {type === "audio" ? "audio" : "subtitle"} tracks</div>}
      {tracks.map((t) => (
        <MenuItem
          key={t.id}
          active={current === t.id}
          onClick={() => pick(t.id)}
          hint={[t.codec?.toUpperCase(), t["demux-channel-count"] ? `${t["demux-channel-count"]}ch` : null].filter(Boolean).join(" ")}
        >
          {trackLabel(t)}
        </MenuItem>
      ))}
    </div>
  );
}

function AspectMenu({ onDone }: { onDone: () => void }) {
  const aspect = usePlayer((s) => s.props.aspect);
  const panscan = usePlayer((s) => s.props.panscan);
  const options: { label: string; aspect: string; panscan: number }[] = [
    { label: "Original", aspect: "no", panscan: 0 },
    { label: "Fill screen (crop)", aspect: "no", panscan: 1 },
    { label: "16:9", aspect: "16:9", panscan: 0 },
    { label: "4:3", aspect: "4:3", panscan: 0 },
    { label: "21:9", aspect: "2.35:1", panscan: 0 },
  ];
  return (
    <div className="flex flex-col">
      {options.map((o) => (
        <MenuItem
          key={o.label}
          active={(aspect === o.aspect || (o.aspect === "no" && (aspect === "-1" || aspect === "no"))) && Math.abs(panscan - o.panscan) < 0.01}
          onClick={() => {
            void api.set("video-aspect-override", o.aspect);
            void api.set("panscan", o.panscan);
            onDone();
          }}
        >
          {o.label}
        </MenuItem>
      ))}
    </div>
  );
}

function SpeedMenu({ onDone }: { onDone: () => void }) {
  const speed = usePlayer((s) => s.props.speed);
  return (
    <div className="flex flex-col">
      {[0.5, 0.75, 1, 1.25, 1.5, 2].map((v) => (
        <MenuItem
          key={v}
          active={Math.abs(speed - v) < 0.01}
          onClick={() => {
            void api.set("speed", v);
            onDone();
          }}
        >
          {v === 1 ? "Normal" : `${v}×`}
        </MenuItem>
      ))}
    </div>
  );
}

function ChannelPanel({ channels, current, onPick }: { channels: Channel[]; current: string; onPick: (c: Channel) => void }) {
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    ref.current?.querySelector("[data-current=true]")?.scrollIntoView({ block: "center" });
  }, []);
  return (
    <div className="absolute bottom-0 right-0 top-0 flex w-[380px] flex-col bg-black/75 backdrop-blur-2xl animate-fade-in" onClick={(e) => e.stopPropagation()}>
      <div className="px-5 pb-3 pt-6 text-sm font-bold uppercase tracking-wider text-white/60">Channels</div>
      <div ref={ref} className="flex-1 overflow-y-auto px-2 pb-6">
        {channels.map((c) => (
          <button
            key={`${c.sourceId}-${c.id}`}
            data-current={c.id === current}
            onClick={() => onPick(c)}
            className={clsx("flex w-full items-center gap-3 rounded-xl px-3 py-2 text-left transition-colors", c.id === current ? "bg-white/15" : "hover:bg-white/[0.07]")}
          >
            <span className="w-8 text-right text-xs tabular-nums text-white/40">{c.num ?? ""}</span>
            <ChannelLogo src={c.logo} title={c.title} size={36} className="size-9" />
            <span className="min-w-0 flex-1">
              <span className="flex items-center gap-1.5 truncate text-sm font-semibold text-white">
                {c.id === current && <LiveDot />}
                {c.title}
              </span>
              <span className="block truncate text-xs text-white/50">{c.now?.title ?? ""}</span>
            </span>
          </button>
        ))}
      </div>
    </div>
  );
}

function StatsOverlay() {
  const p = usePlayer((s) => s.props);
  const rows: [string, string][] = [
    ["Video", `${p.videoCodec || "—"}${p.videoW ? ` · ${p.videoW}×${p.videoH}` : ""}${p.fps ? ` · ${p.fps.toFixed(2)} fps` : ""}`],
    ["Decoder", p.hwdec && p.hwdec !== "no" ? `hardware (${p.hwdec})` : "software"],
    ["Audio", p.audioCodec || "—"],
    ["Buffer", p.cacheTime > p.timePos ? `${(p.cacheTime - p.timePos).toFixed(1)} s` : "—"],
    ["Position", `${clock(p.timePos)}${p.duration ? ` / ${clock(p.duration)}` : ""}`],
  ];
  return (
    <div className="absolute left-8 top-24 rounded-2xl bg-black/70 p-4 font-mono text-xs text-white/85 ring-1 ring-white/10 backdrop-blur-xl">
      {rows.map(([k, v]) => (
        <div key={k} className="flex gap-4 py-0.5">
          <span className="w-16 text-white/45">{k}</span>
          <span>{v}</span>
        </div>
      ))}
    </div>
  );
}

/** At the end of an episode: count down and continue with the next one. */
function NextEpisode() {
  const now = usePlayer((s) => s.now);
  const detail = useQuery({
    queryKey: ["series-detail", now?.sourceId, now?.seriesId],
    queryFn: () => api.seriesDetail(now!.sourceId, now!.seriesId!),
    enabled: !!now?.seriesId,
  });
  const next: Episode | undefined = useMemo(() => {
    const eps = (detail.data?.seasons ?? []).filter((s) => s.season > 0).flatMap((s) => s.episodes);
    // the list merges every copy of the show: find ours by its number
    let i = eps.findIndex((e) => e.season === now?.season && e.episode === now?.episode);
    if (i < 0) i = eps.findIndex((e) => e.id === now?.id);
    return i >= 0 ? eps[i + 1] : undefined;
  }, [detail.data, now?.id, now?.season, now?.episode]);
  const [left, setLeft] = useState(10);
  const [cancelled, setCancelled] = useState(false);
  useEffect(() => {
    if (!next || cancelled) return;
    const i = window.setInterval(() => setLeft((v) => v - 1), 1000);
    return () => window.clearInterval(i);
  }, [next, cancelled]);
  useEffect(() => {
    if (next && !cancelled && left <= 0 && detail.data) void playEpisode(detail.data, next, undefined, true);
  }, [left, next, cancelled, detail.data]);
  if (!next || !detail.data) return null;
  return (
    <div className="absolute bottom-32 right-8 flex w-[360px] flex-col gap-3 rounded-2xl bg-panel/95 p-4 ring-1 ring-white/10 backdrop-blur-xl animate-rise-in">
      <div className="text-xs font-bold uppercase tracking-wider text-accent-strong">{cancelled ? "Up next" : `Up next in ${Math.max(0, left)}s`}</div>
      <div className="text-[15px] font-semibold">
        S{next.season} E{next.episode} · {next.title}
      </div>
      <div className="flex gap-2">
        <Button variant="primary" size="sm" icon={<Play className="size-4 fill-current" />} onClick={() => void playEpisode(detail.data!, next, undefined, true)}>
          Play now
        </Button>
        {!cancelled && (
          <Button size="sm" onClick={() => setCancelled(true)}>
            Cancel
          </Button>
        )}
      </div>
    </div>
  );
}
