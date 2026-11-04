import clsx from "clsx";
import { keepPreviousData, useQuery } from "@tanstack/react-query";
import { Film, Heart, LayoutGrid, X } from "lucide-react";
import { useEffect, useMemo, useState } from "react";
import { useNavigate, useSearchParams } from "react-router";
import { api } from "../lib/api";
import { itemKey } from "../lib/item";
import { ROUTES } from "../lib/routes";
import { groupBy } from "../lib/groupBy";
import { queryKeys } from "../lib/queryKeys";
import type { FacetFilter, FacetName, Facets, FacetValue, MediaQuery, MediaSort, Movie, Series, WorkKind } from "../lib/types";
import { useFolds } from "../hooks/useFolds";
import { PosterCard } from "./media";
import { PosterGrid } from "./PosterGrid";
import { Badge, EmptyState, FoldHeader, NavRow, SearchInput, Segmented, Spinner } from "./ui";

/** How each kind is named in the browser. */
const KIND_TEXT: Record<WorkKind, { title: string; plural: string; singular: string }> = {
  movie: { title: "Movies", plural: "movies", singular: "movie" },
  series: { title: "Series", plural: "series", singular: "series" },
};

const SORTS: { value: MediaSort; label: string }[] = [
  { value: "added", label: "Recent" },
  { value: "title", label: "A–Z" },
  { value: "rating", label: "Rating" },
  { value: "year", label: "Year" },
];

/** Panel sections in display order; `rows` = values shown before "Show all". */
const SECTIONS: { facet: FacetName; title: string; rows: number }[] = [
  { facet: "service", title: "Services", rows: 8 },
  { facet: "genre", title: "Genres", rows: 10 },
  // TMDB (with an API key): the TV network, the movie series
  { facet: "network", title: "Networks", rows: 8 },
  { facet: "franchise", title: "Collections", rows: 6 },
  { facet: "language", title: "Languages", rows: 6 },
  { facet: "original", title: "Original language", rows: 6 },
  { facet: "quality", title: "Quality", rows: 6 },
  { facet: "decade", title: "Decades", rows: 8 },
  { facet: "collection", title: "Provider categories", rows: 0 },
];
const FACET_NAMES = SECTIONS.map((s) => s.facet);
/** Active-filter chip prefixes. */
const FILTER_NAMES: Record<FacetName, string> = {
  service: "Service",
  genre: "Genre",
  network: "Network",
  franchise: "Collection",
  language: "Language",
  original: "Original language",
  quality: "Quality",
  decade: "Decade",
  collection: "Category",
};

/** Browse filters from the URL (`?service=Netflix&genre=Crime&fav=1`). */
function filtersFromParams(params: URLSearchParams): FacetFilter[] {
  return FACET_NAMES.flatMap((facet) => {
    const value = params.get(facet);
    return value ? [{ facet, value }] : [];
  });
}

