import { BRIDGE, browserPreview } from "./bridge";

// Artwork goes through the backend's img:// protocol (disk cache + resize).
const isWindows = typeof navigator !== "undefined" && /Windows/i.test(navigator.userAgent);
const base = browserPreview ? `${BRIDGE}/img` : isWindows ? "http://img.localhost/" : "img://localhost/";

/** URL for remote artwork, downscaled to `width` CSS px (× devicePixelRatio). */
export function img(url: string | null | undefined, width = 0): string | undefined {
  if (!url || !/^https?:\/\//i.test(url)) return undefined;
  const w = width > 0 ? Math.round(width * Math.min(window.devicePixelRatio || 1, 2)) : 0;
  return `${base}?u=${encodeURIComponent(url)}${w ? `&w=${w}` : ""}`;
}
