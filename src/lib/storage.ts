/** Per-viewer UI state kept in localStorage as JSON (folds, modes, recent searches). */

/** The stored value, or `fallback` when missing or unreadable. */
export function loadJson<T>(key: string, fallback: T): T {
  try {
    const v = localStorage.getItem(key);
    return v === null ? fallback : (JSON.parse(v) as T);
  } catch {
    return fallback;
  }
}

export function saveJson(key: string, value: unknown) {
  try {
    localStorage.setItem(key, JSON.stringify(value));
  } catch {
    /* per-viewer convenience only */
  }
}
