import { useEffect } from "react";
import { useNavigate } from "react-router";

export type Dir = "left" | "right" | "up" | "down";
const KEYS: Record<string, Dir> = { ArrowLeft: "left", ArrowRight: "right", ArrowUp: "up", ArrowDown: "down" };

const FOCUSABLE = 'button:not([disabled]), a[href], [role="button"], input, select, textarea, [tabindex]:not([tabindex="-1"])';

function visible(el: HTMLElement): boolean {
  const r = el.getBoundingClientRect();
  if (r.width < 2 || r.height < 2) return false;
  const style = getComputedStyle(el);
  return style.visibility !== "hidden" && style.display !== "none" && Number(style.opacity) > 0.05;
}

/** Does `r` share a row (left/right) or column (up/down) with `from`? */
function inLine(from: DOMRect, r: DOMRect, dir: Dir): boolean {
  return dir === "left" || dir === "right" ? r.top < from.bottom && from.top < r.bottom : r.left < from.right && from.left < r.right;
}

/**
 * Best candidate in direction `dir` from `from`, TV-remote style: elements in
 * the same row/column first (e.g. the next poster on a shelf, however far),
 * then the nearest by distance.
 */
export function pick(from: DOMRect, dir: Dir, candidates: HTMLElement[]): HTMLElement | null {
  const cx = from.left + from.width / 2;
  const cy = from.top + from.height / 2;
  let best: HTMLElement | null = null;
  let bestScore = Infinity;
  for (const el of candidates) {
    const r = el.getBoundingClientRect();
    const x = r.left + r.width / 2;
    const y = r.top + r.height / 2;
    const dx = x - cx;
    const dy = y - cy;
    let primary: number;
    let secondary: number;
    switch (dir) {
      case "right":
        if (r.left < from.right - 4) continue;
        primary = dx;
        secondary = Math.abs(dy);
        break;
      case "left":
        if (r.right > from.left + 4) continue;
        primary = -dx;
        secondary = Math.abs(dy);
        break;
      case "down":
        if (r.top < from.bottom - 4) continue;
        primary = dy;
        secondary = Math.abs(dx);
        break;
      case "up":
        if (r.bottom > from.top + 4) continue;
        primary = -dy;
        secondary = Math.abs(dx);
        break;
    }
    // in-line candidates beat everything else; then distance, off-axis weighted
    const score = (inLine(from, r, dir) ? 0 : 1e9) + primary + secondary * 2.5;
    if (score < bestScore) {
      bestScore = score;
      best = el;
    }
  }
  return best;
}

/**
 * Arrow keys move focus between focusable elements by screen position (like a
 * TV remote), Backspace goes back. Components that handle arrows themselves
 * (lists, the player) call preventDefault and are left alone.
 */
export function useSpatialNav(enabled: boolean) {
  const navigate = useNavigate();
  useEffect(() => {
    if (!enabled) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.defaultPrevented || e.altKey || e.ctrlKey || e.metaKey) return;
      const target = e.target as HTMLElement | null;
      const typing = !!target && (target.tagName === "INPUT" || target.tagName === "TEXTAREA" || target.tagName === "SELECT" || target.isContentEditable);
      if (e.key === "Backspace" && !typing) {
        e.preventDefault();
        navigate(-1);
        return;
      }
      const dir = KEYS[e.key];
      if (!dir) return;
      // inside text fields only up/down leave the field
      if (typing && (dir === "left" || dir === "right")) return;
      const all = [...document.querySelectorAll<HTMLElement>(FOCUSABLE)].filter((el) => el !== document.activeElement && visible(el));
      const active = document.activeElement instanceof HTMLElement && document.activeElement !== document.body ? document.activeElement : null;
      let next: HTMLElement | null;
      if (!active) {
        // nothing focused yet: start with the first element in the main area
        next = all.find((el) => el.closest("main")) ?? all[0] ?? null;
      } else {
        next = pick(active.getBoundingClientRect(), dir, all);
      }
      if (next) {
        e.preventDefault();
        next.focus({ preventScroll: true });
        next.scrollIntoView({ block: "nearest", inline: "nearest", behavior: "smooth" });
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [enabled, navigate]);
}
