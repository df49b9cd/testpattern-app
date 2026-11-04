import { errorMessage } from "./api";

/**
 * Fire-and-forget backend calls (mpv properties, TV controls, background
 * settings): a failure is logged — and handed to `onError` where the caller
 * has a place to say it — instead of vanishing in an empty `.catch`. Never
 * rejects; resolves true when the call landed.
 */
export function playerWrite(p: Promise<unknown>, onError?: (message: string) => void): Promise<boolean> {
  return p.then(
    () => true,
    (e: unknown) => {
      console.warn("player call failed:", e);
      onError?.(errorMessage(e));
      return false;
    },
  );
}
