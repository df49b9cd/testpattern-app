import { useQuery } from "@tanstack/react-query";
import { Info, Play, Star } from "lucide-react";
import { useNavigate } from "react-router";
import { api } from "../lib/api";
import { elapsed, hhmm } from "../lib/format";
import { playChannel, playEpisode, resumeHistory } from "../lib/play";
import type { Channel, HistoryItem, Series } from "../lib/types";
import { Artwork, ChannelLogo, PosterCard, Shelf, ShelfItem } from "../components/media";
import { Button, LiveDot, ProgressBar } from "../components/ui";

export function HomePage() {
  const navigate = useNavigate();
  const cont = useQuery({ queryKey: ["continue"], queryFn: () => api.continueWatching(20) });
  const upNext = useQuery({ queryKey: ["continue", "up-next"], queryFn: () => api.upNext(20) });
  const favs = useQuery({ queryKey: ["channels", "favorites"], queryFn: () => api.channels({ favorites: true, limit: 50 }) });
  const recent = useQuery({ queryKey: ["recent-channels"], queryFn: () => api.recentChannels(20) });
  const movies = useQuery({ queryKey: ["movies", "home"], queryFn: () => api.movies({ sort: "added", limit: 24 }) });
  const series = useQuery({ queryKey: ["series", "home"], queryFn: () => api.series({ sort: "added", limit: 24 }) });

  const favMovies = useQuery({ queryKey: ["movies", "fav-home"], queryFn: () => api.movies({ favorites: true, limit: 30 }) });
  const favSeries = useQuery({ queryKey: ["series", "fav-home"], queryFn: () => api.series({ favorites: true, limit: 30 }) });
  const myList = [
    ...(favMovies.data?.items ?? []).map((m) => ({ kind: "movie" as const, key: `m-${m.sourceId}-${m.id}`, title: m.title, sub: m.year, image: m.poster, rating: m.rating, href: `/movies/${m.sourceId}/${m.id}` })),
    ...(favSeries.data?.items ?? []).map((s) => ({ kind: "series" as const, key: `s-${s.sourceId}-${s.id}`, title: s.title, sub: s.year, image: s.cover, rating: s.rating, href: `/series/${s.sourceId}/${s.id}` })),
  ];

  const featured = series.data?.items.find((s) => s.backdrop) ?? null;

  return (
    <div className="pb-12">
      <Hero series={featured} />
      <div className="-mt-24 relative flex flex-col gap-8">
        {!!cont.data?.length && (
          <Shelf title="Continue Watching" itemWidth={300}>
            {cont.data.map((h) => (
              <ShelfItem key={`${h.kind}-${h.sourceId}-${h.itemId}`} width={300}>
                <ContinueCard item={h} onClick={() => void resumeHistory(h, navigate)} />
              </ShelfItem>
            ))}
          </Shelf>
        )}
        {myList.length > 0 && (
          <Shelf title="My List">
            {myList.map((x) => (
              <ShelfItem key={x.key} width={170}>
                <PosterCard title={x.title} subtitle={x.sub ?? undefined} image={x.image} rating={x.rating} favorite width={170} onClick={() => navigate(x.href)} />
              </ShelfItem>
            ))}
          </Shelf>
        )}
        {!!upNext.data?.length && (
          <Shelf title="Up Next" itemWidth={300}>
            {upNext.data.map(({ series, episode }) => (
              <ShelfItem key={`${series.sourceId}-${episode.id}`} width={300}>
                <button onClick={() => void playEpisode(series, episode, navigate, true)} className="group flex w-full flex-col gap-2 text-left">
                  <div className="relative aspect-video w-full overflow-hidden rounded-xl bg-raised ring-1 ring-white/[0.06] transition-transform duration-300 group-hover:-translate-y-1">
                    <Artwork src={episode.image ?? series.backdrop ?? series.cover} width={300} alt={episode.title} className="absolute inset-0" />
                    <div className="absolute inset-0 bg-gradient-to-t from-black/80 via-black/10 to-transparent" />
                    <span className="absolute bottom-2.5 left-3 rounded-md bg-accent px-1.5 py-0.5 text-[10px] font-bold uppercase text-white">
                      Next episode
                    </span>
                  </div>
                  <div className="min-w-0 px-0.5">
                    <div className="truncate text-[13.5px] font-semibold">{series.title}</div>
                    <div className="truncate text-xs text-dim">
                      S{episode.season} E{episode.episode} · {episode.title}
                    </div>
                  </div>
                </button>
              </ShelfItem>
            ))}
          </Shelf>
        )}
        {!!favs.data?.items.length && (
          <Shelf title="Favorite Channels" itemWidth={260}>
            {favs.data.items.map((c) => (
              <ShelfItem key={`${c.sourceId}-${c.id}`} width={260}>
                <ChannelCard channel={c} onClick={() => void playChannel(c, navigate, favs.data.items)} />
              </ShelfItem>
            ))}
          </Shelf>
        )}
        {!!recent.data?.length && (
          <Shelf title="Recently Watched Channels" itemWidth={260}>
            {recent.data.map((c) => (
              <ShelfItem key={`${c.sourceId}-${c.id}`} width={260}>
                <ChannelCard channel={c} onClick={() => void playChannel(c, navigate, recent.data)} />
              </ShelfItem>
            ))}
          </Shelf>
        )}
        {!!movies.data?.items.length && (
          <Shelf
            title="Recently Added Movies"
            action={<SeeAll onClick={() => navigate("/movies")} />}
          >
            {movies.data.items.map((m) => (
              <ShelfItem key={`${m.sourceId}-${m.id}`} width={170}>
                <PosterCard
                  title={m.title}
                  subtitle={[m.year, m.tag].filter(Boolean).join(" · ") || undefined}
                  image={m.poster}
                  rating={m.rating}
                  progress={m.progress}
                  watched={m.watched}
                  favorite={m.favorite}
                  width={170}
                  onClick={() => navigate(`/movies/${m.sourceId}/${m.id}`)}
                />
              </ShelfItem>
            ))}
          </Shelf>
        )}
        {!!series.data?.items.length && (
          <Shelf title="Recently Updated Series" action={<SeeAll onClick={() => navigate("/series")} />}>
            {series.data.items.map((s) => (
              <ShelfItem key={`${s.sourceId}-${s.id}`} width={170}>
                <PosterCard
                  title={s.title}
                  subtitle={s.year ?? undefined}
                  image={s.cover}
                  rating={s.rating}
                  favorite={s.favorite}
                  width={170}
                  onClick={() => navigate(`/series/${s.sourceId}/${s.id}`)}
                />
              </ShelfItem>
            ))}
          </Shelf>
        )}
      </div>
    </div>
  );
}

