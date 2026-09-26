import { ChevronLeft } from "lucide-react";
import type { ReactNode } from "react";
import { useNavigate } from "react-router";
import { Artwork } from "./media";
import { IconButton } from "./ui";

/** Full-bleed backdrop + poster header shared by movie and series pages. */
export function DetailHero({
  backdrop,
  poster,
  title,
  eyebrow,
  meta,
  badges,
  actions,
  plot,
}: {
  backdrop?: string | null;
  poster?: string | null;
  title: string;
  eyebrow?: ReactNode;
  meta?: ReactNode;
  badges?: ReactNode;
  actions?: ReactNode;
  plot?: string | null;
}) {
  const navigate = useNavigate();
  return (
    <section className="relative">
      <div className="absolute inset-x-0 top-0 h-[78vh] min-h-[520px] overflow-hidden">
        {/* no backdrop: plain background, not a "?" monogram */}
        <Artwork src={backdrop ?? poster} width={1600} alt="" className="absolute inset-0 scale-105 opacity-70" fallback={<span />} />
        <div className="absolute inset-0 scrim-l" />
        <div className="absolute inset-0 scrim-b" />
      </div>
      <div className="relative px-10 pt-6">
        <IconButton label="Back" variant="glass" onClick={() => navigate(-1)}>
          <ChevronLeft className="size-5" />
        </IconButton>
      </div>
      <div className="relative flex gap-10 px-10 pb-8 pt-[18vh] animate-rise-in">
        <Artwork
          src={poster}
          width={260}
          alt={title}
          className="hidden aspect-[2/3] w-[240px] shrink-0 self-end rounded-2xl shadow-[0_30px_60px_-20px_rgb(0_0_0/0.9)] ring-1 ring-white/10 lg:block"
        />
        <div className="flex min-w-0 max-w-3xl flex-col justify-end gap-4">
          {eyebrow && <div className="text-xs font-bold uppercase tracking-[0.2em] text-accent-strong">{eyebrow}</div>}
          <h1 className="text-5xl leading-[1.04] font-extrabold tracking-tight drop-shadow-xl">{title}</h1>
          {meta && <div className="flex flex-wrap items-center gap-x-3 gap-y-1 text-[15px] text-fg/75">{meta}</div>}
          {badges && <div className="flex flex-wrap items-center gap-1.5">{badges}</div>}
          {actions && <div className="mt-1 flex flex-wrap items-center gap-3">{actions}</div>}
          {plot && <p className="max-w-2xl text-[15px] leading-relaxed text-fg/80">{plot}</p>}
        </div>
      </div>
    </section>
  );
}

export function MetaDot() {
  return <span className="text-faint">·</span>;
}

export function Facts({ items }: { items: [string, ReactNode | null | undefined][] }) {
  const shown = items.filter(([, v]) => v);
  if (!shown.length) return null;
  return (
    <dl className="grid max-w-5xl grid-cols-[140px_1fr] gap-x-6 gap-y-3 px-10 text-[14px]">
      {shown.map(([k, v]) => (
        <div key={k} className="contents">
          <dt className="text-faint">{k}</dt>
          <dd className="text-fg/85">{v}</dd>
        </div>
      ))}
    </dl>
  );
}
