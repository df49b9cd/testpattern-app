import { useCallback, useEffect, useRef, useState } from "react";
import { usePlayer } from "../../stores/player";

const HIDE_AFTER = 3000;

/**
 * The player chrome shows on activity (`poke`) and hides again after a few
 * quiet seconds — not while paused. `onHide` runs as it hides (closes menus).
 */
export function useChromeAutoHide(onHide: () => void) {
  const [chrome, setChrome] = useState(true);
  const timer = useRef(0);
  const hide = useRef(onHide);
  useEffect(() => {
    hide.current = onHide;
  });
  const poke = useCallback(() => {
    setChrome(true);
    window.clearTimeout(timer.current);
    timer.current = window.setTimeout(() => {
      if (!usePlayer.getState().props.pause) {
        setChrome(false);
        hide.current();
      }
    }, HIDE_AFTER);
  }, []);
  useEffect(() => {
    poke();
    return () => window.clearTimeout(timer.current);
  }, [poke]);
  return { chrome, poke };
}
