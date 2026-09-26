import clsx from "clsx";
import { useInfiniteQuery, useQuery } from "@tanstack/react-query";
import { useVirtualizer } from "@tanstack/react-virtual";
import { CalendarRange, ChevronLeft, ChevronRight, History, Play, X } from "lucide-react";
import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { useNavigate, useSearchParams } from "react-router";
import { api } from "../lib/api";
import { day, hhmm, nowUnix } from "../lib/format";
import { playCatchup, playChannel } from "../lib/play";
import type { Channel, GuideRow, Programme } from "../lib/types";
import { ChannelLogo } from "../components/media";
import { Button, EmptyState, IconButton, LiveDot, Spinner } from "../components/ui";

const PX_PER_MIN = 4;
const HOURS = 12;
const ROW = 64;
const CHANNEL_COL = 220;
const HALF_HOUR = 1800;

export function GuidePage() {
  const navigate = useNavigate();
  const [params, setParams] = useSearchParams();
  const list = params.get("list") ?? "epg";
  const [origin, setOrigin] = useState(() => Math.floor((nowUnix() - 3600) / HALF_HOUR) * HALF_HOUR);
  const from = origin;
  const to = origin + HOURS * 3600;
  const width = ((to - from) / 60) * PX_PER_MIN;
  const [selected, setSelected] = useState<{ channel: Channel; programme: Programme } | null>(null);
  const [now, setNow] = useState(nowUnix);
  useEffect(() => {
    const i = window.setInterval(() => setNow(nowUnix()), 30_000);
    return () => window.clearInterval(i);
  }, []);

  const categories = useQuery({ queryKey: ["categories", "live"], queryFn: () => api.categories("live") });
  const [sourceId, categoryId] = list.includes(":") ? [Number(list.split(":")[0]), list.slice(list.indexOf(":") + 1)] : [undefined, undefined];
  const base = {
    favorites: list === "favorites" || undefined,
    withEpg: list === "epg" || undefined,
    sourceId,
    categoryId,
  };

  const q = useInfiniteQuery({
    queryKey: ["guide", list, from],
    initialPageParam: 0,
    queryFn: ({ pageParam }) => api.guide({ ...base, from, to, offset: pageParam, limit: 60 }),
    getNextPageParam: (last, pages) => {
      const loaded = pages.reduce((n, p) => n + p.items.length, 0);
      return loaded < last.total ? loaded : undefined;
    },
  });
  const rows: GuideRow[] = useMemo(() => q.data?.pages.flatMap((p) => p.items) ?? [], [q.data]);
  const total = q.data?.pages[0]?.total ?? 0;

  const scrollRef = useRef<HTMLDivElement>(null);
  const virtualizer = useVirtualizer({ count: total, getScrollElement: () => scrollRef.current, estimateSize: () => ROW, overscan: 8 });
  const items = virtualizer.getVirtualItems();
  const last = items.length ? items[items.length - 1].index : 0;
  useEffect(() => {
    if (last >= rows.length - 10 && q.hasNextPage && !q.isFetchingNextPage) void q.fetchNextPage();
  }, [last, rows.length, q]);

  // start scrolled so "now" sits near the left edge
  useLayoutEffect(() => {
    const el = scrollRef.current;
    if (el) el.scrollLeft = Math.max(0, ((nowUnix() - from) / 60) * PX_PER_MIN - 120);
  }, [from, q.isSuccess]);

  const shift = (hours: number) => setOrigin((o) => o + hours * 3600);
  const x = (t: number) => ((t - from) / 60) * PX_PER_MIN;
  const ticks = Array.from({ length: HOURS * 2 }, (_, i) => from + i * HALF_HOUR);

  const zapList = rows.map((r) => r.channel);
  const openChannel = (c: Channel) => void playChannel(c, navigate, zapList);

  return (
    <div className="flex h-full flex-col">
      <div className="flex flex-wrap items-center gap-4 px-8 pb-4 pt-6">
        <h1 className="text-3xl font-bold tracking-tight">TV Guide</h1>
        <select
          value={list}
          onChange={(e) => setParams({ list: e.target.value }, { replace: true })}
          className="h-9 max-w-72 rounded-lg bg-white/[0.06] px-3 text-sm outline-none ring-1 ring-white/[0.08] focus:ring-accent"
        >
          <option value="epg">All channels with guide</option>
          <option value="favorites">Favorites</option>
          {(categories.data ?? []).map((c) => (
            <option key={`${c.sourceId}:${c.id}`} value={`${c.sourceId}:${c.id}`}>
              {c.region ? `${c.region} · ` : ""}
              {c.title}
            </option>
          ))}
        </select>
        <div className="ml-auto flex items-center gap-2">
          <span className="mr-2 text-sm text-dim">{day(from)}</span>
          <IconButton label="Earlier" onClick={() => shift(-3)}>
            <ChevronLeft className="size-5" />
          </IconButton>
          <Button size="sm" onClick={() => setOrigin(Math.floor((nowUnix() - 3600) / HALF_HOUR) * HALF_HOUR)}>
            Now
          </Button>
          <IconButton label="Later" onClick={() => shift(3)}>
            <ChevronRight className="size-5" />
          </IconButton>
        </div>
      </div>

      {q.isLoading ? (
        <Spinner className="px-8" label="Loading guide…" />
      ) : total === 0 ? (
        <EmptyState
          icon={<CalendarRange />}
          title="No guide data here"
          text={list === "favorites" ? "None of your favorite channels has programme information." : "Channels in this list have no programme information from your provider."}
        />
      ) : (
        <div ref={scrollRef} className="relative flex-1 overflow-auto border-t border-line">
          <div className="relative" style={{ width: CHANNEL_COL + width, height: virtualizer.getTotalSize() + 40 }}>
            {/* time ruler */}
            <div className="sticky top-0 z-20 flex h-10 border-b border-line bg-bg/95 backdrop-blur" style={{ width: CHANNEL_COL + width }}>
              <div className="sticky left-0 z-10 shrink-0 bg-bg/95" style={{ width: CHANNEL_COL }} />
              <div className="relative" style={{ width }}>
                {ticks.map((t) => (
                  <span key={t} className="absolute top-0 flex h-10 items-center border-l border-line pl-2 text-xs font-semibold tabular-nums text-dim" style={{ left: x(t) }}>
                    {hhmm(t)}
                  </span>
                ))}
              </div>
            </div>
            {/* now line */}
            {now >= from && now < to && (
              <div className="pointer-events-none absolute bottom-0 top-10 z-10 w-0.5 bg-live" style={{ left: CHANNEL_COL + x(now) }}>
                <span className="absolute -left-1 -top-1 size-2.5 rounded-full bg-live" />
              </div>
            )}
            {items.map((v) => {
              const row = rows[v.index];
              return (
                <div key={v.key} className="absolute left-0 flex w-full" style={{ top: 40 + v.start, height: ROW }}>
                  <button
                    onClick={() => row && openChannel(row.channel)}
                    className="sticky left-0 z-10 flex shrink-0 items-center gap-3 border-b border-r border-line bg-panel px-3 text-left hover:bg-hover"
                    style={{ width: CHANNEL_COL }}
                  >
                    {row ? (
                      <>
                        <ChannelLogo src={row.channel.logo} title={row.channel.title} size={40} className="size-10" />
                        <span className="min-w-0 truncate text-[13px] font-semibold">{row.channel.title}</span>
                      </>
                    ) : (
                      <span className="h-4 w-32 rounded skeleton" />
                    )}
                  </button>
                  <div className="relative border-b border-line" style={{ width }}>
                    {row?.programmes.map((p) => {
                      const left = Math.max(0, x(p.start));
                      const right = Math.min(width, x(p.stop));
                      if (right - left < 2) return null;
                      const live = p.start <= now && p.stop > now;
                      const past = p.stop <= now;
                      return (
                        <button
                          key={p.start}
                          onClick={() => setSelected({ channel: row.channel, programme: p })}
                          className={clsx(
                            "absolute inset-y-1 overflow-hidden rounded-lg px-2.5 text-left transition-colors",
                            live ? "bg-accent/25 ring-1 ring-accent/50 hover:bg-accent/35" : past ? "bg-white/[0.03] hover:bg-white/[0.07]" : "bg-white/[0.06] hover:bg-white/[0.11]",
                          )}
                          style={{ left: left + 1, width: right - left - 2 }}
                          title={p.title}
                        >
                          <div className={clsx("truncate text-[13px] font-semibold", past && "text-dim")}>{p.title}</div>
                          <div className="truncate text-[11px] tabular-nums text-faint">
                            {hhmm(p.start)} – {hhmm(p.stop)}
                          </div>
                        </button>
                      );
                    })}
                  </div>
                </div>
              );
            })}
          </div>
        </div>
      )}

      {selected && <ProgrammeDialog {...selected} now={now} onClose={() => setSelected(null)} zapList={zapList} />}
    </div>
  );
}

