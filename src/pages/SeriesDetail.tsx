import clsx from "clsx";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Check, CircleCheck, Heart, MonitorPlay, Play, Star } from "lucide-react";
import { useEffect, useState } from "react";
import { useNavigate, useParams, useSearchParams } from "react-router";
import { api } from "../lib/api";
import { duration, resolutionLabel } from "../lib/format";
import { openExternal, youtubeUrl } from "../lib/open";
import { playEpisode } from "../lib/play";
import type { Episode, SeriesDetail } from "../lib/types";
import { DetailHero, Facts, MetaDot } from "../components/DetailHero";
import { VersionPill, Versions } from "../components/Versions";
import { Artwork } from "../components/media";
import { Badge, Button, EmptyState, IconButton, ProgressBar, Spinner } from "../components/ui";

export function SeriesDetailPage() {
  const { sourceId, id } = useParams();
  const [params, setParams] = useSearchParams();
  const navigate = useNavigate();
  const qc = useQueryClient();
  const sid = Number(sourceId);
  const q = useQuery({
    queryKey: ["series-detail", sid, id],
    queryFn: () => api.seriesDetail(sid, id!),
    enabled: !!id,
    // other versions' episode lists still loading: ask again a few times
    refetchInterval: (query) => (query.state.data?.versionsPending && query.state.dataUpdateCount < 6 ? 3000 : false),
  });
  const s = q.data;
  const [season, setSeason] = useState<number | null>(() => (params.get("season") ? Number(params.get("season")) : null));

  // default to the season of the episode we'd resume
  useEffect(() => {
    if (season === null && s?.seasons.length) setSeason(s.resume?.season ?? s.seasons[0].season);
  }, [s, season]);

  if (q.isLoading) return <Spinner className="p-10" label="Loading…" />;
  if (q.error || !s) return <EmptyState title="Series not available" text={String(q.error ?? "")} />;

  const current = s.seasons.find((x) => x.season === season) ?? s.seasons[0];
  const allEpisodes = s.seasons.flatMap((x) => x.episodes);
  const resumeEp = s.resume ? allEpisodes.find((e) => e.id === s.resume!.episodeId) : undefined;
  const refresh = () => {
    void qc.invalidateQueries({ queryKey: ["series-detail", sid, id] });
    void qc.invalidateQueries({ queryKey: ["series"] });
    void qc.invalidateQueries({ queryKey: ["continue"] });
  };
  const pickSeason = (n: number) => {
    setSeason(n);
    setParams({ season: String(n) }, { replace: true });
  };

  return (
    <div className="pb-16">
      <DetailHero
        backdrop={s.backdrop}
        poster={s.cover}
        title={s.title}
        meta={
          <>
            {s.year && <span>{s.year}</span>}
            {s.seasons.length > 0 && (
              <>
                <MetaDot />
                <span>
                  {s.seasons.length} season{s.seasons.length > 1 ? "s" : ""}
                </span>
              </>
            )}
            {s.genre && (
              <>
                <MetaDot />
                <span>{s.genre}</span>
              </>
            )}
            {s.rating ? (
              <>
                <MetaDot />
                <span className="inline-flex items-center gap-1">
                  <Star className="size-4 fill-gold text-gold" />
                  {s.rating.toFixed(1)}
                </span>
              </>
            ) : null}
          </>
        }
        badges={s.versions.length > 1 ? <VersionPill versions={s.versions} /> : undefined}
        actions={
          <>
            {resumeEp && (
              <Button variant="primary" size="lg" icon={<Play className="size-5 fill-current" />} onClick={() => void playEpisode(s, resumeEp, navigate)}>
                {s.resume!.started ? (s.resume!.position > 30 ? "Resume" : "Play") : "Play"} S{resumeEp.season} E{resumeEp.episode}
              </Button>
            )}
            {s.trailer && (
              <Button variant="glass" size="lg" icon={<MonitorPlay className="size-5" />} onClick={() => void openExternal(youtubeUrl(s.trailer!))}>
                Trailer
              </Button>
            )}
            <IconButton
              label={s.favorite ? "Remove from favorites" : "Add to favorites"}
              variant="glass"
              size="lg"
              onClick={async () => {
                await api.toggleFavorite("series", s.sourceId, s.id);
                refresh();
              }}
            >
              <Heart className={s.favorite ? "size-5 fill-live text-live" : "size-5"} />
            </IconButton>
          </>
        }
        plot={s.plot}
      />

      {s.seasons.length === 0 ? (
        <EmptyState className="relative" title="No episodes available" text="The provider has not published any episodes for this series yet." />
      ) : (
        // positioned: the hero's backdrop reaches down here and would cover the tabs
        <section className="relative px-10">
          <div className="no-scrollbar mb-5 flex gap-2 overflow-x-auto">
            {s.seasons.map((x) => (
              <button
                key={x.season}
                onClick={() => pickSeason(x.season)}
                className={clsx(
                  "h-9 shrink-0 rounded-full px-4 text-sm font-semibold transition-colors",
                  x.season === current.season ? "bg-fg text-bg" : "bg-white/[0.07] text-dim hover:bg-white/[0.12] hover:text-fg",
                )}
              >
                {x.name}
              </button>
            ))}
          </div>
          <div className="flex flex-col gap-2">
            {current.episodes.map((e) => (
              <EpisodeRow
                key={e.id}
                series={s}
                episode={e}
                highlighted={e.id === s.resume?.episodeId}
                onPlay={() => void playEpisode(s, e, navigate)}
                onToggleWatched={async () => {
                  // same fields as a playback save (stores/player.ts saveProgress);
                  // the episode's own copy of the series
                  await api.markWatched("episode", e.sourceId, e.id, !e.watched, {
                    seriesId: e.seriesId,
                    season: e.season,
                    episode: e.episode,
                    title: s.title,
                    subtitle: e.title,
                    image: s.cover,
                    backdrop: s.backdrop ?? e.image,
                    ext: e.ext,
                    duration: e.duration,
                  });
                  refresh();
                }}
              />
            ))}
          </div>
        </section>
      )}
      <div className="relative mt-10">
        <Versions
          kind="series"
          versions={s.versions}
          pending={s.versionsPending}
          onPicked={() => {
            void qc.invalidateQueries({ queryKey: ["series-detail", sid, id] });
            void qc.invalidateQueries({ queryKey: ["continue"] });
          }}
        />
      </div>
      <div className="mt-10">
        <Facts
          items={[
            ["Director", s.director],
            ["Cast", s.cast],
            ["Genre", s.genre],
            ["First aired", s.releaseDate],
            [
              "Network",
              s.tmdb?.networks.length ? (
                <span className="flex flex-wrap gap-x-3">
                  {s.tmdb.networks.map((n) => (
                    <button key={n} className="font-medium text-accent-strong hover:text-fg" onClick={() => navigate(`/series?network=${encodeURIComponent(n)}`)}>
                      {n}
                    </button>
                  ))}
                </span>
              ) : null,
            ],
            ["Country", s.tmdb?.countries.length ? s.tmdb.countries.join(", ") : null],
            ["Original language", s.tmdb?.originalLanguage],
          ]}
        />
      </div>
    </div>
  );
}

