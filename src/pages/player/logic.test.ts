import { describe, expect, it } from "vitest";
import type { Channel, Episode, Season } from "../../lib/types";
import type { NowPlaying } from "../../stores/player";
import { nextEpisode, zapIndex, zapStep } from "./logic";
import { findBinding, type KeyBinding } from "./usePlayerKeys";

const channel = (sourceId: number, id: string, groupKey?: string): Channel => ({
  sourceId,
  id,
  title: id,
  badges: [],
  archive: false,
  archiveDays: 0,
  favorite: false,
  group: groupKey ? { key: groupKey, variants: 2, genre: "News" } : null,
});

describe("zapIndex", () => {
  const list = [channel(1, "a", "ga"), channel(1, "b", "gb"), channel(2, "b", "gc")];
  const live = (c: Channel): NowPlaying => ({ kind: "live", sourceId: c.sourceId, id: c.id, title: c.title, channel: c });

  it("finds the playing feed by source and id", () => {
    expect(zapIndex(list, live(channel(2, "b")))).toBe(2);
  });
  it("falls back to another feed of the same channel", () => {
    expect(zapIndex(list, live(channel(3, "x", "gb")))).toBe(1);
  });
  it("is -1 off the list and for anything but live TV", () => {
    expect(zapIndex(list, live(channel(3, "x")))).toBe(-1);
    expect(zapIndex(list, { kind: "movie", sourceId: 1, id: "a", title: "A" })).toBe(-1);
    expect(zapIndex(list, null)).toBe(-1);
  });
});

describe("zapStep", () => {
  it("wraps around both ends", () => {
    expect(zapStep(0, -1, 3)).toBe(2);
    expect(zapStep(2, 1, 3)).toBe(0);
    expect(zapStep(1, 1, 3)).toBe(2);
  });
  it("starts at the first channel when the current one is not in the list", () => {
    expect(zapStep(-1, 1, 3)).toBe(0);
    expect(zapStep(-1, -1, 3)).toBe(0);
  });
});

describe("nextEpisode", () => {
  const ep = (season: number, episode: number, id = `s${season}e${episode}`): Episode => ({
    id, season, episode, title: id, position: 0, watched: false, sourceId: 1, seriesId: "s",
  });
  const seasons: Season[] = [
    { season: 0, name: "Specials", episodes: [ep(0, 1)] },
    { season: 1, name: "S1", episodes: [ep(1, 1), ep(1, 2)] },
    { season: 2, name: "S2", episodes: [ep(2, 1)] },
  ];

  it("continues within the season and into the next one", () => {
    expect(nextEpisode(seasons, { id: "x", season: 1, episode: 1 })?.id).toBe("s1e2");
    expect(nextEpisode(seasons, { id: "x", season: 1, episode: 2 })?.id).toBe("s2e1");
  });
  it("finds the episode by id when the numbers are unknown", () => {
    expect(nextEpisode(seasons, { id: "s1e1" })?.id).toBe("s1e2");
  });
  it("has nothing after the last episode, and skips specials", () => {
    expect(nextEpisode(seasons, { id: "s2e1", season: 2, episode: 1 })).toBeUndefined();
    expect(nextEpisode(seasons, { id: "s0e1", season: 0, episode: 1 })).toBeUndefined();
  });
});

describe("player key table", () => {
  const bindings: KeyBinding[] = [
    { keys: ["ArrowUp"], when: true, run: () => {} },
    { keys: ["ArrowUp", "x"], run: () => {} },
    { keys: ["y"], when: false, run: () => {} },
  ];
  it("takes the first entry that applies", () => {
    expect(findBinding(bindings, "ArrowUp")).toBe(bindings[0]);
    expect(findBinding([{ ...bindings[0], when: false }, bindings[1]], "ArrowUp")).toBe(bindings[1]);
  });
  it("skips entries that don't apply right now", () => {
    expect(findBinding(bindings, "y")).toBeUndefined();
    expect(findBinding(bindings, "z")).toBeUndefined();
  });
});
