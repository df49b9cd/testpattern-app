import { describe, expect, it } from "vitest";
import { pick } from "./useSpatialNav";

const rect = (left: number, top: number, width = 100, height = 50) =>
  ({ left, top, width, height, right: left + width, bottom: top + height }) as DOMRect;
const el = (name: string, r: DOMRect) => ({ name, getBoundingClientRect: () => r }) as unknown as HTMLElement & { name: string };

describe("spatial navigation", () => {
  // a 3×2 grid of 100×50 tiles with 20px gaps
  const tiles = [0, 1, 2].flatMap((col) => [0, 1].map((row) => el(`${col},${row}`, rect(col * 120, row * 70))));
  const from = (col: number, row: number) => rect(col * 120, row * 70);
  const name = (e: HTMLElement | null) => (e as unknown as { name: string } | null)?.name ?? null;

  it("moves to the neighbour in the pressed direction", () => {
    expect(name(pick(from(1, 0), "right", tiles))).toBe("2,0");
    expect(name(pick(from(1, 0), "left", tiles))).toBe("0,0");
    expect(name(pick(from(1, 0), "down", tiles))).toBe("1,1");
    expect(name(pick(from(1, 1), "up", tiles))).toBe("1,0");
  });
  it("prefers the same row over a closer diagonal", () => {
    const wide = [el("row", rect(400, 0)), el("diagonal", rect(130, 60))];
    expect(name(pick(rect(0, 0), "right", wide))).toBe("row");
  });
  it("stops at the edge", () => {
    expect(pick(from(2, 0), "right", tiles)).toBeNull();
    expect(pick(from(0, 0), "up", tiles)).toBeNull();
  });
});
