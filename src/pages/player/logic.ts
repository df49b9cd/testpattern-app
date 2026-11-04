import { sameItem } from "../../lib/item";
import type { Channel, Episode, Season } from "../../lib/types";
import type { NowPlaying } from "../../stores/player";

/**
 * Where the playing channel sits in its zap list: by feed, else by channel
 * (another feed of it may be playing); -1 when not live or not in the list.
 */
export function zapIndex(zapList: Channel[], now: NowPlaying | null | undefined): number {
  if (now?.kind !== "live") return -1;
  const i = zapList.findIndex((c) => sameItem(c, now));
  const groupKey = now.channel?.group?.key;
  if (i < 0 && groupKey) return zapList.findIndex((c) => c.group?.key === groupKey);
  return i;
}

/** One channel up (-1) or down (+1), wrapping around; the first when not in the list. */
export function zapStep(index: number, dir: number, length: number): number {
  return index < 0 ? 0 : (index + dir + length) % length;
}

/** The episode after the playing one (specials, season 0, are skipped). */
export function nextEpisode(seasons: Season[], now: { id?: string; season?: number; episode?: number } | null | undefined): Episode | undefined {
  const eps = seasons.filter((s) => s.season > 0).flatMap((s) => s.episodes);
  // the list merges every copy of the show: find ours by its number
  let i = eps.findIndex((e) => e.season === now?.season && e.episode === now?.episode);
  if (i < 0) i = eps.findIndex((e) => e.id === now?.id);
  return i >= 0 ? eps[i + 1] : undefined;
}
