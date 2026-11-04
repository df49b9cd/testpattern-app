import { describe, expect, it } from "vitest";
import { itemKey, sameItem } from "./item";

describe("item identity", () => {
  it("compares source and id", () => {
    expect(sameItem({ sourceId: 1, id: "a" }, { sourceId: 1, id: "a" })).toBe(true);
    expect(sameItem({ sourceId: 1, id: "a" }, { sourceId: 2, id: "a" })).toBe(false);
    expect(sameItem({ sourceId: 1, id: "a" }, null)).toBe(false);
    expect(sameItem(undefined, undefined)).toBe(false);
  });
  it("builds one key format", () => {
    expect(itemKey({ sourceId: 3, id: "uk:1" })).toBe("3:uk:1");
  });
});
