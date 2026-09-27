import clsx from "clsx";
import { useInfiniteQuery, useQuery, useQueryClient } from "@tanstack/react-query";
import { useVirtualizer } from "@tanstack/react-virtual";
import { ChevronDown, Clock, Heart, History, Layers, LayoutList, Maximize2, Search, Tv } from "lucide-react";
import { memo, useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useNavigate, useSearchParams } from "react-router";
import { api } from "../lib/api";
import { browserPreview } from "../lib/bridge";
import { elapsed, hhmm, nowUnix } from "../lib/format";
import { keyString, parseKey, queryFor, titleFor, type ListKey } from "../lib/liveLists";
import { playChannel } from "../lib/play";
import type { Category, Channel, LiveNav } from "../lib/types";
import { usePlayer } from "../stores/player";
import { useVideoViewport } from "../hooks/useVideoViewport";
import { ChannelLogo } from "../components/media";
import { Badge, Button, EmptyState, IconButton, LiveDot, ProgressBar, Segmented, Spinner } from "../components/ui";

const PAGE = 1000;
const ROW = 68;

type Mode = "countries" | "genres" | "provider";

function stored<T>(key: string, fallback: T): T {
  try {
    const v = localStorage.getItem(key);
    return v === null ? fallback : (JSON.parse(v) as T);
  } catch {
    return fallback;
  }
}
function store(key: string, value: unknown) {
  try {
    localStorage.setItem(key, JSON.stringify(value));
  } catch {
    /* per-viewer convenience only */
  }
}

export function LivePage() {
  const [params, setParams] = useSearchParams();
  const key = parseKey(params.get("list"));
  const setKey = (k: ListKey) => setParams({ list: keyString(k) }, { replace: true });
  const [filter, setFilter] = useState("");
  const [selected, setSelected] = useState<Channel | null>(null);

  const categories = useQuery({ queryKey: ["categories", "live"], queryFn: () => api.categories("live") });
  const nav = useQuery({ queryKey: ["channels", "nav"], queryFn: api.liveNav });

  // default to Favorites, or the viewer's first country when there are none
  const favCount = useQuery({ queryKey: ["channels", "fav-count"], queryFn: () => api.channels({ grouped: true, favorites: true, limit: 1 }) });
  useEffect(() => {
    if (!params.get("list") && favCount.data && favCount.data.total === 0 && nav.data?.countries.length) {
      setKey({ type: "nav", country: nav.data.countries[0].code ?? "" });
    }
  }, [favCount.data, nav.data]);

  return (
    <div className="flex h-full">
      <ListPane categories={categories.data ?? []} nav={nav.data} loading={categories.isLoading || nav.isLoading} active={key} onSelect={setKey} />
      <ChannelPane
        listKey={key}
        title={titleFor(key, nav.data, categories.data)}
        filter={filter}
        setFilter={setFilter}
        selected={selected}
        onSelect={setSelected}
      />
      <PreviewPane channel={selected} onChannel={setSelected} />
    </div>
  );
}

// ----------------------------------------------------------------- lists

