import clsx from "clsx";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { AudioLines, Check, ChevronDown, Captions, Layers, Loader2, ScanSearch } from "lucide-react";
import { api, errorMessage } from "../lib/api";
import { channelsLabel, duration, languageName, resolutionLabel, seasonsLabel } from "../lib/format";
import type { VersionInfo } from "../lib/types";
import { usePlayer } from "../stores/player";
import { Badge, ProgressBar } from "./ui";

/**
 * Unique, readable track languages, the viewer's own first:
 * "Danish, English, Arabic, Bulgarian +38".
 */
export function languages(list: { lang?: string | null }[] | undefined, preferred: string[] = [], max = 4): string | null {
  const names = [...new Set((list ?? []).map((t) => languageName(t.lang)).filter((n): n is string => !!n))];
  if (!names.length) return list?.length ? `${list.length} track${list.length > 1 ? "s" : ""}` : null;
  const rank = (n: string) => {
    const i = preferred.indexOf(n);
    return i < 0 ? preferred.length : i;
  };
  names.sort((a, b) => rank(a) - rank(b));
  return names.length > max ? `${names.slice(0, max).join(", ")} +${names.length - max}` : names.join(", ");
}

/** The player's preferred audio + subtitle languages as names ("Danish"). */
function usePreferredLanguages(): string[] {
  const settings = useQuery({ queryKey: ["settings"], queryFn: api.settings });
  const codes = ["player.subLang", "player.audioLang"].flatMap((k) => String(settings.data?.[k] ?? "").split(","));
  return [...new Set(codes.map((c) => languageName(c)).filter((n): n is string => !!n))];
}

/** Picture and sound of a version: "4K · HEVC · HDR · 5.1 · 2h 11m · MKV". */
export function techLine(v: VersionInfo): string {
  const video = v.tracks?.video;
  const width = video?.width ?? v.video?.width;
  const height = video?.height ?? v.video?.height;
  const codec = (video?.codec ?? v.video?.codec)?.toUpperCase().replace("H264", "H.264").replace("H265", "HEVC");
  // the richest audio track ("5.1" when there is one)
  const channels = Math.max(0, ...(v.tracks?.audio ?? []).map((a) => a.channels ?? 0)) || v.audio?.channels;
  return [
    resolutionLabel(width, height),
    codec,
    (video?.hdr || v.video?.hdr) && "HDR",
    channelsLabel(channels),
    v.duration ? duration(v.duration) : null,
    v.ext?.toUpperCase(),
  ]
    .filter(Boolean)
    .join(" · ");
}

/** Hero badge: the version that plays and how many there are; jumps to the list. */
export function VersionPill({ versions }: { versions: VersionInfo[] }) {
  if (versions.length < 2) return null;
  const selected = versions.find((v) => v.selected);
  return (
    <button
      type="button"
      onClick={() => document.getElementById("versions")?.scrollIntoView({ behavior: "smooth", block: "start" })}
      className="inline-flex h-6 items-center gap-1.5 rounded-md bg-white/10 px-2 text-[12px] font-semibold text-fg/90 ring-1 ring-white/10 backdrop-blur hover:bg-white/15"
    >
      <Layers className="size-3.5 text-accent-strong" />
      {selected?.label ?? "Version"}
      <span className="font-medium text-dim">· {versions.length} versions</span>
      <ChevronDown className="size-3.5 text-dim" />
    </button>
  );
}

/**
 * Every provider copy of a movie or series: where it comes from, picture,
 * sound and language, what it covers. Picking one makes it the one that
 * plays (remembered per title).
 */
