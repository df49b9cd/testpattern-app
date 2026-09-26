import { openUrl } from "@tauri-apps/plugin-opener";
import { inTauri } from "./bridge";

/** Opens a web page in the user's browser. */
export async function openExternal(url: string) {
  if (inTauri) await openUrl(url);
  else window.open(url, "_blank", "noopener");
}

export const youtubeUrl = (idOrUrl: string) =>
  /^https?:\/\//.test(idOrUrl) ? idOrUrl : `https://www.youtube.com/watch?v=${encodeURIComponent(idOrUrl)}`;
