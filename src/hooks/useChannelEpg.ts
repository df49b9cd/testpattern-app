import { useQuery } from "@tanstack/react-query";
import { api } from "../lib/api";
import { nowUnix } from "../lib/format";
import type { ItemRef } from "../lib/item";

/** Guide window around now: the running show's start … well past the next ones. */
export const EPG_BEHIND = 3600;
export const EPG_AHEAD = 12 * 3600;
const EPG_REFRESH = 5 * 60_000;

/**
 * A channel's programmes for the player and the Live TV preview. One hook so
 * the shared cache key always holds the same window.
 */
export function useChannelEpg(channel: ItemRef | null | undefined) {
  return useQuery({
    queryKey: ["epg", channel?.sourceId, channel?.id],
    queryFn: () => {
      const t = nowUnix();
      return api.epg(channel!.sourceId, channel!.id, t - EPG_BEHIND, t + EPG_AHEAD);
    },
    enabled: !!channel,
    refetchInterval: EPG_REFRESH,
  });
}
