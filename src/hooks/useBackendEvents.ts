import { useEffect } from "react";
import { api } from "../lib/api";
import { listen } from "../lib/bridge";
import { useQueryClient } from "@tanstack/react-query";
import type { PlayerEvent, SyncProgress } from "../lib/types";
import { usePlayer } from "../stores/player";
import { useSync } from "../stores/sync";

/** Subscribes once (app root) to backend events and fans them out. */
export function useBackendEvents() {
  const qc = useQueryClient();
  useEffect(() => {
    const unlisteners = [
      listen<PlayerEvent>("player://event", (e) => usePlayer.getState().onEvent(e.payload)),
      // mpv applied the saved volume before the UI was listening: start in sync
      Promise.all([api.get<number>("volume"), api.get<boolean>("mute")])
        .then(([volume, mute]) =>
          usePlayer.setState((s) => ({ props: { ...s.props, volume: volume ?? s.props.volume, mute: mute ?? s.props.mute } })),
        )
        .catch(() => {})
        .then(() => () => {}),
      listen<SyncProgress>("sync://progress", (e) => {
        useSync.getState().apply(e.payload);
        if (e.payload.stage === "done" || e.payload.stage === "failed") {
          // catalog changed underneath every cached query
          void qc.invalidateQueries();
        }
      }),
    ];
    return () => {
      for (const u of unlisteners) void u.then((f) => f());
    };
  }, [qc]);
}