function SeeAll({ onClick }: { onClick: () => void }) {
  return (
    <button onClick={onClick} className="text-[13px] font-semibold text-dim transition-colors hover:text-fg">
      See all
    </button>
  );
}

function Hero({ series }: { series: Series | null }) {
  const navigate = useNavigate();
  const detail = useQuery({
    queryKey: ["series-detail", series?.sourceId, series?.id],
    queryFn: () => api.seriesDetail(series!.sourceId, series!.id),
    enabled: !!series,
    staleTime: 10 * 60_000,
  });
  if (!series) return <div className="h-40" />;
  const d = detail.data;
  return (
    <section className="relative h-[62vh] min-h-[420px] w-full overflow-hidden">
      <Artwork src={series.backdrop} width={1600} alt={series.title} className="absolute inset-0" />
      <div className="absolute inset-0 scrim-l" />
      <div className="absolute inset-0 scrim-b" />
      <div className="relative flex h-full max-w-2xl flex-col justify-end gap-4 px-10 pb-36 animate-rise-in">
        <span className="text-xs font-bold uppercase tracking-[0.2em] text-accent-strong">New episodes</span>
        <h1 className="text-5xl leading-[1.05] font-extrabold tracking-tight drop-shadow-lg">{series.title}</h1>
        <div className="flex flex-wrap items-center gap-x-3 gap-y-1 text-sm text-dim">
          {series.year && <span>{series.year}</span>}
          {series.genre && <span>{series.genre}</span>}
          {series.rating ? (
            <span className="inline-flex items-center gap-1">
              <Star className="size-3.5 fill-gold text-gold" /> {series.rating.toFixed(1)}
            </span>
          ) : null}
          {d && d.seasons.length > 0 && <span>{d.seasons.length} season{d.seasons.length > 1 ? "s" : ""}</span>}
        </div>
        {d?.plot && <p className="line-clamp-3 max-w-xl text-[15px] leading-relaxed text-fg/80">{d.plot}</p>}
        <div className="mt-2 flex gap-3">
          <Button variant="primary" size="lg" icon={<Play className="size-5 fill-current" />} onClick={() => navigate(`/series/${series.sourceId}/${series.id}`)}>
            {d?.resume?.started ? `Resume S${d.resume.season} E${d.resume.episode}` : "Watch now"}
          </Button>
          <Button variant="glass" size="lg" icon={<Info className="size-5" />} onClick={() => navigate(`/series/${series.sourceId}/${series.id}`)}>
            Details
          </Button>
        </div>
      </div>
    </section>
  );
}

