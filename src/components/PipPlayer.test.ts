import { describe, expect, it } from "vitest";
import { pipClipPath, pipRect } from "./PipPlayer";

describe("picture-in-picture geometry", () => {
  it("sits in the bottom-right corner", () => {
    expect(pipRect(1440, 900)).toEqual({ left: 1016, top: 651, width: 400, height: 225 });
  });
  it("cuts exactly that rectangle (with rounded corners) out of the UI", () => {
    const path = pipClipPath(1440, 900);
    expect(path.startsWith('path(evenodd, "M0 0 H1440 V900 H0 Z ')).toBe(true);
    // hole: from x+r along the top edge to the right edge, corners as arcs
    expect(path).toContain("M1032 651 H1400 A16 16 0 0 1 1416 667");
    expect(path.match(/A16 16/g)).toHaveLength(4);
  });
});
