import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("./api", () => ({ api: {}, errorMessage: (e: unknown) => String(e) }));

import { usePlayer, type NowPlaying } from "../stores/player";
import { playCatchup, playEpisode, playMovie, resumeHistory } from "./play";
import { ROUTES } from "./routes";
import type { Channel, Episode, HistoryItem, Movie, Programme, Series } from "./types";

const play = vi.fn(async (_np: NowPlaying) => {});
const navigate = vi.fn();
beforeEach(() => {
  play.mockClear();
  navigate.mockClear();
  usePlayer.setState({ play });
});
/** What the last call handed to the player. */
const started = () => play.mock.calls.at(-1)![0];

const work = { versionCount: 1, quality: [], services: [] };
const series: Series = { ...work, sourceId: 1, id: "s1", title: "Show", cover: "cover.jpg", backdrop: "back.jpg", favorite: false };
const episode: Episode = { id: "e2", season: 1, episode: 2, title: "Pilot", ext: "mkv", position: 300, watched: false, sourceId: 2, seriesId: "s9" };

describe("play helpers", () => {
  it("plays a movie from its saved position and opens the player", async () => {
    const movie: Movie = { ...work, sourceId: 1, id: "m1", title: "Heat", year: 1995, poster: "p.jpg", ext: "mp4", favorite: false, watched: false };
    await playMovie({ ...movie, position: 120 } as Movie, navigate);
    expect(started()).toMatchObject({ kind: "movie", id: "m1", title: "Heat", subtitle: "1995", start: 120 });
    expect(navigate).toHaveBeenCalledWith(ROUTES.player);
    await playMovie({ ...movie, year: null }, navigate, true);
    expect(started()).toMatchObject({ subtitle: null, start: 0 });
  });

  it("plays an episode from the copy it belongs to, titled by episode", async () => {
    await playEpisode(series, episode, navigate);
    expect(started()).toMatchObject({
      kind: "episode",
      sourceId: 2,
      id: "e2",
      title: "Pilot",
      subtitle: "Show · S1 E2",
      seriesId: "s9",
      seriesTitle: "Show",
      start: 300,
    });
    // watched episodes and autoplay start over; without navigate the page stays
    await playEpisode(series, { ...episode, watched: true });
    expect(started()).toMatchObject({ start: 0 });
    expect(navigate).toHaveBeenCalledTimes(1);
  });

  it("plays catch-up as the programme on its channel", async () => {
    const ch = { sourceId: 1, id: "c1", title: "BBC One", logo: "bbc.png" } as Channel;
    const p: Programme = { epgId: "bbc1", start: 1000, stop: 1000 + 45 * 60, title: "News" };
    await playCatchup(ch, p, navigate);
    expect(started()).toMatchObject({ kind: "catchup", title: "News", subtitle: "BBC One · catch-up", catchupStart: 1000, catchupMinutes: 45 });
  });

  it("resumes history entries with series title and episode as subtitle", async () => {
    const base = { sourceId: 1, itemId: "x", image: null, backdrop: null, ext: "mkv", position: 50, duration: 100, updatedAt: 0 };
    const ep: HistoryItem = { ...base, kind: "episode", seriesId: "s1", season: 2, episode: 3, title: "Show", subtitle: "The One" };
    await resumeHistory(ep, navigate);
    expect(started()).toMatchObject({ kind: "episode", title: "The One", subtitle: "Show · S2 E3", seriesTitle: "Show", start: 50 });
    // unknown numbers read "?"; no episode title falls back to the series'
    await resumeHistory({ ...ep, season: null, episode: null, subtitle: null }, navigate);
    expect(started()).toMatchObject({ title: "Show", subtitle: "Show · S? E?" });
    const movie: HistoryItem = { ...base, kind: "movie", title: "Heat", subtitle: "1995" };
    await resumeHistory(movie, navigate);
    expect(started()).toMatchObject({ kind: "movie", title: "Heat", subtitle: "1995", seriesTitle: undefined });
  });
});
