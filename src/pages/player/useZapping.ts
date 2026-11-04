import { useCallback, useRef, useState } from "react";
import { channelNowPlaying } from "../../lib/play";
import type { Channel } from "../../lib/types";
import { usePlayer } from "../../stores/player";
import { zapIndex, zapStep } from "./logic";

/** Live zapping: up/down through the channel list, or a typed channel number. */
export function useZapping(poke: () => void) {
  const now = usePlayer((s) => s.now);
  const zapList = now?.zapList ?? [];
  const index = zapIndex(zapList, now);
  const zapTo = useCallback(
    (c: Channel) => {
      void usePlayer.getState().play(channelNowPlaying(c, usePlayer.getState().now?.zapList));
      poke();
    },
    [poke],
  );
  const zap = useCallback(
    (dir: number) => {
      if (!zapList.length) return;
      zapTo(zapList[zapStep(index, dir, zapList.length)]);
    },
    [zapList, index, zapTo],
  );

  // number keys: up to 4 digits, tuned once typing pauses
  const [digits, setDigits] = useState("");
  const typed = useRef("");
  const digitTimer = useRef(0);
  const enterDigit = useCallback(
    (d: string) => {
      const next = (typed.current + d).slice(-4);
      typed.current = next;
      setDigits(next);
      window.clearTimeout(digitTimer.current);
      digitTimer.current = window.setTimeout(() => {
        const target = zapList.find((c) => String(c.num) === next);
        if (target) zapTo(target);
        typed.current = "";
        setDigits("");
      }, 1400);
    },
    [zapList, zapTo],
  );

  return { zapList, zap, zapTo, digits, enterDigit };
}
