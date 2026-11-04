import { useQuery, type QueryClient } from "@tanstack/react-query";
import { api } from "./api";
import type { Settings } from "./types";

/** Query key of the settings cache (shared by every reader and writer). */
export const SETTINGS_KEY = ["settings"] as const;

export function useSettings() {
  return useQuery({ queryKey: SETTINGS_KEY, queryFn: api.settings });
}

/**
 * Writes one setting: the cache is patched right away so every view of it
 * (player chrome, Settings page) stays in sync, and a failed write rolls the
 * patch back instead of diverging from the backend. Rejects on failure.
 */
export async function writeSetting<K extends keyof Settings>(qc: QueryClient, key: K, value: Settings[K]): Promise<void> {
  const before = qc.getQueryData<Settings>(SETTINGS_KEY)?.[key];
  qc.setQueryData<Settings>(SETTINGS_KEY, (cur) => ({ ...cur, [key]: value }));
  try {
    await api.setSetting(key, value);
  } catch (e) {
    qc.setQueryData<Settings>(SETTINGS_KEY, (cur) => ({ ...cur, [key]: before }));
    throw e;
  }
}