function ContinueCard({ item, onClick }: { item: HistoryItem; onClick: () => void }) {
  const progress = item.duration > 0 ? item.position / item.duration : 0;
  const remaining = Math.max(0, item.duration - item.position);
  return (
    <button onClick={onClick} className="group flex w-full flex-col gap-2 text-left">
      <div className="relative aspect-video w-full overflow-hidden rounded-xl bg-raised ring-1 ring-white/[0.06] transition-transform duration-300 group-hover:-translate-y-1">
        <Artwork src={item.backdrop ?? item.image} width={300} alt={item.title} className="absolute inset-0" />
        <div className="absolute inset-0 bg-gradient-to-t from-black/80 via-black/10 to-transparent" />
        <div className="absolute inset-0 grid place-items-center opacity-0 transition-opacity group-hover:opacity-100">
          <span className="grid size-12 place-items-center rounded-full bg-white/90 text-black">
            <Play className="size-5 translate-x-px fill-current" />
          </span>
        </div>
        <div className="absolute inset-x-3 bottom-2.5 flex flex-col gap-1.5">
          <span className="text-[11px] font-semibold text-white/80">{Math.round(remaining / 60)} min left</span>
          <ProgressBar value={progress} className="bg-white/25" />
        </div>
      </div>
      <div className="min-w-0 px-0.5">
        <div className="truncate text-[13.5px] font-semibold">{item.title}</div>
        {item.subtitle && (
          <div className="truncate text-xs text-dim">
            {item.kind === "episode" && item.season != null ? `S${item.season} E${item.episode} · ` : ""}
            {item.subtitle}
          </div>
        )}
      </div>
    </button>
  );
}

export function ChannelCard({ channel, onClick }: { channel: Channel; onClick: () => void }) {
  const now = channel.now;
  return (
    <button onClick={onClick} className="group flex w-full flex-col gap-2 text-left">
      <div className="relative flex aspect-video w-full items-center justify-center overflow-hidden rounded-xl bg-gradient-to-br from-[#1d1d2b] to-[#111118] ring-1 ring-white/[0.06] transition-transform duration-300 group-hover:-translate-y-1">
        <ChannelLogo src={channel.logo} title={channel.title} className="size-20 bg-transparent ring-0" />
        <span className="absolute left-2.5 top-2.5 inline-flex items-center gap-1.5 rounded-md bg-black/50 px-1.5 py-0.5 text-[10px] font-bold uppercase text-white/90">
          <LiveDot /> Live
        </span>
        {now && <ProgressBar value={elapsed(now.start, now.stop)} tone="live" className="absolute inset-x-3 bottom-2.5 bg-white/15" />}
      </div>
      <div className="min-w-0 px-0.5">
        <div className="truncate text-[13.5px] font-semibold">{channel.title}</div>
        <div className="truncate text-xs text-dim">
          {now ? `${hhmm(now.start)} · ${now.title}` : "No programme information"}
        </div>
      </div>
    </button>
  );
}