function EpisodeRow({
  series,
  episode: e,
  highlighted,
  onPlay,
  onToggleWatched,
}: {
  series: SeriesDetail;
  episode: Episode;
  highlighted: boolean;
  onPlay: () => void;
  onToggleWatched: () => void;
}) {
  const progress = e.duration && e.position > 30 && !e.watched ? e.position / e.duration : 0;
  const res = resolutionLabel(e.video?.width, e.video?.height);
  return (
    <div
      className={clsx(
        "group flex items-center gap-5 rounded-2xl p-3 transition-colors",
        highlighted ? "bg-white/[0.07] ring-1 ring-white/10" : "hover:bg-white/[0.04]",
      )}
    >
      <button onClick={onPlay} className="relative aspect-video w-56 shrink-0 overflow-hidden rounded-xl bg-raised ring-1 ring-white/[0.06]">
        <Artwork src={e.image ?? series.backdrop} width={224} alt={e.title} className="absolute inset-0" />
        <span className="absolute inset-0 grid place-items-center bg-black/30 opacity-0 transition-opacity group-hover:opacity-100">
          <span className="grid size-11 place-items-center rounded-full bg-white/90 text-black">
            <Play className="size-5 translate-x-px fill-current" />
          </span>
        </span>
        {progress > 0 && <ProgressBar value={progress} className="absolute inset-x-2 bottom-2 bg-black/50" />}
      </button>
      <button onClick={onPlay} className="min-w-0 flex-1 text-left">
        <div className="flex items-center gap-2">
          <span className="text-sm font-semibold text-faint">{e.episode}</span>
          <span className="truncate text-[15px] font-semibold">{e.title}</span>
          {res && <Badge>{res}</Badge>}
          {e.version && (
            <span
              title={`Not in the chosen version: plays from ${e.version}`}
              className="shrink-0 truncate rounded-[5px] bg-gold/15 px-1.5 text-[11.5px] font-semibold leading-5 text-gold"
            >
              {e.version}
            </span>
          )}
        </div>
        <div className="mt-0.5 flex items-center gap-2 text-[13px] text-faint">
          {e.duration ? <span>{duration(e.duration)}</span> : null}
          {e.airDate && <span>{e.airDate}</span>}
          {e.watched && (
            <span className="inline-flex items-center gap-1 text-ok">
              <CircleCheck className="size-3.5" /> Watched
            </span>
          )}
        </div>
        {e.plot && <p className="mt-1.5 line-clamp-2 text-[13.5px] leading-relaxed text-dim">{e.plot}</p>}
      </button>
      <IconButton label={e.watched ? "Mark as unwatched" : "Mark as watched"} onClick={onToggleWatched} className="opacity-0 group-hover:opacity-100">
        <Check className={clsx("size-4", e.watched && "text-ok")} />
      </IconButton>
    </div>
  );
}