export function Versions({
  kind,
  versions,
  pending,
  onPicked,
}: {
  kind: "movie" | "series";
  versions: VersionInfo[];
  pending: boolean;
  onPicked: () => void;
}) {
  const qc = useQueryClient();
  const pick = useMutation({
    mutationFn: (v: VersionInfo) => api.workPrefer(kind, v.sourceId, v.id),
    onSuccess: onPicked,
  });
  const playing = usePlayer((s) => s.now !== null);
  const [checking, setChecking] = useState<string | null>(null);
  const unchecked = versions.filter((v) => !v.tracks || v.tracks.unavailable);
  // one after another (one stream per account); stops when playback starts
  const checkAll = async () => {
    for (const v of unchecked) {
      if (usePlayer.getState().now) break;
      setChecking(`${v.sourceId}:${v.id}`);
      try {
        await api.versionProbe(kind, v.sourceId, v.id);
      } catch {
        /* the card offers a retry */
      }
    }
    setChecking(null);
    void qc.invalidateQueries({ queryKey: [kind === "movie" ? "movie-detail" : "series-detail"] });
  };
  if (versions.length < 2) return null;
  const mostEpisodes = Math.max(...versions.map((v) => v.episodes));
  return (
    <section id="versions" className="px-10">
      <div className="mb-3 flex items-baseline gap-3">
        <h2 className="text-[19px] font-bold tracking-tight">Versions</h2>
        <span className="text-sm text-dim">{versions.length} copies from your provider</span>
        {pending && (
          <span className="inline-flex items-center gap-1.5 text-[13px] text-faint">
            <Loader2 className="size-3.5 animate-spin" /> checking the others…
          </span>
        )}
        {unchecked.length > 1 && (
          <button
            type="button"
            disabled={playing || checking !== null}
            title={playing ? "Stop playback first: the account allows one stream at a time" : "Open each version briefly to read its tracks"}
            onClick={() => void checkAll()}
            className="ml-auto inline-flex items-center gap-1.5 text-[13px] font-semibold text-accent-strong hover:text-fg disabled:text-faint"
          >
            {checking ? <Loader2 className="size-3.5 animate-spin" /> : <ScanSearch className="size-3.5" />}
            {checking ? "Checking versions…" : `Check audio & subtitles of all ${unchecked.length}`}
          </button>
        )}
      </div>
      <div className="grid gap-2.5 xl:grid-cols-2">
        {versions.map((v) => (
          <VersionCard
            key={`${v.sourceId}:${v.id}`}
            kind={kind}
            v={v}
            partial={kind === "series" && v.episodes > 0 && v.episodes < mostEpisodes}
            busy={pick.isPending && pick.variables === v}
            checking={checking === `${v.sourceId}:${v.id}`}
            onPick={() => !v.selected && pick.mutate(v)}
          />
        ))}
      </div>
    </section>
  );
}

