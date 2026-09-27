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

// ISO 639-2 bibliographic codes (as in many files) → the terminology codes Intl knows
const B_TO_T: Record<string, string> = {
  ger: "deu", fre: "fra", dut: "nld", chi: "zho", cze: "ces", gre: "ell", ice: "isl", per: "fas", rum: "ron",
  slo: "slk", wel: "cym", baq: "eus", arm: "hye", geo: "kat", may: "msa", mac: "mkd", alb: "sqi", bur: "mya",
  tib: "bod", mao: "mri",
};
let languageNames: Intl.DisplayNames | null | undefined;

/** Track language code ("dan", "da", "ger", "pt-BR") → "Danish"; null for none/undetermined. */
export function languageName(code: string | null | undefined): string | null {
  const c = code?.trim().toLowerCase().replace("_", "-");
  if (!c || c === "und" || c === "unk" || c === "zxx" || c === "mis" || c === "mul") return null;
  if (languageNames === undefined) {
    try {
      languageNames = new Intl.DisplayNames(["en"], { type: "language" });
    } catch {
      languageNames = null;
    }
  }
  const tag = B_TO_T[c] ?? c;
  try {
    const name = languageNames?.of(tag);
    if (name && name.toLowerCase() !== tag) return name;
  } catch {
    /* not a language tag */
  }
  return c.toUpperCase();
}

/** Audio channel count → "5.1", "7.1", "Stereo", "Mono". */
export function channelsLabel(n: number | null | undefined): string | null {
  if (!n) return null;
  if (n === 1) return "Mono";
  if (n === 2) return "Stereo";
  if (n === 6) return "5.1";
  if (n === 8) return "7.1";
  return `${n}ch`;
}

/** [1,2,3,5] → "Seasons 1–3, 5"; [4] → "Season 4". */
export function seasonsLabel(seasons: number[]): string {
  const s = [...new Set(seasons)].sort((a, b) => a - b);
  if (!s.length) return "";
  const runs: string[] = [];
  for (let i = 0; i < s.length; ) {
    let j = i;
    while (j + 1 < s.length && s[j + 1] === s[j] + 1) j++;
    runs.push(j > i ? `${s[i]}–${s[j]}` : `${s[i]}`);
    i = j + 1;
  }
  return `${s.length === 1 ? "Season" : "Seasons"} ${runs.join(", ")}`;
}
