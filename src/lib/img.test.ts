import { describe, expect, it, vi } from "vitest";

vi.stubGlobal("window", { devicePixelRatio: 2 });
const { img } = await import("./img");

describe("artwork URLs", () => {
  it("goes through the artwork proxy, sized for the screen", () => {
    const url = new URL(img("https://cdn.example/poster.jpg?v=1", 170)!);
    expect(url.searchParams.get("u")).toBe("https://cdn.example/poster.jpg?v=1");
    expect(url.searchParams.get("w")).toBe("340"); // 170 css px × dpr 2
  });
  it("skips missing or non-http artwork", () => {
    expect(img(null)).toBeUndefined();
    expect(img("")).toBeUndefined();
    expect(img("data:image/png;base64,xyz")).toBeUndefined();
    expect(new URL(img("http://a/b.png")!).searchParams.has("w")).toBe(false);
  });
});
