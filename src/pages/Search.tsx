import { useQuery } from "@tanstack/react-query";
import { History, Search as SearchIcon, X } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { useNavigate, useSearchParams } from "react-router";
import { api } from "../lib/api";
import { elapsed } from "../lib/format";
import { playChannel } from "../lib/play";
import { ChannelLogo, PosterCard, Shelf, ShelfItem } from "../components/media";
import { EmptyState, LiveDot, ProgressBar, Spinner } from "../components/ui";

const RECENT_KEY = "search.recent";

function loadRecent(): string[] {
  try {
    return JSON.parse(localStorage.getItem(RECENT_KEY) ?? "[]");
  } catch {
    return [];
  }
}
function saveRecent(q: string) {
  const list = [q, ...loadRecent().filter((x) => x.toLowerCase() !== q.toLowerCase())].slice(0, 8);
  try {
    localStorage.setItem(RECENT_KEY, JSON.stringify(list));
  } catch {
    /* ignore */
  }
}

export function SearchPage() {
  const navigate = useNavigate();
  const [params, setParams] = useSearchParams();
  const [q, setQ] = useState(params.get("q") ?? "");
  const [term, setTerm] = useState(q.trim());
  const [recent, setRecent] = useState(loadRecent);
  const input = useRef<HTMLInputElement>(null);

  useEffect(() => input.current?.focus(), []);
  useEffect(() => {
    const t = window.setTimeout(() => {
      setTerm(q.trim());
      setParams(q.trim() ? { q: q.trim() } : {}, { replace: true });
    }, 200);
    return () => window.clearTimeout(t);
  }, [q, setParams]);

  const results = useQuery({
    queryKey: ["search", term],
    queryFn: () => api.search(term, 40),
    enabled: term.length >= 2,
    placeholderData: (prev) => prev,
  });
  const r = term.length >= 2 ? results.data : undefined;
  const remember = () => {
    if (term.length >= 2) {
      saveRecent(term);
      setRecent(loadRecent());
    }
  };
  const nothing = r && !r.channels.length && !r.movies.length && !r.series.length;

  return (
    <div className="pb-16">
      <div className="sticky top-0 z-10 bg-bg/90 px-10 pb-5 pt-8 backdrop-blur-xl">
        <label className="relative flex max-w-3xl items-center">
          <SearchIcon className="pointer-events-none absolute left-5 size-6 text-faint" />
          <input
            ref={input}
            value={q}
            onChange={(e) => setQ(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") remember();
              if (e.key === "Escape") setQ("");
            }}
            placeholder="Search channels, movies and series"
            className="h-16 w-full rounded-2xl bg-white/[0.06] pl-14 pr-12 text-xl font-medium outline-none ring-1 ring-white/[0.08] placeholder:text-faint focus:ring-2 focus:ring-accent"
          />
          {q && (
            <button onClick={() => setQ("")} className="absolute right-4 grid size-8 place-items-center rounded-full text-faint hover:bg-white/10 hover:text-fg" aria-label="Clear">
              <X className="size-5" />
            </button>
          )}
        </label>
      </div>

      {term.length < 2 ? (
        recent.length ? (
          <div className="px-10 pt-2">
            <h2 className="mb-3 text-sm font-semibold uppercase tracking-wider text-faint">Recent searches</h2>
            <div className="flex flex-wrap gap-2">
              {recent.map((x) => (
                <button key={x} onClick={() => setQ(x)} className="inline-flex h-9 items-center gap-2 rounded-full bg-white/[0.07] px-4 text-sm text-dim hover:bg-white/[0.12] hover:text-fg">
                  <History className="size-4" />
                  {x}
                </button>
              ))}
            </div>
          </div>
        ) : (
          <EmptyState icon={<SearchIcon />} title="Find anything" text="Search across every live channel, movie and series from all of your sources." />
        )
      ) : results.isLoading && !r ? (
        <Spinner className="px-10 py-6" label="Searching…" />
      ) : nothing ? (
        <EmptyState icon={<SearchIcon />} title={`No results for “${term}”`} text="Check the spelling or try fewer words." />
      ) : r ? (
        <div className="flex flex-col gap-8 pt-2">
          {r.channels.length > 0 && (
            <section className="px-10">
              <h2 className="mb-3 text-[19px] font-bold tracking-tight">Channels</h2>
              <div className="grid grid-cols-1 gap-1 xl:grid-cols-2">
                {r.channels.slice(0, 12).map((c) => (
                  <button
                    key={`${c.sourceId}-${c.id}`}
                    onClick={() => {
                      remember();
                      void playChannel(c, navigate, r.channels);
                    }}
                    className="flex items-center gap-3.5 rounded-xl px-3 py-2.5 text-left transition-colors hover:bg-white/[0.05]"
                  >
                    <ChannelLogo src={c.logo} title={c.title} size={44} className="size-11" />
                    <span className="min-w-0 flex-1">
                      <span className="flex items-center gap-2 truncate text-[14.5px] font-semibold">
                        <LiveDot /> {c.title}
                      </span>
                      {c.now ? (
                        <span className="mt-1 flex items-center gap-2 text-[12.5px] text-dim">
                          <span className="truncate">{c.now.title}</span>
                          <ProgressBar value={elapsed(c.now.start, c.now.stop)} className="h-[3px] w-12 shrink-0" />
                        </span>
                      ) : (
                        <span className="mt-1 block text-[12.5px] text-faint">Live</span>
                      )}
                    </span>
                  </button>
                ))}
              </div>
            </section>
          )}
          {r.movies.length > 0 && (
            <Shelf title="Movies">
              {r.movies.map((m) => (
                <ShelfItem key={`${m.sourceId}-${m.id}`} width={160}>
                  <PosterCard
                    title={m.title}
                    subtitle={[m.year, m.tag].filter(Boolean).join(" · ") || undefined}
                    image={m.poster}
                    rating={m.rating}
                    progress={m.progress}
                    watched={m.watched}
                    width={160}
                    onClick={() => {
                      remember();
                      navigate(`/movies/${m.sourceId}/${m.id}`);
                    }}
                  />
                </ShelfItem>
              ))}
            </Shelf>
          )}
          {r.series.length > 0 && (
            <Shelf title="Series">
              {r.series.map((s) => (
                <ShelfItem key={`${s.sourceId}-${s.id}`} width={160}>
                  <PosterCard
                    title={s.title}
                    subtitle={s.year ?? undefined}
                    image={s.cover}
                    rating={s.rating}
                    width={160}
                    onClick={() => {
                      remember();
                      navigate(`/series/${s.sourceId}/${s.id}`);
                    }}
                  />
                </ShelfItem>
              ))}
            </Shelf>
          )}
        </div>
      ) : null}
    </div>
  );
}
