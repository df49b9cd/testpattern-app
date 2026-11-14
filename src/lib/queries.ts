import { useQuery, type Query } from "@tanstack/react-query";
import { api } from "./api";
import type { ItemRef } from "./item";
import { queryKeys } from "./queryKeys";
import type { SeriesDetail, Source } from "./types";

/** Per-caller polling/freshness on top of the shared key and fetcher. */
interface QueryOpts<T> {
  staleTime?: number;
  refetchInterval?: number | false | ((query: Query<T, Error, T, readonly unknown[]>) => number | false | undefined);
}

/** Other versions' details still loading: ask again a few times. */
export const pollWhileVersionsPending = <T extends { versionsPending: boolean }>(query: Query<T, Error, T, readonly unknown[]>) =>
  query.state.data?.versionsPending && query.state.dataUpdateCount < 6 ? 3000 : false;

export function useSources(opts: QueryOpts<Source[]> = {}) {
  return useQuery({ queryKey: queryKeys.sources, queryFn: api.sources, ...opts });
}

/** A series with its seasons; disabled while `ref` is unknown. */
export function useSeriesDetail(ref: ItemRef | null | undefined, opts: QueryOpts<SeriesDetail> = {}) {
  return useQuery({
    queryKey: queryKeys.workDetail("series", ref ?? {}),
    queryFn: () => api.seriesDetail(ref!.sourceId, ref!.id),
    enabled: !!ref,
    ...opts,
  });
}
