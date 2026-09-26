import { describe, expect, it } from "vitest";
import type { VersionInfo } from "../lib/types";
import { languages, techLine } from "./Versions";

const version = (over: Partial<VersionInfo>): VersionInfo => ({
  sourceId: 1,
  id: "1",
  label: "Apple TV+ · 4K Dolby Vision",
  origin: "web",
  quality: ["4K", "Dolby Vision"],
  selected: false,
  position: 0,
  watched: false,
  seasons: [],
  episodes: 0,
  ...over,
});

describe("version cards", () => {
  it("lists the viewer's languages first, then the rest", () => {
    const subs = ["ar", "bg", "da", "en", "sv", "fi", "no"].map((lang) => ({ lang }));
    expect(languages(subs, ["Danish", "English"])).toBe("Danish, English, Arabic, Bulgarian +3");
    expect(languages([{ lang: "eng" }, { lang: "en" }], [])).toBe("English");
    // tracks without a language still count
    expect(languages([{ lang: null }, { lang: "und" }], [])).toBe("2 tracks");
    expect(languages([], [])).toBeNull();
  });
  it("describes picture and sound from the file, else the provider", () => {
    const fromFile = version({
      tracks: { video: { codec: "hevc", width: 3840, height: 1920, hdr: true }, audio: [{ channels: 2 }, { channels: 6 }] },
      duration: 3900,
      ext: "mkv",
    });
    expect(techLine(fromFile)).toBe("4K · HEVC · HDR · 5.1 · 1h 5m · MKV");
    const fromProvider = version({ video: { codec: "h264", width: 1920, height: 1080, hdr: false }, audio: { channels: 2, hdr: false } });
    expect(techLine(fromProvider)).toBe("1080p · H.264 · Stereo");
    expect(techLine(version({ tracks: { unavailable: true } }))).toBe("");
  });
});
