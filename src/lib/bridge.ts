import { invoke as tauriInvoke } from "@tauri-apps/api/core";
import { listen as tauriListen, type EventCallback, type UnlistenFn } from "@tauri-apps/api/event";

/** True inside the desktop app (Tauri webview). */
export const inTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

/**
 * In a plain browser during development (e.g. the Claude desktop preview
 * pane) commands are forwarded to a running debug build of the app through
 * its devtools server (src-tauri/src/devtools.rs). No native video there.
 */
export const browserPreview = !inTauri && import.meta.env.DEV;
export const BRIDGE = "http://127.0.0.1:17777";

export async function invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  if (inTauri) return tauriInvoke<T>(cmd, args);
  if (!browserPreview) throw "testpattern has to run inside its desktop app";
  let r: Response;
  try {
    r = await fetch(`${BRIDGE}/invoke`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ cmd, args: args ?? {} }),
    });
  } catch {
    throw "Preview mode needs a running debug build (scripts/headless.sh start)";
  }
  const text = await r.text();
  if (!r.ok) throw text; // same shape as Tauri: errors are plain strings
  return JSON.parse(text) as T;
}

/** Backend events; a no-op in browser preview mode. */
export function listen<T>(event: string, cb: EventCallback<T>): Promise<UnlistenFn> {
  if (inTauri) return tauriListen<T>(event, cb);
  return Promise.resolve(() => {});
}
