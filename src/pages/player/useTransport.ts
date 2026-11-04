import { useCallback, useEffect, useState } from "react";
import { useTimeout } from "../../hooks/useTimeout";
import { control, maxVolume, usePlayer } from "../../stores/player";

/** Seek steps (seconds): back, forward, and either way with Shift. */
export const SEEK_BACK = 10;
export const SEEK_FWD = 30;
export const SEEK_BIG = 60;
/** Volume change per arrow key (percent). */
export const VOLUME_STEP = 5;
/** How long the play/pause symbol flashes (ms). */
const FLASH_DURATION = 500;
/** mpv applies volume async: read it back this much later (ms). */
const VOLUME_READBACK = 60;

/**
 * Play/pause, seeking, volume and mute from the player chrome and keys, with
 * the feedback they give (`say`, the play/pause flash).
 */
export function useTransport(say: (text: string) => void, poke: () => void) {
  const [flash, setFlash] = useState<null | "play" | "pause">(null);
  const flashTimer = useTimeout();
  const volumeTimer = useTimeout();

  // Live TV can be paused (mpv keeps buffering). The normal read-ahead gap
  // between playback and buffer end is noted at pause time, so "behind" only
  // counts real delay.
  const nowId = usePlayer((s) => s.now?.id);
  const [liveGap, setLiveGap] = useState<number | null>(null);
  useEffect(() => setLiveGap(null), [nowId]);

  const togglePause = useCallback(() => {
    const { props: cur, now: np } = usePlayer.getState();
    const paused = cur.pause;
    if (!paused && np?.kind === "live" && !usePlayer.getState().cast) setLiveGap((g) => g ?? Math.max(0, cur.cacheTime - cur.timePos));
    void control.setPause(!paused);
    setFlash(paused ? "play" : "pause");
    flashTimer.start(() => setFlash(null), FLASH_DURATION);
  }, [flashTimer]);

  /** Back to the live edge: retunes the channel. */
  const goLive = useCallback(() => {
    setLiveGap(null);
    const cur = usePlayer.getState().now;
    if (cur) void usePlayer.getState().play(cur);
  }, []);

  const seekBy = useCallback(
    (secs: number) => {
      void control.seekBy(secs);
      say(secs < 0 ? `« ${secs} s` : `» +${secs} s`);
      poke();
    },
    [poke, say],
  );

  const nudgeVolume = useCallback(
    (delta: number) => {
      const st = usePlayer.getState();
      const target = Math.max(0, Math.min(maxVolume(), st.props.volume + delta));
      void control.setVolume(target).then((ok) => {
        if (!ok) return;
        if (st.cast) {
          say(`TV volume ${Math.round(target)}`);
          return;
        }
        // mpv applies async; read what it has a moment later so the chip shows
        // the real level, not the pre-clamp guess.
        volumeTimer.start(() => {
          void control.readVolume().then((v) => v !== null && say(`Volume ${Math.round(v)}`));
        }, VOLUME_READBACK);
      });
      poke();
    },
    [say, poke, volumeTimer],
  );

  const toggleMute = useCallback(() => {
    const st = usePlayer.getState();
    void control.setMute(!st.props.mute).then((ok) => ok && say(st.props.mute ? "Sound on" : "Muted"));
    poke();
  }, [say, poke]);

  return { flash, liveGap, goLive, togglePause, seekBy, nudgeVolume, toggleMute };
}
