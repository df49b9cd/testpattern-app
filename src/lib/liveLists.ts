import type { Category, ChannelQuery, LiveNav } from "./types";

/**
 * A list of live channels (URL `?list=`). Favorites, recent, all and the
 * country/genre lists show one row per channel (its chosen feed); provider
 * categories show the provider's feeds as they are.
 */
export type ListKey =
  | { type: "favorites" }
  | { type: "recent" }
  | { type: "all" }
  /** country: undefined = every country, "" = channels without a region */
  | { type: "nav"; country?: string; genre?: string }
  | { type: "cat"; sourceId: number; id: string };

/** "favorites" | "recent" | "all" | "nav:<country|*|->/<genre|*>" | "<sourceId>:<categoryId>" */
export function parseKey(v: string | null): ListKey {
  if (!v || v === "favorites") return { type: "favorites" };
  if (v === "recent" || v === "all") return { type: v };
  if (v.startsWith("nav:")) {
    const rest = v.slice(4);
    const slash = rest.indexOf("/");
    const c = slash < 0 ? rest : rest.slice(0, slash);
    const g = slash < 0 ? "*" : rest.slice(slash + 1);
    return {
      type: "nav",
      country: c === "*" ? undefined : c === "-" ? "" : c,
      genre: g === "*" || g === "" ? undefined : g,
    };
  }
  const [sid, ...rest] = v.split(":");
  return { type: "cat", sourceId: Number(sid), id: rest.join(":") };
}

export function keyString(k: ListKey): string {
  switch (k.type) {
    case "cat":
      return `${k.sourceId}:${k.id}`;
    case "nav":
      return `nav:${k.country === undefined ? "*" : k.country === "" ? "-" : k.country}/${k.genre ?? "*"}`;
    default:
      return k.type;
  }
}

/** The channel query behind a list (not for "recent", which has its own command). */
export function queryFor(k: ListKey): ChannelQuery {
  switch (k.type) {
    case "cat":
      return { sourceId: k.sourceId, categoryId: k.id };
    case "nav":
      return { grouped: true, country: k.country, genre: k.genre };
    case "favorites":
      return { grouped: true, favorites: true };
    default:
      return { grouped: true };
  }
}

export function countryName(nav: LiveNav | undefined, code: string | undefined): string {
  if (code === undefined) return "";
  return nav?.countries.find((c) => (c.code ?? "") === code)?.name ?? (code || "Other");
}

export function titleFor(k: ListKey, nav?: LiveNav, categories?: Category[]): string {
  switch (k.type) {
    case "favorites":
      return "Favorites";
    case "recent":
      return "Recently watched";
    case "all":
      return "All channels";
    case "nav": {
      const parts = [countryName(nav, k.country), k.genre].filter(Boolean);
      return parts.length ? parts.join(" · ") : "All channels";
    }
    case "cat": {
      const c = categories?.find((c) => c.sourceId === k.sourceId && c.id === k.id);
      return c ? `${c.region ? `${c.region} · ` : ""}${c.title}` : "Channels";
    }
  }
}
