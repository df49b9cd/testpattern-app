import { beforeEach, describe, expect, it, vi } from "vitest";

const calls = vi.hoisted(() => ({ updateHistory: [] as unknown[] }));
vi.mock("../lib/api", () => ({
  api: {
    updateHistory: vi.fn(async (entry: unknown) => void calls.updateHistory.push(entry)),
    setSetting: vi.fn(async () => {}),
  },
  errorMessage: (e: unknown) => String(e),
}));

import { applyProp, saveProgress, setSubsEnabled, subKeyAction, subKeyHint, toggleSubs, trackId, usePlayer, type NowPlaying } from "./player";

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
  it("maps sub-visibility to subVisible", () => {
    expect(applyProp(props(), "sub-visibility", true)).toEqual({ subVisible: true });
    expect(applyProp(props(), "sub-visibility", false)).toEqual({ subVisible: false });
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

describe("offPageKey (Layout off-page gating, PL-101)", () => {
  it("fires Space/K only when body is focused; M toggles mute; ignored on /player and with modifiers", async () => {
    const { offPageKey } = await import("../app/Layout");
    const np: NowPlaying = { kind: "live", sourceId: 1, id: "c1", title: "BBC" };
    usePlayer.setState({ now: np });
    expect(offPageKey({ key: " ", ctrlKey: false, metaKey: false, altKey: false }, { pathname: "/live", onBody: true })).toBe("play");
    expect(offPageKey({ key: " ", ctrlKey: false, metaKey: false, altKey: false }, { pathname: "/live", onBody: false })).toBeNull();
    expect(offPageKey({ key: "m", ctrlKey: false, metaKey: false, altKey: false }, { pathname: "/live", onBody: false })).toBe("mute");
    expect(offPageKey({ key: " ", ctrlKey: false, metaKey: false, altKey: false }, { pathname: "/player", onBody: true })).toBeNull();
    expect(offPageKey({ key: "m", ctrlKey: true, metaKey: false, altKey: false }, { pathname: "/live", onBody: true })).toBeNull();
    usePlayer.setState({ now: null });
    expect(offPageKey({ key: " ", ctrlKey: false, metaKey: false, altKey: false }, { pathname: "/live", onBody: true })).toBeNull();
  });
});

describe("subtitles toggle (PL-99)", () => {
  it("persists the setting and patches the settings cache", async () => {
    const { queryClient } = await import("../lib/queryClient");
    const api = (await import("../lib/api")).api as unknown as { setSetting: ReturnType<typeof vi.fn> };
    queryClient.setQueryData(["settings"], { "player.subsEnabled": false });
    api.setSetting.mockClear();
    api.setSetting.mockResolvedValue(undefined);
    void setSubsEnabled(true);
    expect(api.setSetting).toHaveBeenCalledWith("player.subsEnabled", true);
    expect(queryClient.getQueryData(["settings"])).toMatchObject({ "player.subsEnabled": true });
    toggleSubs(); // store says subVisible=false → turns on
    expect(api.setSetting).toHaveBeenLastCalledWith("player.subsEnabled", true);
  });

  it("rolls the cache back when the write fails", async () => {
    const { queryClient } = await import("../lib/queryClient");
    const api = (await import("../lib/api")).api as unknown as { setSetting: ReturnType<typeof vi.fn> };
    queryClient.setQueryData(["settings"], { "player.subsEnabled": false });
    api.setSetting.mockRejectedValueOnce(new Error("backend down"));
    await expect(setSubsEnabled(true)).rejects.toThrow("backend down");
    expect(queryClient.getQueryData(["settings"])).toMatchObject({ "player.subsEnabled": false });
  });
});

describe("subKeyAction (S key, PL-100)", () => {
  const sub = (id: number) => ({ id, type: "sub" as const });
  it("cycles sid through tracks when more than one exists", () => {
    const p = { ...props(), tracks: [sub(1), sub(2), sub(3)], sid: 1 };
    expect(subKeyAction(p)).toEqual({ cycle: 2 });
    expect(subKeyAction({ ...p, sid: 3 })).toEqual({ cycle: false });
    // sid off / unknown: start at the first track
    expect(subKeyAction({ ...p, sid: false })).toEqual({ cycle: 1 });
    expect(subKeyAction({ ...p, sid: null })).toEqual({ cycle: 1 });
  });
  it("toggles visibility with a single track", () => {
    const p = { ...props(), tracks: [sub(1)], sid: 1, subVisible: false };
    expect(subKeyAction(p)).toEqual({ toggle: true });
    expect(subKeyAction({ ...p, subVisible: true })).toEqual({ toggle: false });
  });
  it("returns null when there are no subtitle tracks", () => {
    expect(subKeyAction({ ...props(), tracks: [] })).toBeNull();
  });
  it("reports the next action for the PL-100 hint", () => {
    expect(subKeyHint({ ...props(), tracks: [sub(1), sub(2)], sid: 1 })).toBe("cycle");
    expect(subKeyHint({ ...props(), tracks: [sub(1)], subVisible: false })).toBe("on");
    expect(subKeyHint({ ...props(), tracks: [sub(1)], subVisible: true })).toBe("off");
    expect(subKeyHint({ ...props(), tracks: [] })).toBeNull();
  });
});
