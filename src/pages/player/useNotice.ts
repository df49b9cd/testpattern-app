import { useCallback, useState } from "react";
import { useTimeout } from "../../hooks/useTimeout";

/** How long a player notice ("Volume 80", "Muted") stays up (ms). */
const NOTICE_DURATION = 4000;

/** A short-lived message over the video; `say` replaces the one showing. */
export function useNotice() {
  const [notice, setNotice] = useState<string | null>(null);
  const timer = useTimeout();
  const say = useCallback(
    (text: string) => {
      setNotice(text);
      timer.start(() => setNotice(null), NOTICE_DURATION);
    },
    [timer],
  );
  return { notice, say };
}
