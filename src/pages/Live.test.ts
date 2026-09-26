import { describe, expect, it } from "vitest";
import { keyString, parseKey } from "./Live";

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
});
