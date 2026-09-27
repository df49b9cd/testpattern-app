import { describe, expect, it } from "vitest";
import { keyString, parseKey, queryFor, titleFor } from "../lib/liveLists";
import type { LiveNav } from "../lib/types";

describe("live list keys (URL ?list=)", () => {
  it("parses the special lists, favorites by default", () => {
    expect(parseKey(null)).toEqual({ type: "favorites" });
    expect(parseKey("recent")).toEqual({ type: "recent" });
    expect(parseKey("all")).toEqual({ type: "all" });
  });
  it("round-trips categories whose ids contain colons", () => {
    const key = parseKey("3:uk:sports");
    expect(key).toEqual({ type: "cat", sourceId: 3, id: "uk:sports" });
    expect(keyString(key)).toBe("3:uk:sports");
  });
  it("round-trips countries and genres", () => {
    for (const s of ["nav:DK/*", "nav:*/Sports", "nav:DK/Events & PPV", "nav:-/News", "nav:*/*"]) {
      expect(keyString(parseKey(s))).toBe(s);
    }
    expect(parseKey("nav:DK/Sports")).toEqual({ type: "nav", country: "DK", genre: "Sports" });
    expect(parseKey("nav:-/*")).toEqual({ type: "nav", country: "", genre: undefined });
  });
  it("groups feeds everywhere but in provider categories", () => {
    expect(queryFor(parseKey("nav:DK/Sports"))).toEqual({ grouped: true, country: "DK", genre: "Sports" });
    expect(queryFor(parseKey("favorites"))).toEqual({ grouped: true, favorites: true });
    expect(queryFor(parseKey("3:uk"))).toEqual({ sourceId: 3, categoryId: "uk" });
  });
  it("titles lists by country and genre", () => {
    const nav: LiveNav = { countries: [{ code: "DK", name: "Denmark", count: 3 }], genres: ["Sports"], cells: [] };
    expect(titleFor(parseKey("nav:DK/Sports"), nav)).toBe("Denmark · Sports");
    expect(titleFor(parseKey("nav:*/Sports"), nav)).toBe("Sports");
    expect(titleFor(parseKey("nav:DK/*"), nav)).toBe("Denmark");
  });
});
