import { useEffect, useMemo, useRef } from "react";

/**
 * One pending timeout: `start` replaces it, unmounting clears it (and later
 * `start`s do nothing), so a callback never runs after its component left.
 */
export function useTimeout() {
  const id = useRef(0);
  const gone = useRef(false);
  useEffect(() => {
    gone.current = false;
    return () => {
      gone.current = true;
      window.clearTimeout(id.current);
    };
  }, []);
  return useMemo(
    () => ({
      start(fn: () => void, ms: number) {
        window.clearTimeout(id.current);
        if (!gone.current) id.current = window.setTimeout(fn, ms);
      },
      clear() {
        window.clearTimeout(id.current);
      },
    }),
    [],
  );
}
