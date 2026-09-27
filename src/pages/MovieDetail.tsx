import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Check, Heart, Play, RotateCcw, Star, MonitorPlay } from "lucide-react";
import { useNavigate, useParams } from "react-router";
import { api } from "../lib/api";
import { clock, duration, resolutionLabel } from "../lib/format";
import { openExternal, youtubeUrl } from "../lib/open";
import { playMovie } from "../lib/play";
import { DetailHero, Facts, MetaDot } from "../components/DetailHero";
import { VersionPill, Versions } from "../components/Versions";
import { Badge, Button, EmptyState, IconButton, ProgressBar, Spinner } from "../components/ui";

export function MovieDetailPage() {
  const { sourceId, id } = useParams();
  const navigate = useNavigate();
  const qc = useQueryClient();
  const sid = Number(sourceId);
  const q = useQuery({
    queryKey: ["movie-detail", sid, id],
    queryFn: () => api.movieDetail(sid, id!),
    enabled: !!id,
    // other versions' details still loading: ask again a few times
    refetchInterval: (query) => (query.state.data?.versionsPending && query.state.dataUpdateCount < 6 ? 3000 : false),
  });

  if (q.isLoading) return <Spinner className="p-10" label="Loading…" />;
  if (q.error || !q.data) return <EmptyState title="Movie not available" text={String(q.error ?? "")} />;
  const m = q.data;

  const resumable = m.position > 60 && !m.watched && (!m.duration || m.position < m.duration * 0.95);
  const res = resolutionLabel(m.video?.width, m.video?.height);
  const channels = m.audio?.channels;

  const refresh = () => {
    void qc.invalidateQueries({ queryKey: ["movie-detail", sid, id] });
    void qc.invalidateQueries({ queryKey: ["movies"] });
    void qc.invalidateQueries({ queryKey: ["continue"] });
  };

  return (
    <div className="pb-16">
      <DetailHero
        backdrop={m.backdrop}
        poster={m.poster}
        title={m.title}
        meta={
          <>
            {m.year && <span>{m.year}</span>}
            {m.duration ? (
              <>
                <MetaDot />
                <span>{duration(m.duration)}</span>
              </>
            ) : null}
            {m.genre && (
              <>
                <MetaDot />
                <span>{m.genre}</span>
              </>
            )}
            {m.rating ? (
              <>
                <MetaDot />
                <span className="inline-flex items-center gap-1">
                  <Star className="size-4 fill-gold text-gold" />
                  {m.rating.toFixed(1)}
                </span>
              </>
            ) : null}
            {m.ageRating && <Badge>{m.ageRating}</Badge>}
          </>
        }
        badges={
          <>
            {res && <Badge>{res}</Badge>}
            {m.video?.hdr && <Badge tone="gold">HDR</Badge>}
            {m.video?.codec && <Badge>{m.video.codec}</Badge>}
            {channels && channels > 2 && <Badge>{channels === 6 ? "5.1" : channels === 8 ? "7.1" : `${channels}ch`}</Badge>}
            {m.ext && <Badge>{m.ext}</Badge>}
            {m.watched && <Badge tone="accent">Watched</Badge>}
            <VersionPill versions={m.versions} />
          </>
        }
        actions={
          <>
            <Button variant="primary" size="lg" icon={<Play className="size-5 fill-current" />} onClick={() => void playMovie(m, navigate)}>
              {resumable ? `Resume from ${clock(m.position)}` : "Play"}
            </Button>
            {resumable && (
              <Button variant="glass" size="lg" icon={<RotateCcw className="size-5" />} onClick={() => void playMovie(m, navigate, true)}>
                From start
              </Button>
            )}
            {m.trailer && (
              <Button variant="glass" size="lg" icon={<MonitorPlay className="size-5" />} onClick={() => void openExternal(youtubeUrl(m.trailer!))}>
                Trailer
              </Button>
            )}
            <IconButton
              label={m.favorite ? "Remove from favorites" : "Add to favorites"}
              variant="glass"
              size="lg"
              onClick={async () => {
                await api.toggleFavorite("movie", m.sourceId, m.id);
                refresh();
              }}
            >
              <Heart className={m.favorite ? "size-5 fill-live text-live" : "size-5"} />
            </IconButton>
            <IconButton
              label={m.watched ? "Mark as unwatched" : "Mark as watched"}
              variant="glass"
              size="lg"
              onClick={async () => {
                await api.markWatched("movie", m.sourceId, m.id, !m.watched, {
                  title: m.title,
                  subtitle: m.year ? String(m.year) : null,
                  image: m.poster,
                  backdrop: m.backdrop,
                  ext: m.ext,
                  duration: m.duration,
                });
                refresh();
              }}
            >
              <Check className={m.watched ? "size-5 text-ok" : "size-5"} />
            </IconButton>
          </>
        }
        plot={m.plot}
      />
      {resumable && m.duration ? (
        <div className="relative -mt-4 mb-8 flex max-w-xl items-center gap-3 px-10 text-sm text-dim">
          <ProgressBar value={m.position / m.duration} className="flex-1" />
          <span>{Math.round((m.duration - m.position) / 60)} min left</span>
        </div>
      ) : null}
      {/* positioned: the hero's backdrop reaches down here */}
      <div className="relative mb-10">
        <Versions
          kind="movie"
          versions={m.versions}
          pending={m.versionsPending}
          onPicked={() => {
            void qc.invalidateQueries({ queryKey: ["movie-detail", sid, id] });
            void qc.invalidateQueries({ queryKey: ["movies"] });
          }}
        />
      </div>
      <Facts
        items={[
          ["Director", m.director],
          ["Cast", m.cast],
          ["Genre", m.genre],
          ["Released", m.releaseDate],
          // TMDB's when known: the provider's field often lists languages ("English, Español")
          ["Country", m.tmdb?.countries.length ? m.tmdb.countries.join(", ") : m.country],
          ["Original language", m.tmdb?.originalLanguage],
          [
            "Collection",
            m.tmdb?.collection && (
              <button className="font-medium text-accent-strong hover:text-fg" onClick={() => navigate(`/movies?franchise=${encodeURIComponent(m.tmdb!.collection!)}`)}>
                {m.tmdb.collection}
              </button>
            ),
          ],
        ]}
      />
    </div>
  );
}
