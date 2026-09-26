import clsx from "clsx";
import { ChevronLeft, ChevronRight, Heart, Star } from "lucide-react";
import { memo, useRef, useState, type ReactNode } from "react";
import { img } from "../lib/img";
import { initials } from "../lib/format";
import { ProgressBar } from "./ui";

/** Remote image through the img:// cache, fading in, with a text fallback. */
export const Artwork = memo(function Artwork({
  src,
  width,
  alt,
  className,
  fit = "cover",
  fallback,
}: {
  src?: string | null;
  width: number;
  alt: string;
  className?: string;
  fit?: "cover" | "contain";
  fallback?: ReactNode;
}) {
  const url = img(src, width);
  const [status, setStatus] = useState<{ url?: string; state: "loading" | "ok" | "error" }>(() => ({
    url,
    state: url ? "loading" : "error",
  }));
  // a new image (e.g. the next channel's logo) starts over
  if (status.url !== url) setStatus({ url, state: url ? "loading" : "error" });
  const state = status.url === url ? status.state : url ? "loading" : "error";
  const settle = (next: "ok" | "error") => setStatus((s) => (s.url === url ? { url, state: next } : s));
  // callers may position us (e.g. "absolute inset-0"); only default to relative
  const positioned = /\b(absolute|fixed|sticky)\b/.test(className ?? "");
  return (
    <div className={clsx("overflow-hidden", !positioned && "relative", className)}>
      {state !== "ok" && (
        <div className={clsx("absolute inset-0 grid place-items-center", state === "loading" && "skeleton")}>
          {state === "error" && (fallback ?? <Monogram title={alt} />)}
        </div>
      )}
      {url && state !== "error" && (
        <img
          key={url}
          src={url}
          alt={alt}
          draggable={false}
          loading="lazy"
          decoding="async"
          onLoad={() => settle("ok")}
          onError={() => settle("error")}
          className={clsx(
            "absolute inset-0 size-full transition-opacity duration-300",
            fit === "cover" ? "object-cover" : "object-contain",
            state === "ok" ? "opacity-100" : "opacity-0",
          )}
        />
      )}
    </div>
  );
});

function Monogram({ title }: { title: string }) {
  return (
    <div className="grid size-full place-items-center bg-gradient-to-br from-[#262640] to-[#15151f]">
      <span className="text-2xl font-bold tracking-tight text-white/40">{initials(title)}</span>
    </div>
  );
}

export interface PosterCardProps {
  title: string;
  subtitle?: ReactNode;
  image?: string | null;
  rating?: number | null;
  progress?: number | null;
  watched?: boolean;
  favorite?: boolean;
  width?: number;
  aspect?: "poster" | "landscape";
  onClick?: () => void;
  badge?: ReactNode;
  className?: string;
}