function ListPane({
  categories,
  nav,
  loading,
  active,
  onSelect,
}: {
  categories: Category[];
  nav?: LiveNav;
  loading: boolean;
  active: ListKey;
  onSelect: (k: ListKey) => void;
}) {
  const [mode, setModeState] = useState<Mode>(() => (active.type === "cat" ? "provider" : stored<Mode>("live.mode", "countries")));
  const setMode = (m: Mode) => {
    setModeState(m);
    store("live.mode", m);
  };
  const [q, setQ] = useState("");
  const [collapsed, setCollapsed] = useState<Record<string, boolean>>(() => stored("live.collapsed", {}));
  const toggle = (name: string) =>
    setCollapsed((c) => {
      const next = { ...c, [name]: !(c[name] ?? defaultCollapsed(name)) };
      store("live.collapsed", next);
      return next;
    });
  // countries/genres start folded, except the first country and the open list's
  const firstCountry = nav?.countries[0]?.code ?? "";
  const openCountry = active.type === "nav" && active.country !== undefined ? `country:${active.country}` : null;
  const openGenre = active.type === "nav" && active.genre ? `genre:${active.genre}` : null;
  const defaultCollapsed = (name: string) =>
    name === openCountry || name === openGenre
      ? false
      : name.startsWith("country:")
        ? name !== `country:${firstCountry}`
        : name.startsWith("genre:");
  const isCollapsed = (name: string) => !q && (collapsed[name] ?? defaultCollapsed(name));

  const needle = q.trim().toLowerCase();
  const matches = (...texts: (string | null | undefined)[]) => !needle || texts.some((t) => t?.toLowerCase().includes(needle));

  const isActive = (k: ListKey) => keyString(k) === keyString(active);
  const special: { key: ListKey; label: string; icon: typeof Heart; count?: number }[] = [
    { key: { type: "favorites" }, label: "Favorites", icon: Heart },
    { key: { type: "recent" }, label: "Recently watched", icon: History },
    { key: { type: "all" }, label: "All channels", icon: LayoutList, count: nav?.cells.reduce((n, c) => n + c.count, 0) },
  ];

  const providerGroups = useMemo(() => {
    const map = new Map<string, Category[]>();
    for (const c of categories) {
      if (needle && !`${c.region ?? ""} ${c.title}`.toLowerCase().includes(needle)) continue;
      const g = c.region ?? "Other";
      if (!map.has(g)) map.set(g, []);
      map.get(g)!.push(c);
    }
    return [...map.entries()];
  }, [categories, needle]);

  const cell = (country: string, genre: string) =>
    nav?.cells.find((c) => (c.country ?? "") === country && c.genre === genre)?.count ?? 0;
  const genreTotal = (genre: string) => nav?.cells.filter((c) => c.genre === genre).reduce((n, c) => n + c.count, 0) ?? 0;

  return (
    <div className="flex w-[268px] shrink-0 flex-col border-r border-line bg-panel">
      <div className="px-4 pb-3 pt-6">
        <h1 className="mb-4 text-2xl font-bold tracking-tight">Live TV</h1>
        <Segmented
          value={mode}
          onChange={setMode}
          className="mb-3 flex w-full [&>button]:flex-1 [&>button]:px-2"
          options={[
            { value: "countries", label: "Countries" },
            { value: "genres", label: "Genres" },
            { value: "provider", label: "Provider" },
          ]}
        />
        <label className="relative flex items-center">
          <Search className="pointer-events-none absolute left-3 size-4 text-faint" />
          <input
            value={q}
            onChange={(e) => setQ(e.target.value)}
            placeholder={mode === "provider" ? "Filter categories" : mode === "genres" ? "Filter genres" : "Filter countries"}
            className="h-9 w-full rounded-lg bg-white/[0.06] pl-9 pr-3 text-sm outline-none ring-1 ring-white/[0.06] placeholder:text-faint focus:ring-accent"
          />
        </label>
      </div>
      <div className="flex-1 overflow-y-auto px-2 pb-6">
        {special.map(({ key, label, icon: Icon, count }) => (
          <ListRow key={label} active={isActive(key)} onClick={() => onSelect(key)} count={count}>
            <Icon className={clsx("size-4 shrink-0", isActive(key) ? "text-accent-strong" : "text-faint")} />
            <span className="min-w-0 flex-1 truncate font-medium">{label}</span>
          </ListRow>
        ))}
        {loading && <Spinner className="px-3 py-4" />}

        {mode === "countries" &&
          nav?.countries
            .filter((c) => matches(c.name, c.code))
            .map((c) => {
              const code = c.code ?? "";
              const fold = `country:${code}`;
              return (
                <div key={fold} className="mt-2">
                  <TreeHeader
                    open={!isCollapsed(fold)}
                    onToggle={() => toggle(fold)}
                    active={isActive({ type: "nav", country: code })}
                    onClick={() => onSelect({ type: "nav", country: code })}
                    count={c.count}
                  >
                    {c.name}
                  </TreeHeader>
                  {!isCollapsed(fold) &&
                    nav.genres
                      .filter((g) => cell(code, g) > 0)
                      .map((g) => {
                        const k: ListKey = { type: "nav", country: code, genre: g };
                        return (
                          <ListRow key={g} inset active={isActive(k)} onClick={() => onSelect(k)} count={cell(code, g)}>
                            <span className="min-w-0 flex-1 truncate">{g}</span>
                          </ListRow>
                        );
                      })}
                </div>
              );
            })}

        {mode === "genres" &&
          nav?.genres
            .filter((g) => matches(g))
            .map((g) => {
              const fold = `genre:${g}`;
              return (
                <div key={fold} className="mt-2">
                  <TreeHeader
                    open={!isCollapsed(fold)}
                    onToggle={() => toggle(fold)}
                    active={isActive({ type: "nav", genre: g })}
                    onClick={() => onSelect({ type: "nav", genre: g })}
                    count={genreTotal(g)}
                  >
                    {g}
                  </TreeHeader>
                  {!isCollapsed(fold) &&
                    nav.countries
                      .filter((c) => cell(c.code ?? "", g) > 0)
                      .map((c) => {
                        const k: ListKey = { type: "nav", country: c.code ?? "", genre: g };
                        return (
                          <ListRow key={c.code ?? "-"} inset active={isActive(k)} onClick={() => onSelect(k)} count={cell(c.code ?? "", g)}>
                            <span className="min-w-0 flex-1 truncate">{c.name}</span>
                          </ListRow>
                        );
                      })}
                </div>
              );
            })}

        {mode === "provider" &&
          providerGroups.map(([region, cats]) => {
            const fold = `region:${region}`;
            return (
              <div key={region} className="mt-3">
                <button
                  onClick={() => toggle(fold)}
                  className="flex w-full items-center gap-1.5 px-3 py-1.5 text-[11px] font-bold uppercase tracking-[0.12em] text-faint hover:text-dim"
                >
                  <ChevronDown className={clsx("size-3.5 transition-transform", isCollapsed(fold) && "-rotate-90")} />
                  {region}
                  <span className="ml-auto font-medium normal-case tracking-normal">{cats.length}</span>
                </button>
                {!isCollapsed(fold) &&
                  cats.map((c) => {
                    const k: ListKey = { type: "cat", sourceId: c.sourceId, id: c.id };
                    return (
                      <ListRow key={keyString(k)} active={isActive(k)} onClick={() => onSelect(k)} count={c.count}>
                        <span className="min-w-0 flex-1 truncate">{c.title}</span>
                      </ListRow>
                    );
                  })}
              </div>
            );
          })}
      </div>
    </div>
  );
}

