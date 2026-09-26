import { beforeEach, describe, expect, it, vi } from "vitest";

const calls = vi.hoisted(() => ({ updateHistory: [] as unknown[] }));
vi.mock("../lib/api", () => ({
  api: {
    updateHistory: vi.fn(async (entry: unknown) => void calls.updateHistory.push(entry)),
    setSetting: vi.fn(async () => {}),
  },
  errorMessage: (e: unknown) => String(e),
}));

import { applyProp, saveProgress, trackId, usePlayer, type NowPlaying } from "./player";

const props = () => usePlayer.getState().props;

describe("applyProp", () => {
  it("maps mpv properties to store fields", () => {
    expect(applyProp(props(), "time-pos", 12.5)).toEqual({ timePos: 12.5 });
    expect(applyProp(props(), "pause", true)).toEqual({ pause: true });
    expect(applyProp(props(), "video-params", { w: 1920, h: 1080, pixelformat: "yuv420p" })).toEqual({ videoW: 1920, videoH: 1080 });
    expect(applyProp(props(), "video-aspect-override", "")).toEqual({ aspect: "no" });
    expect(applyProp(props(), "track-list", "garbage")).toEqual({ tracks: [] });
    expect(applyProp(props(), "stream-record", "/v/BBC One.ts")).toEqual({ recording: "/v/BBC One.ts" });
    expect(applyProp(props(), "stream-record", null)).toEqual({ recording: "" });
  });
  it("keeps the last position while mpv reports none", () => {
    const p = { ...props(), timePos: 42 };
    expect(applyProp(p, "time-pos", null)).toEqual({ timePos: 42 });
  });
  it("ignores properties it does not track", () => {
    expect(applyProp(props(), "some-other-property", 1)).toBeNull();
  });
});

describe("trackId", () => {
  it("normalizes mpv track selections", () => {
    expect(trackId(2)).toBe(2);
    expect(trackId(false)).toBe(false);
    expect(trackId("no")).toBe(false);
    expect(trackId(undefined)).toBeNull();
  });
});

describe("saveProgress", () => {
  beforeEach(() => void (calls.updateHistory.length = 0));
  const movie: NowPlaying = { kind: "movie", sourceId: 1, id: "m1", title: "Heat", subtitle: "1995", ext: "mkv" };
  const episode: NowPlaying = {
    kind: "episode", sourceId: 1, id: "e2", title: "Pilot", seriesId: "s1", seriesTitle: "Show", season: 1, episode: 2,
  };

  it("writes nothing for live TV or before the duration is known", async () => {
    expect(await saveProgress({ ...movie, kind: "live" }, { ...props(), timePos: 50, duration: 100 })).toBe(false);
    expect(await saveProgress(movie, { ...props(), timePos: 50, duration: 0 })).toBe(false);
    expect(calls.updateHistory).toHaveLength(0);
  });
  it("saves movies and episodes with their titles", async () => {
    expect(await saveProgress(movie, { ...props(), timePos: 50, duration: 100 })).toBe(true);
    expect(await saveProgress(episode, { ...props(), timePos: 10, duration: 2400 })).toBe(true);
    expect(calls.updateHistory).toMatchObject([
      { kind: "movie", itemId: "m1", title: "Heat", subtitle: "1995", position: 50, duration: 100 },
      // episodes: series title first, episode title as subtitle (library.rs conventions)
      { kind: "episode", itemId: "e2", seriesId: "s1", title: "Show", subtitle: "Pilot", season: 1, episode: 2 },
    ]);
  });
});