export const PosterCard = memo(function PosterCard({
  title,
  subtitle,
  image,
  rating,
  progress,
  watched,
  favorite,
  width = 180,
  aspect = "poster",
  onClick,
  badge,
  className,
}: PosterCardProps) {
  return (
    <button
      type="button"
      onClick={onClick}
      className={clsx("group flex w-full flex-col gap-2 text-left outline-none", className)}
    >
      <div
        className={clsx(
          "relative w-full overflow-hidden rounded-xl bg-raised ring-1 ring-white/[0.06]",
          "shadow-[0_12px_30px_-12px_rgb(0_0_0/0.8)] transition-[transform,box-shadow] duration-300 ease-[var(--ease-soft)]",
          "group-hover:-translate-y-1 group-hover:shadow-[0_22px_40px_-14px_rgb(0_0_0/0.9)] group-hover:ring-white/20",
          "group-focus-visible:ring-2 group-focus-visible:ring-accent",
          aspect === "poster" ? "aspect-[2/3]" : "aspect-video",
        )}
      >
        <Artwork src={image} width={width} alt={title} className="absolute inset-0" />
        <div className="pointer-events-none absolute inset-0 bg-gradient-to-t from-black/50 via-transparent to-transparent opacity-0 transition-opacity group-hover:opacity-100" />
        <div className="absolute left-2 top-2 flex gap-1">{badge}</div>
        {favorite && (
          <Heart className="absolute right-2 top-2 size-4 fill-live text-live drop-shadow" aria-label="Favorite" />
        )}
        {rating != null && rating > 0 && (
          <span className="absolute bottom-2 right-2 inline-flex items-center gap-0.5 rounded-md bg-black/60 px-1.5 py-0.5 text-[11px] font-semibold text-white backdrop-blur">
            <Star className="size-3 fill-gold text-gold" />
            {rating.toFixed(1)}
          </span>
        )}
        {watched && (
          <span className="absolute bottom-2 left-2 rounded-md bg-ok/90 px-1.5 py-0.5 text-[10px] font-bold uppercase text-black">
            Watched
          </span>
        )}
        {progress != null && progress > 0 && !watched && (
          <ProgressBar value={progress} className="absolute inset-x-2 bottom-2 h-1 bg-black/50" />
        )}
      </div>
      <div className="min-w-0 px-0.5">
        <div className="truncate text-[13.5px] font-semibold text-fg/95">{title}</div>
        {subtitle && <div className="truncate text-xs text-dim">{subtitle}</div>}
      </div>
    </button>
  );
});

export const ChannelLogo = memo(function ChannelLogo({
  src,
  title,
  size = 44,
  className,
}: {
  src?: string | null;
  title: string;
  size?: number;
  className?: string;
}) {
  return (
    <Artwork
      src={src}
      width={size * 2}
      alt={title}
      fit="contain"
      className={clsx("shrink-0 rounded-lg bg-white/[0.04] p-1 ring-1 ring-white/[0.06] [&_img]:p-1", className)}
      fallback={<span className="text-[11px] font-bold text-white/45">{initials(title)}</span>}
    />
  );
});

/** Horizontal scroller with a title and hover arrows. */
export function Shelf({
  title,
  action,
  children,
  itemWidth = 180,
}: {
  title: ReactNode;
  action?: ReactNode;
  children: ReactNode;
  itemWidth?: number;
}) {
  const ref = useRef<HTMLDivElement>(null);
  const scroll = (dir: number) => {
    const el = ref.current;
    if (el) el.scrollBy({ left: dir * Math.max(itemWidth * 2, el.clientWidth * 0.8), behavior: "smooth" });
  };
  return (
    <section className="group/shelf relative animate-rise-in">
      <div className="mb-3 flex items-end justify-between px-10">
        <h2 className="text-[19px] font-bold tracking-tight">{title}</h2>
        {action}
      </div>
      <div className="relative">
        <div
          ref={ref}
          className="no-scrollbar flex snap-x snap-mandatory gap-4 overflow-x-auto scroll-px-10 px-10 pb-4 pt-1"
        >
          {children}
        </div>
        <button
          aria-label="Scroll left"
          onClick={() => scroll(-1)}
          className="absolute left-2 top-[40%] grid size-10 -translate-y-1/2 place-items-center rounded-full bg-black/60 text-white opacity-0 ring-1 ring-white/10 backdrop-blur transition-opacity group-hover/shelf:opacity-100 hover:bg-black/80"
        >
          <ChevronLeft className="size-5" />
        </button>
        <button
          aria-label="Scroll right"
          onClick={() => scroll(1)}
          className="absolute right-2 top-[40%] grid size-10 -translate-y-1/2 place-items-center rounded-full bg-black/60 text-white opacity-0 ring-1 ring-white/10 backdrop-blur transition-opacity group-hover/shelf:opacity-100 hover:bg-black/80"
        >
          <ChevronRight className="size-5" />
        </button>
      </div>
    </section>
  );
}

export function ShelfItem({ width, children }: { width: number; children: ReactNode }) {
  return (
    <div className="shrink-0 snap-start" style={{ width }}>
      {children}
    </div>
  );
}
