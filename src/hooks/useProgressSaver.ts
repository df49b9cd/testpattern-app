import { useEffect } from "react";
import { saveProgress, usePlayer } from "../stores/player";

/** Persists VOD resume positions every 10 s while something plays. */
export function useProgressSaver() {
  useEffect(() => {
    const t = window.setInterval(() => {
      const { now, status, props } = usePlayer.getState();
      if (now && status === "playing" && !props.pause) void saveProgress(now, props);
    }, 10_000);
    return () => window.clearInterval(t);
  }, []);
}
