import { useEffect } from "react";
import { api } from "../lib/api";
import { listen } from "../lib/bridge";
import { backgroundWrite } from "../lib/backgroundWrite";
import { useQueryClient } from "@tanstack/react-query";
import type { CastStatus, PlayerEvent, SyncProgress, TmdbStatus } from "../lib/types";
import { syncLocalVolume, usePlayer } from "../stores/player";
import { useSync } from "../stores/sync";

/** Subscribes once (app root) to backend events and fans them out. */
export function useBackendEvents() {
  const qc = useQueryClient();
  useEffect(() => {
    const unlisteners = [
      listen<PlayerEvent>("player://event", (e) => usePlayer.getState().onEvent(e.payload)),
      listen<CastStatus>("cast://status", (e) => usePlayer.getState().onCastStatus(e.payload)),
      // a cast that outlived a reload of the UI: show it, so it can be stopped
      backgroundWrite(
        api.castStatus().then((s) => {
          if (s && !usePlayer.getState().cast) usePlayer.setState({ cast: s });
        }),
      ).then(() => () => {}),
      // mpv applied the saved volume before the UI was listening: start in sync
      syncLocalVolume().then(() => () => {}),
      listen<TmdbStatus>("tmdb://progress", (e) => {
        const before = qc.getQueryData<TmdbStatus>(["tmdb-status"]);
        qc.setQueryData(["tmdb-status"], e.payload);
        // regrouped with the new details: genres, languages, collections
        if (!e.payload.running || (before && e.payload.known - before.known >= 4000)) {
          void qc.invalidateQueries({ queryKey: ["movies"] });
          void qc.invalidateQueries({ queryKey: ["series"] });
        }
      }),
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
