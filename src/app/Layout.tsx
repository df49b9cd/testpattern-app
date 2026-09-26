import clsx from "clsx";
import { CalendarRange, Clapperboard, Film, House, RefreshCw, Search, Settings, Tv } from "lucide-react";
import { useEffect } from "react";
import { NavLink, Outlet, useLocation, useMatches, useNavigate } from "react-router";
import { useQuery } from "@tanstack/react-query";
import { api } from "../lib/api";
import { useSync } from "../stores/sync";
import { usePlayer } from "../stores/player";
import { Artwork } from "../components/media";
import { LiveDot } from "../components/ui";
import { useSpatialNav } from "../hooks/useSpatialNav";
import { ErrorBoundary } from "../components/ErrorBoundary";
import { pipClipPath, usePipVisible, useViewportSize } from "../components/PipPlayer";
import type { RouteHandle } from "./router";

const NAV = [
  { to: "/", label: "Home", icon: House, end: true },
  { to: "/live", label: "Live TV", icon: Tv },
  { to: "/guide", label: "TV Guide", icon: CalendarRange },
  { to: "/movies", label: "Movies", icon: Film },
  { to: "/series", label: "Series", icon: Clapperboard },
  { to: "/search", label: "Search", icon: Search },
];

export function Layout() {
  useSpatialNav(true);
  const navigate = useNavigate();
  const location = useLocation();
  const matches = useMatches();
  const transparent = matches.some((m) => (m.handle as RouteHandle | undefined)?.transparent);
  // picture-in-picture: a hole in the whole UI where the video shows through
  const pip = usePipVisible();
  const { w, h } = useViewportSize();

  // "/" or Ctrl/Cmd+K jumps to search from anywhere
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const t = e.target as HTMLElement | null;
      if (t && (t.tagName === "INPUT" || t.tagName === "TEXTAREA" || t.isContentEditable)) return;
      if (e.key === "/" || ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "k")) {
        e.preventDefault();
        navigate("/search");
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [navigate]);

  return (
    <div className="flex h-full" style={pip ? { clipPath: pipClipPath(w, h) } : undefined}>
      <Sidebar />
      <main className={clsx("relative min-w-0 flex-1", !transparent && "overflow-y-auto bg-bg")}>
        <ErrorBoundary resetKey={location.pathname}>
          <Outlet />
        </ErrorBoundary>
      </main>
    </div>
  );
}

function Logo() {
  const bars = ["#c0c0c0", "#c0c000", "#00c0c0", "#00c000", "#c000c0", "#c00000", "#0000c0"];
  return (
    <div className="flex items-center gap-3 px-5 pt-6 pb-7">
      <div className="flex h-7 overflow-hidden rounded-md ring-1 ring-white/10">
        {bars.map((c) => (
          <span key={c} className="h-full w-[5px]" style={{ background: c }} />
        ))}
      </div>
      <span className="text-[17px] font-bold tracking-tight">testpattern</span>
    </div>
  );
}

function Sidebar() {
  return (
    <aside className="flex w-[232px] shrink-0 flex-col border-r border-line bg-panel">
      <Logo />
      <nav className="flex flex-col gap-0.5 px-3">
        {NAV.map(({ to, label, icon: Icon, end }) => (
          <NavLink
            key={to}
            to={to}
            end={end}
            className={({ isActive }) =>
              clsx(
                "group flex h-10 items-center gap-3 rounded-xl px-3 text-[14.5px] font-medium transition-colors",
                isActive ? "bg-white/[0.08] text-fg" : "text-dim hover:bg-white/[0.04] hover:text-fg",
              )
            }
          >
            {({ isActive }) => (
              <>
                <Icon className={clsx("size-[18px]", isActive ? "text-accent-strong" : "text-faint group-hover:text-dim")} />
                {label}
              </>
            )}
          </NavLink>
        ))}
      </nav>
      <div className="flex-1" />
      <NowPlayingCard />
      <SyncStatus />
      <NavLink
        to="/settings"
        className={({ isActive }) =>
          clsx(
            "mx-3 mb-4 flex h-10 items-center gap-3 rounded-xl px-3 text-[14.5px] font-medium transition-colors",
            isActive ? "bg-white/[0.08] text-fg" : "text-dim hover:bg-white/[0.04] hover:text-fg",
          )
        }
      >
        <Settings className="size-[18px] text-faint" />
        Settings
      </NavLink>
    </aside>
  );
}

function NowPlayingCard() {
  const now = usePlayer((s) => s.now);
  const navigate = useNavigate();
  if (!now) return null;
  return (
    <button
      onClick={() => navigate("/player")}
      className="mx-3 mb-3 flex items-center gap-3 rounded-xl bg-white/[0.05] p-2 text-left ring-1 ring-white/[0.06] transition-colors hover:bg-white/[0.09]"
    >
      <Artwork
        src={now.image}
        width={48}
        alt={now.title}
        fit={now.kind === "live" ? "contain" : "cover"}
        className="size-10 shrink-0 rounded-lg bg-black/40"
      />
      <span className="min-w-0">
        <span className="flex items-center gap-1.5 text-[11px] font-semibold uppercase tracking-wide text-dim">
          {now.kind === "live" && <LiveDot />}
          Now playing
        </span>
        <span className="block truncate text-[13px] font-semibold">{now.title}</span>
      </span>
    </button>
  );
}

function SyncStatus() {
  const bySource = useSync((s) => s.bySource);
  const sources = useQuery({ queryKey: ["sources"], queryFn: api.sources });
  const running = Object.entries(bySource).find(([, v]) => v.running);
  const backendRunning = sources.data?.find((s) => s.syncing);
  if (!running && !backendRunning) return null;
  const message = running?.[1].message || "Updating library…";
  return (
    <div className="mx-3 mb-3 flex items-center gap-2.5 rounded-xl bg-accent/10 px-3 py-2.5 text-[12.5px] text-accent-strong ring-1 ring-accent/20">
      <RefreshCw className="size-4 shrink-0 animate-spin" />
      <span className="line-clamp-2 leading-snug">{message}</span>
    </div>
  );
}