function ListRow({
  active,
  onClick,
  count,
  inset,
  children,
}: {
  active: boolean;
  onClick: () => void;
  count?: number;
  inset?: boolean;
  children: React.ReactNode;
}) {
  return (
    <button
      onClick={onClick}
      aria-pressed={active}
      className={clsx(
        "flex min-h-9 w-full items-center gap-2.5 rounded-lg px-3 py-1.5 text-left text-[13.5px] transition-colors",
        inset && "pl-8",
        active ? "bg-accent/15 font-semibold text-fg" : "text-dim hover:bg-white/[0.04] hover:text-fg",
      )}
    >
      {children}
      {count != null && <span className="shrink-0 text-[11px] font-normal tabular-nums text-faint">{count.toLocaleString()}</span>}
    </button>
  );
}

/** A country (or genre) that is a list itself and folds its sub-lists. */
function TreeHeader({
  open,
  onToggle,
  active,
  onClick,
  count,
  children,
}: {
  open: boolean;
  onToggle: () => void;
  active: boolean;
  onClick: () => void;
  count: number;
  children: React.ReactNode;
}) {
  return (
    <div
      className={clsx(
        "flex min-h-9 items-center rounded-lg pr-3 transition-colors",
        active ? "bg-accent/15 text-fg" : "text-fg/90 hover:bg-white/[0.04]",
      )}
    >
      <button
        aria-label={open ? "Collapse" : "Expand"}
        aria-expanded={open}
        onClick={onToggle}
        className="grid h-9 w-8 shrink-0 place-items-center text-faint hover:text-fg"
      >
        <ChevronDown className={clsx("size-4 transition-transform", !open && "-rotate-90")} />
      </button>
      <button onClick={onClick} aria-pressed={active} className="flex min-w-0 flex-1 items-center gap-2 py-1.5 text-left text-[14px] font-semibold">
        <span className="min-w-0 flex-1 truncate">{children}</span>
        <span className="shrink-0 text-[11px] font-normal tabular-nums text-faint">{count.toLocaleString()}</span>
      </button>
    </div>
  );
}

// -------------------------------------------------------------- channels