function VersionCard({
  kind,
  v,
  partial,
  busy,
  checking,
  onPick,
}: {
  kind: "movie" | "series";
  v: VersionInfo;
  partial: boolean;
  busy: boolean;
  /** "check all" is reading this one's tracks */
  checking: boolean;
  onPick: () => void;
}) {
  const tech = techLine(v);
  const preferred = usePreferredLanguages();
  const audio = languages(v.tracks?.audio, preferred);
  const subs = languages(v.tracks?.subtitles, preferred);
  const noSubs = v.tracks && !v.tracks.subtitles?.length;
  const progress = kind === "movie" && v.duration && v.position > 60 && !v.watched ? v.position / v.duration : 0;
  return (
    <div
      role="radio"
      aria-checked={v.selected}
      tabIndex={0}
      onClick={onPick}
      onKeyDown={(e) => (e.key === "Enter" || e.key === " ") && (e.preventDefault(), onPick())}
      className={clsx(
        "group relative flex cursor-pointer gap-3.5 rounded-2xl p-4 text-left ring-1 transition-colors outline-none focus-visible:ring-2 focus-visible:ring-accent",
        v.selected ? "bg-accent/[0.12] ring-accent/50" : "bg-white/[0.035] ring-white/[0.06] hover:bg-white/[0.06]",
      )}
    >
      <span
        className={clsx(
          "mt-0.5 grid size-5 shrink-0 place-items-center rounded-full ring-2",
          v.selected ? "bg-accent text-white ring-accent" : "ring-white/25",
        )}
      >
        {busy ? <Loader2 className="size-3 animate-spin" /> : v.selected && <Check className="size-3" strokeWidth={3} />}
      </span>
      <div className="min-w-0 flex-1">
        <div className="flex flex-wrap items-center gap-2">
          <span className="text-[15px] font-semibold">{v.label}</span>
          {v.origin === "cam" && <Badge tone="live">CAM</Badge>}
          {v.watched && <Badge tone="accent">Watched</Badge>}
          {v.selected && <span className="text-[12px] font-semibold text-accent-strong">Plays</span>}
        </div>
        {(v.category || v.sourceName) && (
          <div className="mt-0.5 truncate text-[12.5px] text-faint">{[v.category, v.sourceName].filter(Boolean).join(" · ")}</div>
        )}
        {tech && <div className="mt-1.5 text-[13px] text-dim">{tech}</div>}
        {kind === "series" && v.episodes > 0 && (
          <div className={clsx("mt-1 text-[13px]", partial ? "text-gold" : "text-dim")}>
            {seasonsLabel(v.seasons)} · {v.episodes} episode{v.episodes > 1 ? "s" : ""}
          </div>
        )}
        {v.tracks?.unavailable && !checking ? (
          <div className="mt-1.5 flex flex-col gap-0.5">
            <span className="text-[13px] text-live">The server couldn’t open this version.</span>
            <ProbeButton kind={kind} v={v} again />
          </div>
        ) : v.tracks && !v.tracks.unavailable ? (
          <div className="mt-1.5 flex flex-col gap-0.5 text-[13px] text-dim">
            {audio && (
              <span className="inline-flex items-center gap-1.5">
                <AudioLines className="size-3.5 shrink-0 text-faint" /> {audio}
              </span>
            )}
            <span className="inline-flex items-center gap-1.5">
              <Captions className="size-3.5 shrink-0 text-faint" /> {noSubs ? "No subtitles" : subs}
            </span>
          </div>
        ) : checking ? (
          <div className="mt-1.5 inline-flex items-center gap-1.5 text-[12.5px] text-faint">
            <Loader2 className="size-3.5 animate-spin" /> Checking audio & subtitles…
          </div>
        ) : (
          <ProbeButton kind={kind} v={v} />
        )}
        {progress > 0 && <ProgressBar value={progress} className="mt-2.5 max-w-60" />}
      </div>
    </div>
  );
}

/** Reads audio/subtitle tracks of a version nobody has played yet. */
function ProbeButton({ kind, v, again }: { kind: "movie" | "series"; v: VersionInfo; again?: boolean }) {
  const qc = useQueryClient();
  // the account allows one stream: no checks while something plays
  const playing = usePlayer((s) => s.now !== null);
  const probe = useMutation({
    mutationFn: () => api.versionProbe(kind, v.sourceId, v.id),
    // a failed check is remembered too (the card says so)
    onSettled: () => void qc.invalidateQueries({ queryKey: [kind === "movie" ? "movie-detail" : "series-detail"] }),
  });
  // (a stream that doesn't open is shown by the card itself)
  const failed = probe.isError ? errorMessage(probe.error) : null;
  return (
    <div className="mt-1.5 flex items-center gap-2 text-[12.5px]">
      <button
        type="button"
        disabled={playing || probe.isPending}
        title={playing ? "Stop playback first: the account allows one stream at a time" : "Open this version briefly to read its tracks"}
        onClick={(e) => {
          e.stopPropagation();
          probe.mutate();
        }}
        className="inline-flex items-center gap-1.5 font-semibold text-accent-strong hover:text-fg disabled:text-faint"
      >
        {probe.isPending ? <Loader2 className="size-3.5 animate-spin" /> : <ScanSearch className="size-3.5" />}
        {probe.isPending ? "Checking audio & subtitles…" : again ? "Check again" : "Check audio & subtitles"}
      </button>
      {failed && <span className="text-live">{failed}</span>}
    </div>
  );
}
