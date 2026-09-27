import { afterEach, describe, expect, it, vi } from "vitest";
import { ago, channelsLabel, clock, duration, elapsed, initials, languageName, resolutionLabel, seasonsLabel } from "./format";

describe("clock", () => {
  it("shows minutes, and hours when needed", () => {
    expect(clock(65)).toBe("1:05");
    expect(clock(5025)).toBe("1:23:45");
    expect(clock(65, true)).toBe("0:01:05");
  });
  it("never goes negative or breaks on missing values", () => {
    expect(clock(-3)).toBe("0:00");
    expect(clock(null)).toBe("0:00");
    expect(clock(undefined)).toBe("0:00");
  });
});

describe("duration", () => {
  it("rounds to minutes", () => {
    expect(duration(900)).toBe("15m");
    expect(duration(5025)).toBe("1h 24m");
    expect(duration(7200)).toBe("2h");
  });
  it("carries rounded minutes into hours", () => {
    expect(duration(3599)).toBe("1h");
    expect(duration(7170)).toBe("2h");
  });
});

describe("elapsed", () => {
  it("is the played fraction, clamped", () => {
    expect(elapsed(100, 200, 150)).toBe(0.5);
    expect(elapsed(100, 200, 50)).toBe(0);
    expect(elapsed(100, 200, 500)).toBe(1);
    expect(elapsed(200, 200, 200)).toBe(0);
  });
});

describe("ago", () => {
  afterEach(() => vi.useRealTimers());
  it("describes how long ago", () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-09-26T12:00:00Z"));
    const now = Date.parse("2026-09-26T12:00:00Z") / 1000;
    expect(ago(null)).toBe("never");
    expect(ago(now - 30)).toBe("just now");
    expect(ago(now - 5 * 60)).toBe("5 min ago");
    expect(ago(now - 3 * 3600)).toBe("3 h ago");
    expect(ago(now - 30 * 3600)).toBe("yesterday");
    expect(ago(now - 4 * 86400)).toBe("4 days ago");
  });
});

describe("initials", () => {
  it("takes the first letters of two words", () => {
    expect(initials("BBC One")).toBe("BO");
    expect(initials("the #1 hits")).toBe("T1");
    expect(initials("ᴿᴬᵂ")).toBe("ᴿ");
    expect(initials("  ")).toBe("?");
  });
});

describe("resolutionLabel", () => {
  it("names common resolutions", () => {
    expect(resolutionLabel(3840, 2160)).toBe("4K");
    expect(resolutionLabel(1920, 1080)).toBe("1080p");
    expect(resolutionLabel(1920, 800)).toBe("1080p"); // scope
    expect(resolutionLabel(1280, 720)).toBe("720p");
    expect(resolutionLabel(720, 576)).toBe("SD");
    expect(resolutionLabel(null, 1080)).toBeNull();
  });
});

describe("track labels", () => {
  it("names track languages from 2- and 3-letter codes", () => {
    expect(languageName("dan")).toBe("Danish");
    expect(languageName("da")).toBe("Danish");
    expect(languageName("ger")).toBe("German");
    expect(languageName("swe")).toBe("Swedish");
    expect(languageName("und")).toBeNull();
    expect(languageName(null)).toBeNull();
    expect(languageName("qqq")).toBe("QQQ");
  });
  it("labels channels and season coverage", () => {
    expect(channelsLabel(6)).toBe("5.1");
    expect(channelsLabel(2)).toBe("Stereo");
    expect(seasonsLabel([5])).toBe("Season 5");
    expect(seasonsLabel([3, 1, 2, 5])).toBe("Seasons 1–3, 5");
    expect(seasonsLabel([])).toBe("");
  });
});
