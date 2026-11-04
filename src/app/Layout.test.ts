import { describe, expect, it } from "vitest";
import { offPageKey } from "./Layout";

const key = (k: string, mods: Partial<{ ctrlKey: boolean; metaKey: boolean; altKey: boolean }> = {}) => ({
  key: k,
  ctrlKey: false,
  metaKey: false,
  altKey: false,
  ...mods,
});

describe("offPageKey (Layout off-page gating, PL-101)", () => {
  it.each([
    ["Space on the body plays", key(" "), { pathname: "/live", onBody: true, playing: true }, "play"],
    ["K on the body plays", key("K"), { pathname: "/live", onBody: true, playing: true }, "play"],
    ["Space belongs to a focused control", key(" "), { pathname: "/live", onBody: false, playing: true }, null],
    ["M mutes even with a control focused", key("m"), { pathname: "/live", onBody: false, playing: true }, "mute"],
    ["the player page handles its own keys", key(" "), { pathname: "/player", onBody: true, playing: true }, null],
    ["modifiers are not ours", key("m", { ctrlKey: true }), { pathname: "/live", onBody: true, playing: true }, null],
    ["nothing plays", key(" "), { pathname: "/live", onBody: true, playing: false }, null],
    ["other keys", key("x"), { pathname: "/live", onBody: true, playing: true }, null],
  ] as const)("%s", (_name, e, opts, expected) => {
    expect(offPageKey(e, opts)).toBe(expected);
  });
});
