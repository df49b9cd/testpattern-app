import { useEffect, useRef } from "react";

/**
 * One keyboard shortcut: the keys (`KeyboardEvent.key`) it takes, whether it
 * applies right now, and what it does — returning false leaves the key
 * unhandled (no preventDefault, no chrome poke).
 */
export interface KeyBinding {
  keys: readonly string[];
  when?: boolean;
  run: (e: KeyboardEvent) => void | false;
}

/** The first binding that takes `key` right now; later ones are fallbacks. */
export function findBinding(bindings: readonly KeyBinding[], key: string): KeyBinding | undefined {
  return bindings.find((b) => b.when !== false && b.keys.includes(key));
}

/** Window-wide shortcuts (not while typing in an input); `onHandled` after each. */
export function usePlayerKeys(bindings: readonly KeyBinding[], onHandled: () => void) {
  const latest = useRef({ bindings, onHandled });
  useEffect(() => {
    latest.current = { bindings, onHandled };
  });
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.target as HTMLElement)?.tagName === "INPUT") return;
      const b = findBinding(latest.current.bindings, e.key);
      if (!b || b.run(e) === false) return;
      e.preventDefault();
      latest.current.onHandled();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);
}
