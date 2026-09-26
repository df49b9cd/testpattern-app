import { useEffect, type RefObject } from "react";
import { setVideoViewport } from "../stores/player";

/**
 * Keeps the native video positioned inside `ref`'s box (live previews).
 * Resets to fullscreen when disabled or unmounted.
 */
export function useVideoViewport(ref: RefObject<HTMLElement | null>, enabled: boolean) {
  useEffect(() => {
    const el = ref.current;
    if (!enabled || !el) return;
    let frame = 0;
    let last = "";
    const update = () => {
      cancelAnimationFrame(frame);
      frame = requestAnimationFrame(() => {
        const r = el.getBoundingClientRect();
        const key = `${r.left},${r.top},${r.width},${r.height},${window.innerWidth},${window.innerHeight}`;
        if (key === last) return;
        last = key;
        void setVideoViewport(r);
      });
    };
    update();
    const ro = new ResizeObserver(update);
    ro.observe(el);
    window.addEventListener("resize", update);
    return () => {
      cancelAnimationFrame(frame);
      ro.disconnect();
      window.removeEventListener("resize", update);
      void setVideoViewport(null);
    };
  }, [ref, enabled]);
}
