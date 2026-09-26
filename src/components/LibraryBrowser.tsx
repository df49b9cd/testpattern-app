import clsx from "clsx";
import { useQuery } from "@tanstack/react-query";
import { Film, Heart, Search } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { useNavigate, useSearchParams } from "react-router";
import { api } from "../lib/api";
import type { MediaSort, Movie, Series } from "../lib/types";
import { PosterCard } from "./media";
import { PosterGrid } from "./PosterGrid";
import { EmptyState, Segmented } from "./ui";

type Kind = "movie" | "series";

const SORTS: { value: MediaSort; label: string }[] = [
  { value: "added", label: "Recent" },
  { value: "title", label: "A–Z" },
  { value: "rating", label: "Rating" },
  { value: "year", label: "Year" },
];

export function LibraryBrowser({ kind }: { kind: Kind }) {
  const navigate = useNavigate();
  const [params, setParams] = useSearchParams();
  const cat = params.get("cat") ?? "all";
  const sort = (params.get("sort") as MediaSort) ?? "added";
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

  const categories = useQuery({ queryKey: ["categories", kind], queryFn: () => api.categories(kind) });
  const [sourceId, categoryId] = cat.includes(":") ? [Number(cat.split(":")[0]), cat.slice(cat.indexOf(":") + 1)] : [undefined, undefined];
  const favorites = cat === "favorites";
  const query = { sourceId, categoryId, favorites: favorites || undefined, q: debounced || undefined, sort };

  const title = kind === "movie" ? "Movies" : "Series";
  const chipsRef = useRef<HTMLDivElement>(null);

  const header = (
    <div className="sticky top-0 z-10 bg-bg/90 pb-4 pt-6 backdrop-blur-xl">
      <div className="flex flex-wrap items-end justify-between gap-4 px-10">
        <h1 className="text-3xl font-bold tracking-tight">{title}</h1>
        <div className="flex items-center gap-3">
          <label className="relative flex w-64 items-center">
            <Search className="pointer-events-none absolute left-3 size-4 text-faint" />
            <input
              value={q}
              onChange={(e) => setQ(e.target.value)}
              placeholder={`Filter ${title.toLowerCase()}`}
              className="h-9 w-full rounded-lg bg-white/[0.06] pl-9 pr-3 text-sm outline-none ring-1 ring-white/[0.06] placeholder:text-faint focus:ring-accent"
            />
          </label>
          <Segmented value={sort} options={SORTS} onChange={(v) => update({ sort: v === "added" ? null : v })} />
        </div>
      </div>
      <div ref={chipsRef} className="no-scrollbar mt-4 flex gap-2 overflow-x-auto px-10">
        <Chip active={cat === "all"} onClick={() => update({ cat: null })}>
          All
        </Chip>
        <Chip active={favorites} onClick={() => update({ cat: "favorites" })}>
          <Heart className="size-3.5" /> Favorites
        </Chip>
        {(categories.data ?? []).map((c) => {
          const key = `${c.sourceId}:${c.id}`;
          return (
            <Chip key={key} active={cat === key} onClick={() => update({ cat: key })}>
              {c.region ? `${c.region} · ` : ""}
              {c.title}
              {c.badges.includes("4K") && <span className="text-[10px] font-bold text-gold">4K</span>}
              <span className="text-[11px] tabular-nums text-faint">{c.count}</span>
            </Chip>
          );
        })}
      </div>
    </div>
  );

  const empty = favorites ? (
    <EmptyState icon={<Heart />} title="No favorites yet" text={`Tap the heart on any ${kind === "movie" ? "movie" : "series"} to collect it here.`} />
  ) : (
    <EmptyState icon={<Film />} title="Nothing found" text={debounced ? "Try a different filter." : "This category is empty."} />
  );

  return kind === "movie" ? (
    <PosterGrid<Movie>
      queryKey={["movies", "grid", query]}
      fetchPage={(offset, limit) => api.movies({ ...query, offset, limit })}
      itemKey={(m) => `${m.sourceId}-${m.id}`}
      header={header}
      empty={empty}
      renderItem={(m, w) => (
        <PosterCard
          title={m.title}
          subtitle={[m.year, m.tag].filter(Boolean).join(" · ") || undefined}
          image={m.poster}
          rating={m.rating}
          progress={m.progress}
          watched={m.watched}
          favorite={m.favorite}
          width={w}
          onClick={() => navigate(`/movies/${m.sourceId}/${m.id}`)}
        />
      )}
    />
  ) : (
    <PosterGrid<Series>
      queryKey={["series", "grid", query]}
      fetchPage={(offset, limit) => api.series({ ...query, offset, limit })}
      itemKey={(s) => `${s.sourceId}-${s.id}`}
      header={header}
      empty={empty}
      renderItem={(s, w) => (
        <PosterCard
          title={s.title}
          subtitle={[s.year, s.genre?.split(/[,/]/)[0]?.trim()].filter(Boolean).join(" · ") || undefined}
          image={s.cover}
          rating={s.rating}
          favorite={s.favorite}
          width={w}
          onClick={() => navigate(`/series/${s.sourceId}/${s.id}`)}
        />
      )}
    />
  );
}

function Chip({ active, onClick, children }: { active: boolean; onClick: () => void; children: React.ReactNode }) {
  return (
    <button
      onClick={onClick}
      className={clsx(
        "inline-flex h-8 shrink-0 items-center gap-1.5 whitespace-nowrap rounded-full px-3.5 text-[13px] font-medium transition-colors",
        active ? "bg-fg text-bg" : "bg-white/[0.07] text-dim hover:bg-white/[0.12] hover:text-fg",
      )}
    >
      {children}
    </button>
  );
}
