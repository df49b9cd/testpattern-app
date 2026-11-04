import clsx from "clsx";
import { CalendarRange, Cast, Clapperboard, Film, House, RefreshCw, Search, Settings, Tv, VolumeX, X } from "lucide-react";
import { useEffect } from "react";
import { NavLink, Outlet, useLocation, useMatches, useNavigate } from "react-router";
import { useSources } from "../lib/queries";
import { ROUTES } from "../lib/routes";
import { runningSync, useSync } from "../stores/sync";
import { control, usePlayer } from "../stores/player";
import { Artwork } from "../components/media";
import { IconButton, LiveDot } from "../components/ui";
import { CastMenu } from "../components/CastMenu";
import { useSpatialNav } from "../hooks/useSpatialNav";
import { ErrorBoundary } from "../components/ErrorBoundary";
import { pipClipPath, usePipVisible, useViewportSize } from "../components/PipPlayer";
import type { RouteHandle } from "./router";

const NAV = [
  { to: ROUTES.home, label: "Home", icon: House, end: true },
  { to: ROUTES.live, label: "Live TV", icon: Tv },
  { to: ROUTES.guide, label: "TV Guide", icon: CalendarRange },
  { to: ROUTES.movieList, label: "Movies", icon: Film },
  { to: ROUTES.seriesList, label: "Series", icon: Clapperboard },
  { to: ROUTES.search, label: "Search", icon: Search },
];

/**
 * Should an off-page keydown go to the (hidden) player? Only while something
 * plays; Space/K/M only, no modifiers, not while a control has focus (spatial
 * nav activates on Space), and not on the player page (its own handler owns
 * the keys there).
 */
export function offPageKey(
  e: { key: string; ctrlKey: boolean; metaKey: boolean; altKey: boolean },
  opts: { pathname: string; onBody: boolean; playing: boolean },
): "play" | "mute" | null {
  if (!opts.playing) return null;
  if (opts.pathname === ROUTES.player) return null;
  if (e.ctrlKey || e.metaKey || e.altKey) return null;
  if (e.key === " " || e.key === "k" || e.key === "K") return opts.onBody ? "play" : null;
  if (e.key === "m" || e.key === "M") return "mute";
  return null;
}

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
        navigate(ROUTES.search);
        return;
      }
      // PL-101: playback may run without the player page open (preview on
      // /live, autoplay-next, a script/bridge start); off-page Space/K/M go
      // straight to that player instead of doing nothing. Space with a
      // focused control belongs to it (spatial nav activates on Space).
      const onBody = !(document.activeElement instanceof HTMLElement) || document.activeElement === document.body;
      const action = offPageKey(e, { pathname: location.pathname, onBody, playing: usePlayer.getState().now !== null });
      if (action === "play") {
        e.preventDefault();
        navigate(ROUTES.player);
      } else if (action === "mute") {
        e.preventDefault();
        void control.setMute(!usePlayer.getState().props.mute);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [navigate, location.pathname]);

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
      <span className="flex-1 text-[17px] font-bold tracking-tight">testpattern</span>
      {/* pick a TV first, then whatever is played goes there */}
      <CastMenu placement="below" className="-my-2 -mr-2" triggerClassName="text-dim" />
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
        to={ROUTES.settings}
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
  const mute = usePlayer((s) => s.props.mute);
  const cast = usePlayer((s) => s.cast);
  const notice = usePlayer((s) => s.castNotice);
  const navigate = useNavigate();
  if (!now) {
    if (cast) return <CastCard name={cast.deviceName} />;
    return notice ? <CastNotice text={notice} /> : null;
  }
  return (
    <button
      onClick={() => navigate(ROUTES.player)}
      className="mx-3 mb-3 flex items-center gap-3 rounded-xl bg-white/[0.05] p-2 text-left ring-1 ring-white/[0.06] transition-colors hover:bg-white/[0.09]"
    >
      <div className="relative shrink-0">
        <Artwork
          src={now.image}
          width={48}
          alt={now.title}
          fit={now.kind === "live" ? "contain" : "cover"}
          className="size-10 rounded-lg bg-black/40"
        />
        {mute && (
          <span className="absolute -bottom-1 -right-1 grid size-4 place-items-center rounded-full bg-panel ring-1 ring-white/10" title="Muted (M)">
            <VolumeX className="size-2.5 text-dim" />
          </span>
        )}
      </div>
      <span className="min-w-0">
        <span className="flex items-center gap-1.5 text-[11px] font-semibold uppercase tracking-wide text-dim">
          {now.kind === "live" && <LiveDot />}
          {cast ? <span className="truncate text-accent-strong">On {cast.deviceName}</span> : "Now playing"}
        </span>
        <span className="block truncate text-[13px] font-semibold">{now.title}</span>
      </span>
    </button>
  );
}

/** Casting with nothing playing yet: everything played goes to the TV. */
function CastCard({ name }: { name: string }) {
  return (
    <div className="mx-3 mb-3 flex items-center gap-3 rounded-xl bg-accent/10 p-2.5 ring-1 ring-accent/20">
      <Cast className="size-5 shrink-0 text-accent-strong" />
      <span className="min-w-0 flex-1">
        <span className="block text-[11px] font-semibold uppercase tracking-wide text-dim">Casting to</span>
        <span className="block truncate text-[13px] font-semibold">{name}</span>
      </span>
      <IconButton label="Stop casting" size="sm" onClick={() => void usePlayer.getState().stopCasting()}>
        <X className="size-4" />
      </IconButton>
    </div>
  );
}

/** Why casting ended (TV switched off, another app took over, …). */
function CastNotice({ text }: { text: string }) {
  return (
    <div className="mx-3 mb-3 flex items-start gap-2.5 rounded-xl bg-white/[0.05] p-2.5 text-[12.5px] text-dim ring-1 ring-white/[0.06]">
      <Cast className="mt-0.5 size-4 shrink-0 text-faint" />
      <span className="min-w-0 flex-1 leading-snug">{text}</span>
      <IconButton label="Dismiss" size="sm" className="-m-1" onClick={() => usePlayer.setState({ castNotice: null })}>
        <X className="size-4" />
      </IconButton>
    </div>
  );
}

function SyncStatus() {
  const running = useSync(runningSync);
  const sources = useSources();
  const backendRunning = sources.data?.find((s) => s.syncing);
  if (!running && !backendRunning) return null;
  const message = running?.message || "Updating library…";
  return (
    <div className="mx-3 mb-3 flex items-center gap-2.5 rounded-xl bg-accent/10 px-3 py-2.5 text-[12.5px] text-accent-strong ring-1 ring-accent/20">
      <RefreshCw className="size-4 shrink-0 animate-spin" />
      <span className="line-clamp-2 leading-snug">{message}</span>
    </div>
  );
}