function useChannelList(key: ListKey, filter: string) {
  const q = filter.trim() || undefined;
  const infinite = useInfiniteQuery({
    queryKey: ["channels", "list", keyString(key), q],
    enabled: key.type !== "recent",
    initialPageParam: 0,
    refetchInterval: 60_000,
    queryFn: ({ pageParam }) => api.channels({ ...queryFor(key), q, offset: pageParam, limit: PAGE }),
    getNextPageParam: (last, pages) => {
      const loaded = pages.reduce((n, p) => n + p.items.length, 0);
      return loaded < last.total ? loaded : undefined;
    },
  });
  const recent = useQuery({
    queryKey: ["recent-channels", "live-page"],
    queryFn: () => api.recentChannels(60),
    enabled: key.type === "recent",
    refetchInterval: 60_000,
  });
  if (key.type === "recent") {
    const items = (recent.data ?? []).filter((c) => !q || c.title.toLowerCase().includes(q.toLowerCase()));
    return { items, total: items.length, loading: recent.isLoading, fetchMore: () => {}, hasMore: false };
  }
  const items = infinite.data?.pages.flatMap((p) => p.items) ?? [];
  return {
    items,
    total: infinite.data?.pages[0]?.total ?? 0,
    loading: infinite.isLoading,
    fetchMore: () => {
      if (infinite.hasNextPage && !infinite.isFetchingNextPage) void infinite.fetchNextPage();
    },
    hasMore: !!infinite.hasNextPage,
  };
}

function ChannelPane({
  listKey,
  title,
  filter,
  setFilter,
  selected,
  onSelect,
}: {
  listKey: ListKey;
  title: string;
  filter: string;
  setFilter: (v: string) => void;
  selected: Channel | null;
  onSelect: (c: Channel) => void;
}) {
  const navigate = useNavigate();
  const qc = useQueryClient();
  const { items, total, loading, fetchMore } = useChannelList(listKey, filter);
  const playing = usePlayer((s) => (s.now?.kind === "live" ? `${s.now.sourceId}:${s.now.id}` : null));
  const scrollRef = useRef<HTMLDivElement>(null);
  const previewTimer = useRef<number>(0);

  // channel rows (one per channel) vs. the provider's own feeds
  const grouped = listKey.type !== "cat";

  const virtualizer = useVirtualizer({
    count: total,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => ROW,
    overscan: 10,
  });

  // lazy-load further pages as the user scrolls ("All channels")
  const virtualItems = virtualizer.getVirtualItems();
  const lastIndex = virtualItems.length ? virtualItems[virtualItems.length - 1].index : 0;
  useEffect(() => {
    if (lastIndex >= items.length - 20) fetchMore();
  }, [lastIndex, items.length, fetchMore]);

  // a new list starts at the top
  useEffect(() => {
    scrollRef.current?.scrollTo({ top: 0 });
  }, [keyString(listKey)]);

  const select = useCallback(
    (c: Channel, preview = true) => {
      onSelect(c);
      window.clearTimeout(previewTimer.current);
      if (!preview) return;
      previewTimer.current = window.setTimeout(() => {
        const now = usePlayer.getState().now;
        if (now?.kind === "live" && now.sourceId === c.sourceId && now.id === c.id) return;
        void playChannel(c, undefined, items, false);
      }, 350);
    },
    [onSelect, items],
  );

  const openFullscreen = useCallback(
    (c: Channel) => {
      window.clearTimeout(previewTimer.current);
      const now = usePlayer.getState().now;
      if (now?.kind === "live" && now.sourceId === c.sourceId && now.id === c.id) navigate("/player");
      else void playChannel(c, navigate, items);
    },
    [navigate, items],
  );

  const toggleFavorite = useCallback(
    async (c: Channel) => {
      // a channel row stands for all of its feeds
      const fav = grouped && c.group ? await api.toggleChannelFavorite(c.group.key) : await api.toggleFavorite("live", c.sourceId, c.id);
      if (selected && selected.sourceId === c.sourceId && selected.id === c.id) onSelect({ ...selected, favorite: fav });
      void qc.invalidateQueries({ queryKey: ["channels"] });
    },
    [qc, selected, onSelect, grouped],
  );

  const onKeyDown = (e: React.KeyboardEvent) => {
    if (!items.length) return;
    const idx = selected ? items.findIndex((c) => c.sourceId === selected.sourceId && c.id === selected.id) : -1;
    const move = (to: number) => {
      const t = Math.max(0, Math.min(items.length - 1, to));
      select(items[t]);
      virtualizer.scrollToIndex(t, { align: "auto" });
      e.preventDefault();
    };
    if (e.key === "ArrowDown") move(idx + 1);
    else if (e.key === "ArrowUp") move(idx - 1);
    else if (e.key === "PageDown") move(idx + 10);
    else if (e.key === "PageUp") move(idx - 10);
    else if (e.key === "Enter" && selected) openFullscreen(selected);
    else if ((e.key === "f" || e.key === "F") && selected) void toggleFavorite(selected);
  };

  return (
    <div className="flex min-w-[380px] flex-1 flex-col border-r border-line bg-bg">
      {/* title above the search: "United Kingdom · Documentary" needs the width */}
      <div className="flex flex-col gap-3 px-6 pb-3 pt-6">
        <div className="flex min-w-0 items-baseline justify-between gap-3">
          <h2 className="line-clamp-2 min-w-0 text-xl leading-tight font-bold tracking-tight" title={title}>
            {title}
          </h2>
          <p className="shrink-0 text-[13px] tabular-nums text-dim">{loading ? "Loading…" : `${total.toLocaleString()} channels`}</p>
        </div>
        <label className="relative flex items-center">
          <Search className="pointer-events-none absolute left-3 size-4 text-faint" />
          <input
            value={filter}
            onChange={(e) => setFilter(e.target.value)}
            placeholder="Find channel"
            className="h-9 w-full rounded-lg bg-white/[0.06] pl-9 pr-3 text-sm outline-none ring-1 ring-white/[0.06] placeholder:text-faint focus:ring-accent"
          />
        </label>
      </div>
      <div ref={scrollRef} tabIndex={0} onKeyDown={onKeyDown} className="flex-1 overflow-y-auto px-3 pb-6 outline-none">
        {!loading && total === 0 ? (
          listKey.type === "favorites" ? (
            <EmptyState icon={<Heart />} title="No favorite channels yet" text="Open a country or genre and press the heart on any channel (or F) to pin it here." />
          ) : (
            <EmptyState icon={<Tv />} title="No channels" text={filter ? "Nothing matches your filter." : "This list is empty."} />
          )
        ) : (
          <div className="relative w-full" style={{ height: virtualizer.getTotalSize() }}>
            {virtualItems.map((v) => {
              const c = items[v.index];
              return (
                <div key={v.key} className="absolute left-0 top-0 w-full" style={{ height: v.size, transform: `translateY(${v.start}px)` }}>
                  {c ? (
                    <ChannelRow
                      channel={c}
                      grouped={grouped}
                      index={v.index}
                      selected={!!selected && selected.sourceId === c.sourceId && selected.id === c.id}
                      playing={playing === `${c.sourceId}:${c.id}`}
                      onSelect={select}
                      onOpen={openFullscreen}
                      onFavorite={toggleFavorite}
                    />
                  ) : (
                    <div className="mx-1 my-1 h-[60px] rounded-xl skeleton" />
                  )}
                </div>
              );
            })}
          </div>
        )}
      </div>
    </div>
  );
}

