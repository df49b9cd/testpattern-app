import type { NavigateFunction } from "react-router";
import { usePlayer, type NowPlaying } from "../stores/player";
import type { Channel, Episode, HistoryItem, Movie, MovieDetail, Programme, Series } from "./types";

/** Starts playback and opens the fullscreen player. */
async function start(np: NowPlaying, navigate?: NavigateFunction, open = true) {
  await usePlayer.getState().play(np);
  if (open && navigate) navigate("/player");
}

export function channelNowPlaying(ch: Channel, zapList?: Channel[]): NowPlaying {
  return {
    kind: "live",
    sourceId: ch.sourceId,
    id: ch.id,
    title: ch.title,
    subtitle: ch.now?.title ?? null,
    image: ch.logo,
    channel: ch,
    zapList,
  };
}

export function playChannel(ch: Channel, navigate?: NavigateFunction, zapList?: Channel[], open = true) {
  return start(channelNowPlaying(ch, zapList), navigate, open);
}

export function playMovie(m: Movie | MovieDetail, navigate?: NavigateFunction, fromStart = false) {
  const detail = m as Partial<MovieDetail>;
  return start(
    {
      kind: "movie",
      sourceId: m.sourceId,
      id: m.id,
      title: m.title,
      subtitle: m.year ? String(m.year) : null,
      image: m.poster,
      backdrop: detail.backdrop ?? null,
      ext: m.ext,
      start: fromStart ? 0 : (detail.position ?? null),
    },
    navigate,
  );
}

/**
 * Without `navigate` the player page is not (re)opened — e.g. autoplay.
 * The episode may come from another copy of the series than `series`
 * (`ep.sourceId`/`ep.seriesId`).
 */
export function playEpisode(series: Series, ep: Episode, navigate?: NavigateFunction, fromStart = false) {
  return start(
    {
      kind: "episode",
      sourceId: ep.sourceId ?? series.sourceId,
      id: ep.id,
      title: ep.title,
      subtitle: `${series.title} · S${ep.season} E${ep.episode}`,
      image: series.cover,
      backdrop: series.backdrop ?? ep.image ?? null,
      ext: ep.ext,
      start: fromStart || ep.watched ? 0 : ep.position,
      seriesId: ep.seriesId ?? series.id,
      seriesTitle: series.title,
      season: ep.season,
      episode: ep.episode,
    },
    navigate,
  );
}

export function playCatchup(ch: Channel, p: Programme, navigate: NavigateFunction) {
  return start(
    {
      kind: "catchup",
      sourceId: ch.sourceId,
      id: ch.id,
      title: p.title,
      subtitle: `${ch.title} · catch-up`,
      image: ch.logo,
      channel: ch,
      catchupStart: p.start,
      catchupMinutes: Math.max(1, Math.round((p.stop - p.start) / 60)),
    },
    navigate,
  );
}

/** Continue-watching entries resume where they left off. */
export function resumeHistory(h: HistoryItem, navigate: NavigateFunction) {
  const isEpisode = h.kind === "episode";
  return start(
    {
      kind: isEpisode ? "episode" : "movie",
      sourceId: h.sourceId,
      id: h.itemId,
      title: isEpisode ? (h.subtitle ?? h.title) : h.title,
      subtitle: isEpisode ? `${h.title} · S${h.season ?? "?"} E${h.episode ?? "?"}` : h.subtitle,
      image: h.image,
      backdrop: h.backdrop,
      ext: h.ext,
      start: h.position,
      seriesId: h.seriesId ?? undefined,
      seriesTitle: isEpisode ? h.title : undefined,
      season: h.season ?? undefined,
      episode: h.episode ?? undefined,
    },
    navigate,
  );
}
