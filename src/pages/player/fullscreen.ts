import { useCallback, useEffect, useState } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { inTauri } from "../../lib/bridge";
import { playerWrite } from "../../lib/playerWrite";

async function toggleFullscreen() {
  if (inTauri) {
    const w = getCurrentWindow();
    await w.setFullscreen(!(await w.isFullscreen()));
  } else if (document.fullscreenElement) {
    await document.exitFullscreen();
  } else {
    await document.documentElement.requestFullscreen();
  }
}

async function isFullscreen() {
  return inTauri ? getCurrentWindow().isFullscreen() : !!document.fullscreenElement;
}

/** The window's fullscreen state, a toggle (F, double-click) and a way out. */
export function useFullscreen() {
  const [fullscreen, setFullscreen] = useState(false);
  useEffect(() => void isFullscreen().then(setFullscreen), []);
  const flipFullscreen = useCallback(() => void playerWrite(toggleFullscreen().then(() => isFullscreen().then(setFullscreen))), []);
  /** Leaves fullscreen; true when it was. */
  const leaveFullscreen = useCallback(async () => {
    if (!(await isFullscreen())) return false;
    await toggleFullscreen();
    setFullscreen(false);
    return true;
  }, []);
  return { fullscreen, flipFullscreen, leaveFullscreen };
}