const ChannelRow = memo(function ChannelRow({
  channel: c,
  grouped,
  index,
  selected,
  playing,
  onSelect,
  onOpen,
  onFavorite,
}: {
  channel: Channel;
  grouped: boolean;
  index: number;
  selected: boolean;
  playing: boolean;
  onSelect: (c: Channel) => void;
  onOpen: (c: Channel) => void;
  onFavorite: (c: Channel) => void;
}) {
  const now = c.now;
  return (
    <div
      role="button"
      tabIndex={-1}
      onClick={() => onSelect(c)}
      onDoubleClick={() => onOpen(c)}
      className={clsx(
        "group flex h-[64px] items-center gap-3.5 rounded-xl px-3 transition-colors",
        selected ? "bg-white/[0.09] ring-1 ring-white/10" : "hover:bg-white/[0.04]",
      )}
    >
      <span className="w-8 shrink-0 text-right text-[12px] font-semibold tabular-nums text-faint">{c.num ?? index + 1}</span>
      <ChannelLogo src={c.logo} title={c.title} size={44} className="size-11" />
      <div className="min-w-0 flex-1">
        <div className="flex items-center gap-2">
          {playing && <LiveDot />}
          <span className={clsx("truncate text-[14px] font-semibold", playing && "text-accent-strong")}>{c.title}</span>
          {c.badges.slice(0, 2).map((b) => (
            <Badge key={b}>{b}</Badge>
          ))}
          {grouped && c.group && c.group.variants > 1 && (
            <span title={`${c.group.variants} feeds of this channel`} className="inline-flex shrink-0 items-center gap-1 text-[11px] font-semibold text-faint">
              <Layers className="size-3.5" />
              {c.group.variants}
            </span>
          )}
          {c.archive && (
            <span title="Catch-up available">
              <Clock className="size-3.5 shrink-0 text-faint" />
            </span>
          )}
        </div>
        {now ? (
          <div className="mt-1 flex items-center gap-2.5">
            <span className="truncate text-[12.5px] text-dim">{now.title}</span>
            <ProgressBar value={elapsed(now.start, now.stop)} className="h-[3px] w-14 shrink-0" />
          </div>
        ) : (
          <div className="mt-1 truncate text-[12.5px] text-faint">No programme information</div>
        )}
      </div>
      <IconButton
        label={c.favorite ? "Remove from favorites" : "Add to favorites"}
        size="sm"
        onClick={(e) => {
          e.stopPropagation();
          onFavorite(c);
        }}
        className={clsx(!c.favorite && "opacity-0 group-hover:opacity-100")}
      >
        <Heart className={clsx("size-4", c.favorite && "fill-live text-live")} />
      </IconButton>
    </div>
  );
});