export function LibraryBrowser({ kind }: { kind: WorkKind }) {
  const navigate = useNavigate();
  const [params, setParams] = useSearchParams();
  const sort = (params.get("sort") as MediaSort) ?? "added";
  const favorites = params.get("fav") === "1";
  const facets = filtersFromParams(params);
  const [q, setQ] = useState(params.get("q") ?? "");
  const [debounced, setDebounced] = useState(q);
  useEffect(() => {
    const t = window.setTimeout(() => setDebounced(q.trim()), 250);
    return () => window.clearTimeout(t);
  }, [q]);

  const update = (patch: Record<string, string | null>) => {
    const next = new URLSearchParams(params);
    for (const [k, v] of Object.entries(patch)) {
      if (v === null || v === "") next.delete(k);
      else next.set(k, v);
    }
    setParams(next, { replace: true });
  };
  useEffect(() => {
    if ((params.get("q") ?? "") !== debounced) update({ q: debounced || null });
  }, [debounced]);

  const query: MediaQuery = { favorites: favorites || undefined, q: debounced || undefined, sort, facets };
  const facetQuery = useQuery({
    queryKey: [...queryKeys.works(kind), "facets", { favorites, q: debounced, facets }],
    queryFn: () => api.workFacets(kind, { favorites: favorites || undefined, q: debounced || undefined, facets }),
    placeholderData: keepPreviousData,
  });

  const title = KIND_TEXT[kind].title;
  const labelOf = (f: FacetFilter) =>
    facetQuery.data?.[f.facet].find((v) => v.value === f.value)?.label ?? f.value;
  const clearAll = () => {
    const next = new URLSearchParams();
    if (params.get("sort")) next.set("sort", params.get("sort")!);
    if (params.get("q")) next.set("q", params.get("q")!);
    setParams(next, { replace: true });
  };

  const header = (
    <div className="sticky top-0 z-10 bg-bg/90 pb-4 pt-6 backdrop-blur-xl">
      <div className="flex flex-wrap items-center justify-between gap-4 px-10">
        <div className="flex min-w-0 items-baseline gap-3">
          <h1 className="text-3xl font-bold tracking-tight">{title}</h1>
          {facetQuery.data && <span className="text-sm tabular-nums text-dim">{facetQuery.data.total.toLocaleString()}</span>}
        </div>
        <div className="flex items-center gap-3">
          <SearchInput value={q} onChange={setQ} placeholder={`Filter ${KIND_TEXT[kind].plural}`} className="w-64" />
          <Segmented value={sort} options={SORTS} onChange={(v) => update({ sort: v === "added" ? null : v })} />
        </div>
      </div>
      {(facets.length > 0 || favorites) && (
        <div className="mt-3 flex flex-wrap items-center gap-2 px-10">
          {favorites && (
            <FilterChip onRemove={() => update({ fav: null })}>
              <Heart className="size-3.5 fill-live text-live" /> Favorites
            </FilterChip>
          )}
          {facets.map((f) => (
            <FilterChip key={f.facet} onRemove={() => update({ [f.facet]: null })}>
              <span className="text-faint">{FILTER_NAMES[f.facet]}:</span> {labelOf(f)}
            </FilterChip>
          ))}
          <button onClick={clearAll} className="ml-1 text-[13px] font-semibold text-dim hover:text-fg">
            Clear all
          </button>
        </div>
      )}
    </div>
  );

  const empty = favorites && !facets.length && !debounced ? (
    <EmptyState icon={<Heart />} title="No favorites yet" text={`Tap the heart on any ${KIND_TEXT[kind].singular} to collect it here.`} />
  ) : (
    <EmptyState icon={<Film />} title="Nothing found" text="No title matches all of these filters." />
  );

  return (
    <div className="flex h-full">
      <FacetPanel
        kind={kind}
        facets={facetQuery.data}
        loading={facetQuery.isLoading}
        active={facets}
        favorites={favorites}
        onAll={clearAll}
        onFavorites={() => update({ fav: favorites ? null : "1" })}
        onToggle={(facet, value) => update({ [facet]: params.get(facet) === value ? null : value })}
      />
      <div className="min-w-0 flex-1">
        {kind === "movie" ? (
          <PosterGrid<Movie>
            queryKey={[...queryKeys.works("movie"), "grid", query]}
            fetchPage={(offset, limit) => api.movies({ ...query, offset, limit })}
            itemKey={(m) => m.key ?? itemKey(m)}
            header={header}
            empty={empty}
            renderItem={(m, w) => (
              <PosterCard
                title={m.title}
                subtitle={workSubtitle(m.year, m)}
                image={m.poster}
                rating={m.rating}
                progress={m.progress}
                watched={m.watched}
                favorite={m.favorite}
                badge={<QualityBadges quality={m.quality} />}
                width={w}
                onClick={() => navigate(ROUTES.movie(m))}
              />
            )}
          />
        ) : (
          <PosterGrid<Series>
            queryKey={[...queryKeys.works("series"), "grid", query]}
            fetchPage={(offset, limit) => api.series({ ...query, offset, limit })}
            itemKey={(s) => s.key ?? itemKey(s)}
            header={header}
            empty={empty}
            renderItem={(s, w) => (
              <PosterCard
                title={s.title}
                subtitle={workSubtitle(s.year, s, s.genre?.split(/[,/]/)[0]?.trim())}
                image={s.cover}
                rating={s.rating}
                favorite={s.favorite}
                badge={<QualityBadges quality={s.quality} />}
                width={w}
                onClick={() => navigate(ROUTES.series(s))}
              />
            )}
          />
        )}
      </div>
    </div>
  );
}

/** "2019 · 6 versions", else "2019 · Netflix" / the genre. */
export function workSubtitle(year: number | null | undefined, w: { versionCount: number; services: string[] }, fallback?: string) {
  const extra = w.versionCount > 1 ? `${w.versionCount} versions` : (w.services[0] ?? fallback);
  return [year, extra].filter(Boolean).join(" · ") || undefined;
}

/** Poster corner badges: the best picture a title is available in. */
export function QualityBadges({ quality }: { quality: string[] }) {
  const shown = [quality.includes("4K") && "4K", quality.includes("Dolby Vision") && "DV"].filter(Boolean) as string[];
  if (!shown.length) return null;
  return (
    <>
      {/* no backdrop blur over artwork: see the rating chip in media.tsx */}
      {shown.map((b) => (
        <Badge key={b} tone="gold" className="bg-black/75">
          {b}
        </Badge>
      ))}
    </>
  );
}

function FilterChip({ children, onRemove }: { children: React.ReactNode; onRemove: () => void }) {
  return (
    <span className="inline-flex h-7 items-center gap-1.5 rounded-full bg-white/[0.08] pl-3 pr-1 text-[13px] font-medium text-fg">
      {children}
      <button aria-label="Remove filter" onClick={onRemove} className="grid size-5 place-items-center rounded-full text-dim hover:bg-white/15 hover:text-fg">
        <X className="size-3.5" />
      </button>
    </span>
  );
}

