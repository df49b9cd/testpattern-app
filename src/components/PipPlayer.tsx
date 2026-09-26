import clsx from "clsx";
import { Maximize2, Pause, Play, X } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { useLocation, useNavigate } from "react-router";
import { api } from "../lib/api";
import { useVideoViewport } from "../hooks/useVideoViewport";
import { usePlayer } from "../stores/player";
import { IconButton } from "./ui";

// Picture-in-picture (T-034). The video renders *behind* the web view, so
// the layout cuts a hole where the window is (pipClipPath) and the video is
// positioned into that rectangle (useVideoViewport).

const WIDTH = 400;
const HEIGHT = 225;
const MARGIN = 24;
const RADIUS = 16;

/** The PiP window in viewport pixels. */
export function pipRect(viewportWidth: number, viewportHeight: number) {
  return { left: viewportWidth - WIDTH - MARGIN, top: viewportHeight - HEIGHT - MARGIN, width: WIDTH, height: HEIGHT };
}

/** clip-path that keeps everything but a rounded hole for the PiP window. */
export function pipClipPath(viewportWidth: number, viewportHeight: number): string {
  const { left: x, top: y, width: w, height: h } = pipRect(viewportWidth, viewportHeight);
  const r = RADIUS;
  const outer = `M0 0 H${viewportWidth} V${viewportHeight} H0 Z`;
  const hole =
    `M${x + r} ${y} H${x + w - r} A${r} ${r} 0 0 1 ${x + w} ${y + r} V${y + h - r} ` +
    `A${r} ${r} 0 0 1 ${x + w - r} ${y + h} H${x + r} A${r} ${r} 0 0 1 ${x} ${y + h - r} ` +
    `V${y + r} A${r} ${r} 0 0 1 ${x + r} ${y} Z`;
  return `path(evenodd, "${outer} ${hole}")`;
}

/** Is the PiP window showing on the current page? */
export function usePipVisible(): boolean {
  const pip = usePlayer((s) => s.pip);
  const kind = usePlayer((s) => s.now?.kind);
  const { pathname } = useLocation();
  // the player itself is fullscreen; on Live TV the preview pane shows live channels
  return pip && !!kind && pathname !== "/player" && !(pathname === "/live" && kind === "live");
}

export function useViewportSize() {
  const [size, setSize] = useState(() => ({ w: window.innerWidth, h: window.innerHeight }));
  useEffect(() => {
    const onResize = () => setSize({ w: window.innerWidth, h: window.innerHeight });
    window.addEventListener("resize", onResize);
    return () => window.removeEventListener("resize", onResize);
  }, []);
  return size;
}

export function PipPlayer() {
  const visible = usePipVisible();
  const { w, h } = useViewportSize();
  const ref = useRef<HTMLDivElement>(null);
  const navigate = useNavigate();
  const now = usePlayer((s) => s.now);
  const paused = usePlayer((s) => s.props.pause);
  const busy = usePlayer((s) => s.status !== "playing" || s.props.buffering);
  useVideoViewport(ref, visible);
  if (!visible || !now) return null;
  const rect = pipRect(w, h);
  return (
    <div
      ref={ref}
      className="group fixed z-40 rounded-2xl shadow-[0_24px_60px_-12px_rgb(0_0_0/0.8)] ring-1 ring-white/15"
      style={rect}
      aria-label="Picture in picture"
    >
      <div
        className={clsx(
          "absolute inset-0 flex flex-col justify-between rounded-2xl bg-gradient-to-b from-black/70 via-transparent to-black/70 p-3 transition-opacity",
          busy ? "opacity-100" : "opacity-0 group-hover:opacity-100 group-focus-within:opacity-100",
        )}
      >
        <div className="flex items-start gap-2">
          <span className="min-w-0 flex-1 truncate text-[13px] font-semibold text-white drop-shadow">{now.title}</span>
          <IconButton label="Close" size="sm" variant="glass" onClick={() => void usePlayer.getState().stop()}>
            <X className="size-4" />
          </IconButton>
        </div>
        <div className="flex items-center justify-center gap-3">
          <IconButton label={paused ? "Play" : "Pause"} variant="glass" onClick={() => void api.set("pause", !paused)}>
            {paused ? <Play className="size-5 fill-current" /> : <Pause className="size-5 fill-current" />}
          </IconButton>
          <IconButton
            label="Back to full screen"
            variant="glass"
            onClick={() => {
              usePlayer.getState().setPip(false);
              navigate("/player");
            }}
          >
            <Maximize2 className="size-5" />
          </IconButton>
        </div>
      </div>
    </div>
  );
}