// --------------------------------------------------------------- preview

/** The provider's feeds of a channel (quality variants); picking one remembers it. */
function Feeds({ channel, playing, onChannel }: { channel: Channel; playing: boolean; onChannel: (c: Channel) => void }) {
  const qc = useQueryClient();
  const key = channel.group!.key;
  const variants = useQuery({ queryKey: ["channels", "variants", key], queryFn: () => api.channelVariants(key) });
  const pick = async (v: Channel & { label: string }) => {
    await api.channelPrefer(key, v.sourceId, v.id);
    // keep the channel's name; play the feed if the channel is on
    const next: Channel = { ...v, title: channel.title, group: channel.group };
    onChannel(next);
    if (playing) void playChannel(next, undefined, usePlayer.getState().now?.zapList, false);
    void qc.invalidateQueries({ queryKey: ["channels"] });
  };
  return (
    <div>
      <h4 className="mb-2 text-[12px] font-semibold uppercase tracking-wider text-faint">Feeds</h4>
      <div className="flex flex-wrap gap-1.5">
        {(variants.data ?? []).map((v) => {
          const current = v.sourceId === channel.sourceId && v.id === channel.id;
          return (
            <button
              key={`${v.sourceId}:${v.id}`}
              onClick={() => void pick(v)}
              title={[v.category, v.selected ? "plays by default" : null].filter(Boolean).join(" · ")}
              aria-pressed={current}
              className={clsx(
                "inline-flex h-8 items-center gap-1.5 rounded-lg px-2.5 text-[12.5px] font-semibold ring-1 transition-colors",
                current ? "bg-accent/20 text-fg ring-accent/60" : "bg-white/[0.05] text-dim ring-white/[0.06] hover:bg-white/[0.1] hover:text-fg",
              )}
            >
              {v.label}
              {!v.epgId && !v.now && <span className="text-[10px] font-medium text-faint">no guide</span>}
            </button>
          );
        })}
        {variants.isLoading && <Spinner />}
      </div>
    </div>
  );
}

