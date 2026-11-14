import { create } from "zustand";
import type { SyncProgress } from "../lib/types";

interface SyncState {
  running: boolean;
  message: string;
  error: string | null;
  finishedAt: number | null;
}

interface SyncStore {
  bySource: Record<number, SyncState>;
  apply: (p: SyncProgress) => void;
}

const idle: SyncState = { running: false, message: "", error: null, finishedAt: null };

export const useSync = create<SyncStore>((set) => ({
  bySource: {},
  apply: (p) =>
    set((s) => {
      const cur = s.bySource[p.sourceId] ?? idle;
      let next: SyncState = cur;
      switch (p.stage) {
        case "started":
          next = { running: true, message: "Starting…", error: null, finishedAt: null };
          break;
        case "step":
          next = { ...cur, running: true, message: p.message };
          break;
        case "done":
          next = { running: false, message: "Up to date", error: null, finishedAt: Date.now() };
          break;
        case "failed":
          next = { running: false, message: "", error: p.error, finishedAt: Date.now() };
          break;
      }
      return { bySource: { ...s.bySource, [p.sourceId]: next } };
    }),
}));

/** A sync in progress (any source), for the sidebar's status line; undefined when none. */
export const runningSync = (s: SyncStore): SyncState | undefined => Object.values(s.bySource).find((v) => v.running);
