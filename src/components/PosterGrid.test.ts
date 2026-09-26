import { describe, expect, it } from "vitest";
import { gridLayout } from "./PosterGrid";

describe("poster grid layout", () => {
  it("fits as many columns as the width allows", () => {
    const g = gridLayout(1200, 168);
    expect(g.cols).toBe(6);
    expect(g.itemWidth).toBeCloseTo(168.33, 2);
    expect(g.rowHeight).toBeCloseTo(168.33 * 1.5 + 46 + 22, 1);
  });
  it("keeps at least two columns", () => {
    expect(gridLayout(300, 168).cols).toBe(2);
  });
  it("never produces negative sizes before the grid is measured", () => {
    const g = gridLayout(0, 168);
    expect(g.itemWidth).toBe(168);
    expect(g.rowHeight).toBeGreaterThan(0);
  });
});