function PreviewPane({ channel, onChannel }: { channel: Channel | null; onChannel: (c: Channel) => void }) {
  const navigate = useNavigate();
  const holeRef = useRef<HTMLDivElement>(null);
  const now = usePlayer((s) => s.now);
  const status = usePlayer((s) => s.status);
  const error = usePlayer((s) => s.error);
  const buffering = usePlayer((s) => s.props.buffering);
  const previewing = now?.kind === "live";
  useVideoViewport(holeRef, previewing && !browserPreview);

  const shown = channel ?? (previewing ? (now?.channel ?? null) : null);
  const isPlaying = !!shown && previewing && now?.sourceId === shown.sourceId && now?.id === shown.id;

  const t = nowUnix();
  const epg = useQuery({
    queryKey: ["epg", shown?.sourceId, shown?.id],
    queryFn: () => api.epg(shown!.sourceId, shown!.id, t - 3600, t + 12 * 3600),
    enabled: !!shown,
    refetchInterval: 5 * 60_000,
  });
  const programmes = (epg.data ?? []).filter((p) => p.stop > t);
  const current = programmes.find((p) => p.start <= t);
  const upcoming = programmes.filter((p) => p.start > t).slice(0, 6);

  return (
    // The hole's giant box-shadow paints this column's background, leaving
    // only the rounded 16:9 window transparent for the native video.
    <div className="relative flex w-[44%] min-w-[400px] max-w-[760px] shrink-0 flex-col overflow-hidden">
      <div className="p-6 pb-0">
        <div
          ref={holeRef}
          className="relative aspect-video w-full rounded-2xl shadow-[0_0_0_100vmax_var(--color-bg)] ring-1 ring-white/10"
        >
          {(!isPlaying || status !== "playing" || browserPreview) && (
            <div className="absolute inset-0 grid place-items-center rounded-2xl bg-black/70">
              {browserPreview && isPlaying ? (
                <p className="px-6 text-center text-sm text-dim">Live video plays in the desktop app window.</p>
              ) : isPlaying && (status === "loading" || status === "reconnecting") ? (
                <Spinner label={status === "reconnecting" ? "Reconnecting…" : "Tuning…"} />
              ) : isPlaying && status === "error" ? (
                <p className="px-6 text-center text-sm text-live">{error}</p>
              ) : shown ? (
                <ChannelLogo src={shown.logo} title={shown.title} size={96} className="size-24 bg-transparent ring-0" />
              ) : (
                <div className="flex flex-col items-center gap-2 text-dim">
                  <Tv className="size-8" />
                  <span className="text-sm">Select a channel to preview</span>
                </div>
              )}
            </div>
          )}
          {isPlaying && status === "playing" && buffering && (
            <div className="absolute right-3 top-3 rounded-lg bg-black/60 px-2 py-1 text-xs text-white">Buffering…</div>
          )}
        </div>
      </div>
      {shown && (
        <div className="relative flex min-h-0 flex-1 flex-col gap-5 overflow-y-auto p-6">
          <div className="flex items-start gap-4">
            <ChannelLogo src={shown.logo} title={shown.title} size={56} className="size-14" />
            <div className="min-w-0 flex-1">
              <div className="flex items-center gap-2">
                <h3 className="truncate text-xl font-bold tracking-tight">{shown.title}</h3>
                {shown.badges.map((b) => (
                  <Badge key={b}>{b}</Badge>
                ))}
              </div>
              <div className="mt-1 flex items-center gap-2 text-[13px] text-dim">
                <LiveDot /> Live{shown.num ? ` · Channel ${shown.num}` : ""}
                {shown.archive && ` · ${shown.archiveDays}-day catch-up`}
              </div>
            </div>
          </div>
          <div className="flex gap-2.5">
            <Button
              variant="primary"
              icon={<Maximize2 className="size-4" />}
              onClick={() => (isPlaying ? navigate("/player") : void playChannel(shown, navigate))}
            >
              Watch fullscreen
            </Button>
          </div>
          {shown.group && shown.group.variants > 1 && <Feeds channel={shown} playing={isPlaying} onChannel={onChannel} />}
          {current ? (
            <div className="rounded-2xl bg-white/[0.04] p-4 ring-1 ring-white/[0.06]">
              <div className="mb-1 flex items-center justify-between text-[12px] font-semibold uppercase tracking-wider text-accent-strong">
                <span>Now</span>
                <span className="tabular-nums text-dim">
                  {hhmm(current.start)} – {hhmm(current.stop)}
                </span>
              </div>
              <div className="text-[17px] font-semibold">{current.title}</div>
              {current.episode && <div className="text-[13px] text-dim">{current.episode}</div>}
              <ProgressBar value={elapsed(current.start, current.stop)} className="my-3" />
              {current.description && <p className="line-clamp-4 text-[13.5px] leading-relaxed text-dim">{current.description}</p>}
            </div>
          ) : (
            !epg.isLoading && <p className="text-sm text-faint">No programme guide for this channel.</p>
          )}
          {upcoming.length > 0 && (
            <div>
              <h4 className="mb-2 text-[12px] font-semibold uppercase tracking-wider text-faint">Up next</h4>
              <div className="flex flex-col">
                {upcoming.map((p) => (
                  <div key={p.start} className="flex gap-4 rounded-lg px-2 py-2 text-[13.5px] hover:bg-white/[0.03]">
                    <span className="w-[72px] shrink-0 whitespace-nowrap tabular-nums text-dim">{hhmm(p.start)}</span>
                    <span className="min-w-0 truncate">{p.title}</span>
                  </div>
                ))}
              </div>
            </div>
          )}
        </div>
      )}
    </div>
  );
}
