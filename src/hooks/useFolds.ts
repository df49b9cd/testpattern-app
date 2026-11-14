import { useState } from "react";
import { loadJson, saveJson } from "../lib/storage";

/**
 * Which sections of a side panel are folded, kept per viewer under
 * `storageKey`. Sections never toggled follow `defaultCollapsed`.
 */
export function useFolds(storageKey: string, defaultCollapsed: (name: string) => boolean = () => false) {
  const [folds, setFolds] = useState<Record<string, boolean>>(() => loadJson(storageKey, {}));
  const isCollapsed = (name: string) => folds[name] ?? defaultCollapsed(name);
  const toggle = (name: string) =>
    setFolds((f) => {
      const next = { ...f, [name]: !(f[name] ?? defaultCollapsed(name)) };
      saveJson(storageKey, next);
      return next;
    });
  return { isCollapsed, toggle };
}