// ------------------------------------------------------------ facet panel

function FacetPanel({
  kind,
  facets,
  loading,
  active,
  favorites,
  onAll,
  onFavorites,
  onToggle,
}: {
  kind: WorkKind;
  facets?: Facets;
  loading: boolean;
  active: FacetFilter[];
  favorites: boolean;
  onAll: () => void;
  onFavorites: () => void;
  onToggle: (facet: FacetName, value: string) => void;
}) {
  // collapsed sections; provider categories start collapsed (there are ~80)
  const folds = useFolds(`library.collapsed.${kind}`, (facet) => facet === "collection");
  const [expanded, setExpanded] = useState<Record<string, boolean>>({});
  const isActive = (facet: FacetName, value: string) => active.some((f) => f.facet === facet && f.value === value);

  return (
    <nav aria-label="Browse" className="flex w-[248px] shrink-0 flex-col border-r border-line bg-panel">
      <div className="flex-1 overflow-y-auto px-2 pb-8 pt-6">
        <NavRow dense icon={<LayoutGrid className="size-4" />} active={!active.length && !favorites} onClick={onAll}>
          All {KIND_TEXT[kind].plural}
        </NavRow>
        <NavRow dense icon={<Heart className={clsx("size-4", favorites && "fill-live text-live")} />} active={favorites} onClick={onFavorites}>
          Favorites
        </NavRow>
        {loading && <Spinner className="px-3 py-4" />}
        {facets &&
          SECTIONS.map(({ facet, title, rows }) => {
            const values = facets[facet];
            const selected = active.find((f) => f.facet === facet);
            if (!values.length && !selected) return null;
            const open = !folds.isCollapsed(facet) || !!selected;
            return (
              <section key={facet} className="mt-4">
                <FoldHeader open={open} onToggle={() => folds.toggle(facet)} count={values.length}>
                  {title}
                </FoldHeader>
                {open &&
                  (facet === "collection" ? (
                    <CollectionList values={values} isActive={(v) => isActive(facet, v)} onToggle={(v) => onToggle(facet, v)} />
                  ) : (
                    <ValueList
                      values={values}
                      limit={expanded[facet] ? Infinity : rows}
                      isActive={(v) => isActive(facet, v)}
                      onToggle={(v) => onToggle(facet, v)}
                      onMore={() => setExpanded((e) => ({ ...e, [facet]: !e[facet] }))}
                      more={!!expanded[facet]}
                    />
                  ))}
              </section>
            );
          })}
      </div>
    </nav>
  );
}

function ValueList({
  values,
  limit,
  isActive,
  onToggle,
  onMore,
  more,
}: {
  values: FacetValue[];
  limit: number;
  isActive: (value: string) => boolean;
  onToggle: (value: string) => void;
  onMore: () => void;
  more: boolean;
}) {
  // the chosen value stays visible even beyond the limit
  const shown = values.filter((v, i) => i < limit || isActive(v.value));
  return (
    <div className="flex flex-col">
      {shown.map((v) => (
        <NavRow key={v.value} dense active={isActive(v.value)} onClick={() => onToggle(v.value)} count={v.count}>
          {v.label}
        </NavRow>
      ))}
      {values.length > limit && !more && (
        <button onClick={onMore} className="px-3 py-1.5 text-left text-[12.5px] font-semibold text-accent-strong hover:text-fg">
          Show all {values.length}
        </button>
      )}
      {more && (
        <button onClick={onMore} className="px-3 py-1.5 text-left text-[12.5px] font-semibold text-accent-strong hover:text-fg">
          Show less
        </button>
      )}
    </div>
  );
}

/** Provider categories under the service/language they belong to. */
function CollectionList({
  values,
  isActive,
  onToggle,
}: {
  values: FacetValue[];
  isActive: (value: string) => boolean;
  onToggle: (value: string) => void;
}) {
  const groups = useMemo(() => {
    // bigger groups first, "Other" last
    return [...groupBy(values, (v) => v.group ?? "Other")].sort(
      (a, b) => Number(a[0] === "Other") - Number(b[0] === "Other") || b[1].length - a[1].length || a[0].localeCompare(b[0]),
    );
  }, [values]);
  return (
    <div className="flex flex-col">
      {groups.map(([group, vs]) => (
        <div key={group} className="mt-1">
          <div className="px-3 pb-0.5 pt-1.5 text-[11.5px] font-semibold text-dim">{group}</div>
          {vs.map((v) => (
            <NavRow key={v.value} dense inset active={isActive(v.value)} onClick={() => onToggle(v.value)} count={v.count}>
              {v.label}
            </NavRow>
          ))}
        </div>
      ))}
    </div>
  );
}
