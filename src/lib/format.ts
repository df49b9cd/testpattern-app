/** 5025 → "1:23:45"-style clock; `withHours` forces the hour part. */
export function clock(seconds: number | null | undefined, withHours = false): string {
  const s = Math.max(0, Math.floor(seconds ?? 0));
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const sec = s % 60;
  const mm = h > 0 || withHours ? String(m).padStart(2, "0") : String(m);
  return h > 0 || withHours ? `${h}:${mm}:${String(sec).padStart(2, "0")}` : `${mm}:${String(sec).padStart(2, "0")}`;
}

/** 5025 → "1h 24m", 900 → "15m". */
export function duration(seconds: number | null | undefined): string {
  // round the total first, so 59.9 min reads "1h", not "60m"
  const total = Math.round(Math.max(0, seconds ?? 0) / 60);
  const h = Math.floor(total / 60);
  const m = total % 60;
  if (h === 0) return `${m}m`;
  return m === 0 ? `${h}h` : `${h}h ${m}m`;
}

const timeFmt = new Intl.DateTimeFormat(undefined, { hour: "2-digit", minute: "2-digit" });
const dayFmt = new Intl.DateTimeFormat(undefined, { weekday: "short", day: "numeric", month: "short" });
const dateFmt = new Intl.DateTimeFormat(undefined, { day: "numeric", month: "short", year: "numeric" });

/** unix seconds → "20:30" */
export const hhmm = (unix: number) => timeFmt.format(new Date(unix * 1000));
/** unix seconds → "Sat 26 Sep" */
export const day = (unix: number) => dayFmt.format(new Date(unix * 1000));
/** unix seconds → "26 Sep 2026" */
export const date = (unix: number) => dateFmt.format(new Date(unix * 1000));

export const nowUnix = () => Math.floor(Date.now() / 1000);

/** Fraction (0..1) of a programme that has elapsed. */
export function elapsed(start: number, stop: number, now = nowUnix()): number {
  if (stop <= start) return 0;
  return Math.min(1, Math.max(0, (now - start) / (stop - start)));
}

/** "3 min ago" / "2 h ago" / "yesterday" style relative time. */
export function ago(unix: number | null | undefined): string {
  if (!unix) return "never";
  const d = nowUnix() - unix;
  if (d < 60) return "just now";
  if (d < 3600) return `${Math.floor(d / 60)} min ago`;
  if (d < 86400) return `${Math.floor(d / 3600)} h ago`;
  if (d < 2 * 86400) return "yesterday";
  return `${Math.floor(d / 86400)} days ago`;
}

export function initials(title: string): string {
  const words = title.replace(/[^\p{L}\p{N} ]/gu, " ").trim().split(/\s+/).filter(Boolean);
  return (words.slice(0, 2).map((w) => w[0]).join("") || "?").toUpperCase();
}

export function resolutionLabel(w?: number | null, h?: number | null): string | null {
  if (!w || !h) return null;
  if (w >= 3800 || h >= 2100) return "4K";
  if (w >= 1900 || h >= 1000) return "1080p";
  if (w >= 1270 || h >= 700) return "720p";
  return "SD";
}
