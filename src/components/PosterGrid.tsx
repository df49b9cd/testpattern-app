import { useInfiniteQuery } from "@tanstack/react-query";
import { useVirtualizer } from "@tanstack/react-virtual";
import { useEffect, useLayoutEffect, useRef, useState, type ReactNode } from "react";
import type { Page } from "../lib/types";
import { EmptyState, Spinner } from "./ui";

const PAGE = 120;
const GAP = 22;
const CAPTION = 46;

/**
 * Virtualized, infinitely paged poster grid. Columns adapt to the width;
 * only visible rows are rendered, so 40k titles scroll smoothly.
 */
export function PosterGrid<T>({
  queryKey,
  fetchPage,
  renderItem,
  itemKey,
  minItemWidth = 168,
  empty,
  header,
}: {
  queryKey: unknown[];
  fetchPage: (offset: number, limit: number) => Promise<Page<T>>;
  renderItem: (item: T, width: number) => ReactNode;
  itemKey: (item: T) => string;
  minItemWidth?: number;
  empty?: ReactNode;
  header?: ReactNode;
}) {
  const scrollRef = useRef<HTMLDivElement>(null);
  const [width, setWidth] = useState(0);

  useLayoutEffect(() => {
    const el = scrollRef.current;
    if (!el) return;
    const ro = new ResizeObserver(() => setWidth(el.clientWidth));
    ro.observe(el);
    setWidth(el.clientWidth);
    return () => ro.disconnect();
  }, []);

  const q = useInfiniteQuery({
    queryKey,
    initialPageParam: 0,
    queryFn: ({ pageParam }) => fetchPage(pageParam, PAGE),
    getNextPageParam: (last, pages) => {
      const loaded = pages.reduce((n, p) => n + p.items.length, 0);
      return loaded < last.total ? loaded : undefined;
    },
  });
  const items = q.data?.pages.flatMap((p) => p.items) ?? [];
  const total = q.data?.pages[0]?.total ?? 0;

  const padX = 40;
  const inner = Math.max(0, width - padX * 2);
  const cols = Math.max(2, Math.floor((inner + GAP) / (minItemWidth + GAP)));
  const itemWidth = cols > 0 ? (inner - GAP * (cols - 1)) / cols : minItemWidth;
  const rowHeight = itemWidth * 1.5 + CAPTION + GAP;
  const rows = Math.ceil(total / cols);

  const virtualizer = useVirtualizer({
    count: rows,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => rowHeight,
    overscan: 3,
  });
  useEffect(() => {
    virtualizer.measure();
  }, [rowHeight, virtualizer]);

  const virtualRows = virtualizer.getVirtualItems();
  const lastRow = virtualRows.length ? virtualRows[virtualRows.length - 1].index : 0;
  useEffect(() => {
    if ((lastRow + 3) * cols >= items.length && q.hasNextPage && !q.isFetchingNextPage) void q.fetchNextPage();
  }, [lastRow, cols, items.length, q]);

  // new filter → back to top
  const keyStr = JSON.stringify(queryKey);
  useEffect(() => {
    scrollRef.current?.scrollTo({ top: 0 });
  }, [keyStr]);

  return (
    <div ref={scrollRef} className="h-full overflow-y-auto">
      {header}
      {q.isLoading ? (
        <Spinner className="px-10 py-10" label="Loading…" />
      ) : total === 0 ? (
        (empty ?? <EmptyState title="Nothing here" />)
      ) : (
        <div className="relative mx-10 mb-10" style={{ height: virtualizer.getTotalSize() }}>
          {virtualRows.map((row) => (
            <div
              key={row.key}
              className="absolute left-0 top-0 grid w-full"
              style={{
                transform: `translateY(${row.start}px)`,
                gridTemplateColumns: `repeat(${cols}, minmax(0, 1fr))`,
                columnGap: GAP,
              }}
            >
              {Array.from({ length: cols }, (_, c) => {
                const i = row.index * cols + c;
                if (i >= total) return <div key={c} />;
                const item = items[i];
                return item ? (
                  <div key={itemKey(item)}>{renderItem(item, itemWidth)}</div>
                ) : (
                  <div key={c} className="flex flex-col gap-2">
                    <div className="aspect-[2/3] w-full rounded-xl skeleton" />
                    <div className="h-3 w-3/4 rounded skeleton" />
                  </div>
                );
              })}
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