function ProgrammeDialog({
  channel,
  programme: p,
  now,
  zapList,
  onClose,
}: {
  channel: Channel;
  programme: Programme;
  now: number;
  zapList: Channel[];
  onClose: () => void;
}) {
  const navigate = useNavigate();
  const live = p.start <= now && p.stop > now;
  const canCatchup = channel.archive && p.start < now && p.start > now - channel.archiveDays * 86400;
  return (
    <div className="fixed inset-0 z-50 grid place-items-center bg-black/60 p-6 backdrop-blur-sm animate-fade-in" onClick={onClose}>
      <div className="relative w-full max-w-lg rounded-3xl bg-panel p-6 ring-1 ring-white/10 animate-rise-in" onClick={(e) => e.stopPropagation()}>
        <IconButton label="Close" size="sm" className="absolute right-4 top-4" onClick={onClose}>
          <X className="size-4" />
        </IconButton>
        <div className="mb-4 flex items-center gap-3">
          <ChannelLogo src={channel.logo} title={channel.title} size={40} className="size-10" />
          <div>
            <div className="text-sm font-semibold">{channel.title}</div>
            <div className="text-xs tabular-nums text-dim">
              {day(p.start)} · {hhmm(p.start)} – {hhmm(p.stop)}
            </div>
          </div>
        </div>
        <h2 className="text-2xl font-bold tracking-tight">{p.title}</h2>
        {(p.episode || p.category) && <p className="mt-1 text-sm text-dim">{[p.episode, p.category].filter(Boolean).join(" · ")}</p>}
        {p.description && <p className="mt-3 max-h-60 overflow-y-auto text-[14px] leading-relaxed text-fg/80">{p.description}</p>}
        <div className="mt-6 flex flex-wrap gap-3">
          {live && (
            <Button variant="primary" icon={<LiveDot />} onClick={() => void playChannel(channel, navigate, zapList)}>
              Watch live
            </Button>
          )}
          {canCatchup && (
            <Button variant={live ? "secondary" : "primary"} icon={live ? <History className="size-4" /> : <Play className="size-4 fill-current" />} onClick={() => void playCatchup(channel, p, navigate)}>
              {live ? "Watch from start" : "Play catch-up"}
            </Button>
          )}
          {!live && !canCatchup && p.start > now && <p className="text-sm text-faint">Airs {day(p.start)} at {hhmm(p.start)}.</p>}
        </div>
      </div>
    </div>
  );
}
