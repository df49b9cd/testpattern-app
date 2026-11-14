import { useInfiniteQuery, type QueryKey } from "@tanstack/react-query";
import { useCallback, useMemo } from "react";
import type { Page } from "../lib/types";

/**
 * An offset-paged list as one growing array. `fetchMoreNear(index, threshold)`
 * loads the next page once `index` (the last one on screen) is within
 * `threshold` items of the end of what is loaded.
 */
export function usePagedQuery<T>({
  queryKey,
  fetchPage,
  pageSize,
  enabled,
  refetchInterval,
}: {
  queryKey: QueryKey;
  fetchPage: (offset: number, limit: number) => Promise<Page<T>>;
  pageSize: number;
  enabled?: boolean;
  refetchInterval?: number;
}) {
  const query = useInfiniteQuery({
    queryKey,
    enabled,
    refetchInterval,
    initialPageParam: 0,
    queryFn: ({ pageParam }) => fetchPage(pageParam, pageSize),
    getNextPageParam: (last, pages) => {
      const loaded = pages.reduce((n, p) => n + p.items.length, 0);
      return loaded < last.total ? loaded : undefined;
    },
  });
  const items = useMemo(() => query.data?.pages.flatMap((p) => p.items) ?? [], [query.data]);
  const total = query.data?.pages[0]?.total ?? 0;
  const { hasNextPage, isFetchingNextPage, fetchNextPage } = query;
  const fetchMoreNear = useCallback(
    (index: number, threshold: number) => {
      if (index >= items.length - threshold && hasNextPage && !isFetchingNextPage) void fetchNextPage();
    },
    [items.length, hasNextPage, isFetchingNextPage, fetchNextPage],
  );
  return { items, total, ...query, fetchMoreNear };
}
